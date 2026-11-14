import { useEffect, useMemo, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { Badge, Button, Card, EmptyState, ErrorBanner, Input, Loading, Select, Stat, Tabs, cx } from "@usie/design-system";
import { CONFIDENCE_LEVELS, type Confidence, type GraphNode, RELATIONSHIP_KINDS, type Subgraph, keyPath } from "@usie/sdk";
import { api } from "../api";
import { useAsync } from "../hooks";
import { PageHeader } from "../App";
import { GraphCanvas } from "../components/GraphCanvas";
import { EdgeTable, RepoLinks, VersionPicker, useDocumentTitle, useRepoVersion } from "../components/shared";

type Mode = "explore" | "dependents" | "files";

/** Relationship kinds followed when listing what depends on a symbol. */
const DEPENDENT_KINDS = ["CALLS", "INSTANTIATES", "EXTENDS", "IMPLEMENTS", "REFERENCES", "IMPORTS"];

export default function GraphPage() {
  const { repoId = "" } = useParams();
  const { repo, versions, version, select } = useRepoVersion(repoId);
  const [params, setParams] = useSearchParams();
  const key = params.get("key") ?? "";
  const mode = (params.get("mode") as Mode | null) ?? "explore";
  useDocumentTitle(`Graph · ${repo.data?.name ?? ""}`);

  const set = (patch: Record<string, string | undefined>) => {
    const next = new URLSearchParams(params);
    for (const [k, v] of Object.entries(patch)) {
      if (v) next.set(k, v);
      else next.delete(k);
    }
    setParams(next);
  };

  if (repo.error) return <div className="p-6"><ErrorBanner error={repo.error} /></div>;
  if (!repo.data || !versions.data) return <Loading />;
  return (
    <div className="flex h-full flex-col">
      <PageHeader title={repo.data.name} subtitle="Knowledge graph of an analyzed version. Relationships carry their confidence and the rule that produced them." />
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-6 py-2">
        <RepoLinks repoId={repoId} versionId={version?.id} active="graph" />
        <VersionPicker versions={versions.data} value={version?.id} onChange={select} />
      </div>
      {!version ? (
        <EmptyState title="No analyzed version">Analyze the repository to build its knowledge graph.</EmptyState>
      ) : (
        <div className="flex min-h-0 flex-1 flex-col">
          <div className="px-6 pt-2">
            <Tabs<Mode>
              value={mode}
              onChange={(m) => set({ mode: m === "explore" ? undefined : m })}
              items={[
                { value: "explore", label: "Explore" },
                { value: "dependents", label: <span>Dependents <Badge tone="info">experimental</Badge></span> },
                { value: "files", label: "File imports" },
              ]}
            />
          </div>
          {mode === "files" ? (
            <FileGraph versionId={version.id} onOpen={(k) => set({ key: k, mode: undefined })} />
          ) : (
            <Explorer key={version.id} versionId={version.id} repoId={repoId} nodeKey={key} dependents={mode === "dependents"} onKey={(k) => set({ key: k })} />
          )}
        </div>
      )}
    </div>
  );
}

function SearchBox({ versionId, onPick }: { versionId: string; onPick: (n: GraphNode) => void }) {
  const [q, setQ] = useState("");
  const [kind, setKind] = useState("");
  const [results, setResults] = useState<{ nodes: GraphNode[]; total: number } | null>(null);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    if (q.trim().length < 2) {
      setResults(null);
      return;
    }
    const ctrl = new AbortController();
    const t = window.setTimeout(() => {
      api.searchGraph(versionId, { q: q.trim(), kind: kind ? [kind] : undefined, limit: 25 }, ctrl.signal).then(setResults, (e: unknown) => {
        if ((e as Error).name !== "AbortError") setError(e);
      });
    }, 200);
    return () => {
      ctrl.abort();
      window.clearTimeout(t);
    };
  }, [q, kind, versionId]);
  return (
    <div className="relative">
      <div className="flex gap-2">
        <Input aria-label="Search symbols, files and packages" placeholder="Search symbols, files, packages…" value={q} onChange={(e) => { setQ(e.target.value); setError(null); }} />
        <Select aria-label="Kind" value={kind} onChange={(e) => setKind(e.target.value)}>
          <option value="">Any kind</option>
          {["function", "method", "class", "interface", "type", "variable", "namespace", "file", "package"].map((k) => <option key={k} value={k}>{k}</option>)}
        </Select>
      </div>
      <ErrorBanner error={error} />
      {results && (
        <ul className="absolute z-20 mt-1 max-h-80 w-full overflow-auto rounded-md border border-border bg-surface shadow-lg" role="listbox">
          {results.nodes.length === 0 && <li className="px-3 py-2 text-sm text-muted">No matches.</li>}
          {results.nodes.map((n) => (
            <li key={n.key}>
              <button type="button" className="flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left text-sm hover:bg-surface-2" onClick={() => { onPick(n); setQ(""); setResults(null); }}>
                <span className="truncate"><span className="font-mono">{n.qualified_name || n.name}</span> <span className="text-xs text-muted">{n.path}</span></span>
                <Badge>{n.kind}</Badge>
              </button>
            </li>
          ))}
          {results.total > results.nodes.length && <li className="px-3 py-1.5 text-xs text-muted">{results.total - results.nodes.length} more; refine the search.</li>}
        </ul>
      )}
    </div>
  );
}

