# 0002 — Rust for analysis, Go for services, TypeScript for the web

## Context
The specification proposes Rust for analysis, Go for services, Python for AI
and React/TypeScript for the interface, and warns against introducing every
technology at once.

## Decision
- **Rust** for parsing and resolution: tree-sitter's native API, predictable
  memory use on hostile input, and data-parallel parsing (rayon).
- **Go** for services: one module, standard-library HTTP, pgx for
  PostgreSQL, straightforward process supervision.
- **TypeScript/React** for the web application, with a typed SDK.
- **No Python service yet.** It enters with the AI subsystem.

The Go/Rust boundary is the NDJSON protocol of `usie-engine` (ADR 0006),
covered by contract tests (`services/semantic-analysis/engine_test.go`).

## Consequences
Three toolchains (Rust 1.90+, Go 1.24+, Node 20+). Dependencies are pinned to
versions that build on these minimums; CI checks each toolchain separately.
