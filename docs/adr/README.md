# Architecture decision records

| # | Decision | Status |
|---|---|---|
| [0001](0001-scope-and-sequencing.md) | Build a vertical slice before breadth | accepted |
| [0002](0002-language-split.md) | Rust for analysis, Go for services, TypeScript for the web | accepted |
| [0003](0003-graph-storage.md) | PostgreSQL is the graph system of record; Neo4j deferred | accepted |
| [0004](0004-blob-storage.md) | Content-addressed blob store behind an interface | accepted |
| [0005](0005-job-queue.md) | PostgreSQL job queue instead of Kafka or Temporal | accepted |
| [0006](0006-engine-process-boundary.md) | The analysis engine is a separate unprivileged process | accepted |
| [0007](0007-confidence-and-provenance.md) | Confidence levels and provenance on every relationship | accepted |
| [0008](0008-authentication.md) | Sessions with CSRF tokens, API tokens, organization-scoped RBAC | accepted |
| [0009](0009-ai-reasoning.md) | Grounded AI answers: evidence retrieval, validated citations, no model tools | accepted |
| [0010](0010-change-impact.md) | Change impact from the knowledge graph, with an explained score | accepted |
| [0011](0011-changes-and-verification.md) | AI-proposed changes as validated diffs; verification in a bubblewrap sandbox | accepted |
