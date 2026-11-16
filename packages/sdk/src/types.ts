// Types mirroring services/api-gateway/openapi.yaml.

export type Role = "owner" | "admin" | "member" | "viewer";
export type Confidence = "confirmed" | "probable" | "inferred" | "unknown";
export type VersionStatus = "pending" | "analyzing" | "ready" | "failed" | "cancelled";
export type JobStatus = "queued" | "running" | "succeeded" | "failed" | "cancelled";
export type SourceKind = "git" | "local" | "upload";
export type ParseStatus = "parsed" | "parsed_with_errors" | "unsupported" | "skipped" | "failed";
export type FeatureStatus = "implemented" | "experimental" | "planned" | "unsupported";
export type RelationshipKind =
  | "CONTAINS" | "IMPORTS" | "EXPORTS" | "CALLS" | "INSTANTIATES" | "EXTENDS" | "IMPLEMENTS" | "REFERENCES" | "DEPENDS_ON";

export const CONFIDENCE_LEVELS: Confidence[] = ["confirmed", "probable", "inferred", "unknown"];
export const RELATIONSHIP_KINDS: RelationshipKind[] = [
  "CONTAINS", "IMPORTS", "EXPORTS", "CALLS", "INSTANTIATES", "EXTENDS", "IMPLEMENTS", "REFERENCES", "DEPENDS_ON",
];

export interface Principal {
  user_id: string;
  email: string;
  display_name: string;
  organization_id: string;
  organization_name: string;
  role: Role;
  kind: "session" | "token";
}

export interface Session {
  principal: Principal;
  csrf_token?: string;
}

export interface Feature {
  id: string;
  status: FeatureStatus;
  detail: string;
}

export interface SystemInfo {
  version: string;
  signup_allowed: boolean;
  features: Feature[];
  sources: Record<SourceKind, boolean>;
  limits: Record<string, number>;
  engine: { version: string; ir_schema_version: number; languages: { language: string; fingerprint: string }[] } | null;
}

export interface ApiToken {
  id: string;
  name: string;
  prefix: string;
  role: Role;
  created_at: string;
  last_used_at: string | null;
  expires_at: string | null;
  revoked_at: string | null;
}

export interface Workspace {
  id: string;
  name: string;
  created_at: string;
}

/** Measured statistics of an analysis run. Present once a version is ready. */
export interface VersionStats {
  files?: number;
  bytes?: number;
  lines?: number;
  files_by_language?: Record<string, number>;
  files_by_category?: Record<string, number>;
  files_by_parse_status?: Record<string, number>;
  changes?: Record<"added" | "modified" | "unchanged" | "removed", number>;
  parse_cache_hits?: number;
  parse_cache_misses?: number;
  symbols?: number;
  graph?: { nodes: number; edges: number; edges_by_kind: Record<string, number>; edges_by_confidence: Record<string, number> };
  references_by_outcome?: Record<string, number>;
  dropped_edges?: number;
  build_systems?: string[];
  declared_dependencies?: number;
  manifest_problems?: { path: string; message: string }[];
  skipped_symlinks?: number;
  skipped_ignored?: number;
  engine_version?: string;
  durations_ms?: Record<string, number>;
}

export interface Version {
  id: string;
  repository_id: string;
  sequence: number;
  status: VersionStatus;
  requested_ref: string;
  commit_sha: string;
  branch: string;
  tree_hash: string;
  parent_version_id: string | null;
  stats: VersionStats;
  error: string;
  created_at: string;
  analyzed_at: string | null;
}

export interface Repository {
  id: string;
  workspace_id: string;
  name: string;
  source_kind: SourceKind;
  source_location: string;
  default_ref: string;
  created_at: string;
  latest_version: Version | null;
}

export interface Enqueued {
  version_id: string;
  job_id: string;
}

export interface Commit {
  sha: string;
  parents: string[];
  author_name: string;
  authored_at: string | null;
  committed_at: string | null;
  subject: string;
}

export interface Branch {
  name: string;
  head_sha: string;
  is_default: boolean;
}

export interface DirEntry {
  name: string;
  path: string;
  files: number;
}

export interface SourceFile {
  path: string;
  content_hash: string;
  size_bytes: number;
  language: string;
  category: string;
  encoding: string;
  is_generated: boolean;
  is_vendored: boolean;
  content_stored: boolean;
  parse_status: ParseStatus;
  parse_detail: string;
  change: "added" | "modified" | "unchanged";
  line_count: number;
  symbol_count: number;
  diagnostic_count: number;
  analyzed_at: string | null;
}

export interface GraphNode {
  id: number;
  key: string;
  kind: string;
  name: string;
  qualified_name?: string;
  path?: string;
  start_line?: number;
  end_line?: number;
  signature?: string;
  doc?: string;
  exported?: boolean;
  attrs?: Record<string, unknown>;
}

export interface GraphEdge {
  src: number;
  dst: number;
  kind: RelationshipKind;
  confidence: Confidence;
  rule: string;
  evidence_path?: string;
  evidence_line?: number;
  occurrences: number;
}

export interface Subgraph {
  nodes: GraphNode[];
  edges: GraphEdge[];
  truncated: boolean;
}

