import { useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import {
  Badge, Button, Card, ConfidenceBadge, EmptyState, ErrorBanner, Input, Loading, Proportions, Stat, StatusBadge, Table, Tabs,
  formatBytes, formatDuration, formatRelative, shortSha,
} from "@usie/design-system";
import type { Repository, Version } from "@usie/sdk";
import { keyLabel } from "@usie/sdk";
import { api } from "../api";
import { useCan } from "../auth";
import { useAsync } from "../hooks";
import { PageHeader } from "../App";
import { RepoLinks, VersionPicker, VersionStatusLine, sourceLabel, useDocumentTitle, useRepoVersion } from "../components/shared";

type Tab = "analysis" | "versions" | "dependencies" | "history" | "changes";

export function RepositoryPage() {
  const { repoId = "" } = useParams();
  const { repo, versions, version, select } = useRepoVersion(repoId);
  const [tab, setTab] = useState<Tab>("analysis");
  useDocumentTitle(repo.data?.name ?? "Repository");

  if (repo.error) return <div className="p-6"><ErrorBanner error={repo.error} onRetry={repo.reload} /></div>;
  if (!repo.data || !versions.data) return <Loading />;
  const latest = versions.data[0];

  return (
    <>
      <PageHeader
        title={repo.data.name}
        subtitle={<span className="font-mono text-xs">{sourceLabel(repo.data)}</span>}
        actions={<RepoActions repo={repo.data} onQueued={() => { versions.reload(); repo.reload(); }} />}
      />
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-6 py-2">
        <RepoLinks repoId={repoId} versionId={version?.id} active="overview" />
        <VersionPicker versions={versions.data} value={version?.id} onChange={select} />
      </div>
      <div className="space-y-6 p-6">
        {latest && latest.id !== version?.id && <LatestBanner v={latest} />}
        <Tabs<Tab>
          value={tab}
          onChange={setTab}
          items={[
            { value: "analysis", label: "Analysis" },
            { value: "versions", label: `Versions (${versions.data.length})` },
            { value: "dependencies", label: "Dependencies" },
            { value: "history", label: "History" },
            { value: "changes", label: "Changes" },
          ]}
        />
        {tab === "versions" && <Versions versions={versions.data} repoId={repoId} />}
        {tab === "history" && <History repo={repo.data} />}
        {!version && tab !== "versions" && tab !== "history" && (
          <Card>
            <EmptyState title="No analyzed version yet">
              {latest ? <>The latest version is <StatusBadge status={latest.status} />{latest.error && <span className="mt-2 block text-danger">{latest.error}</span>}</> : "Start an analysis to see results."}
            </EmptyState>
          </Card>
        )}
        {version && tab === "analysis" && <Analysis v={version} repoId={repoId} />}
        {version && tab === "dependencies" && <Dependencies versionId={version.id} />}
        {version && tab === "changes" && <Changes v={version} versions={versions.data} repoId={repoId} />}
      </div>
    </>
  );
}

function LatestBanner({ v }: { v: Version }) {
  if (v.status === "ready") return null;
  const tone = v.status === "failed" ? "border-danger/40 bg-danger/10 text-danger" : "border-info/40 bg-info/10 text-info";
  return (
    <div role="status" className={`rounded-md border px-3 py-2 text-sm ${tone}`}>
      Version {v.sequence} is <strong>{v.status}</strong>
      {v.error && <>: {v.error}</>}
      {(v.status === "pending" || v.status === "analyzing") && <> — this page refreshes automatically. <Link className="underline" to="/jobs">View jobs</Link></>}
    </div>
  );
}

function RepoActions({ repo, onQueued }: { repo: Repository; onQueued: () => void }) {
  const canAnalyze = useCan("member");
  const canDelete = useCan("admin");
  const navigate = useNavigate();
  const fileRef = useRef<HTMLInputElement>(null);
  const [ref, setRef] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await fn();
      onQueued();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="flex flex-wrap items-center gap-2">
      {error ? <span className="max-w-xs text-xs text-danger" role="alert">{(error as Error).message}</span> : null}
      {canAnalyze && repo.source_kind === "git" && (
        <Input aria-label="Branch, tag or commit to analyze" placeholder={repo.default_ref || "default branch"} value={ref} onChange={(e) => setRef(e.target.value)} className="h-9 w-40" />
      )}
      {canAnalyze && repo.source_kind !== "upload" && (
        <Button variant="primary" busy={busy} onClick={() => run(() => api.analyzeRepository(repo.id, ref.trim() || undefined))}>Analyze now</Button>
      )}
      {canAnalyze && repo.source_kind === "upload" && (
        <>
          <input ref={fileRef} type="file" accept=".zip,application/zip" className="hidden" onChange={(e) => { const f = e.target.files?.[0]; if (f) void run(() => api.uploadArchive(repo.id, f)); e.target.value = ""; }} />
          <Button variant="primary" busy={busy} onClick={() => fileRef.current?.click()}>Upload new version</Button>
        </>
      )}
      {canDelete && (
        <Button
          variant="danger"
          onClick={() => {
            if (window.confirm(`Delete "${repo.name}" and all of its analysis results? This cannot be undone.`)) {
              void api.deleteRepository(repo.id).then(() => navigate("/repositories"), setError);
            }
          }}
        >
          Delete
        </Button>
      )}
    </div>
  );
}

