package store

import (
	"context"
	"errors"
	"time"

	"github.com/jackc/pgx/v5"
)

// RuleError is a refused team change; Message explains the rule.
type RuleError struct{ Message string }

func (e *RuleError) Error() string { return e.Message }

func rule(msg string) error { return &RuleError{Message: msg} }

type Member struct {
	UserID      string    `json:"user_id"`
	Email       string    `json:"email"`
	DisplayName string    `json:"display_name"`
	Role        string    `json:"role"`
	Disabled    bool      `json:"disabled"`
	JoinedAt    time.Time `json:"joined_at"`
}

func (s *Store) ListMembers(ctx context.Context, orgID string) ([]Member, error) {
	rows, err := s.Pool.Query(ctx, `SELECT u.id, u.email, u.display_name, m.role, u.disabled, m.created_at
		FROM memberships m JOIN users u ON u.id = m.user_id WHERE m.organization_id = $1
		ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 WHEN 'member' THEN 2 ELSE 3 END, lower(u.email)`, orgID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []Member{}
	for rows.Next() {
		var m Member
		if err := rows.Scan(&m.UserID, &m.Email, &m.DisplayName, &m.Role, &m.Disabled, &m.JoinedAt); err != nil {
			return nil, err
		}
		out = append(out, m)
	}
	return out, rows.Err()
}

// canManage applies the team rules for an actor acting on a target:
// nobody acts on themselves this way, admins cannot touch owners, and only
// owners grant or take the owner role.
func canManage(actorRole, targetRole, newRole string, self bool) error {
	switch {
	case self:
		return rule("you cannot change your own membership here")
	case !RoleAtLeast(actorRole, "admin"):
		return rule("only administrators manage members")
	case targetRole == "owner" && actorRole != "owner":
		return rule("only an owner can change or remove an owner")
	case newRole == "owner" && actorRole != "owner":
		return rule("only an owner can make someone an owner")
	}
	return nil
}

// lockMembers locks the organization's memberships for a team change, so
// the last-owner check cannot race.
func lockMembers(ctx context.Context, tx pgx.Tx, orgID string) (map[string]string, error) {
	rows, err := tx.Query(ctx, `SELECT user_id, role FROM memberships WHERE organization_id = $1 FOR UPDATE`, orgID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	roles := map[string]string{}
	for rows.Next() {
		var id, role string
		if err := rows.Scan(&id, &role); err != nil {
			return nil, err
		}
		roles[id] = role
	}
	return roles, rows.Err()
}

func owners(roles map[string]string) int {
	n := 0
	for _, r := range roles {
		if r == "owner" {
			n++
		}
	}
	return n
}

// SetRole changes a member's role.
func (s *Store) SetRole(ctx context.Context, orgID, actorID, actorRole, userID, role string) error {
	if _, ok := roleRank[role]; !ok {
		return rule("unknown role")
	}
	return pgx.BeginFunc(ctx, s.Pool, func(tx pgx.Tx) error {
		roles, err := lockMembers(ctx, tx, orgID)
		if err != nil {
			return err
		}
		current, ok := roles[userID]
		if !ok {
			return ErrNotFound
		}
		if err := canManage(actorRole, current, role, actorID == userID); err != nil {
			return err
		}
		if current == "owner" && role != "owner" && owners(roles) == 1 {
			return rule("an organization needs at least one owner")
		}
		_, err = tx.Exec(ctx, `UPDATE memberships SET role = $3 WHERE organization_id = $1 AND user_id = $2`, orgID, userID, role)
		return err
	})
}

// RemoveMember removes a membership, its sessions and its API tokens in
// this organization. Members may remove themselves (leave) unless they are
// the last owner.
func (s *Store) RemoveMember(ctx context.Context, orgID, actorID, actorRole, userID string) error {
	return pgx.BeginFunc(ctx, s.Pool, func(tx pgx.Tx) error {
		roles, err := lockMembers(ctx, tx, orgID)
		if err != nil {
			return err
		}
		current, ok := roles[userID]
		if !ok {
			return ErrNotFound
		}
		if actorID != userID {
			if err := canManage(actorRole, current, "", false); err != nil {
				return err
			}
		}
		if current == "owner" && owners(roles) == 1 {
			return rule("the last owner cannot leave or be removed; make someone else an owner first")
		}
		for _, q := range []string{
			`DELETE FROM memberships WHERE organization_id = $1 AND user_id = $2`,
			`DELETE FROM sessions WHERE organization_id = $1 AND user_id = $2`,
			`UPDATE api_tokens SET revoked_at = now() WHERE organization_id = $1 AND user_id = $2 AND revoked_at IS NULL`,
		} {
			if _, err := tx.Exec(ctx, q, orgID, userID); err != nil {
				return err
			}
		}
		return nil
	})
}

type Organization struct {
	ID   string `json:"id"`
	Name string `json:"name"`
	Role string `json:"role"`
}

func (s *Store) UserOrganizations(ctx context.Context, userID string) ([]Organization, error) {
	rows, err := s.Pool.Query(ctx, `SELECT o.id, o.name, m.role FROM memberships m JOIN organizations o ON o.id = m.organization_id
		WHERE m.user_id = $1 ORDER BY lower(o.name), o.id`, userID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []Organization{}
	for rows.Next() {
		var o Organization
		if err := rows.Scan(&o.ID, &o.Name, &o.Role); err != nil {
			return nil, err
		}
		out = append(out, o)
	}
	return out, rows.Err()
}

func (s *Store) IsMember(ctx context.Context, orgID, userID string) (bool, error) {
	var ok bool
	err := s.Pool.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM memberships WHERE organization_id = $1 AND user_id = $2)`, orgID, userID).Scan(&ok)
	return ok, err
}

// ---- invitations -----------------------------------------------------------

type Invitation struct {
	ID         string     `json:"id"`
	Email      string     `json:"email"`
	Role       string     `json:"role"`
	CreatedBy  *string    `json:"created_by"`
	CreatedAt  time.Time  `json:"created_at"`
	ExpiresAt  time.Time  `json:"expires_at"`
	AcceptedAt *time.Time `json:"accepted_at"`
	RevokedAt  *time.Time `json:"revoked_at"`
	// Status is derived: open, accepted, revoked or expired.
	Status string `json:"status"`
}

const invitationColumns = `id, email, role, created_by::text, created_at, expires_at, accepted_at, revoked_at,
	CASE WHEN accepted_at IS NOT NULL THEN 'accepted' WHEN revoked_at IS NOT NULL THEN 'revoked' WHEN expires_at < now() THEN 'expired' ELSE 'open' END`

func scanInvitation(row pgx.Row) (Invitation, error) {
	var i Invitation
	err := row.Scan(&i.ID, &i.Email, &i.Role, &i.CreatedBy, &i.CreatedAt, &i.ExpiresAt, &i.AcceptedAt, &i.RevokedAt, &i.Status)
	return i, mapErr(err)
}

// CreateInvitation records an invitation. An expired open invitation for
// the same address is closed first; a live one is a conflict.
func (s *Store) CreateInvitation(ctx context.Context, orgID, actorID, actorRole, email, role string, tokenHash []byte, ttl time.Duration) (Invitation, error) {
	if _, ok := roleRank[role]; !ok {
		return Invitation{}, rule("unknown role")
	}
	if err := canManage(actorRole, "", role, false); err != nil {
		return Invitation{}, err
	}
	var inv Invitation
	err := pgx.BeginFunc(ctx, s.Pool, func(tx pgx.Tx) error {
		var member bool
		if err := tx.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM memberships m JOIN users u ON u.id = m.user_id
			WHERE m.organization_id = $1 AND lower(u.email) = lower($2))`, orgID, email).Scan(&member); err != nil {
			return err
		}
		if member {
			return rule("this address already belongs to a member")
		}
		if _, err := tx.Exec(ctx, `UPDATE invitations SET revoked_at = now() WHERE organization_id = $1 AND lower(email) = lower($2)
			AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at < now()`, orgID, email); err != nil {
			return err
		}
		var err error
		inv, err = scanInvitation(tx.QueryRow(ctx, `INSERT INTO invitations (organization_id, email, role, token_hash, created_by, expires_at)
			VALUES ($1, $2, $3, $4, $5, now() + $6::interval) RETURNING `+invitationColumns, orgID, email, role, tokenHash, actorID, ttl.String()))
		return err
	})
	if errors.Is(err, ErrConflict) {
		return inv, rule("an invitation to this address is already open; revoke it to send a new one")
	}
	return inv, err
}

