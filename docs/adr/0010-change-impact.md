# 0010 — Change impact from the knowledge graph, with an explained score

Status: accepted

## Context

The product specification asks for change impact analysis: given a commit,
a diff or a function, what does the change affect, how risky is it, and
which tests cover it. USIE already stores a resolved graph per version with
confidence and provenance on every relationship. It does not run tests, so
it cannot measure coverage, and it has no history of failures to learn a
risk model from.

## Decision

* **Inputs.** A change is node keys, whole files, a unified diff that
  applies to the analyzed version, a base version of the same repository
  (changes between the two analyzed versions, computed by a line diff of the
  stored contents), or any combination. Diff hunks map to the innermost
  declarations whose lines they touch; changes between declarations count
  as a change of the file.
* **Dependents.** CALLS, INSTANTIATES, REFERENCES, EXTENDS, IMPLEMENTS and
  IMPORTS relationships are followed backwards, level by level (one store
  query per level, `Store.Incoming`), to a bounded depth (default 3, at most
  6) and node count. Each dependent keeps the shortest path to a changed
  declaration and, among equally short paths, the strongest; its confidence
  is the weakest link on that path. Only relationships at least as strong as
  the requested minimum are followed (default *probable*: heuristic name
  matches are excluded unless asked for).
* **Tests.** A dependent is a test when its file is classified as a test
  file (by directory and naming conventions). A changed declaration is
  *untested* when no test is among the dependents reached from it. This is
  reachability through resolved relationships, not measured coverage, and
  is reported as such.
* **Risk.** A 0–100 score is the weighted sum of six named factors, each in
  0..1 and each returned with its value, weight, points and a sentence
  explaining it: dependents (30), changes no test reaches (25), exported
  declarations changed (15), largest direct fan-in (15), files affected
  (10), dependents reached only through weaker evidence (5). Counts use
  logarithmic scales that saturate at stated values. The score ranks changes
  for review; it is not a probability of failure, and the response says so.
* **Access.** The endpoint (`POST /versions/{id}/impact`) needs the
  *viewer* role: it reads the version and changes nothing.

## Consequences

* The analysis is fast and deterministic and every number in it can be
  traced to graph relationships and stated weights.
* Its completeness is bounded by resolution: calls through values of
  unknown type, reflection, dependency injection, configuration and
  generated code are not followed, so dependents and tests can be missing.
  The response lists these limitations.
* Weights are a documented heuristic. If verification runs (ADR 0011)
  produce test results over time, they can calibrate the weights; that is
  not done yet.