function Explorer({ versionId, repoId, nodeKey, dependents, onKey }: { versionId: string; repoId: string; nodeKey: string; dependents: boolean; onKey: (k: string) => void }) {
  const [depth, setDepth] = useState(dependents ? 3 : 1);
  const [direction, setDirection] = useState<"in" | "out" | "both">("both");
  const [kinds, setKinds] = useState<string[]>(["CALLS", "INSTANTIATES", "EXTENDS", "IMPLEMENTS", "IMPORTS", "REFERENCES", "CONTAINS"]);
  const [minConf, setMinConf] = useState<Confidence>("inferred");
  const [layout, setLayout] = useState<"cose" | "breadthfirst" | "concentric">("cose");
  useEffect(() => setDepth(dependents ? 3 : 1), [dependents]);

  const stats = useAsync(() => api.graphStats(versionId), [versionId]);
  const sub = useAsync<Subgraph | undefined>(
    () => (nodeKey ? api.neighborhood(versionId, dependents
      ? { key: nodeKey, depth, direction: "in", kinds: DEPENDENT_KINDS, min_confidence: minConf, max_nodes: 300 }
      : { key: nodeKey, depth, direction, kinds, min_confidence: minConf, max_nodes: 200 }) : Promise.resolve(undefined)),
    [versionId, nodeKey, depth, direction, kinds.join(","), minConf, dependents],
  );
  const nodes = useMemo(() => new Map((sub.data?.nodes ?? []).map((n) => [n.id, n])), [sub.data]);
  const focus = sub.data?.nodes[0];
  const codeLink = (path: string, line?: number) => `/repositories/${repoId}/code?version=${versionId}&path=${encodeURIComponent(path)}${line ? `&line=${line}` : ""}`;

  return (
    <div className="grid min-h-0 flex-1 grid-cols-[1fr_26rem]">
      <div className="flex min-h-0 flex-col">
        <div className="space-y-2 border-b border-border p-3">
          <SearchBox versionId={versionId} onPick={(n) => onKey(n.key)} />
          {nodeKey && (
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <label className="flex items-center gap-1">Depth
                <Select aria-label="Depth" value={depth} onChange={(e) => setDepth(Number(e.target.value))} className="h-8">
                  {[1, 2, 3, 4, 5, 6].map((d) => <option key={d}>{d}</option>)}
                </Select>
              </label>
              {!dependents && (
                <label className="flex items-center gap-1">Direction
                  <Select aria-label="Direction" value={direction} onChange={(e) => setDirection(e.target.value as typeof direction)} className="h-8">
                    <option value="both">both</option>
                    <option value="out">outgoing</option>
                    <option value="in">incoming</option>
                  </Select>
                </label>
              )}
              <label className="flex items-center gap-1">Minimum confidence
                <Select aria-label="Minimum confidence" value={minConf} onChange={(e) => setMinConf(e.target.value as Confidence)} className="h-8">
                  {CONFIDENCE_LEVELS.filter((c) => c !== "unknown").map((c) => <option key={c}>{c}</option>)}
                </Select>
              </label>
              <label className="flex items-center gap-1">Layout
                <Select aria-label="Layout" value={layout} onChange={(e) => setLayout(e.target.value as typeof layout)} className="h-8">
                  <option value="cose">force</option>
                  <option value="breadthfirst">hierarchy</option>
                  <option value="concentric">concentric</option>
                </Select>
              </label>
            </div>
          )}
          {nodeKey && !dependents && (
            <fieldset className="flex flex-wrap gap-x-3 gap-y-1 text-xs">
              <legend className="sr-only">Relationship kinds</legend>
              {RELATIONSHIP_KINDS.map((k) => (
                <label key={k} className="flex items-center gap-1">
                  <input type="checkbox" checked={kinds.includes(k)} onChange={(e) => setKinds(e.target.checked ? [...kinds, k] : kinds.filter((x) => x !== k))} />
                  <span className="font-mono">{k}</span>
                </label>
              ))}
            </fieldset>
          )}
          {dependents && nodeKey && (
            <p className="text-xs text-muted">
              Everything that reaches this node through calls, instantiation, inheritance, JSX/decorator references or imports, up to {depth} steps.
              This is a structural reachability list, not a risk assessment: dynamic dispatch, reflection and runtime configuration are not modelled.
            </p>
          )}
        </div>
        <div className="relative min-h-0 flex-1 bg-bg">
          {!nodeKey ? (
            stats.data ? (
              <div className="space-y-4 p-6">
                <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
                  <Stat label="Nodes" value={stats.data.nodes.toLocaleString()} />
                  <Stat label="Relationships" value={stats.data.edges.toLocaleString()} />
                  <Stat label="Symbols" value={(["function", "method", "class", "interface", "type", "variable", "namespace"] as const).reduce((s, k) => s + (stats.data!.nodes_by_kind[k] ?? 0), 0).toLocaleString()} />
                  <Stat label="External packages" value={(stats.data.nodes_by_kind.package ?? 0).toLocaleString()} />
                </div>
                <Card title="Relationships by kind">
                  <ul className="grid grid-cols-2 gap-1 text-sm sm:grid-cols-3">
                    {Object.entries(stats.data.edges_by_kind).sort((a, b) => b[1] - a[1]).map(([k, n]) => (
                      <li key={k} className="flex justify-between gap-2"><span className="font-mono text-xs">{k}</span><span className="tabular-nums">{n.toLocaleString()}</span></li>
                    ))}
                  </ul>
                </Card>
                <p className="text-sm text-muted">Search for a symbol, file or package to explore its relationships.</p>
              </div>
            ) : stats.error ? <div className="p-4"><ErrorBanner error={stats.error} /></div> : <Loading />
          ) : sub.error ? (
            <div className="p-4"><ErrorBanner error={sub.error} onRetry={sub.reload} /></div>
          ) : !sub.data ? (
            <Loading label="Loading graph" />
          ) : (
            <>
              <GraphCanvas nodes={sub.data.nodes} edges={sub.data.edges} focusId={focus?.id} onSelect={(n) => onKey(n.key)} layout={dependents ? "breadthfirst" : layout} />
              <div className="pointer-events-none absolute bottom-2 left-2 rounded border border-border bg-surface/90 px-2 py-1 text-xs text-muted">
                {sub.data.nodes.length} nodes · {sub.data.edges.length} relationships
                {sub.data.truncated && <span className="text-warn"> · truncated at the size limit</span>}
                <span className="ml-2">— solid: confirmed · dashed: probable · dotted: inferred</span>
              </div>
            </>
          )}
        </div>
      </div>
      <aside className="min-h-0 overflow-auto border-l border-border bg-surface" aria-label="Node details">
        {focus ? (
          <NodePanel node={focus} sub={sub.data!} nodes={nodes} dependents={dependents} codeLink={codeLink} onSelect={(n) => onKey(n.key)} />
        ) : (
          <p className="p-4 text-sm text-muted">Select a node to see its details and relationships.</p>
        )}
      </aside>
    </div>
  );
}

