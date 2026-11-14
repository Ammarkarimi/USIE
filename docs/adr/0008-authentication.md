# 0008 — Sessions with CSRF tokens, API tokens, organization-scoped RBAC

## Decision
- Browser: email + password (argon2id, OWASP 2023 parameters) → random
  256-bit session token in an `HttpOnly`, `SameSite=Strict`, `Secure` cookie;
  only its SHA-256 is stored. State-changing requests must also send the
  session's CSRF token header.
- Programs: `Authorization: Bearer usie_…` API tokens, minted only from an
  interactive session, hashed at rest, optionally expiring, revocable. A
  token's effective role is the lower of its own role and its owner's current
  role.
- Roles per organization: viewer (read), member (create repositories, run
  and cancel analyses), admin (delete repositories, register local paths,
  workspaces, audit log), owner.
- Registration modes: `first-user` (default: one bootstrap account),
  `open`, `closed`.
- Per-IP token-bucket rate limits, stricter on authentication endpoints.

## Consequences
SSO/OIDC, invitations and membership management are not implemented. Rate
limits are per API replica and use the direct peer address; behind a reverse
proxy the proxy should enforce its own limits.
