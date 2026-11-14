import { useEffect, useMemo } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { ConfidenceBadge, Select, StatusBadge, Table, formatRelative, shortSha } from "@usie/design-system";
import type { GraphEdge, GraphNode, Repository, Version } from "@usie/sdk";
import { api } from "../api";
import { useAsync, useInterval } from "../hooks";

/**
 * Loads a repository and its versions and picks the version shown: the
 * `version` query parameter if present, otherwise the newest ready version.
 */
export function useRepoVersion(repoId: string) {
  const [params, setParams] = useSearchParams();
  const repo = useAsync(() => api.getRepository(repoId), [repoId]);
  const versions = useAsync(() => api.listVersions(repoId), [repoId]);
  const requested = params.get("version");
  const version: Version | undefined = useMemo(() => {
    const list = versions.data ?? [];
    return (requested ? list.find((v) => v.id === requested) : undefined) ?? list.find((v) => v.status === "ready");
  }, [versions.data, requested]);
  const active = (versions.data ?? []).some((v) => v.status === "pending" || v.status === "analyzing");
  useInterval(() => {
    versions.reload();
    repo.reload();
  }, 3000, active);
  const select = (id: string) => {
    const next = new URLSearchParams(params);
    next.set("version", id);
    setParams(next);
  };
  return { repo, versions, version, select };
}

export function VersionPicker({ versions, value, onChange, readyOnly = true }: { versions: Version[]; value?: string; onChange: (id: string) => void; readyOnly?: boolean }) {
  const options = readyOnly ? versions.filter((v) => v.status === "ready") : versions;
  if (options.length === 0) return null;
  return (
    <Select aria-label="Version" value={value} onChange={(e) => onChange(e.target.value)} className="max-w-xs">
      {options.map((v) => (
        <option key={v.id} value={v.id}>
          v{v.sequence}
          {v.commit_sha ? ` · ${shortSha(v.commit_sha)}` : ""}
          {v.branch ? ` · ${v.branch}` : ""} · {v.status}
        </option>
      ))}
    </Select>
  );
}

export function sourceLabel(r: Repository): string {
  switch (r.source_kind) {
    case "git":
      return r.source_location + (r.default_ref ? ` @ ${r.default_ref}` : "");
    case "local":
      return `local: ${r.source_location}`;
    default:
      return "uploaded archives";
  }
}

export function RepoLinks({ repoId, versionId, active }: { repoId: string; versionId?: string; active: "overview" | "code" | "graph" }) {
  const q = versionId ? `?version=${versionId}` : "";
  const item = (key: typeof active, to: string, label: string) => (
    <Link to={to} className={key === active ? "rounded-md bg-accent/10 px-3 py-1.5 text-sm font-medium text-accent" : "rounded-md px-3 py-1.5 text-sm text-muted hover:bg-surface-2 hover:text-fg"}>
      {label}
    </Link>
  );
  return (
    <nav aria-label="Repository" className="flex gap-1">
      {item("overview", `/repositories/${repoId}${q}`, "Overview")}
      {item("code", `/repositories/${repoId}/code${q}`, "Code")}
      {item("graph", `/repositories/${repoId}/graph${q}`, "Knowledge graph")}
    </nav>
  );
}

/** Relationship table with full provenance for each edge. */
export function EdgeTable({ edges, nodes, focus, onSelect, codeLink }: { edges: GraphEdge[]; nodes: Map<number, GraphNode>; focus?: number; onSelect?: (n: GraphNode) => void; codeLink?: (path: string, line?: number) => string }) {
  if (edges.length === 0) return <p className="px-3 py-2 text-sm text-muted">No relationships.</p>;
  return (
    <Table>
      <thead>
        <tr>
          <th>Relationship</th>
          <th>Other end</th>
          <th>Confidence</th>
          <th>Rule</th>
          <th>Evidence</th>
        </tr>
      </thead>
      <tbody>
        {edges.map((e) => {
          const outgoing = e.src === focus;
          const other = nodes.get(outgoing ? e.dst : e.src);
          return (
            <tr key={`${e.src}-${e.dst}-${e.kind}`}>
              <td className="whitespace-nowrap font-mono text-xs">
                {focus === undefined ? e.kind : outgoing ? `→ ${e.kind}` : `← ${e.kind}`}
                {e.occurrences > 1 && <span className="text-muted"> ×{e.occurrences}</span>}
              </td>
              <td className="max-w-xs truncate">
                {other ? (
                  <button type="button" className="text-left text-accent hover:underline" onClick={() => onSelect?.(other)} title={other.key}>
                    {other.qualified_name || other.name}
                  </button>
                ) : (
                  "—"
                )}
              </td>
              <td>
                <ConfidenceBadge level={e.confidence} />
              </td>
              <td className="font-mono text-xs text-muted">{e.rule}</td>
              <td className="whitespace-nowrap text-xs">
                {e.evidence_path ? (
                  codeLink ? (
                    <Link className="text-accent hover:underline" to={codeLink(e.evidence_path, e.evidence_line)}>
                      {e.evidence_path}
                      {e.evidence_line ? `:${e.evidence_line}` : ""}
                    </Link>
                  ) : (
                    `${e.evidence_path}:${e.evidence_line ?? ""}`
                  )
                ) : (
                  "—"
                )}
              </td>
            </tr>
          );
        })}
      </tbody>
    </Table>
  );
}

export function VersionStatusLine({ v }: { v: Version }) {
  return (
    <span className="inline-flex items-center gap-2">
      <StatusBadge status={v.status} />
      <span className="text-xs text-muted">
        v{v.sequence} · {v.analyzed_at ? `analyzed ${formatRelative(v.analyzed_at)}` : `created ${formatRelative(v.created_at)}`}
      </span>
    </span>
  );
}

export function useDocumentTitle(title: string) {
  useEffect(() => {
    document.title = title ? `${title} · USIE` : "USIE";
  }, [title]);
}
