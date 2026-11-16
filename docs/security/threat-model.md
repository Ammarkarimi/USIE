# Security model

## Assets
Source code of tenants, analysis results, credentials (passwords, sessions,
API tokens), and the integrity of the hosts running USIE.

## Trust boundaries and controls

| Threat | Control | Where |
|---|---|---|
| Malicious repository content attacks the analyzer | Content is data only: never executed, imported or evaluated. Parsing in a separate process with an empty environment; time, size, depth and count limits; IR validated on input; property tests over arbitrary bytes. | `engines/*`, ADR 0006 |
| Repository escapes its directory | Symlinks never followed (scan, engine); `O_NOFOLLOW`; engine refuses absolute paths and `..`; ZIP path validation; local paths confined to allowed roots after symlink resolution. | `scan.go`, `archive.go`, `engines/cli` |
| Git used to run code or reach internal systems | HTTPS only; hooks, submodules, LFS filters, credential helpers, redirects disabled; empty git config; host must resolve to a public address. | `git.go` |
| Decompression bombs and oversized repositories | Decompressed bytes counted; file-count and byte limits fail loudly. | `archive.go`, `scan.go` |
| Cross-tenant access | Organization id in every query; composite foreign keys; 404 for foreign resources; parse cache and blobs scoped per organization; tests for isolation. | `internal/store`, data-model.md |
| Credential theft | argon2id; session and API token secrets stored as SHA-256 only; API token secret shown once; tokens cannot mint tokens; constant-time comparisons; generic login failure message with equalized work for unknown users. | `authn`, ADR 0008 |
| CSRF | `SameSite=Strict` cookies plus a per-session CSRF token header on every non-GET request. | `server.go` |
| XSS | React escaping; repository content delivered as JSON strings and rendered in Monaco; strict CSP without inline scripts; `nosniff`, `X-Frame-Options: DENY`. | `server.go`, `apps/web` |
| Brute force and abuse | Per-IP rate limits, stricter on authentication; request body limits; bounded query parameters (depth, page sizes). | `server.go` |
| Privilege escalation | Role checks per route from one table that the OpenAPI test cross-checks; local paths require admin; token role capped by owner role; denials audited. | `routes.go` |
| Repudiation | Append-only audit trail of security-relevant actions with actor, IP and request id. | `audit_events` |
| Prompt injection through repository content | Evidence is framed as untrusted data inside a random per-request boundary; item delimiters in content are neutralized; the system prompt says not to follow instructions in evidence; the model has **no tools** and its output is only displayed; citations are checked server-side and unbacked claims are marked unsupported; answers are rendered as text, never HTML. | `services/ai-reasoning`, ADR 0009 |
| Source code sent to a third party | AI is off unless a provider key is configured; OpenAI requests are sent with `store: false`; the interface states what is sent and to whom before every question; only the asking organization's evidence is sent. | `ai/status`, Ask page |
| AI cost abuse | Per-organization daily token budget, per-user hourly limit, bounded concurrency and evidence size; viewers cannot ask; questions are audited (length only, not text). | `services/ai-reasoning` |
| AI-proposed code does harm | Proposals are only text: edits are applied in memory to stored sources and kept as a diff; nothing is written to the repository or executed by the API. Edits may only touch files the model was shown in full, must match exactly once, use clean repository paths (never `.git/`), and are size- and count-limited; one invalid edit rejects the proposal. The interface labels every proposal unverified until a sandbox run passes, and renders diffs as text. | `services/transformation`, ADR 0011 |
| Repository or AI code executed during verification | Never in the API or worker process: commands run in a bubblewrap sandbox on a worker with new user/PID/IPC/UTS/network namespaces (no network unless the operator enables it), all capabilities dropped, UID 65534, a cleared environment (no database URL, keys or tokens), `/usr` read-only, only the job's work tree and a tmpfs writable, and CPU, memory, file-size, process, wall-clock and output limits; the process group is killed on timeout. Commands are fixed per profile, never user-supplied. Verification is off unless an operator sets `USIE_SANDBOX=bubblewrap`; the sandbox self-tests at worker start. Tests check that secrets, host files and the network are unreachable. | `services/verification`, ADR 0011 |
| Account takeover through invitations or resets | Invitation and reset tokens are 256-bit random, stored only as SHA-256, single use, short-lived (invitations 7 days, emailed resets 1 hour, administrator-issued resets 24 hours), and carried in the URL fragment so they never reach server logs or `Referer` headers; the page removes them from the address bar. Accepting an invitation for an address that already has an account requires that account's password. Administrators can issue reset links only for people who belong to no other organization, so an administrator cannot take over an account that also belongs elsewhere. A reset or password change ends the user's other sessions; removal from an organization ends the member's sessions there and revokes their tokens for it. Reset requests answer the same whether or not an address has an account and send email in the background. Role changes follow fixed rules enforced in one transaction with row locks: nobody changes their own role, only owners grant or change the owner role, the last owner can be neither demoted nor removed. | `internal/store/team.go`, `handlers_team.go`, ADR 0012 |
| Email header injection | Recipients, subjects and the sender are rejected if they contain line breaks; subjects are Q-encoded; credentials are sent only over STARTTLS. | `internal/platform/mail` |
| Secrets in analyzed repositories exposed by USIE | Credential findings never store or return the value: messages show at most its first four characters and fingerprints hash the line with the value replaced. Secret and password patterns are RE2 expressions (linear time) over lines truncated to 4 KB, at most 100 findings per file. Rule matching runs on data only and never executes repository code. Reviewing findings and changing a quality gate are role-checked (member, admin) and audited. | `services/code-quality`, ADR 0013 |
| Information leakage in errors | Internal errors return a generic message and request id; details only in logs. | `writeError` |

## Known gaps (not implemented)

- OS-level sandboxing of the engine and git (seccomp, user namespaces,
  network-less execution). Container limits are applied in the compose file
  only. (Verification runs, which execute repository code, are sandboxed;
  parsing never executes code.)
- DNS rebinding between the host check and git's own resolution; restrict
  worker egress at the network layer for a hard guarantee.
- Private repository credentials (the API accepts public HTTPS URLs only).
- PostgreSQL row-level security as a second isolation layer.
- Encryption of blobs at rest beyond what the storage provides.
- SSO, MFA, account lockout (logins and password attempts are rate-limited
  per IP and per account, not locked).

### AI-specific residual risks

- Prompt injection is mitigated, not eliminated: a model can still be
  influenced by persuasive text in evidence. Because it can take no action
  and every claim shows its support and evidence, the impact is limited to a
  misleading answer, which the interface tells users to verify.
- Answers can be wrong even when every citation is valid: a citation shows
  which code the model looked at, not that its reading is correct.
- Evidence and answers are stored in the database for audit; deleting a
  conversation deletes them.

### Verification residual risks

- The sandbox shares the host kernel: a kernel vulnerability reachable from
  an unprivileged user namespace could escape it. Run workers that verify
  changes on hosts dedicated to USIE, and keep the kernel patched. No
  seccomp filter is applied yet.
- `RLIMIT_NPROC` counts processes per host user and is not enforced when
  the worker runs as root; the image runs it as an unprivileged user.
- With `USIE_SANDBOX_NETWORK=true`, sandboxed code can send what it reads
  (the repository) anywhere and reach internal services; leave it off unless
  dependencies must be downloaded and the network is otherwise restricted.
- Toolchains on the worker host are visible read-only inside the sandbox;
  their configuration files under `/usr` are readable by tested code.