func (s *Store) ListInvitations(ctx context.Context, orgID string) ([]Invitation, error) {
	rows, err := s.Pool.Query(ctx, `SELECT `+invitationColumns+` FROM invitations WHERE organization_id = $1 ORDER BY created_at DESC LIMIT 200`, orgID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []Invitation{}
	for rows.Next() {
		i, err := scanInvitation(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, i)
	}
	return out, rows.Err()
}

func (s *Store) RevokeInvitation(ctx context.Context, orgID, id string) error {
	tag, err := s.Pool.Exec(ctx, `UPDATE invitations SET revoked_at = now() WHERE organization_id = $1 AND id = $2 AND accepted_at IS NULL AND revoked_at IS NULL`, orgID, id)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return nil
}

// InvitationPreview is what the holder of an invitation link may see.
type InvitationPreview struct {
	OrganizationID   string `json:"-"`
	OrganizationName string `json:"organization_name"`
	Email            string `json:"email"`
	Role             string `json:"role"`
	ExistingAccount  bool   `json:"existing_account"`
}

// OpenInvitation resolves an invitation token that is open and unexpired.
func (s *Store) OpenInvitation(ctx context.Context, tokenHash []byte) (InvitationPreview, error) {
	var p InvitationPreview
	err := s.Pool.QueryRow(ctx, `SELECT i.organization_id, o.name, i.email, i.role, EXISTS (SELECT 1 FROM users u WHERE lower(u.email) = lower(i.email))
		FROM invitations i JOIN organizations o ON o.id = i.organization_id
		WHERE i.token_hash = $1 AND i.accepted_at IS NULL AND i.revoked_at IS NULL AND i.expires_at > now()`, tokenHash).
		Scan(&p.OrganizationID, &p.OrganizationName, &p.Email, &p.Role, &p.ExistingAccount)
	return p, mapErr(err)
}

// AcceptInvitation adds the invited address to the organization. For an
// existing account, verify is called with its password hash and must
// succeed; for a new address, the account is created with newHash.
func (s *Store) AcceptInvitation(ctx context.Context, tokenHash []byte, displayName, newHash string, verify func(hash string) bool) (userID, orgID string, err error) {
	err = pgx.BeginFunc(ctx, s.Pool, func(tx pgx.Tx) error {
		var invID, email, role string
		if err := tx.QueryRow(ctx, `SELECT id, organization_id, email, role FROM invitations
			WHERE token_hash = $1 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > now() FOR UPDATE`, tokenHash).
			Scan(&invID, &orgID, &email, &role); err != nil {
			return mapErr(err)
		}
		var hash string
		var disabled bool
		err := tx.QueryRow(ctx, `SELECT id, password_hash, disabled FROM users WHERE lower(email) = lower($1)`, email).Scan(&userID, &hash, &disabled)
		switch {
		case err == nil:
			if disabled || !verify(hash) {
				return ErrInvalidCredentials
			}
		case errors.Is(err, pgx.ErrNoRows):
			if newHash == "" {
				return rule("a password is required to create the account")
			}
			if err := tx.QueryRow(ctx, `INSERT INTO users (email, display_name, password_hash) VALUES ($1, $2, $3) RETURNING id`,
				email, displayName, newHash).Scan(&userID); err != nil {
				return mapErr(err)
			}
		default:
			return err
		}
		if _, err := tx.Exec(ctx, `INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, $3)
			ON CONFLICT (organization_id, user_id) DO NOTHING`, orgID, userID, role); err != nil {
			return err
		}
		_, err = tx.Exec(ctx, `UPDATE invitations SET accepted_at = now(), accepted_by = $2 WHERE id = $1`, invID, userID)
		return err
	})
	return userID, orgID, err
}

// ErrInvalidCredentials means a password did not match.
var ErrInvalidCredentials = errors.New("invalid credentials")

// ---- passwords ---------------------------------------------------------------

// CreatePasswordReset stores a reset token for a user.
func (s *Store) CreatePasswordReset(ctx context.Context, userID, issuedBy string, tokenHash []byte, ttl time.Duration) error {
	var by any
	if issuedBy != "" {
		by = issuedBy
	}
	return pgx.BeginFunc(ctx, s.Pool, func(tx pgx.Tx) error {
		// Earlier unused links stop working.
		if _, err := tx.Exec(ctx, `UPDATE password_resets SET used_at = now() WHERE user_id = $1 AND used_at IS NULL`, userID); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `INSERT INTO password_resets (user_id, token_hash, issued_by, expires_at) VALUES ($1, $2, $3, now() + $4::interval)`,
			userID, tokenHash, by, ttl.String())
		return err
	})
}

