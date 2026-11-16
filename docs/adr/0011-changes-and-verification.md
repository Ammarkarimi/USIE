# 0011 — AI-proposed changes as validated diffs; verification in a bubblewrap sandbox

Status: accepted

## Context

The specification asks for AI-generated code changes and for verifying them
by building and testing. Its security rules say repository and generated
code are untrusted, must never run inside privileged services, and that
there is no unrestricted shell or AI code execution. USIE has read access to
analyzed sources only; it has no write access to users' repositories.

## Decision

**Proposals.** A member describes a change for an analyzed version. The
evidence retriever of ADR 0009 collects context, and up to six related files
(the focus symbol's file first, 120 000 characters in total) are given to
the model in full. The model returns a strict JSON proposal: feasibility,
summary, rationale citing evidence, risks, test plan, limitations, and edits
of the form *(path, modify|create|delete, exact search text, replacement)*.
The server applies them in memory. An edit may only modify or delete a file
the model was shown in full; its search text must occur exactly once; paths
must be clean, repository-relative and outside `.git/`; edits, files and
sizes are bounded. If any edit is invalid the whole proposal fails with the
reason, because a partly applied change is not what the model described.
The server, not the model, renders the unified diff, and the change impact
of that diff (ADR 0010) is stored with it. A proposal is never written to
the repository: users download it as a patch. It is labelled *unverified*
until a verification run passes. Proposals share the daily AI token budget
with answers and have their own hourly per-user limit.

**Verification.** A member picks a profile, a fixed argv list per ecosystem
(`go build/test`, `cargo test --offline`, `python -m pytest` /
`unittest`, `npm test`, `mvn -o test`, `gradle test --offline`,
`dotnet test`, `make test`). There is no free-form command input. The run
is a `verify_change` job on the existing queue, executed by a worker: the
version's stored sources are written to a job directory, the change's files
are applied, and each step runs under `prlimit` inside `bwrap` with new
user, PID, IPC, UTS and (by default) network namespaces, all capabilities
dropped, UID 65534, a cleared environment, `/usr` read-only, the work tree
and a tmpfs as the only writable places, and limits on CPU time, address
space, file size, process count, wall-clock time and kept output. By
default the profile also runs on the unchanged version, so the result shows
tests fixed and newly failing. A job that dies marks its run as failed and
never touches the analyzed version.

Bubblewrap was chosen over Docker because it needs no daemon or root, works
inside a single container (the Fly deployment) when unprivileged user
namespaces are available, and can be tested in CI. Workers report their
sandbox and available toolchains as capabilities, so the API can say which
profiles can run without running anything itself.

## Consequences

* The model can at worst produce a wrong or harmful diff that a person
  reviews; it cannot write files, run commands or reach anything.
* Verification depends on toolchains installed on workers and, without
  network, on dependencies being present. The default image installs
  Python only; other ecosystems need a worker image with their toolchain
  (`USIE_SANDBOX_READONLY_PATHS` mounts toolchains outside `/usr`).
* Binary and oversize files are not retained by the analysis, so they are
  missing from the sandbox; runs report how many.
* The sandbox shares the kernel with the worker; see the threat model for
  residual risks. Seccomp filtering is not applied yet.