const statusColors: Record<string, string> = {
  parsed: "bg-ok", parsed_with_errors: "bg-warn", skipped: "bg-muted", unsupported: "bg-border", failed: "bg-danger",
};

function Analysis({ v, repoId }: { v: Version; repoId: string }) {
  const s = v.stats;
  const refs = s.references_by_outcome ?? {};
  const conf = s.graph?.edges_by_confidence ?? {};
  const byLang = Object.entries(s.files_by_language ?? {}).sort((a, b) => b[1] - a[1]);
  const totalRefs = Object.values(refs).reduce((a, b) => a + b, 0);
  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center gap-3 text-sm">
        <VersionStatusLine v={v} />
        {v.commit_sha && <Badge>commit {shortSha(v.commit_sha)}</Badge>}
        {v.branch && <Badge>branch {v.branch}</Badge>}
        {(s.build_systems ?? []).map((b) => <Badge key={b} tone="accent">{b}</Badge>)}
      </div>
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-6">
        <Stat label="Files" value={(s.files ?? 0).toLocaleString()} />
        <Stat label="Lines" value={(s.lines ?? 0).toLocaleString()} />
        <Stat label="Size" value={formatBytes(s.bytes ?? 0)} />
        <Stat label="Symbols" value={(s.symbols ?? 0).toLocaleString()} />
        <Stat label="Relationships" value={(s.graph?.edges ?? 0).toLocaleString()} />
        <Stat label="Declared deps" value={(s.declared_dependencies ?? 0).toLocaleString()} />
      </div>
      <div className="grid gap-6 xl:grid-cols-2">
        <Card title="Parsing">
          <Proportions items={Object.entries(s.files_by_parse_status ?? {}).map(([k, n]) => ({ label: k.replace(/_/g, " "), value: n, className: statusColors[k] ?? "bg-muted" }))} />
          <p className="mt-3 text-xs text-muted">
            Python, JavaScript and TypeScript are parsed. Other languages are inventoried only. Vendored and generated files are skipped.
            Parse cache: {s.parse_cache_hits ?? 0} reused, {s.parse_cache_misses ?? 0} parsed in this run.{" "}
            <Link className="text-accent hover:underline" to={`/repositories/${repoId}/code?version=${v.id}`}>Browse files</Link>
          </p>
        </Card>
        <Card title="Relationship confidence">
          <Proportions
            items={[
              { label: "confirmed", value: conf.confirmed ?? 0, className: "bg-confirmed" },
              { label: "probable", value: conf.probable ?? 0, className: "bg-probable" },
              { label: "inferred", value: conf.inferred ?? 0, className: "bg-inferred" },
            ]}
          />
          <p className="mt-3 text-xs text-muted">
            Every relationship records the rule that produced it. <ConfidenceBadge level="inferred" /> relationships are heuristic and should be verified.
          </p>
        </Card>
        <Card title="References examined by the resolver">
          <Proportions
            items={[
              { label: "resolved in repository", value: refs.resolved ?? 0, className: "bg-ok" },
              { label: "external package", value: refs.external ?? 0, className: "bg-info" },
              { label: "language builtin", value: refs.builtin ?? 0, className: "bg-muted" },
              { label: "unresolved", value: refs.unresolved ?? 0, className: "bg-warn" },
            ]}
          />
          <p className="mt-3 text-xs text-muted">
            {totalRefs.toLocaleString()} call, instantiation, decorator, JSX, import and inheritance sites. Unresolved sites are kept with the reason
            (for example, a receiver whose type is not statically known) rather than guessed.
          </p>
        </Card>
        <Card title="Languages">
          <Table>
            <tbody>
              {byLang.map(([lang, n]) => (
                <tr key={lang}>
                  <td>{lang}</td>
                  <td className="text-right tabular-nums">{n.toLocaleString()} files</td>
                </tr>
              ))}
            </tbody>
          </Table>
        </Card>
      </div>
      <Card title="Run details">
        <dl className="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-2 lg:grid-cols-3">
          <Detail k="Changes vs previous ready version" v={s.changes ? `+${s.changes.added} added, ${s.changes.modified} modified, ${s.changes.removed} removed, ${s.changes.unchanged} unchanged` : "—"} />
          <Detail k="Tree hash" v={<span className="font-mono text-xs">{v.tree_hash.slice(0, 16)}…</span>} />
          <Detail k="Engine" v={s.engine_version ?? "—"} />
          <Detail k="Skipped by ignore rules" v={String(s.skipped_ignored ?? 0)} />
          <Detail k="Symlinks not followed" v={String(s.skipped_symlinks ?? 0)} />
          <Detail k="Stage durations" v={Object.entries(s.durations_ms ?? {}).map(([k, ms]) => `${k} ${formatDuration(ms)}`).join(", ") || "—"} />
        </dl>
        {(s.manifest_problems ?? []).length > 0 && (
          <div className="mt-4">
            <h3 className="text-sm font-medium text-warn">Manifests that could not be read</h3>
            <ul className="mt-1 space-y-1 text-xs">
              {s.manifest_problems!.map((p) => <li key={p.path}><span className="font-mono">{p.path}</span>: {p.message}</li>)}
            </ul>
          </div>
        )}
        {(s.dropped_edges ?? 0) > 0 && (
          <p className="mt-3 text-sm text-warn">{s.dropped_edges} relationships referenced unknown nodes and were not stored.</p>
        )}
      </Card>
    </div>
  );
}