function NodePanel({ node, sub, nodes, dependents, codeLink, onSelect }: { node: GraphNode; sub: Subgraph; nodes: Map<number, GraphNode>; dependents: boolean; codeLink: (p: string, l?: number) => string; onSelect: (n: GraphNode) => void }) {
  const direct = sub.edges.filter((e) => e.src === node.id || e.dst === node.id);
  const path = keyPath(node.key);
  const attrs = Object.entries(node.attrs ?? {}).filter(([k]) => k !== "language");
  return (
    <div className="space-y-4 p-4">
      <div>
        <div className="flex items-center gap-2"><Badge>{node.kind}</Badge>{node.exported && <Badge tone="accent">exported</Badge>}</div>
        <h2 className="mt-1 break-all font-mono text-sm font-semibold">{node.qualified_name || node.name}</h2>
        {path && (
          <Link className="text-xs text-accent hover:underline" to={codeLink(path, node.start_line)}>
            {path}{node.start_line ? `:${node.start_line}` : ""}
          </Link>
        )}
        {node.signature && <pre className="mt-2 whitespace-pre-wrap break-all rounded bg-surface-2 p-2 font-mono text-xs">{node.signature}</pre>}
        {node.doc && <p className="mt-2 text-sm text-muted">{node.doc}</p>}
        {attrs.length > 0 && (
          <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-xs">
            {attrs.map(([k, v]) => (<><dt key={`k-${k}`} className="text-muted">{k}</dt><dd key={`v-${k}`} className="break-all font-mono">{JSON.stringify(v)}</dd></>))}
          </dl>
        )}
        <p className="mt-2 break-all font-mono text-[11px] text-muted" title="Stable node key">{node.key}</p>
      </div>
      {dependents ? (
        <div>
          <h3 className="mb-1 text-sm font-semibold">Dependents ({sub.nodes.length - 1}{sub.truncated ? "+" : ""})</h3>
          <ul className="divide-y divide-border rounded border border-border text-sm">
            {sub.nodes.slice(1).map((n) => (
              <li key={n.key} className="flex items-center justify-between gap-2 px-2 py-1">
                <button type="button" className="truncate text-left text-accent hover:underline" onClick={() => onSelect(n)} title={n.key}>{n.qualified_name || n.name}</button>
                <span className="shrink-0 text-xs text-muted">{n.path}</span>
              </li>
            ))}
            {sub.nodes.length === 1 && <li className="px-2 py-1 text-muted">Nothing in the repository depends on this node at the chosen confidence.</li>}
          </ul>
        </div>
      ) : (
        <div>
          <h3 className="mb-1 text-sm font-semibold">Direct relationships ({direct.length})</h3>
          <div className="rounded border border-border">
            <EdgeTable edges={direct} nodes={nodes} focus={node.id} onSelect={onSelect} codeLink={codeLink} />
          </div>
        </div>
      )}
    </div>
  );
}

