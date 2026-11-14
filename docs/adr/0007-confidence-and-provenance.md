# 0007 — Confidence levels and provenance on every relationship

## Context
The specification requires relationships to be classified as confirmed,
probable, inferred or unknown, with evidence, and forbids presenting
assumptions as facts.

## Decision
Each graph relationship stores `confidence`, the identifier of the `rule`
that produced it, the strongest evidence location and the occurrence count.
Every examined reference is stored, resolved or not; unresolved ones carry a
reason. The UI shows confidence as text with a distinct line style, filters
traversal by minimum confidence, and lists unresolved references beside the
code. Rules and levels are specified in
[semantic-resolution.md](../specifications/semantic-resolution.md).

## Consequences
Coverage is visible: the analysis page reports how many references resolved,
were external, builtin or unresolved, so users can judge how much to trust a
traversal for a given repository.
