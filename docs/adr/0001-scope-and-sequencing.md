# 0001 — Build a vertical slice before breadth

## Context
The product specification describes ten subsystems (ingestion through AI
reasoning, transformation, verification and distributed execution) and asks
not to implement them all at once. Each later subsystem consumes the
knowledge graph; its quality bounds theirs.

## Decision
Implement phase 1 and the parts of phase 2 needed for one complete,
production-shaped path: repository → inventory → parse → resolve → versioned
graph → API → web interface, with authentication, tenancy, auditing, limits
and tests. Support three language families (Python, JavaScript, TypeScript)
deeply rather than ten shallowly. Detect all ten target languages in the
inventory so that later parsers slot in.

## Consequences
- Every feature visible in the product works end to end on real
  repositories. Unbuilt subsystems are marked *planned* in the API
  (`GET /system`), the Settings page and the README, and have no screens.
- Change-impact analysis exists only as reverse traversal, marked
  *experimental*.
- Adding a language means an extraction adapter in `engines/parser` and
  resolution rules in `engines/semantic`; the IR, storage, API and UI are
  language-neutral.
