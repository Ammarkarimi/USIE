// Helpers for knowledge-graph node keys:
//   repo | dir:<path> | file:<path> | sym:<path>#<qualified name>[~n] | ext:<ecosystem>:<name>

export type ParsedKey =
  | { type: "repo" }
  | { type: "dir"; path: string }
  | { type: "file"; path: string }
  | { type: "symbol"; path: string; qualifiedName: string }
  | { type: "external"; ecosystem: string; name: string }
  | { type: "unknown"; raw: string };

export function parseKey(key: string): ParsedKey {
  if (key === "repo") return { type: "repo" };
  if (key.startsWith("dir:")) return { type: "dir", path: key.slice(4) };
  if (key.startsWith("file:")) return { type: "file", path: key.slice(5) };
  if (key.startsWith("sym:")) {
    const rest = key.slice(4);
    const hash = rest.indexOf("#");
    if (hash > 0) return { type: "symbol", path: rest.slice(0, hash), qualifiedName: rest.slice(hash + 1).replace(/~\d+$/, "") };
  }
  if (key.startsWith("ext:")) {
    const rest = key.slice(4);
    const colon = rest.indexOf(":");
    if (colon > 0) return { type: "external", ecosystem: rest.slice(0, colon), name: rest.slice(colon + 1) };
  }
  return { type: "unknown", raw: key };
}

/** The repository file a key belongs to, if any. */
export function keyPath(key: string): string | undefined {
  const k = parseKey(key);
  return k.type === "file" || k.type === "symbol" || k.type === "dir" ? k.path : undefined;
}

export function keyLabel(key: string): string {
  const k = parseKey(key);
  switch (k.type) {
    case "repo": return "repository";
    case "dir": return `${k.path}/`;
    case "file": return k.path;
    case "symbol": return `${k.qualifiedName} (${k.path})`;
    case "external": return `${k.name} [${k.ecosystem}]`;
    default: return k.raw;
  }
}
