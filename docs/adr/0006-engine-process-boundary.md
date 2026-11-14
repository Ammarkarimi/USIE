# 0006 — The analysis engine is a separate unprivileged process

## Context
Repositories are untrusted input. Parsers handle adversarial syntax; a crash
or hang in analysis must not take a service down.

## Decision
`usie-engine` runs as a short-lived child process per parse batch and per
resolution, with an empty environment (no credentials, no proxy settings),
reading only under the root it is given. Communication is NDJSON on
stdin/stdout; errors are one JSON line on stderr. The engine:

- refuses absolute paths, `..`, and symlinked files or directories;
- bounds file size, parse time (tree-sitter progress callback), recursion
  depth and per-file symbol/reference/import counts, reporting truncation;
- validates IR it receives (dense ids, in-range scopes) instead of trusting
  the caller;
- never executes, imports or evaluates the code it reads.

The services never run repository code at all. Git runs with hooks,
submodules, LFS filters, symlinks, credential helpers and non-HTTPS
transports disabled.

## Consequences
Process start-up per batch (milliseconds) is accepted for isolation. OS-level
sandboxing of the engine (seccomp, namespaces, cgroups) is the next step and
is not implemented; the compose file applies container-level limits.
