# 0013 — Rule-based code quality checks in the engine, reviews by fingerprint

Status: accepted

## Context

USIE explained structure and the effect of changes but did not say whether
code was well written or safe. Teams expect the checks of tools such as
SonarQube: bugs, vulnerabilities, security hotspots, maintainability, secrets,
duplication, a gate on new code, and a way to dismiss false positives. The
platform rules apply: no fake results, and an assumption must not be shown as
a fact.

## Decision

- **Syntax-tree rules run in the engine**, in the same pass that parses a
  file, for all 11 languages. They reuse the tree, inherit the engine's
  process isolation and limits (ADR 0006), and are cached with the parse
  result; the rule-set version is part of the parser fingerprint, so changing
  a rule re-checks every file.
- **Text rules run in the worker** while scanning: credential formats and
  hard-coded passwords (every text file, including formats the engine cannot
  parse) and duplicated code across files. Go's RE2 regular expressions run in
  linear time, so repository content cannot cause catastrophic backtracking.
- **One catalog** describes every rule: category, severity, languages, what it
  matches, its known blind spots and how to fix. The API serves it and the UI
  shows it with each finding. A finding is presented as a pattern match, never
  as proof.
- **Fingerprints** (rule, path, whitespace-normalized matched code,
  occurrence) identify a finding across versions. New code is "not present in
  the previous analyzed version", which needs no line mapping or diff.
- **Reviews are stored per fingerprint**, not per version, so a decision
  carries over until the code changes. A false positive requires a reason.
  Counts, grades and the gate are computed when read, so a review takes effect
  without re-analysis.
- **Secrets are never stored.** Messages show at most the first four
  characters; fingerprints hash the line with the value replaced.
- **Quality gates** are per-repository lists of "metric greater than
  threshold fails" conditions with a default that blocks new bugs,
  vulnerabilities, unreviewed hotspots and any blocker.

## Consequences

- Rules are syntactic and per file. Without types and data flow,
  `sql-injection` and `command-execution` report values that are in fact
  constant or validated; the descriptions say so, and reviews handle it.
  Constants declared in the same file are recognized to reduce this noise.
- Test coverage is not measured; analysis never runs code. Verification runs
  (ADR 0011) execute tests but are opt-in and separate.
- Grades are defined by worst open severity and review share, not by an
  estimate of remediation effort; the definitions are shown in the UI.
- Duplication is line-based: renamed variables hide copies. It is bounded by
  a memory budget and reported as not measured above it.
- Versions analyzed before this change have no findings; the UI asks to
  analyze again, and the next version has no baseline for new code.
