# 0003 — PostgreSQL is the graph system of record; Neo4j deferred

## Context
The specification asks to evaluate Neo4j and says the graph database must not
be the only source of truth. Graph snapshots must be versioned, tenant
isolated and transactionally consistent with the rest of a version's data.

## Options
1. **Neo4j as primary store.** Expressive traversal. But a second
   transactional system, cross-store consistency for every version, separate
   tenant isolation, backups and operations.
2. **PostgreSQL tables + a `Store` interface.** Snapshots written in the same
   transaction that marks a version ready; composite keys for tenancy;
   traversal as bounded, level-by-level queries.
3. **Both, with Neo4j as a projection.** Adds value only when queries exceed
   what option 2 serves.

## Decision
Option 2 now. All graph access goes through `graph.Store`
(`services/knowledge-graph`), implemented for PostgreSQL and in memory, with
one conformance suite both must pass. A Neo4j (or other) projection can later
implement the same interface and be rebuilt from PostgreSQL.

## Consequences
- Traversal is bounded (depth ≤ 6, ≤ 2,000 nodes) and reports truncation.
- Path queries, pattern matching and graph algorithms (centrality, community
  detection) are not available; they are the trigger to add a projection.
