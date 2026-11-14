# Architecture overview

This describes the system as built. Planned subsystems are listed at the end
and are not part of the running system.

## Components

```
                ┌──────────────────────────┐
 browser ──────▶│ usie-api (Go)            │── serves apps/web (React SPA)
 API token ────▶│  auth · RBAC · rate limit│
                │  audit · REST · SSE      │
                └─────┬───────────┬────────┘
                      │           │ uploads (ZIP)
                      ▼           ▼
                ┌──────────┐  ┌─────────────────┐
                │PostgreSQL│  │ blob store (FS) │  sources by content hash,
                │ system of│  │                 │  gzip IR, uploads
                │ record   │  └────────▲────────┘
                └────▲─────┘           │
                     │ jobs (SKIP LOCKED, leases)
                ┌────┴─────────────────┴───┐
                │ usie-worker (Go)          │── git clone (hardened), ZIP
                │  ingestion · pipeline     │   extraction, local dirs
                └─────┬─────────────────────┘
                      │ NDJSON over stdin/stdout, empty environment
                      ▼
                ┌──────────────────────────┐
                │ usie-engine (Rust)       │  parse: tree-sitter → IR
                │  short-lived child proc. │  resolve: IR → nodes/edges/refs
                └──────────────────────────┘
```

| Component | Location | Responsibility |
|---|---|---|
| Normalized IR | `engines/intermediate-representation` | Language-neutral per-file model (symbols, imports, exports, references, diagnostics) with language-specific `attrs`. |
| Parsing engine | `engines/parser` | tree-sitter grammars + one extraction adapter per language family. Time-bounded parse, depth/count-bounded extraction. |
| Semantic engine | `engines/semantic` | Cross-file resolution with confidence and provenance ([spec](../specifications/semantic-resolution.md)). |
| Engine process | `engines/cli` | `usie-engine info | parse | resolve`. Reads files only under `--root`, never follows symlinks, never executes input. |
| Repository ingestion | `services/repository-ingestion` | Git/ZIP/local acquisition, scanning, classification, hashing, ignore rules, manifests ([spec](../specifications/ingestion.md)). |
| Engine client | `services/semantic-analysis` | Process supervision and the NDJSON protocol. |
| Knowledge graph | `services/knowledge-graph` | `Store` interface; PostgreSQL and in-memory implementations sharing one conformance test suite. |
| Orchestration | `services/execution-orchestrator` | Job queue (leases, heartbeats, retries, cancellation), worker loop, `usie-worker` binary. |
| Pipeline | `internal/pipeline` | The `analyze_version` job. |
| API gateway | `services/api-gateway` | HTTP API (`openapi.yaml`), `usie-api` binary. |
| Data access | `internal/store`, `database/` | Tenant-scoped queries; embedded forward-only migrations. |
| Web | `apps/web`, `packages/sdk`, `packages/design-system` | React application, typed client, shared components and tokens. |

## The analysis of one version

1. `POST /repositories/{id}/analyze` (or an upload) creates a
   `repository_version` and an `analysis_job` in one transaction.
2. A worker claims the job (`FOR UPDATE SKIP LOCKED`), taking a lease that its
   heartbeat extends. A lost lease re-queues the job; cancellation is observed
   at the next heartbeat.
3. **Fetch.** Clone over HTTPS into a job-private directory, extract the ZIP,
   or resolve the local path under an allowed root.
4. **Scan.** Walk without following symlinks, apply `.gitignore`/
   `.usieignore`, hash every file (SHA-256), classify, detect encoding,
   generated and vendored files, store text content by hash, collect
   manifests. Limits on file count and bytes fail the job; nothing is
   silently truncated.
5. **Compare** with the previous ready version: files are added, modified or
   unchanged by content hash.
6. **Parse** only content that has no cached analysis for the current parser
   fingerprint (`file_analyses`, keyed per organization). The engine reads the
   content-addressed source directly. Results are stored as gzip IR blobs.
7. **Resolve** all IR of the version in one engine run.
8. **Store** files, dependencies, references and the graph snapshot, and
   mark the version `ready`, in one transaction. A cancelled run cannot
   publish.

Re-running a job is idempotent, which makes at-least-once delivery safe.

## Data placement

| Data | Where | Notes |
|---|---|---|
| Accounts, tenancy, repositories, versions, jobs, audit | PostgreSQL | [data model](data-model.md) |
| File inventory, references, dependencies | PostgreSQL | per version |
| Knowledge graph | PostgreSQL (`graph_nodes`, `graph_relationships`) | immutable snapshot per version; [ADR 0003](../adr/0003-graph-storage.md) |
| Source content | blob store `org/<org>/src/<hash>` | deduplicated per organization |
| Parsed IR | blob store `org/<org>/ir/<parser>/<hash>` | gzip; cache keyed by parser fingerprint |
| Uploaded archives | blob store `org/<org>/uploads/` | |

## Scaling characteristics

Measured on this implementation (4 vCPU, release build), not projections:
parsing 3,000 Python files from the Python standard library and
site-packages took 3.5 s; resolving them (80k symbols, 190k relationships,
415k references) took 5.0 s in one process. Resolution holds the whole
version in memory; very large monorepos will need partitioned resolution
(planned).

Workers scale horizontally against one database. The filesystem blob store
must be shared by the API and all workers (a volume); an object-storage
backend is planned ([ADR 0004](../adr/0004-blob-storage.md)).

## Not built yet

These appear in the product specification and are **planned**; there is no
code for them beyond what is listed:

- AI reasoning (retrieval over the graph, provider abstraction).
- Transformation and verification (sandboxed builds and tests).
- Parsers for Java, C, C++, Rust, Go, C# and Kotlin (files are detected and
  inventoried only).
- Change-impact scoring. The *dependents* view (reverse traversal) is
  **experimental**.
- Neo4j projection, Kafka, Temporal, Redis, Kubernetes manifests,
  OpenTelemetry tracing (metrics and structured logs exist).
- Membership management API (memberships exist in the schema).
