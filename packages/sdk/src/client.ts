import type {
  ApiToken, AuditEvent, Branch, Commit, Confidence, Dependency, DirEntry, Enqueued, FileDetail, GraphDiff, GraphNode,
  GraphStats, Job, JobStatus, Reference, Repository, Session, SourceFile, SourceKind, Subgraph, SystemInfo, Version,
  Worker, Workspace,
} from "./types";

/** An error returned by the API, carrying its stable code and request id. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly requestId: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

export interface ClientOptions {
  /** Base URL of the API, e.g. "/api/v1" (default) or "https://usie.example/api/v1". */
  baseUrl?: string;
  /** API token for programmatic use. Browsers use the session cookie instead. */
  token?: string;
  fetch?: typeof fetch;
}

type Query = Record<string, string | number | boolean | string[] | undefined | null>;

function queryString(q: Query | undefined): string {
  if (!q) return "";
  const params = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) {
    if (v === undefined || v === null || v === "") continue;
    params.set(k, Array.isArray(v) ? v.join(",") : String(v));
  }
  const s = params.toString();
  return s ? `?${s}` : "";
}

export class UsieClient {
  private readonly baseUrl: string;
  private readonly token: string | undefined;
  private readonly fetchImpl: typeof fetch;
  /** CSRF token of the current browser session, sent on state-changing requests. */
  csrfToken: string | undefined;

  constructor(opts: ClientOptions = {}) {
    this.baseUrl = (opts.baseUrl ?? "/api/v1").replace(/\/$/, "");
    this.token = opts.token;
    this.fetchImpl = opts.fetch ?? ((...args) => globalThis.fetch(...args));
  }

  private async request<T>(method: string, path: string, opts: { query?: Query; body?: unknown; raw?: BodyInit; contentType?: string; signal?: AbortSignal } = {}): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    let body: BodyInit | undefined;
    if (opts.raw !== undefined) {
      body = opts.raw;
      headers["Content-Type"] = opts.contentType ?? "application/octet-stream";
    } else if (opts.body !== undefined) {
      body = JSON.stringify(opts.body);
      headers["Content-Type"] = "application/json";
    }
    if (this.token) headers.Authorization = `Bearer ${this.token}`;
    if (method !== "GET" && this.csrfToken) headers["X-CSRF-Token"] = this.csrfToken;

