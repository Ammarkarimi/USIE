// Package store is the relational data access layer for tenant data.
//
// Every method that touches tenant-owned rows takes the organization id and
// includes it in the SQL predicate. There is deliberately no method that
// looks a tenant row up by id alone.
package store

import (
	"context"
	"encoding/json"
	"errors"
	"regexp"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
)

var (
	ErrNotFound = errors.New("not found")
	ErrConflict = errors.New("conflict")
	// ErrSignupClosed is returned when registration is not permitted.
	ErrSignupClosed = errors.New("signup is closed")
)

type Store struct{ Pool *pgxpool.Pool }

func New(pool *pgxpool.Pool) *Store { return &Store{Pool: pool} }

var uuidPattern = regexp.MustCompile(`^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$`)

// ValidID reports whether s is a UUID. Checked before ids reach SQL so a
// malformed id is a 404, not a database error.
func ValidID(s string) bool { return uuidPattern.MatchString(s) }

func mapErr(err error) error {
	if errors.Is(err, pgx.ErrNoRows) {
		return ErrNotFound
	}
	var pg *pgconn.PgError
	if errors.As(err, &pg) && pg.Code == "23505" {
		return ErrConflict
	}
	return err
}

// Roles, weakest to strongest.
var roleRank = map[string]int{"viewer": 1, "member": 2, "admin": 3, "owner": 4}

// RoleAtLeast reports whether role grants at least the privileges of min.
func RoleAtLeast(role, min string) bool { return roleRank[role] >= roleRank[min] && roleRank[min] > 0 }

// Principal is an authenticated caller acting within one organization.
type Principal struct {
	UserID      string `json:"user_id"`
	Email       string `json:"email"`
	DisplayName string `json:"display_name"`
	OrgID       string `json:"organization_id"`
	OrgName     string `json:"organization_name"`
	Role        string `json:"role"`
	// Kind is "session" or "token".
	Kind      string `json:"kind"`
	SessionID string `json:"-"`
	CSRFToken string `json:"-"`
	TokenID   string `json:"-"`
}

type User struct {
	ID           string
	Email        string
	DisplayName  string
	PasswordHash string
	Disabled     bool
}

var slugUnsafe = regexp.MustCompile(`[^a-z0-9]+`)

func slugify(s string) string {
	slug := strings.Trim(slugUnsafe.ReplaceAllString(strings.ToLower(s), "-"), "-")
	if len(slug) > 40 {
		slug = strings.Trim(slug[:40], "-")
	}
	if slug == "" {
		slug = "org"
	}
	return slug
}

// Register creates a user, an organization owned by them and a default
// workspace. mode is the configured signup mode; "first-user" permits
// exactly one registration for the lifetime of the installation.
func (s *Store) Register(ctx context.Context, mode, email, displayName, passwordHash, orgName string) (userID, orgID string, err error) {
	err = pgx.BeginFunc(ctx, s.Pool, func(tx pgx.Tx) error {
		// Serialize registrations so the first-user check cannot race.
		if _, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(0x55534946)`); err != nil {
			return err
		}
		switch mode {
		case "open":
		case "first-user":
			var any bool
			if err := tx.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM users)`).Scan(&any); err != nil {
				return err
			}
			if any {
				return ErrSignupClosed
			}
		default:
			return ErrSignupClosed
		}
		if err := tx.QueryRow(ctx, `INSERT INTO users (email, display_name, password_hash) VALUES ($1, $2, $3) RETURNING id`,
			email, displayName, passwordHash).Scan(&userID); err != nil {
			return err
		}
		base := slugify(orgName)
		for attempt := 0; ; attempt++ {
			slug := base
			if attempt > 0 {
				slug = base + "-" + strings.ToLower(userID[:4+attempt])
			}
			err := tx.QueryRow(ctx, `INSERT INTO organizations (name, slug) VALUES ($1, $2) ON CONFLICT (slug) DO NOTHING RETURNING id`, orgName, slug).Scan(&orgID)
			if err == nil {
				break
			}
			if !errors.Is(err, pgx.ErrNoRows) || attempt >= 4 {
				return err
			}
		}
		if _, err := tx.Exec(ctx, `INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, 'owner')`, orgID, userID); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `INSERT INTO workspaces (organization_id, name) VALUES ($1, 'Default')`, orgID)
		return err
	})
	return userID, orgID, mapErr(err)
}

