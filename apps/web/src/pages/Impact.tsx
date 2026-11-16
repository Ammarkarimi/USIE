import { type FormEvent, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { Badge, Button, Card, ConfidenceBadge, EmptyState, ErrorBanner, Field, Loading, Select, Spinner, Table, Tabs, cx } from "@usie/design-system";
import type { Impact, ImpactNode, ImpactRequest, RiskFactor, Version } from "@usie/sdk";
import { keyLabel } from "@usie/sdk";
import { api } from "../api";
import { PageHeader } from "../App";
import { RepoLinks, VersionPicker, useDocumentTitle, useRepoVersion } from "../components/shared";

type Mode = "symbols" | "diff" | "compare";

/** Symbol, diff or version comparison → dependents, tests and an explained risk score. */
export function ImpactPage() {
  const { repoId = "" } = useParams();
  const { repo, versions, version, select } = useRepoVersion(repoId);
  const [params] = useSearchParams();
  const initialKey = params.get("key") ?? "";
  const [mode, setMode] = useState<Mode>(initialKey ? "symbols" : "diff");
  const [symbols, setSymbols] = useState(initialKey);
  const [diff, setDiff] = useState("");
  const [base, setBase] = useState("");
  const [depth, setDepth] = useState(3);
  const [minConfidence, setMinConfidence] = useState<"confirmed" | "probable" | "inferred">("probable");
  const [result, setResult] = useState<Impact | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  useDocumentTitle(`Impact · ${repo.data?.name ?? ""}`);

  if (repo.error) return <div className="p-6"><ErrorBanner error={repo.error} /></div>;
  if (!repo.data || !versions.data) return <Loading />;
  const ready = versions.data.filter((v) => v.status === "ready");
  const others = ready.filter((v) => v.id !== version?.id);

  const run = async (e: FormEvent) => {
    e.preventDefault();
    if (!version) return;
    const req: ImpactRequest = { depth, min_confidence: minConfidence };
    if (mode === "symbols") req.symbols = symbols.split(/\s+/).filter(Boolean);
    if (mode === "diff") req.diff = diff;
    if (mode === "compare") req.base_version_id = base;
    setBusy(true);
    setError(null);
    try {
      setResult((await api.impact(version.id, req)).impact);
    } catch (err) {
      setResult(null);
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  const canRun =
    !!version && !busy && ((mode === "symbols" && symbols.trim() !== "") || (mode === "diff" && diff.trim() !== "") || (mode === "compare" && base !== ""));

  return (
    <div className="flex h-full flex-col">
      <PageHeader
        title={repo.data.name}
        subtitle="What a change affects: the declarations it touches, everything that depends on them through resolved relationships, and the tests that reach them."
      />
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-6 py-2">
        <RepoLinks repoId={repoId} versionId={version?.id} active="impact" />
        <VersionPicker versions={versions.data} value={version?.id} onChange={(id) => { select(id); setResult(null); }} />
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-6">
        {!version ? (
          <EmptyState title="No analyzed version">Analyze the repository first; impact is computed on an analyzed version.</EmptyState>
        ) : (
          <div className="mx-auto max-w-6xl space-y-6">
            <Card title="Change">
              <form onSubmit={run} className="space-y-4">
                <Tabs<Mode>
                  value={mode}
                  onChange={setMode}
                  items={[
                    { value: "diff", label: "Unified diff" },
                    { value: "symbols", label: "Symbols" },
                    { value: "compare", label: "Compare versions" },
                  ]}
                />
                {mode === "diff" && (
                  <Field label="Diff that applies to this version" hint="Output of git diff or diff -u. Hunks map to the declarations whose lines they touch.">
                    <textarea
                      value={diff}
                      onChange={(e) => setDiff(e.target.value)}
                      rows={10}
                      spellCheck={false}
                      className="w-full rounded-md border border-border bg-surface px-3 py-2 font-mono text-xs"
                      placeholder={"--- a/src/app.py\n+++ b/src/app.py\n@@ -10,3 +10,4 @@ ..."}
                    />
                  </Field>
                )}
                {mode === "symbols" && (
                  <Field label="Node keys" hint="One or more keys (sym:path#Name, file:path), separated by spaces or new lines. Open a symbol in Code or Knowledge graph to get its key.">
                    <textarea value={symbols} onChange={(e) => setSymbols(e.target.value)} rows={3} spellCheck={false} className="w-full rounded-md border border-border bg-surface px-3 py-2 font-mono text-xs" />
                  </Field>
                )}
                {mode === "compare" && (
                  <Field label="Base version" hint="What changed between the base and the selected version, and what in the selected version depends on it.">
                    {others.length === 0 ? (
                      <p className="text-sm text-muted">There is no other analyzed version of this repository.</p>
                    ) : (
                      <Select value={base} onChange={(e) => setBase(e.target.value)} aria-label="Base version">
                        <option value="">Choose a version</option>
                        {others.map((v: Version) => (
                          <option key={v.id} value={v.id}>
                            {versionLabel(v)}
                          </option>
                        ))}
                      </Select>
                    )}
                  </Field>
                )}
                <div className="flex flex-wrap items-end gap-4">
                  <Field label="Depth">
                    <Select value={depth} onChange={(e) => setDepth(Number(e.target.value))} aria-label="Depth">
                      {[1, 2, 3, 4, 5, 6].map((d) => <option key={d} value={d}>{d}</option>)}
                    </Select>
                  </Field>
                  <Field label="Follow relationships at least">
                    <Select value={minConfidence} onChange={(e) => setMinConfidence(e.target.value as typeof minConfidence)} aria-label="Minimum confidence">
                      <option value="confirmed">confirmed</option>
                      <option value="probable">probable (default)</option>
                      <option value="inferred">inferred (includes name matches)</option>
                    </Select>
                  </Field>
                  <Button type="submit" disabled={!canRun}>{busy ? <Spinner label="Analyzing" /> : "Analyze impact"}</Button>
                </div>
              </form>
            </Card>
            {error != null && <ErrorBanner error={error} />}
            {result && <ImpactResult result={result} repoId={repoId} versionId={version.id} />}
          </div>
        )}
      </div>
    </div>
  );
}

function versionLabel(v: Version): string {
  const ref = v.commit_sha ? v.commit_sha.slice(0, 10) : v.requested_ref || v.id.slice(0, 8);
  return `${ref} · ${new Date(v.created_at).toLocaleString()}`;
}

const levelTone = { low: "ok", medium: "warn", high: "danger" } as const;

export function ImpactResult({ result, repoId, versionId }: { result: Impact; repoId: string; versionId: string }) {
  const codeLink = (path?: string, line?: number) =>
    path ? `/repositories/${repoId}/code?version=${versionId}&path=${encodeURIComponent(path)}${line ? `&line=${line}` : ""}` : undefined;
  const nonTests = result.affected.filter((a) => !a.test);
  return (
    <div className="space-y-6">
      <div className="grid gap-4 md:grid-cols-[minmax(0,1fr)_minmax(0,2fr)]">
        <Card title="Risk">
          <div className="flex items-baseline gap-3">
            <span className="text-4xl font-semibold tabular-nums" aria-label={`Risk score ${result.risk.score} of 100`}>{result.risk.score}</span>
            <span className="text-sm text-muted">/ 100</span>
            <Badge tone={levelTone[result.risk.level]}>{result.risk.level}</Badge>
          </div>
          <p className="mt-3 text-xs text-muted">{result.risk.note}</p>
          <dl className="mt-4 grid grid-cols-2 gap-3 text-sm">
            <div><dt className="text-muted">Changed</dt><dd className="font-medium tabular-nums">{result.changed.length}</dd></div>
            <div><dt className="text-muted">Dependents</dt><dd className="font-medium tabular-nums">{nonTests.length}</dd></div>
            <div><dt className="text-muted">Files affected</dt><dd className="font-medium tabular-nums">{result.affected_files}</dd></div>
            <div><dt className="text-muted">Tests reaching it</dt><dd className="font-medium tabular-nums">{result.tests.length}</dd></div>
          </dl>
        </Card>
        <Card title="How the score is made" padded={false}>
          <FactorTable factors={result.risk.factors} />
        </Card>
      </div>

      {result.truncated && (
        <div role="status" className="rounded-md border border-warn/40 bg-warn/10 px-4 py-2 text-sm">A size limit was reached; the lists below are incomplete.</div>
      )}

      <Card title={`Changed declarations (${result.changed.length})`} padded={false}>
        {result.changed.length === 0 ? (
          <p className="px-4 py-3 text-sm text-muted">The change touches nothing in this version.</p>
        ) : (
          <Table>
            <thead><tr><th>Declaration</th><th>Kind</th><th>Why</th><th>Tested</th></tr></thead>
            <tbody>
              {result.changed.map((c) => (
                <tr key={c.node.key}>
                  <td><NodeCell node={c.node} href={codeLink(c.node.path, c.node.start_line)} /></td>
                  <td className="text-muted">{c.node.kind}</td>
                  <td className="text-muted">{c.reason}</td>
                  <td>{result.untested_changes.includes(c.node.key) ? <Badge tone="warn">no test found</Badge> : <Badge tone="ok">reached</Badge>}</td>
                </tr>
              ))}
            </tbody>
          </Table>
        )}
        {result.unmatched.length > 0 && (
          <ul className="border-t border-border px-4 py-3 text-sm text-muted">
            {result.unmatched.map((u) => <li key={u}>{u}</li>)}
          </ul>
        )}
      </Card>

      <Card title={`Tests that reach the change (${result.tests.length})`} padded={false}>
        {result.tests.length === 0 ? (
          <p className="px-4 py-3 text-sm text-muted">No test reaches the changed declarations through relationships at this confidence and depth.</p>
        ) : (
          <AffectedTable items={result.tests} codeLink={codeLink} />
        )}
      </Card>

      <Card title={`Dependents (${nonTests.length})`} padded={false}>
        {nonTests.length === 0 ? (
          <p className="px-4 py-3 text-sm text-muted">Nothing outside tests depends on the change through resolved relationships.</p>
        ) : (
          <AffectedTable items={nonTests} codeLink={codeLink} />
        )}
      </Card>

      <Card title="Limitations">
        <ul className="list-disc space-y-1 pl-5 text-sm text-muted">
          {result.limitations.map((l) => <li key={l}>{l}</li>)}
        </ul>
      </Card>
    </div>
  );
}

function FactorTable({ factors }: { factors: RiskFactor[] }) {
  return (
    <Table>
      <thead><tr><th>Factor</th><th className="text-right">Weight</th><th className="w-40">Value</th><th className="text-right">Points</th></tr></thead>
      <tbody>
        {factors.map((f) => (
          <tr key={f.id}>
            <td>
              <div>{f.label}</div>
              <div className="text-xs text-muted">{f.explanation}</div>
            </td>
            <td className="text-right tabular-nums text-muted">{f.weight}</td>
            <td>
              <div className="h-2 rounded bg-surface-2" role="img" aria-label={`${Math.round(f.value * 100)}%`}>
                <div className="h-2 rounded bg-accent" style={{ width: `${Math.round(f.value * 100)}%` }} />
              </div>
            </td>
            <td className="text-right tabular-nums">{f.points.toFixed(1)}</td>
          </tr>
        ))}
      </tbody>
    </Table>
  );
}

function AffectedTable({ items, codeLink }: { items: ImpactNode[]; codeLink: (path?: string, line?: number) => string | undefined }) {
  const [open, setOpen] = useState<string | null>(null);
  return (
    <Table>
      <thead><tr><th>Dependent</th><th className="text-right">Distance</th><th>Confidence</th><th>Path to the change</th></tr></thead>
      <tbody>
        {items.map((a) => (
          <tr key={a.node.key} className="align-top">
            <td><NodeCell node={a.node} href={codeLink(a.node.path, a.node.start_line)} /></td>
            <td className="text-right tabular-nums">{a.distance}</td>
            <td><ConfidenceBadge level={a.confidence} /></td>
            <td>
              <button type="button" className="text-left text-xs text-accent hover:underline" onClick={() => setOpen(open === a.node.key ? null : a.node.key)} aria-expanded={open === a.node.key}>
                {a.kinds.join(" → ")}
              </button>
              {open === a.node.key && (
                <ol className="mt-1 space-y-0.5 text-xs text-muted">
                  {a.via.map((k, i) => (
                    <li key={k} className={cx(i === a.via.length - 1 && "font-medium text-fg")}>
                      {i > 0 && <span className="mr-1">{a.kinds[i - 1]} →</span>}
                      {keyLabel(k)}
                    </li>
                  ))}
                </ol>
              )}
            </td>
          </tr>
        ))}
      </tbody>
    </Table>
  );
}

function NodeCell({ node, href }: { node: { key: string; name: string; path?: string }; href?: string }) {
  const label = node.key.startsWith("file:") ? node.path ?? node.name : node.name;
  return (
    <div className="min-w-0">
      {href ? <Link to={href} className="font-medium text-accent hover:underline">{label}</Link> : <span className="font-medium">{label}</span>}
      <div className="truncate text-xs text-muted" title={node.key}>{keyLabel(node.key)}</div>
    </div>
  );
}
