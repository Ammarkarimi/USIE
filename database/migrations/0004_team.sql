-- Team management: invitations to an organization and password resets.
-- Only SHA-256 hashes of invitation and reset tokens are stored.

CREATE TABLE invitations (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id  uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    email            text NOT NULL CHECK (length(email) BETWEEN 3 AND 320),
    role             text NOT NULL CHECK (role IN ('owner', 'admin', 'member', 'viewer')),
    token_hash       bytea NOT NULL UNIQUE,
    created_by       uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL,
    accepted_at      timestamptz,
    accepted_by      uuid REFERENCES users(id) ON DELETE SET NULL,
    revoked_at       timestamptz
);
CREATE INDEX invitations_org_idx ON invitations (organization_id, created_at DESC);
-- One open invitation per address and organization.
CREATE UNIQUE INDEX invitations_open_idx ON invitations (organization_id, lower(email))
    WHERE accepted_at IS NULL AND revoked_at IS NULL;

CREATE TABLE password_resets (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id          uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash       bytea NOT NULL UNIQUE,
    -- The administrator who issued the link; NULL for a link sent by email
    -- at the user's request.
    issued_by        uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL,
    used_at          timestamptz
);
CREATE INDEX password_resets_user_idx ON password_resets (user_id, created_at DESC);
