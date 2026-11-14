# Data model

Schema: `database/migrations/0001_init.sql` (forward-only, applied by both
services at startup under an advisory lock).

## Tenant isolation

Every tenant-owned table carries `organization_id`. Foreign keys between
tenant tables are composite `(organization_id, id)`, so a row cannot reference
another organization's row even through a bug in application code. Every
query in `internal/store`, `services/knowledge-graph` and
`services/execution-orchestrator` filters on the caller's organization; there
is no lookup by id alone. Resources of other organizations are reported as
`404`, not `403`, so their existence is not disclosed. The API tests
(`TestTenantIsolationAndAnalysisFlow`) and the graph conformance suite check
this. PostgreSQL row-level security is a planned second layer.

## Entities

| Entity (spec name) | Table | Status |
|---|---|---|
| Organization | `organizations` | implemented |
| User | `users` (argon2id hashes) | implemented |
| — membership & role | `memberships` (`owner`, `admin`, `member`, `viewer`) | implemented; no management API yet |
| — sessions, API tokens | `sessions`, `api_tokens` (SHA-256 of secrets only) | implemented |
| Workspace | `workspaces` | implemented |
| Repository | `repositories` | implemented |
| RepositoryVersion | `repository_versions` | implemented; one immutable analyzed state |
| Commit | `commits` | implemented (git sources, bounded history) |
| Branch | `branches` | implemented (git sources) |
| SourceFile | `source_files` | implemented |
| — parse cache | `file_analyses` | implemented |
| Symbol | `symbols` view over `graph_nodes` | implemented |
| SymbolReference | `symbol_references` | implemented, including unresolved references |
| Dependency | `dependencies` | implemented (declared in manifests) |
| GraphNode | `graph_nodes` | implemented |
| GraphRelationship | `graph_relationships` | implemented, with confidence and provenance |
| AnalysisJob | `analysis_jobs` | implemented |
| ExecutionWorker | `workers` | implemented |
| AuditEvent | `audit_events` | implemented |
| AnalysisResult | `repository_versions.stats` | implemented as measured statistics per version |
| CodeTransformation, VerificationRun, TestResult, AIConversation, AIRecommendation, BuildArtifact | — | planned; not created until the subsystems exist |

## Graph tables

`graph_nodes` has a composite primary key `(version_id, id)` where `id` is a
dense integer local to the snapshot, plus a unique stable `key`.
`graph_relationships` references nodes by `(version_id, src)` and
`(version_id, dst)`, unique per `(version_id, src, dst, kind)`, and stores
`confidence`, `rule`, `evidence_path`, `evidence_line` and `occurrences`.
Integer ids keep relationship rows small; stable keys make snapshots
comparable across versions (`Store.Diff`).

## Indexes worth knowing

- `analysis_jobs_claim_idx` — partial index used by `Claim`.
- `graph_relationships_dst_idx` — reverse traversal (callers, dependents).
- `graph_nodes_name_idx` — prefix search on lower-cased names.
- `source_files_hash_idx` — finding files by content hash.

## Audit events

Recorded: registration, login (success and failure), logout, token creation
and revocation, workspace and repository creation and deletion, analysis
requests, job cancellation, and authorization denials. Events are only ever
inserted. Audit write failures are logged as errors and do not undo the
action that was being audited.
