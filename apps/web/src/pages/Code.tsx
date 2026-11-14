import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import Editor, { type OnMount } from "@monaco-editor/react";
import { Badge, ConfidenceBadge, EmptyState, ErrorBanner, Loading, Select, StatusBadge, Tabs, cx, formatBytes } from "@usie/design-system";
import type { GraphNode, Reference } from "@usie/sdk";
import { keyLabel, keyPath } from "@usie/sdk";
import { api } from "../api";
import { useAsync } from "../hooks";
import { PageHeader } from "../App";
import { RepoLinks, VersionPicker, useDocumentTitle, useRepoVersion } from "../components/shared";
import { monacoLanguage } from "../monaco";

export default function CodePage() {
  const { repoId = "" } = useParams();
  const { repo, versions, version, select } = useRepoVersion(repoId);
  const [params, setParams] = useSearchParams();
  const path = params.get("path") ?? "";
  const line = Number(params.get("line") ?? "0") || undefined;
  useDocumentTitle(path ? `${path.split("/").pop()} · ${repo.data?.name ?? ""}` : "Code");

  const open = (p: string, l?: number) => {
    const next = new URLSearchParams(params);
    next.set("path", p);
    if (l) next.set("line", String(l));
    else next.delete("line");
    setParams(next);
  };

  if (repo.error) return <div className="p-6"><ErrorBanner error={repo.error} /></div>;
  if (!repo.data || !versions.data) return <Loading />;
  return (
    <div className="flex h-full flex-col">
      <PageHeader title={repo.data.name} subtitle="Source files of an analyzed version, with symbols and resolved references." />
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-6 py-2">
        <RepoLinks repoId={repoId} versionId={version?.id} active="code" />
        <VersionPicker versions={versions.data} value={version?.id} onChange={select} />
      </div>
      {!version ? (
        <EmptyState title="No analyzed version">Analyze the repository to browse its code.</EmptyState>
      ) : (
        <div className="grid min-h-0 flex-1 grid-cols-[18rem_1fr]">
          <aside className="min-h-0 overflow-auto border-r border-border bg-surface py-2" aria-label="Files">
            <TreeDir versionId={version.id} dir="" depth={0} selected={path} onOpen={open} initiallyOpen />
          </aside>
          <section className="min-h-0 min-w-0">
            {path ? <FileView key={`${version.id}:${path}`} versionId={version.id} repoId={repoId} path={path} line={line} onOpen={open} /> : (
              <EmptyState title="Select a file">Files that are parsed show their symbols and resolved references alongside the source.</EmptyState>
            )}
          </section>
        </div>
      )}
    </div>
  );
}

function TreeDir({ versionId, dir, depth, selected, onOpen, initiallyOpen = false }: { versionId: string; dir: string; depth: number; selected: string; onOpen: (p: string) => void; initiallyOpen?: boolean }) {
  const shouldOpen = initiallyOpen || (selected !== "" && selected.startsWith(dir + "/"));
  const [open, setOpen] = useState(shouldOpen);
  useEffect(() => {
    if (shouldOpen) setOpen(true);
  }, [shouldOpen]);
  const listing = useAsync(() => (open ? api.tree(versionId, dir) : Promise.resolve(undefined)), [versionId, dir, open]);
  const pad = { paddingLeft: `${depth * 12 + 12}px` };
  const name = dir.split("/").pop() || "/";
  return (
    <div>
      {dir !== "" && (
        <button type="button" aria-expanded={open} onClick={() => setOpen(!open)} className="flex w-full items-center gap-1 py-0.5 pr-2 text-left text-sm hover:bg-surface-2" style={pad}>
          <span aria-hidden="true" className="w-3 text-muted">{open ? "▾" : "▸"}</span>
          <span className="truncate">{name}</span>
        </button>
      )}
      {open && listing.error ? <div className="px-3 text-xs text-danger">{(listing.error as Error).message}</div> : null}
      {open && listing.data && (
        <>
          {listing.data.directories.map((d) => (
            <TreeDir key={d.path} versionId={versionId} dir={d.path} depth={dir === "" ? 0 : depth + 1} selected={selected} onOpen={onOpen} />
          ))}
          {listing.data.files.map((f) => (
            <button
              key={f.path}
              type="button"
              onClick={() => onOpen(f.path)}
              aria-current={selected === f.path ? "page" : undefined}
              className={cx("flex w-full items-center justify-between gap-2 py-0.5 pr-2 text-left text-sm", selected === f.path ? "bg-accent/10 text-accent" : "hover:bg-surface-2", f.is_vendored || f.is_generated ? "text-muted" : "")}
              style={{ paddingLeft: `${(dir === "" ? 0 : depth + 1) * 12 + 28}px` }}
              title={`${f.path} · ${f.parse_status.replace(/_/g, " ")}`}
            >
              <span className="truncate">{f.path.split("/").pop()}</span>
              {f.parse_status === "parsed_with_errors" && <span className="text-xs text-warn" aria-label="has syntax errors">●</span>}
            </button>
          ))}
        </>
      )}
    </div>
  );
}