// AdminResetAllowed checks that an administrator may issue a reset link
// for a member: the member belongs to no other organization (a link would
// otherwise give access there too) and, for owners, the actor is an owner.
func (s *Store) AdminResetAllowed(ctx context.Context, orgID, actorID, actorRole, userID string) error {
	var role string
	var others int
	err := s.Pool.QueryRow(ctx, `SELECT m.role, (SELECT count(*) FROM memberships x WHERE x.user_id = m.user_id AND x.organization_id <> m.organization_id)
		FROM memberships m WHERE m.organization_id = $1 AND m.user_id = $2`, orgID, userID).Scan(&role, &others)
	if err != nil {
		return mapErr(err)
	}
	if err := canManage(actorRole, role, "", actorID == userID); err != nil {
		if actorID == userID {
			return rule("change your own password from your account settings")
		}
		return err
	}
	if others > 0 {
		return rule("this person also belongs to other organizations; they must reset their password through the email link")
	}
	return nil
}

// ResetPassword consumes a reset token, sets the password and ends the
// user's sessions.
func (s *Store) ResetPassword(ctx context.Context, tokenHash []byte, newHash string) (userID string, err error) {
	err = pgx.BeginFunc(ctx, s.Pool, func(tx pgx.Tx) error {
		if err := tx.QueryRow(ctx, `UPDATE password_resets SET used_at = now()
			WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now() RETURNING user_id`, tokenHash).Scan(&userID); err != nil {
			return mapErr(err)
		}
		if _, err := tx.Exec(ctx, `UPDATE users SET password_hash = $2 WHERE id = $1`, userID, newHash); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `DELETE FROM sessions WHERE user_id = $1`, userID)
		return err
	})
	return userID, err
}

