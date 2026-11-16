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
| Parsing | **implemented** for Python, JavaScript, TypeScript, TSX, Go, Java, Kotlin, C#, Rust, C, C++ | tree-sitter, time- and size-bounded, syntax errors reported as diagnostics. |
| Language detection | **implemented** for the languages above and common formats | Formats without a parser (shell, SQL, YAML, …) are inventoried, not analyzed. |
| Semantic resolution | **implemented** | Imports, exports, calls, instantiation, inheritance, JSX, decorator and annotation references across files, with confidence and provenance. Go: packages, `go.mod` modules, `vendor/`, methods across files, embedding. Java, Kotlin, C#: fully qualified types, imports and usings, inheritance, partial types, extension methods. Rust: crates, module tree, `use`, `impl` blocks, traits. C/C++: `#include` resolution, declarations vs definitions, namespaces, out-of-class methods. For the statically typed languages, calls also follow the declared types of variables, fields and call results. [Specification](docs/specifications/semantic-resolution.md). |
| Knowledge graph | **implemented** | Immutable snapshot per version in PostgreSQL; search, bounded traversal with confidence filtering, file import graph, snapshot diff. |
| Web interface | **implemented** | Overview, repositories, analysis results, version comparison, dependencies, history, code browser with symbols and resolved/unresolved references, graph explorer, code quality report with reviews and quality gate, change impact, AI questions and changes, jobs, workers, team and account settings, API tokens, audit log. Light and dark themes. |
| REST API | **implemented** | [OpenAPI 3](services/api-gateway/openapi.yaml), typed TypeScript SDK in `packages/sdk`. |
| Code quality and security | **implemented** | 22 syntax-tree rules across the 11 parsed languages (complexity, length, nesting, empty catch blocks, identical branches, unreachable code, SQL injection, OS commands, eval, weak hashes, disabled TLS verification, unsafe deserialization, unbounded C string functions, …), committed credentials and hard-coded passwords in every text file, duplicated code. Findings new since the previous version, reviews (accepted, false positive, safe) that carry over between versions, grades and a configurable quality gate. Pattern matching one file at a time: no types or data flow, so findings are for review. [Rules and definitions](docs/specifications/code-quality.md), [design](docs/adr/0013-code-quality.md). |
| Team management | **implemented** | Invite people by email address with a role; change roles and remove members (admins manage up to admin, only owners manage owners, the last owner is protected); leave an organization; switch between organizations; change password; password reset links. Links are emailed when SMTP is configured (`USIE_SMTP_*`); otherwise the inviting administrator gets the link once to share. [Design](docs/adr/0012-team-management.md). |
| Security baseline | **implemented** | Sessions + CSRF, API tokens, organization isolation, RBAC, rate limits, audit log, hardened git, untrusted-input handling. [Security model](docs/security/threat-model.md). |
| Job orchestration | **implemented** | PostgreSQL queue with leases, heartbeats, retries, cancellation; horizontally scalable workers. |
| Change impact | **implemented** | For node keys, files, a unified diff or a comparison of two analyzed versions: the declarations the change touches, every dependent reached backwards through resolved relationships (with path and weakest-link confidence), the tests that reach the change, and a 0–100 risk score whose six weighted factors are each shown and explained. Reachability through resolved relationships, not measured coverage. [Design](docs/adr/0010-change-impact.md). |
| Distributed execution | **implemented** | Several workers share one PostgreSQL queue (leases, heartbeats, retries, cancellation) for analysis and verification jobs; verification runs in a sandbox. |
| AI reasoning | **experimental** | Ask questions about an analyzed version. Answers come from knowledge-graph evidence; every claim is shown as cited, inference or unsupported after the server checks its citations. Needs `OPENAI_API_KEY` (or `ANTHROPIC_API_KEY`); off otherwise. [Design](docs/adr/0009-ai-reasoning.md). |
| AI code changes | **experimental** | A model proposes search-and-replace edits for files it was shown in full; the server validates and applies them, renders the diff and its change impact. Never written to the repository (download a patch); labelled unverified until a verification run passes. Needs an AI provider. [Design](docs/adr/0011-changes-and-verification.md). |
| Verification | **experimental** | Fixed build-and-test profiles (Go, Cargo, pytest/unittest, npm, Maven, Gradle, .NET, make) run on a worker in a bubblewrap sandbox: no network by default, no credentials, unprivileged, resource-limited; compared against the unchanged version. Off unless `USIE_SANDBOX=bubblewrap`; profiles need their toolchain on the worker (the image includes Python only). |
| Neo4j, Kafka, Temporal, Redis, Kubernetes | **planned / deferred** | See the [ADRs](docs/adr/README.md) for why each is not used yet. |
| Test coverage measurement | **unsupported** | Analysis never runs code. Verification runs (opt-in) execute tests but do not measure coverage. |
| Type inference, data-flow analysis, dynamic imports with computed specifiers | **unsupported** | Only *declared* types are used (Go, Java); generic type arguments are not substituted. Calls through values of unknown type are reported unresolved, or matched heuristically and marked *inferred*. |