    const res = await this.fetchImpl(this.baseUrl + path + queryString(opts.query), {
      method, headers, body, credentials: "same-origin", signal: opts.signal,
    });
    if (res.status === 204) return undefined as T;
    const text = await res.text();
    let data: unknown = undefined;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        throw new ApiError(res.status, "invalid_response", `unexpected response (${res.status})`, res.headers.get("X-Request-Id") ?? "");
      }
    }
    if (!res.ok) {
      const err = (data as { error?: { code?: string; message?: string; request_id?: string } } | undefined)?.error;
      throw new ApiError(res.status, err?.code ?? "http_error", err?.message ?? `request failed (${res.status})`, err?.request_id ?? res.headers.get("X-Request-Id") ?? "");
    }
    return data as T;
  }

  // ---- system and auth ----
  system = () => this.request<SystemInfo>("GET", "/system");

  private remember = (s: Session) => {
    this.csrfToken = s.csrf_token;
    return s;
  };

  me = () => this.request<Session>("GET", "/auth/me").then(this.remember);
  login = (email: string, password: string) =>
    this.request<Session>("POST", "/auth/login", { body: { email, password } }).then(this.remember);
  register = (input: { email: string; password: string; display_name: string; organization: string }) =>
    this.request<Session>("POST", "/auth/register", { body: input }).then(this.remember);
  logout = async () => {
    await this.request<void>("POST", "/auth/logout");
    this.csrfToken = undefined;
  };

  listTokens = () => this.request<{ tokens: ApiToken[] }>("GET", "/tokens").then((r) => r.tokens);
  createToken = (input: { name: string; role: "viewer" | "member" | "admin"; expires_in_days?: number }) =>
    this.request<{ token: ApiToken; secret: string }>("POST", "/tokens", { body: input });
  revokeToken = (id: string) => this.request<void>("DELETE", `/tokens/${encodeURIComponent(id)}`);

  listWorkspaces = () => this.request<{ workspaces: Workspace[] }>("GET", "/workspaces").then((r) => r.workspaces);
  createWorkspace = (name: string) => this.request<Workspace>("POST", "/workspaces", { body: { name } });

  // ---- repositories ----
  listRepositories = () => this.request<{ repositories: Repository[] }>("GET", "/repositories").then((r) => r.repositories);
  getRepository = (id: string) => this.request<Repository>("GET", `/repositories/${id}`);
  createRepository = (input: { name: string; source_kind: SourceKind; source_location?: string; default_ref?: string; workspace_id?: string }) =>
    this.request<Repository>("POST", "/repositories", { body: input });
  deleteRepository = (id: string) => this.request<void>("DELETE", `/repositories/${id}`);
  analyzeRepository = (id: string, ref?: string) =>
    this.request<Enqueued>("POST", `/repositories/${id}/analyze`, ref ? { body: { ref } } : {});
  uploadArchive = (id: string, archive: Blob) =>
    this.request<Enqueued>("POST", `/repositories/${id}/upload`, { raw: archive, contentType: "application/zip" });
  listVersions = (id: string) => this.request<{ versions: Version[] }>("GET", `/repositories/${id}/versions`).then((r) => r.versions);
  listCommits = (id: string, limit = 100) =>
    this.request<{ commits: Commit[] }>("GET", `/repositories/${id}/commits`, { query: { limit } }).then((r) => r.commits);
  listBranches = (id: string) => this.request<{ branches: Branch[] }>("GET", `/repositories/${id}/branches`).then((r) => r.branches);

  // ---- versions ----
  getVersion = (id: string) => this.request<Version>("GET", `/versions/${id}`);
  tree = (id: string, dir = "") =>
    this.request<{ dir: string; directories: DirEntry[]; files: SourceFile[] }>("GET", `/versions/${id}/tree`, { query: { dir } });
  listFiles = (id: string, q: { dir?: string; language?: string; category?: string; parse_status?: string; limit?: number; offset?: number } = {}) =>
    this.request<{ files: SourceFile[]; total: number }>("GET", `/versions/${id}/files`, { query: q });
  file = (id: string, path: string) => this.request<FileDetail>("GET", `/versions/${id}/file`, { query: { path } });
  dependencies = (id: string) =>
    this.request<{ dependencies: Dependency[] }>("GET", `/versions/${id}/dependencies`).then((r) => r.dependencies);
  references = (id: string, q: { path?: string; outcome?: string; target?: string; source?: string; limit?: number; offset?: number } = {}) =>
    this.request<{ references: Reference[]; total: number }>("GET", `/versions/${id}/references`, { query: q });

  // ---- knowledge graph ----
  graphStats = (id: string) => this.request<GraphStats>("GET", `/versions/${id}/graph/stats`);
  searchGraph = (id: string, q: { q?: string; kind?: string[]; path_prefix?: string; limit?: number; offset?: number }, signal?: AbortSignal) =>
    this.request<{ nodes: GraphNode[]; total: number }>("GET", `/versions/${id}/graph/search`, { query: q, signal });
  graphNode = (id: string, key: string) => this.request<GraphNode>("GET", `/versions/${id}/graph/node`, { query: { key } });
  neighborhood = (id: string, q: { key: string; depth?: number; direction?: "in" | "out" | "both"; kinds?: string[]; min_confidence?: Confidence; max_nodes?: number }) =>
    this.request<Subgraph>("GET", `/versions/${id}/graph/neighborhood`, { query: q });
  fileGraph = (id: string, q: { min_confidence?: Confidence; max_edges?: number } = {}) =>
    this.request<Subgraph>("GET", `/versions/${id}/graph/files`, { query: q });
  graphDiff = (id: string, base: string, limit = 200) =>
    this.request<{ base_version_id: string; head_version_id: string; diff: GraphDiff }>("GET", `/versions/${id}/graph/diff`, { query: { base, limit } });

  // ---- jobs and workers ----
  listJobs = (q: { status?: JobStatus; limit?: number } = {}) => this.request<{ jobs: Job[] }>("GET", "/jobs", { query: q }).then((r) => r.jobs);
  getJob = (id: string) => this.request<Job>("GET", `/jobs/${id}`);
  cancelJob = (id: string) => this.request<Job>("POST", `/jobs/${id}/cancel`);
  listWorkers = () => this.request<{ workers: Worker[] }>("GET", "/workers").then((r) => r.workers);
  listAudit = (q: { before?: number; limit?: number } = {}) =>
    this.request<{ events: AuditEvent[] }>("GET", "/audit-events", { query: q }).then((r) => r.events);

  /**
   * Follow a job's state as Server-Sent Events (browser session only).
   * Returns a function that closes the stream.
   */
  watchJob(id: string, onJob: (job: Job) => void, onError?: () => void): () => void {
    const source = new EventSource(`${this.baseUrl}/jobs/${id}/events`, { withCredentials: true });
    source.addEventListener("job", (ev) => {
      const job = JSON.parse((ev as MessageEvent<string>).data) as Job;
      onJob(job);
      if (job.status === "succeeded" || job.status === "failed" || job.status === "cancelled") source.close();
    });
    source.onerror = () => {
      // The server closes the stream after a terminal event; any other error
      // is reported once and the stream is not retried automatically.
      if (source.readyState !== EventSource.CLOSED) onError?.();
      source.close();
    };
    return () => source.close();
  }
}