// SetPassword changes a password and ends the user's other sessions.
func (s *Store) SetPassword(ctx context.Context, userID, keepSessionID, newHash string) error {
	return pgx.BeginFunc(ctx, s.Pool, func(tx pgx.Tx) error {
		if _, err := tx.Exec(ctx, `UPDATE users SET password_hash = $2 WHERE id = $1`, userID, newHash); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `DELETE FROM sessions WHERE user_id = $1 AND id::text <> $2`, userID, keepSessionID)
		return err
	})
}

// SwitchSession moves a session to another organization of its user.
func (s *Store) SwitchSession(ctx context.Context, sessionID, userID, orgID string) error {
	tag, err := s.Pool.Exec(ctx, `UPDATE sessions SET organization_id = $3 WHERE id = $1 AND user_id = $2
		AND EXISTS (SELECT 1 FROM memberships WHERE organization_id = $3 AND user_id = $2)`, sessionID, userID, orgID)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return nil
}

// UserByID returns a user.
func (s *Store) UserByID(ctx context.Context, id string) (User, error) {
	var u User
	err := s.Pool.QueryRow(ctx, `SELECT id, email, display_name, password_hash, disabled FROM users WHERE id = $1`, id).
		Scan(&u.ID, &u.Email, &u.DisplayName, &u.PasswordHash, &u.Disabled)
	return u, mapErr(err)
}