## Quick start

Requirements: Rust 1.90+, Go 1.25+, Node 20+ with pnpm 10, git, PostgreSQL
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

For the statically typed languages, measured with the engine's
`analyze_dir` example on the same machine (repositories as of November
2026):

| Corpus | Files | References | Resolved in repo | External | Builtin | Unresolved | Time |
|---|---|---|---|---|---|---|---|
| this repository's Go code | 49 | 4,912 | 29.7% | 48.4% | 10.4% | 11.4% | 0.16 s |
| [google/gson](https://github.com/google/gson) (Java) | 268 | 30,092 | 43.1% | 34.1% | 7.8% | 14.9% | 0.66 s |
| [lua/lua](https://github.com/lua/lua) (C) | 68 | 9,286 | 93.7% | 1.7% | 3.3% | 1.3% | 0.28 s |
| this repository's Rust engine | 30 | 8,469 | 36.0% | 11.2% | 19.8% | 33.0% | 0.18 s |
| [App-vNext/Polly](https://github.com/App-vNext/Polly) (C#) | 803 | 38,940 | 23.3% | 31.5% | 5.8% | 39.4% | 1.6 s |
| [JakeWharton/timber](https://github.com/JakeWharton/timber) (Kotlin) | 12 | 1,651 | 29.0% | 33.3% | 3.4% | 34.3% | 0.05 s |
| [nlohmann/json](https://github.com/nlohmann/json) (C++) | 635 | 92,523 | 29.6% | 15.8% | 5.1% | 49.6% | 4.6 s |

The languages differ in how much a static analysis without a compiler can
establish. In Rust, C#, Kotlin and Java test code, many unresolved
references are values whose types the compiler infers (closure and lambda
parameters, `let` bindings) or values returned by libraries; nlohmann/json
opens its namespaces through preprocessor macros, which USIE does not
expand.

In Gson, 89% of the unresolved references are calls on values returned by
code outside the repository (for example `assertThat(x).isEqualTo(y)`), whose
types USIE does not know and does not guess. A sample of resolved Java edges
was checked by hand against the source; no wrong targets were found in it,
which is evidence, not proof, of precision.

Measured throughput of the engine on 3,000 Python files (4 vCPU, release
build): 3.5 s to parse, 5.0 s to resolve.

## Repository layout

```
engines/                     Rust workspace
  intermediate-representation  normalized per-file IR
  parser                       tree-sitter extraction (11 languages)
  semantic                     cross-file resolution
  cli                          usie-engine process (NDJSON protocol)
services/                    Go services (one module)
  api-gateway                  HTTP API, OpenAPI contract, usie-api
  repository-ingestion         git, ZIP, local sources; scanning; manifests
  semantic-analysis            engine client
  knowledge-graph              graph Store: PostgreSQL + in-memory
  execution-orchestrator       job queue, worker loop, usie-worker
  change-impact                diff mapping, dependents, tests, risk score
  code-quality                 secrets, duplication, findings, quality gates
  transformation               AI change proposals: edits, validation, diff
  verification                 sandboxed build-and-test profiles (bubblewrap)
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

- **Rust** (71 tests): extraction behaviour per language, property tests over
  arbitrary bytes and source-like text, resolver scenarios (packages, path
  aliases, workspaces, CommonJS, shadowing, inheritance, ambiguity, cycles,
  determinism; Go modules, vendoring, build variants, embedding, interfaces;
  Java imports, static imports, inheritance, typed locals and call chains;
  Kotlin final-by-default dispatch; C# namespaces, usings and partial
  types; Rust modules, `use`, impls and traits; C includes, declarations
  and definitions; C++ namespaces, out-of-class methods and virtual calls),
  code quality rules (each rule matching its pattern and not its closest
  safe variant, stable finding contexts), and process-level tests of the
  engine protocol including path-escape refusal.
- **Go**: ingestion (classification, ignore rules, symlinks, limits, git URL
  and host validation, ZIP attacks, local-root confinement), a graph store
  conformance suite run against both implementations, engine contract tests,
  pipeline integration tests (incremental re-analysis, uploads, hostile
  archives, cancellation), and API tests (authentication, CSRF, tokens, roles,
  tenant isolation, invitations, role rules, password resets, email delivery, full analysis through the API, code quality findings, new code, reviews and gates, OpenAPI coverage), and secret detection, duplication and gate evaluation. Each
  integration test gets its own PostgreSQL database.
- **TypeScript**: SDK, design system and application tests.

## Documentation

- [Architecture overview](docs/architecture/overview.md) and [data model](docs/architecture/data-model.md)
- [Architecture decision records](docs/adr/README.md)
- Specifications: [code quality](docs/specifications/code-quality.md), [semantic resolution](docs/specifications/semantic-resolution.md), [ingestion](docs/specifications/ingestion.md), [intermediate representation](docs/specifications/intermediate-representation.md)
- [Security model](docs/security/threat-model.md)
- [Configuration](docs/operations/configuration.md) and [running](docs/operations/running.md)