function Detail({ k, v }: { k: string; v: React.ReactNode }) {
  return (
    <div>
      <dt className="text-xs text-muted">{k}</dt>
      <dd>{v}</dd>
    </div>
  );
}

function Versions({ versions, repoId }: { versions: Version[]; repoId: string }) {
  if (versions.length === 0) return <Card><EmptyState title="No versions yet" /></Card>;
  return (
    <Card padded={false}>
      <Table>
        <thead>
          <tr><th>Version</th><th>Status</th><th>Commit</th><th>Files</th><th>Created</th><th /></tr>
        </thead>
        <tbody>
          {versions.map((v) => (
            <tr key={v.id}>
              <td className="font-medium">v{v.sequence}</td>
              <td>
                <StatusBadge status={v.status} />
                {v.error && <div className="mt-1 max-w-md text-xs text-danger">{v.error}</div>}
              </td>
              <td className="font-mono text-xs">{shortSha(v.commit_sha) || "—"}{v.branch && <span className="text-muted"> ({v.branch})</span>}</td>
              <td className="tabular-nums">{v.stats.files?.toLocaleString() ?? "—"}</td>
              <td className="text-xs text-muted">{formatRelative(v.created_at)}</td>
              <td className="text-right">
                {v.status === "ready" && <Link className="text-sm text-accent hover:underline" to={`/repositories/${repoId}?version=${v.id}`}>Open</Link>}
              </td>
            </tr>
          ))}
        </tbody>
      </Table>
    </Card>
  );
}

function Dependencies({ versionId }: { versionId: string }) {
  const deps = useAsync(() => api.dependencies(versionId), [versionId]);
  if (deps.error) return <ErrorBanner error={deps.error} onRetry={deps.reload} />;
  if (!deps.data) return <Loading />;
  if (deps.data.length === 0) return <Card><EmptyState title="No declared dependencies found">Supported manifests: package.json, pyproject.toml, requirements*.txt, go.mod, Cargo.toml and pom.xml.</EmptyState></Card>;
  return (
    <Card padded={false} title={`${deps.data.length} declared dependencies`}>
      <Table>
        <thead><tr><th>Ecosystem</th><th>Name</th><th>Requirement</th><th>Scope</th><th>Manifest</th></tr></thead>
        <tbody>
          {deps.data.map((d) => (
            <tr key={`${d.manifest_path}|${d.ecosystem}|${d.name}|${d.scope}`}>
              <td><Badge>{d.ecosystem}</Badge></td>
              <td className="font-mono text-xs">{d.name}</td>
              <td className="font-mono text-xs text-muted">{d.requirement || "any"}</td>
              <td className="text-xs">{d.scope}</td>
              <td className="font-mono text-xs text-muted">{d.manifest_path}</td>
            </tr>
          ))}
        </tbody>
      </Table>
    </Card>
  );
}

