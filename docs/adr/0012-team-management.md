# 0012 — Team management with hashed one-time links; email optional

Status: accepted

## Context

Organizations, memberships and roles existed since ADR 0008, but the only
way to add someone was the first-user registration. Teams need to invite
people, change and revoke access, and recover from forgotten passwords.
Many self-hosted installations have no mail server, and the Vercel + Fly.io
deployment has none by default.

## Decision

- **Invitations** carry an email address and a role. The invitation token is
  random (256 bits), stored only as its SHA-256, valid for seven days and
  usable once. There is at most one open invitation per address and
  organization.
- **Links use the URL fragment** (`/accept-invite#token=…`,
  `/reset-password#token=…`). Fragments are not sent to servers, so tokens do
  not appear in access logs, proxies or `Referer` headers. The page reads the
  token, removes it from the address bar and posts it in a JSON body.
- **Email is optional.** With `USIE_SMTP_ADDR`, `USIE_SMTP_FROM` and
  `USIE_PUBLIC_URL` set, links are emailed. Without email, the inviting
  administrator receives the invitation link once and shares it; forgotten
  passwords are handled by an administrator issuing a 24-hour reset link.
- **Administrator-issued resets are limited** to people who belong to no
  other organization. Otherwise an administrator of one organization could
  take over an account that also has access to another. Such people use the
  emailed self-service reset.
- **Existing accounts** must enter their current password to accept an
  invitation, so a leaked invitation link cannot be used to sign in as them.
- **Role rules** are enforced in the store, in one transaction that locks
  the organization's memberships: admins manage admins, members and
  viewers; only owners grant or change the owner role; nobody changes their
  own role; the last owner can be neither demoted nor removed nor leave.
- **Ending access is immediate**: removal deletes the member's sessions in
  that organization and revokes their API tokens for it; a password reset
  ends every session; a password change ends every other session.
- A user in several organizations switches the current session between
  them; API tokens stay bound to one organization.

## Consequences

- Without email, sharing an invitation link is the administrator's
  responsibility; the interface says so and shows the link only once.
- Self-service resets need email; reset requests return the same answer
  whether or not the address has an account, and the message is sent in the
  background so timing does not reveal it either.
- Not implemented: SSO, MFA, account lockout, disabling users across the
  installation, transferring or deleting organizations.