export interface GraphStats {
  nodes: number;
  edges: number;
  nodes_by_kind: Record<string, number>;
  edges_by_kind: Record<string, number>;
  edges_by_confidence: Record<string, number>;
}

export interface EdgeKeys {
  src: string;
  dst: string;
  kind: string;
  confidence: Confidence;
}

export interface GraphDiff {
  nodes_added: number;
  nodes_removed: number;
  nodes_changed: number;
  edges_added: number;
  edges_removed: number;
  added: GraphNode[];
  removed: GraphNode[];
  changed: GraphNode[];
  added_edges: EdgeKeys[];
  removed_edges: EdgeKeys[];
  truncated: boolean;
}

export interface FileDetail {
  file: SourceFile;
  content: string | null;
  content_unavailable_reason: string;
  symbols: GraphNode[];
}

export interface Dependency {
  manifest_path: string;
  ecosystem: string;
  name: string;
  requirement: string;
  scope: string;
}

export interface Reference {
  path: string;
  line: number;
  col: number;
  kind: string;
  text: string;
  source_key: string;
  outcome: "resolved" | "external" | "builtin" | "unresolved";
  confidence: Confidence;
  target_key: string;
  rule: string;
  reason: string;
}

export interface Job {
  id: string;
  repository_id: string;
  repository_name: string;
  version_id: string;
  kind: string;
  status: JobStatus;
  priority: number;
  attempts: number;
  max_attempts: number;
  cancel_requested: boolean;
  worker_id: string | null;
  stage: string;
  progress: Record<string, unknown>;
  error: string;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  updated_at: string;
}

export interface Worker {
  id: string;
  hostname: string;
  pid: number;
  version: string;
  capabilities: Record<string, unknown>;
  status: "idle" | "busy" | "stopped";
  started_at: string;
  last_heartbeat_at: string;
  jobs_completed: number;
  jobs_failed: number;
  online: boolean;
}

export interface AuditEvent {
  id: number;
  actor_user_id: string | null;
  actor_email: string;
  actor_kind: string;
  action: string;
  target_type: string;
  target_id: string;
  outcome: string;
  ip: string;
  request_id: string;
  detail: Record<string, unknown>;
  created_at: string;
}

// ---- AI reasoning ----

export interface AIStatus {
  enabled: boolean;
  provider: string;
  model: string;
  /** Tokens per organization per UTC day; 0 means no cap. */
  daily_token_budget: number;
  tokens_used_today: number;
  /** What is sent to the provider. Show it to users. */
  data_notice: string;
}

/** How a claim is backed after server-side validation. */
export type AIClaimSupport = "evidence" | "inference" | "unsupported";

export interface AIClaim {
  statement: string;
  support: AIClaimSupport;
  citations: string[];
  invalid_citations?: string[];
}

export interface AIAnswer {
  answerable?: "yes" | "partially" | "no";
  summary?: string;
  claims?: AIClaim[];
  limitations?: string[];
  followups?: string[];
  validation_notes?: string[];
}

export interface AIEvidence {
  id: string;
  kind: "overview" | "readme" | "file_index" | "symbol" | "file";
  key?: string;
  path?: string;
  start_line?: number;
  end_line?: number;
  title: string;
  content: string;
  truncated?: boolean;
}

export interface AIMessage {
  id: string;
  seq: number;
  role: "user" | "assistant";
  content: string;
  focus_key: string;
  status: "pending" | "ok" | "error" | "refused";
  answer: AIAnswer;
  context: AIEvidence[];
  provider: string;
  model: string;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  latency_ms: number;
  error: string;
  created_at: string;
  completed_at: string | null;
}

export interface AIConversation {
  id: string;
  repository_id: string;
  version_id: string;
  title: string;
  created_by: string | null;
  created_at: string;
  updated_at: string;
  messages?: AIMessage[];
}

// ---- change impact ----

export interface ImpactRequest {
  /** Node keys treated as changed. */
  symbols?: string[];
  /** Paths whose declarations are all treated as changed. */
  files?: string[];
  /** A unified diff that applies to the analyzed version. */
  diff?: string;
  /** An earlier analyzed version of the same repository. */
  base_version_id?: string;
  depth?: number;
  min_confidence?: "confirmed" | "probable" | "inferred";
}

export interface ImpactChanged {
  node: GraphNode;
  reason: string;
}

export interface ImpactNode {
  node: GraphNode;
  distance: number;
  confidence: "confirmed" | "probable" | "inferred";
  /** Node keys from this dependent to the changed node. */
  via: string[];
  /** Relationship kinds along `via`. */
  kinds: string[];
  test: boolean;
}

export interface RiskFactor {
  id: string;
  label: string;
  value: number;
  weight: number;
  points: number;
  explanation: string;
}

export interface Impact {
  changed: ImpactChanged[];
  unmatched: string[];
  affected: ImpactNode[];
  tests: ImpactNode[];
  untested_changes: string[];
  affected_files: number;
  risk: { score: number; level: "low" | "medium" | "high"; factors: RiskFactor[]; note: string };
  depth: number;
  min_confidence: string;
  truncated: boolean;
  limitations: string[];
}