type Panel = "symbols" | "references" | "unresolved";

function FileView({ versionId, repoId, path, line, onOpen }: { versionId: string; repoId: string; path: string; line?: number; onOpen: (p: string, l?: number) => void }) {
  const detail = useAsync(() => api.file(versionId, path), [versionId, path]);
  const refs = useAsync(() => api.references(versionId, { path, limit: 1000 }), [versionId, path]);
  const [panel, setPanel] = useState<Panel>("symbols");
  const editorRef = useRef<Parameters<OnMount>[0] | null>(null);
  const [outcome, setOutcome] = useState("");

  const reveal = (l?: number) => {
    const ed = editorRef.current;
    if (!ed || !l) return;
    ed.revealLineInCenter(l);
    ed.setSelection({ startLineNumber: l, startColumn: 1, endLineNumber: l, endColumn: 1 });
  };
  useEffect(() => reveal(line), [line, detail.data]);

  const unresolved = useMemo(() => (refs.data?.references ?? []).filter((r) => r.outcome === "unresolved"), [refs.data]);
  const shownRefs = useMemo(() => (refs.data?.references ?? []).filter((r) => !outcome || r.outcome === outcome), [refs.data, outcome]);

  if (detail.error) return <div className="p-4"><ErrorBanner error={detail.error} onRetry={detail.reload} /></div>;
  if (!detail.data) return <Loading />;
  const { file, content, content_unavailable_reason, symbols } = detail.data;
  const graphLink = (key: string) => `/repositories/${repoId}/graph?version=${versionId}&key=${encodeURIComponent(key)}`;

  return (
    <div className="grid h-full min-h-0 grid-cols-[1fr_24rem]">
      <div className="flex min-h-0 min-w-0 flex-col">
        <div className="flex flex-wrap items-center gap-2 border-b border-border px-3 py-2 text-xs">
          <span className="font-mono text-sm">{path}</span>
          <StatusBadge status={file.parse_status} />
          {file.language && <Badge>{file.language}</Badge>}
          <Badge>{file.category}</Badge>
          {file.is_generated && <Badge tone="warn">generated</Badge>}
          {file.is_vendored && <Badge tone="warn">vendored</Badge>}
          <span className="text-muted">{formatBytes(file.size_bytes)} · {file.line_count} lines · {file.change}</span>
          {file.parse_detail && <span className="text-muted">— {file.parse_detail}</span>}
        </div>
        <div className="min-h-0 flex-1">
          {content === null ? (
            <EmptyState title="Content not available">{content_unavailable_reason}</EmptyState>
          ) : (
            <Editor
              value={content}
              path={path}
              language={monacoLanguage(path, file.language)}
              theme={document.documentElement.classList.contains("dark") ? "vs-dark" : "vs"}
              options={{ readOnly: true, domReadOnly: true, minimap: { enabled: false }, scrollBeyondLastLine: false, fontSize: 13, renderWhitespace: "none", automaticLayout: true }}
              onMount={(ed) => {
                editorRef.current = ed;
                reveal(line);
              }}
              loading={<Loading label="Loading editor" />}
            />
          )}
        </div>
      </div>
      <aside className="flex min-h-0 flex-col border-l border-border bg-surface" aria-label="File analysis">
        <div className="px-2 pt-1">
          <Tabs<Panel>
            value={panel}
            onChange={setPanel}
            items={[
              { value: "symbols", label: `Symbols (${symbols.length})` },
              { value: "references", label: `References (${refs.data?.total ?? "…"})` },
              { value: "unresolved", label: `Unresolved (${unresolved.length})` },
            ]}
          />
        </div>
        <div className="min-h-0 flex-1 overflow-auto">
          {panel === "symbols" && (
            symbols.length === 0 ? <p className="p-3 text-sm text-muted">{file.parse_status === "parsed" || file.parse_status === "parsed_with_errors" ? "No symbols declared in this file." : "This file was not parsed."}</p> : (
              <ul className="divide-y divide-border">
                {symbols.map((s) => <SymbolRow key={s.key} s={s} onReveal={() => reveal(s.start_line)} graphHref={graphLink(s.key)} />)}
              </ul>
            )
          )}
          {panel !== "symbols" && refs.error ? <div className="p-3"><ErrorBanner error={refs.error} /></div> : null}
          {panel === "references" && refs.data && (
            <>
              <div className="flex items-center gap-2 border-b border-border px-3 py-2 text-xs">
                <Select aria-label="Filter references by outcome" value={outcome} onChange={(e) => setOutcome(e.target.value)} className="h-7 text-xs">
                  <option value="">All outcomes</option>
                  <option value="resolved">Resolved</option>
                  <option value="external">External</option>
                  <option value="builtin">Builtin</option>
                  <option value="unresolved">Unresolved</option>
                </Select>
                {refs.data.total > refs.data.references.length && <span className="text-muted">first {refs.data.references.length} shown</span>}
              </div>
              <RefList refs={shownRefs} onReveal={reveal} onOpen={onOpen} graphLink={graphLink} />
            </>
          )}
          {panel === "unresolved" && refs.data && (
            <>
              <p className="border-b border-border px-3 py-2 text-xs text-muted">
                References the resolver could not establish. They are not guessed; the reason is shown for each.
              </p>
              <RefList refs={unresolved} onReveal={reveal} onOpen={onOpen} graphLink={graphLink} />
            </>
          )}
        </div>
      </aside>
    </div>
  );
}

