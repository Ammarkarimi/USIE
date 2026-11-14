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
| Information leakage in errors | Internal errors return a generic message and request id; details only in logs. | `writeError` |

## Known gaps (not implemented)

- OS-level sandboxing of the engine and git (seccomp, user namespaces,
  network-less execution). Container limits are applied in the compose file
  only.
- DNS rebinding between the host check and git's own resolution; restrict
  worker egress at the network layer for a hard guarantee.
- Private repository credentials (the API accepts public HTTPS URLs only).
- PostgreSQL row-level security as a second isolation layer.
- Encryption of blobs at rest beyond what the storage provides.
- SSO, MFA, account lockout, password reset.
- AI-specific defenses (prompt injection) — there is no AI component yet.