function FileGraph({ versionId, onOpen }: { versionId: string; onOpen: (key: string) => void }) {
  const [minConf, setMinConf] = useState<Confidence>("probable");
  const data = useAsync(() => api.fileGraph(versionId, { min_confidence: minConf, max_edges: 1500 }), [versionId, minConf]);
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-3 border-b border-border px-6 py-2 text-sm">
        <label className="flex items-center gap-1">Minimum confidence
          <Select value={minConf} onChange={(e) => setMinConf(e.target.value as Confidence)} className="h-8">
            {CONFIDENCE_LEVELS.filter((c) => c !== "unknown").map((c) => <option key={c}>{c}</option>)}
          </Select>
        </label>
        {data.data && (
          <span className={cx("text-xs", data.data.truncated ? "text-warn" : "text-muted")}>
            {data.data.nodes.length} files · {data.data.edges.length} imports{data.data.truncated && " · truncated at 1,500 relationships"}
          </span>
        )}
        <Button size="sm" variant="ghost" onClick={data.reload}>Refresh</Button>
      </div>
      <div className="min-h-0 flex-1">
        {data.error ? <div className="p-4"><ErrorBanner error={data.error} /></div> : !data.data ? <Loading /> : data.data.edges.length === 0 ? (
          <EmptyState title="No import relationships between repository files" />
        ) : (
          <GraphCanvas nodes={data.data.nodes} edges={data.data.edges} onSelect={(n) => onOpen(n.key)} />
        )}
      </div>
    </div>
  );
}