function History({ repo }: { repo: Repository }) {
  const data = useAsync(async () => {
    const [commits, branches] = await Promise.all([api.listCommits(repo.id, 200), api.listBranches(repo.id)]);
    return { commits, branches };
  }, [repo.id]);
  if (repo.source_kind !== "git") return <Card><EmptyState title="No history">Commit history is recorded for git repositories only.</EmptyState></Card>;
  if (data.error) return <ErrorBanner error={data.error} onRetry={data.reload} />;
  if (!data.data) return <Loading />;
  return (
    <div className="grid gap-6 xl:grid-cols-[2fr_1fr]">
      <Card title="Commits" padded={false}>
        {data.data.commits.length === 0 ? <EmptyState title="No commits recorded yet" /> : (
          <Table>
            <tbody>
              {data.data.commits.map((c) => (
                <tr key={c.sha}>
                  <td className="font-mono text-xs">{shortSha(c.sha)}</td>
                  <td className="max-w-md truncate" title={c.subject}>{c.subject}</td>
                  <td className="text-xs text-muted">{c.author_name}</td>
                  <td className="whitespace-nowrap text-xs text-muted">{formatRelative(c.committed_at)}</td>
                </tr>
              ))}
            </tbody>
          </Table>
        )}
        <p className="border-t border-border px-3 py-2 text-xs text-muted">Clones are shallow; history is limited to the most recent commits fetched.</p>
      </Card>
      <Card title="Branches" padded={false}>
        {data.data.branches.length === 0 ? <EmptyState title="No branches recorded yet" /> : (
          <Table>
            <tbody>
              {data.data.branches.map((b) => (
                <tr key={b.name}>
                  <td className="font-mono text-xs">{b.name} {b.is_default && <Badge tone="accent">default</Badge>}</td>
                  <td className="font-mono text-xs text-muted">{shortSha(b.head_sha)}</td>
                </tr>
              ))}
            </tbody>
          </Table>
        )}
      </Card>
    </div>
  );
}

function Changes({ v, versions, repoId }: { v: Version; versions: Version[]; repoId: string }) {
  const candidates = versions.filter((x) => x.status === "ready" && x.id !== v.id);
  const [base, setBase] = useState(v.parent_version_id && candidates.some((c) => c.id === v.parent_version_id) ? v.parent_version_id : candidates[0]?.id ?? "");
  const diff = useAsync(() => (base ? api.graphDiff(v.id, base, 300) : Promise.resolve(undefined)), [v.id, base]);
  if (candidates.length === 0) return <Card><EmptyState title="Nothing to compare">At least two analyzed versions are needed.</EmptyState></Card>;
  const d = diff.data?.diff;
  const graphLink = (key: string) => `/repositories/${repoId}/graph?version=${v.id}&key=${encodeURIComponent(key)}`;
  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2 text-sm">
        Compare v{v.sequence} against
        <VersionPicker versions={candidates} value={base} onChange={setBase} />
      </div>
      <ErrorBanner error={diff.error} onRetry={diff.reload} />
      {!d ? <Loading /> : (
        <>
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-5">
            <Stat label="Nodes added" value={d.nodes_added} />
            <Stat label="Nodes removed" value={d.nodes_removed} />
            <Stat label="Signatures changed" value={d.nodes_changed} hint="Kind, signature or export status changed. Moved code is not counted." />
            <Stat label="Relationships added" value={d.edges_added} />
            <Stat label="Relationships removed" value={d.edges_removed} />
          </div>
          {d.truncated && <p className="text-xs text-muted">Lists below are samples; counts are exact.</p>}
          <div className="grid gap-6 xl:grid-cols-3">
            {([["Added", d.added], ["Removed", d.removed], ["Changed", d.changed]] as const).map(([title, nodes]) => (
              <Card key={title} title={`${title} (${nodes.length}${d.truncated ? "+" : ""})`} padded={false}>
                {nodes.length === 0 ? <p className="px-3 py-2 text-sm text-muted">None.</p> : (
                  <ul className="max-h-80 divide-y divide-border overflow-auto text-sm">
                    {nodes.map((n) => (
                      <li key={n.key} className="flex items-center justify-between gap-2 px-3 py-1.5">
                        {title === "Removed" ? <span className="truncate" title={n.key}>{keyLabel(n.key)}</span> : <Link className="truncate text-accent hover:underline" to={graphLink(n.key)} title={n.key}>{keyLabel(n.key)}</Link>}
                        <Badge>{n.kind}</Badge>
                      </li>
                    ))}
                  </ul>
                )}
              </Card>
            ))}
          </div>
          <div className="grid gap-6 xl:grid-cols-2">
            {([["Relationships added", d.added_edges], ["Relationships removed", d.removed_edges]] as const).map(([title, edges]) => (
              <Card key={title} title={title} padded={false}>
                {edges.length === 0 ? <p className="px-3 py-2 text-sm text-muted">None.</p> : (
                  <Table className="max-h-80 overflow-auto">
                    <tbody>
                      {edges.filter((e) => e.kind !== "CONTAINS").slice(0, 200).map((e) => (
                        <tr key={`${e.src}|${e.dst}|${e.kind}`}>
                          <td className="max-w-[14rem] truncate text-xs" title={e.src}>{keyLabel(e.src)}</td>
                          <td className="font-mono text-xs">{e.kind}</td>
                          <td className="max-w-[14rem] truncate text-xs" title={e.dst}>{keyLabel(e.dst)}</td>
                          <td><ConfidenceBadge level={e.confidence} /></td>
                        </tr>
                      ))}
                    </tbody>
                  </Table>
                )}
              </Card>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