function SymbolRow({ s, onReveal, graphHref }: { s: GraphNode; onReveal: () => void; graphHref: string }) {
  const depth = (s.qualified_name ?? s.name).split(".").length - 1;
  return (
    <li className="px-3 py-1.5 text-sm" style={{ paddingLeft: `${12 + depth * 12}px` }}>
      <div className="flex items-center justify-between gap-2">
        <button type="button" onClick={onReveal} className="truncate text-left hover:underline" title={`Line ${s.start_line}`}>
          <span className="text-xs text-muted">{s.kind}</span> <span className="font-mono">{s.name}</span>
          {s.exported && <span className="ml-1 text-xs text-accent">exported</span>}
        </button>
        <Link to={graphHref} className="shrink-0 text-xs text-accent hover:underline">graph</Link>
      </div>
      {s.signature && <div className="truncate font-mono text-xs text-muted" title={s.signature}>{s.signature}</div>}
      {s.doc && <div className="truncate text-xs text-muted" title={s.doc}>{s.doc}</div>}
    </li>
  );
}

function RefList({ refs, onReveal, onOpen, graphLink }: { refs: Reference[]; onReveal: (l?: number) => void; onOpen: (p: string, l?: number) => void; graphLink: (k: string) => string }) {
  if (refs.length === 0) return <p className="p-3 text-sm text-muted">None.</p>;
  return (
    <ul className="divide-y divide-border text-sm">
      {refs.map((r, i) => {
        const targetPath = r.target_key ? keyPath(r.target_key) : undefined;
        return (
          <li key={i} className="space-y-0.5 px-3 py-1.5">
            <div className="flex items-center justify-between gap-2">
              <button type="button" onClick={() => onReveal(r.line)} className="truncate text-left font-mono text-xs hover:underline" title={r.text}>
                {r.line}: {r.text}
              </button>
              {r.outcome === "unresolved" ? <Badge tone="warn">unresolved</Badge> : r.outcome === "builtin" ? <Badge>builtin</Badge> : <ConfidenceBadge level={r.confidence} />}
            </div>
            <div className="text-xs text-muted">
              {r.kind}
              {r.target_key && (
                <>
                  {" → "}
                  {targetPath ? (
                    <button type="button" className="text-accent hover:underline" onClick={() => onOpen(targetPath)}>{keyLabel(r.target_key)}</button>
                  ) : (
                    <Link className="text-accent hover:underline" to={graphLink(r.target_key)}>{keyLabel(r.target_key)}</Link>
                  )}
                </>
              )}
              {r.reason && <span className="block text-warn">{r.reason}</span>}
              <span className="block font-mono">rule: {r.rule}</span>
            </div>
          </li>
        );
      })}
    </ul>
  );
}
