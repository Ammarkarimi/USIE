# USIE — Universal Software Intelligence Engine

USIE ingests software repositories, parses them into a normalized
intermediate representation, resolves relationships between files, symbols
and external packages, and stores the result as a versioned knowledge graph
that you can explore through a web interface and a REST API.

Every relationship records **how sure** USIE is (confirmed, probable,
inferred) and **why** (the resolution rule and the source location of the
evidence). References USIE cannot resolve are kept, with the reason, instead
of being guessed.

This repository contains the first complete vertical slice of the product
specification. Some subsystems of that specification are not built yet; the
table below is the authoritative status, and the running system reports the
same status at `GET /api/v1/system` and on its Settings page.

## Status

| Capability | Status | Notes |
|---|---|---|
| Repository ingestion | **implemented** | Git over HTTPS (branch, tag or commit), ZIP upload, server-local directories under allowed roots. Hashing, classification, encoding, generated/vendored detection, ignore files, manifests, build systems, commits and branches, incremental re-analysis with a parse cache. |
| Parsing | **implemented** for Python, JavaScript, TypeScript, TSX | tree-sitter, time- and size-bounded, syntax errors reported as diagnostics. |
| Language detection | **implemented** for Python, JavaScript, TypeScript, Java, C, C++, Rust, Go, C#, Kotlin and common formats | Languages without a parser are inventoried, not analyzed. |
| Semantic resolution | **implemented** | Imports, exports, calls, instantiation, inheritance, JSX and decorator references across files, with confidence and provenance. [Specification](docs/specifications/semantic-resolution.md). |
| Knowledge graph | **implemented** | Immutable snapshot per version in PostgreSQL; search, bounded traversal with confidence filtering, file import graph, snapshot diff. |
| Web interface | **implemented** | Overview, repositories, analysis results, version comparison, dependencies, history, code browser with symbols and resolved/unresolved references, graph explorer, jobs, workers, API tokens, audit log. Light and dark themes. |
| REST API | **implemented** | [OpenAPI 3](services/api-gateway/openapi.yaml), typed TypeScript SDK in `packages/sdk`. |
| Security baseline | **implemented** | Sessions + CSRF, API tokens, organization isolation, RBAC, rate limits, audit log, hardened git, untrusted-input handling. [Security model](docs/security/threat-model.md). |
| Job orchestration | **implemented** | PostgreSQL queue with leases, heartbeats, retries, cancellation; horizontally scalable workers. |
| Change impact | **experimental** | "Dependents" view: reverse reachability through calls, imports and inheritance. No risk scoring. |
| Distributed execution | **experimental** | Several workers share one queue. No sandboxed build/test execution. |
| Parsers for Java, C, C++, Rust, Go, C#, Kotlin | **planned** | |
| AI reasoning, transformation, verification | **planned** | No code exists for these. |
| Neo4j, Kafka, Temporal, Redis, Kubernetes | **planned / deferred** | See the [ADRs](docs/adr/README.md) for why each is not used yet. |
| Type inference, data-flow analysis, dynamic imports with computed specifiers | **unsupported** | Calls through values of unknown type are reported unresolved, or matched heuristically and marked *inferred*. |

## Quick start

Requirements: Rust 1.90+, Go 1.24+, Node 20+ with pnpm 10, git, PostgreSQL
14+ server binaries.

```sh
make build   # engine, services and web app
make dev     # local PostgreSQL + API + worker → http://127.0.0.1:8080
```

Create the first account in the browser (it owns a new organization), add a
repository — for example `https://github.com/pallets/itsdangerous.git` — and
open it once its analysis is ready.

With Docker: `docker compose -f infrastructure/docker/compose.yaml up --build`.

Programmatic access: create an API token under Settings, then

```sh
curl -H "Authorization: Bearer $USIE_TOKEN" http://127.0.0.1:8080/api/v1/repositories
```

## What an analysis produces

For the `itsdangerous` repository (50 files) a run produced 182 symbols and
514 relationships, of which 91.4% were confirmed, 7.4% probable and 1.2%
inferred; of 539 examined reference sites, 37.8% resolved inside the
repository, 26.0% to external packages, 8.3% to builtins and 27.8% were
reported unresolved with a reason. These numbers come from the analysis
statistics shown on the repository page; they vary by codebase and say
nothing about other repositories.

Measured throughput of the engine on 3,000 Python files (4 vCPU, release
build): 3.5 s to parse, 5.0 s to resolve.

## Repository layout

```
engines/                     Rust workspace
  intermediate-representation  normalized per-file IR
  parser                       tree-sitter extraction (Python, JS/TS/TSX)
  semantic                     cross-file resolution
  cli                          usie-engine process (NDJSON protocol)
services/                    Go services (one module)
  api-gateway                  HTTP API, OpenAPI contract, usie-api
  repository-ingestion         git, ZIP, local sources; scanning; manifests
  semantic-analysis            engine client
  knowledge-graph              graph Store: PostgreSQL + in-memory
  execution-orchestrator       job queue, worker loop, usie-worker
internal/                    pipeline, data access, platform packages
database/migrations          SQL schema
apps/web                     React + TypeScript web application
packages/sdk                 typed API client
packages/design-system       shared components and design tokens
infrastructure/docker        container image and compose file
docs/                        architecture, ADRs, specifications, security, operations
scripts/                     test database, dev runner
```

## Testing

```sh
make test   # everything below
make lint   # rustfmt, clippy -D warnings, gofmt, go vet, golangci-lint
```

- **Rust** (43 tests): extraction behaviour per language, property tests over
  arbitrary bytes and source-like text, resolver scenarios (packages, path
  aliases, workspaces, CommonJS, shadowing, inheritance, ambiguity, cycles,
  determinism), and process-level tests of the engine protocol including
  path-escape refusal.
- **Go**: ingestion (classification, ignore rules, symlinks, limits, git URL
  and host validation, ZIP attacks, local-root confinement), a graph store
  conformance suite run against both implementations, engine contract tests,
  pipeline integration tests (incremental re-analysis, uploads, hostile
  archives, cancellation), and API tests (authentication, CSRF, tokens, roles,
  tenant isolation, full analysis through the API, OpenAPI coverage). Each
  integration test gets its own PostgreSQL database.
- **TypeScript**: SDK, design system and application tests.

## Documentation

- [Architecture overview](docs/architecture/overview.md) and [data model](docs/architecture/data-model.md)
- [Architecture decision records](docs/adr/README.md)
- Specifications: [semantic resolution](docs/specifications/semantic-resolution.md), [ingestion](docs/specifications/ingestion.md), [intermediate representation](docs/specifications/intermediate-representation.md)
- [Security model](docs/security/threat-model.md)
- [Configuration](docs/operations/configuration.md) and [running](docs/operations/running.md)