// SignupAllowed reports whether a registration would currently be accepted.
func (s *Store) SignupAllowed(ctx context.Context, mode string) (bool, error) {
	switch mode {
	case "open":
		return true, nil
	case "first-user":
		var any bool
		err := s.Pool.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM users)`).Scan(&any)
		return !any, err
	}
	return false, nil
}

func (s *Store) UserByEmail(ctx context.Context, email string) (User, error) {
	var u User
	err := s.Pool.QueryRow(ctx, `SELECT id, email, display_name, password_hash, disabled FROM users WHERE lower(email) = lower($1)`, email).
		Scan(&u.ID, &u.Email, &u.DisplayName, &u.PasswordHash, &u.Disabled)
	return u, mapErr(err)
}

// FirstMembership returns the organization a user signs in to.
func (s *Store) FirstMembership(ctx context.Context, userID string) (orgID string, err error) {
	err = s.Pool.QueryRow(ctx, `SELECT organization_id FROM memberships WHERE user_id = $1 ORDER BY created_at LIMIT 1`, userID).Scan(&orgID)
	return orgID, mapErr(err)
}

func (s *Store) CreateSession(ctx context.Context, userID, orgID string, tokenHash []byte, csrf string, ttl time.Duration) error {
	_, err := s.Pool.Exec(ctx, `INSERT INTO sessions (token_hash, user_id, organization_id, csrf_token, expires_at) VALUES ($1, $2, $3, $4, now() + $5::interval)`,
		tokenHash, userID, orgID, csrf, ttl.String())
	return err
}

const principalColumns = `u.id, u.email, u.display_name, o.id, o.name, m.role`

// PrincipalBySession resolves a session token hash. Expired sessions,
// disabled users and revoked memberships all yield ErrNotFound.
func (s *Store) PrincipalBySession(ctx context.Context, tokenHash []byte) (Principal, error) {
	p := Principal{Kind: "session"}
	err := s.Pool.QueryRow(ctx, `
		SELECT `+principalColumns+`, s.id, s.csrf_token
		  FROM sessions s
		  JOIN users u ON u.id = s.user_id AND NOT u.disabled
		  JOIN organizations o ON o.id = s.organization_id
		  JOIN memberships m ON m.organization_id = s.organization_id AND m.user_id = s.user_id
		 WHERE s.token_hash = $1 AND s.expires_at > now()`, tokenHash).
		Scan(&p.UserID, &p.Email, &p.DisplayName, &p.OrgID, &p.OrgName, &p.Role, &p.SessionID, &p.CSRFToken)
	return p, mapErr(err)
}

func (s *Store) DeleteSession(ctx context.Context, sessionID string) error {
	_, err := s.Pool.Exec(ctx, `DELETE FROM sessions WHERE id = $1`, sessionID)
	return err
}

func (s *Store) DeleteExpiredSessions(ctx context.Context) error {
	_, err := s.Pool.Exec(ctx, `DELETE FROM sessions WHERE expires_at < now()`)
	return err
}

// PrincipalByToken resolves an API token hash. The effective role is the
// weaker of the token's role and the owner's current membership role, so
// demoting a user also demotes their tokens.
func (s *Store) PrincipalByToken(ctx context.Context, tokenHash []byte) (Principal, error) {
	p := Principal{Kind: "token"}
	var tokenRole string
	err := s.Pool.QueryRow(ctx, `
		UPDATE api_tokens t SET last_used_at = now()
		  FROM users u, organizations o, memberships m
		 WHERE t.token_hash = $1 AND t.revoked_at IS NULL AND (t.expires_at IS NULL OR t.expires_at > now())
		   AND u.id = t.user_id AND NOT u.disabled AND o.id = t.organization_id
		   AND m.organization_id = t.organization_id AND m.user_id = t.user_id
		RETURNING `+principalColumns+`, t.id, t.role`, tokenHash).
		Scan(&p.UserID, &p.Email, &p.DisplayName, &p.OrgID, &p.OrgName, &p.Role, &p.TokenID, &tokenRole)
	if err != nil {
		return p, mapErr(err)
	}
	if roleRank[tokenRole] < roleRank[p.Role] {
		p.Role = tokenRole
	}
	return p, nil
}

type APIToken struct {
	ID         string     `json:"id"`
	Name       string     `json:"name"`
	Prefix     string     `json:"prefix"`
	Role       string     `json:"role"`
	CreatedAt  time.Time  `json:"created_at"`
	LastUsedAt *time.Time `json:"last_used_at"`
	ExpiresAt  *time.Time `json:"expires_at"`
	RevokedAt  *time.Time `json:"revoked_at"`
}

func (s *Store) CreateToken(ctx context.Context, orgID, userID, name, role string, hash []byte, prefix string, expiresAt *time.Time) (APIToken, error) {
	t := APIToken{Name: name, Prefix: prefix, Role: role, ExpiresAt: expiresAt}
	err := s.Pool.QueryRow(ctx, `INSERT INTO api_tokens (organization_id, user_id, name, token_hash, token_prefix, role, expires_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id, created_at`, orgID, userID, name, hash, prefix, role, expiresAt).Scan(&t.ID, &t.CreatedAt)
	return t, mapErr(err)
}

// ListTokens lists a user's own tokens within the organization.
func (s *Store) ListTokens(ctx context.Context, orgID, userID string) ([]APIToken, error) {
	rows, err := s.Pool.Query(ctx, `SELECT id, name, token_prefix, role, created_at, last_used_at, expires_at, revoked_at
		FROM api_tokens WHERE organization_id = $1 AND user_id = $2 ORDER BY created_at DESC`, orgID, userID)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, func(row pgx.CollectableRow) (APIToken, error) {
		var t APIToken
		err := row.Scan(&t.ID, &t.Name, &t.Prefix, &t.Role, &t.CreatedAt, &t.LastUsedAt, &t.ExpiresAt, &t.RevokedAt)
		return t, err
	})
}

func (s *Store) RevokeToken(ctx context.Context, orgID, userID, tokenID string) error {
	tag, err := s.Pool.Exec(ctx, `UPDATE api_tokens SET revoked_at = now() WHERE organization_id = $1 AND user_id = $2 AND id = $3 AND revoked_at IS NULL`, orgID, userID, tokenID)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

// ---- audit ----------------------------------------------------------------

type AuditEvent struct {
	ID         int64          `json:"id"`
	OrgID      string         `json:"-"`
	ActorID    *string        `json:"actor_user_id"`
	ActorEmail string         `json:"actor_email,omitempty"`
	ActorKind  string         `json:"actor_kind"`
	Action     string         `json:"action"`
	TargetType string         `json:"target_type"`
	TargetID   string         `json:"target_id"`
	Outcome    string         `json:"outcome"`
	IP         string         `json:"ip"`
	RequestID  string         `json:"request_id"`
	Detail     map[string]any `json:"detail"`
	CreatedAt  time.Time      `json:"created_at"`
}

// Audit appends an audit event. orgID may be empty for events that precede
// authentication (failed logins).
func (s *Store) Audit(ctx context.Context, e AuditEvent) error {
	detail := []byte("{}")
	if len(e.Detail) > 0 {
		var err error
		if detail, err = json.Marshal(e.Detail); err != nil {
			return err
		}
	}
	if e.Outcome == "" {
		e.Outcome = "success"
	}
	if e.ActorKind == "" {
		e.ActorKind = "user"
	}
	_, err := s.Pool.Exec(ctx, `INSERT INTO audit_events (organization_id, actor_user_id, actor_kind, action, target_type, target_id, outcome, ip, request_id, detail)
		VALUES (NULLIF($1, '')::uuid, $2::uuid, $3, $4, $5, $6, $7, $8, $9, $10)`,
		e.OrgID, e.ActorID, e.ActorKind, e.Action, e.TargetType, e.TargetID, e.Outcome, e.IP, e.RequestID, detail)
	return err
}

func (s *Store) ListAudit(ctx context.Context, orgID string, beforeID int64, limit int) ([]AuditEvent, error) {
	rows, err := s.Pool.Query(ctx, `
		SELECT a.id, a.actor_user_id::text, coalesce(u.email, ''), a.actor_kind, a.action, a.target_type, a.target_id, a.outcome, a.ip, a.request_id, a.detail, a.created_at
		  FROM audit_events a LEFT JOIN users u ON u.id = a.actor_user_id
		 WHERE a.organization_id = $1 AND ($2 = 0 OR a.id < $2) ORDER BY a.id DESC LIMIT $3`, orgID, beforeID, limit)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, func(row pgx.CollectableRow) (AuditEvent, error) {
		var e AuditEvent
		var detail []byte
		if err := row.Scan(&e.ID, &e.ActorID, &e.ActorEmail, &e.ActorKind, &e.Action, &e.TargetType, &e.TargetID, &e.Outcome, &e.IP, &e.RequestID, &detail, &e.CreatedAt); err != nil {
			return e, err
		}
		return e, json.Unmarshal(detail, &e.Detail)
	})
}
