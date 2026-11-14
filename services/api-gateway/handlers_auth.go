package api

import (
	_ "embed"
	"errors"
	"net/http"
	"net/mail"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/ammarkarimi/usie/internal/platform/authn"
	"github.com/ammarkarimi/usie/internal/store"
)

//go:embed openapi.yaml
var openAPI []byte

func (s *Server) getOpenAPI(w http.ResponseWriter, _ *http.Request, _ *store.Principal) error {
	w.Header().Set("Content-Type", "application/yaml; charset=utf-8")
	_, err := w.Write(openAPI)
	return err
}

// Feature status is reported by the service itself so the interface never
// has to guess what is real. Keep in sync with README.md.
var features = []map[string]string{
	{"id": "repository-ingestion", "status": "implemented", "detail": "Git (HTTPS), local directories and ZIP uploads; classification, hashing, manifests, incremental re-analysis."},
	{"id": "parsing", "status": "implemented", "detail": "Python, JavaScript, TypeScript and TSX via tree-sitter. Other languages are detected and inventoried but not parsed."},
	{"id": "semantic-resolution", "status": "implemented", "detail": "Imports, exports, calls, instantiation and inheritance with confidence levels and provenance. No type inference or data-flow analysis."},
	{"id": "knowledge-graph", "status": "implemented", "detail": "Versioned snapshots in PostgreSQL with search, bounded traversal and snapshot diff."},
	{"id": "change-impact", "status": "experimental", "detail": "Dependents are available as reverse graph traversal. No risk scoring or test selection."},
	{"id": "ai-reasoning", "status": "planned", "detail": "Not implemented."},
	{"id": "transformation", "status": "planned", "detail": "Not implemented."},
	{"id": "verification", "status": "planned", "detail": "Not implemented."},
	{"id": "distributed-execution", "status": "experimental", "detail": "Multiple workers share a PostgreSQL job queue with leases and retries. No sandboxed build or test execution."},
}

func (s *Server) getSystem(w http.ResponseWriter, r *http.Request, _ *store.Principal) error {
	signup, err := s.Store.SignupAllowed(r.Context(), s.Cfg.SignupMode)
	if err != nil {
		return err
	}
	out := map[string]any{
		"version": s.Version, "signup_allowed": signup, "features": features,
		"sources": map[string]bool{"git": true, "upload": true, "local": len(s.Cfg.AllowedLocalRoots) > 0},
		"limits": map[string]int64{"max_upload_bytes": s.Cfg.MaxUploadBytes, "max_file_bytes": s.Cfg.MaxFileBytes,
			"max_repository_files": int64(s.Cfg.MaxRepoFiles), "max_repository_bytes": s.Cfg.MaxRepoBytes},
		"engine": s.Engine,
	}
	writeJSON(w, http.StatusOK, out)
	return nil
}

func validEmail(email string) bool {
	addr, err := mail.ParseAddress(email)
	return err == nil && addr.Address == email && len(email) <= 320 && strings.Contains(email[strings.LastIndex(email, "@"):], ".")
}

func validName(s string, max int) bool {
	if s == "" || len(s) > max || !utf8.ValidString(s) || strings.TrimSpace(s) != s {
		return false
	}
	for _, r := range s {
		if r < 0x20 || r == 0x7f {
			return false
		}
	}
	return true
}

func (s *Server) startSession(w http.ResponseWriter, r *http.Request, userID, orgID string) error {
	token, err := authn.NewToken("")
	if err != nil {
		return err
	}
	csrf, err := authn.NewToken("")
	if err != nil {
		return err
	}
	if err := s.Store.CreateSession(r.Context(), userID, orgID, authn.HashToken(token), csrf, s.Cfg.SessionTTL); err != nil {
		return err
	}
	http.SetCookie(w, &http.Cookie{
		Name: sessionCookie, Value: token, Path: "/", HttpOnly: true, Secure: s.Cfg.CookieSecure,
		SameSite: http.SameSiteStrictMode, MaxAge: int(s.Cfg.SessionTTL.Seconds()),
	})
	p, err := s.Store.PrincipalBySession(r.Context(), authn.HashToken(token))
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, map[string]any{"principal": p, "csrf_token": csrf})
	return nil
}

func (s *Server) register(w http.ResponseWriter, r *http.Request, _ *store.Principal) error {
	var in struct {
		Email        string `json:"email"`
		Password     string `json:"password"`
		DisplayName  string `json:"display_name"`
		Organization string `json:"organization"`
	}
	if err := decode(r, &in); err != nil {
		return err
	}
	in.Email = strings.TrimSpace(in.Email)
	switch {
	case !validEmail(in.Email):
		return errBadRequest("a valid email address is required")
	case len(in.Password) < authn.MinPasswordLength || len(in.Password) > authn.MaxPasswordLength:
		return errBadRequest("password must be between %d and %d characters", authn.MinPasswordLength, authn.MaxPasswordLength)
	case !validName(in.DisplayName, 100):
		return errBadRequest("display_name is required (max 100 characters)")
	case !validName(in.Organization, 100):
		return errBadRequest("organization is required (max 100 characters)")
	}
	hash, err := authn.HashPassword(in.Password)
	if err != nil {
		return err
	}
	userID, orgID, err := s.Store.Register(r.Context(), s.Cfg.SignupMode, in.Email, in.DisplayName, hash, in.Organization)
	switch {
	case errors.Is(err, store.ErrSignupClosed):
		return &apiError{http.StatusForbidden, "signup_closed", "registration is closed on this installation"}
	case errors.Is(err, store.ErrConflict):
		// Deliberately the same response as success would not be possible
		// here (a session is created), so report a generic conflict.
		return &apiError{http.StatusConflict, "conflict", "registration could not be completed with these details"}
	case err != nil:
		return err
	}
	s.audit(r, &store.Principal{UserID: userID, OrgID: orgID, Kind: "session"}, "user.registered", "user", userID, "success", nil)
	return s.startSession(w, r, userID, orgID)
}

// dummyHash equalizes the work done for unknown accounts and wrong passwords.
var dummyHash, _ = authn.HashPassword("usie-dummy-password")

func (s *Server) login(w http.ResponseWriter, r *http.Request, _ *store.Principal) error {
	var in struct {
		Email    string `json:"email"`
		Password string `json:"password"`
	}
	if err := decode(r, &in); err != nil {
		return err
	}
	if len(in.Password) > authn.MaxPasswordLength || len(in.Email) > 320 {
		return errBadRequest("invalid credentials format")
	}
	user, err := s.Store.UserByEmail(r.Context(), strings.TrimSpace(in.Email))
	if err != nil && !errors.Is(err, store.ErrNotFound) {
		return err
	}
	hash := dummyHash
	if err == nil {
		hash = user.PasswordHash
	}
	ok, _ := authn.VerifyPassword(hash, in.Password)
	if err != nil || !ok || user.Disabled {
		s.audit(r, nil, "auth.login", "user", "", "failure", nil)
		return &apiError{http.StatusUnauthorized, "invalid_credentials", "invalid email or password"}
	}
	orgID, err := s.Store.FirstMembership(r.Context(), user.ID)
	if err != nil {
		return &apiError{http.StatusForbidden, "no_organization", "this account does not belong to an organization"}
	}
	s.audit(r, &store.Principal{UserID: user.ID, OrgID: orgID, Kind: "session"}, "auth.login", "user", user.ID, "success", nil)
	return s.startSession(w, r, user.ID, orgID)
}

func (s *Server) logout(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if p.Kind == "session" {
		if err := s.Store.DeleteSession(r.Context(), p.SessionID); err != nil {
			return err
		}
		s.audit(r, p, "auth.logout", "user", p.UserID, "success", nil)
	}
	http.SetCookie(w, &http.Cookie{Name: sessionCookie, Value: "", Path: "/", HttpOnly: true, Secure: s.Cfg.CookieSecure, SameSite: http.SameSiteStrictMode, MaxAge: -1})
	w.WriteHeader(http.StatusNoContent)
	return nil
}

func (s *Server) me(w http.ResponseWriter, _ *http.Request, p *store.Principal) error {
	out := map[string]any{"principal": p}
	if p.Kind == "session" {
		out["csrf_token"] = p.CSRFToken
	}
	writeJSON(w, http.StatusOK, out)
	return nil
}

func (s *Server) listTokens(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	tokens, err := s.Store.ListTokens(r.Context(), p.OrgID, p.UserID)
	if err != nil {
		return err
	}
	if tokens == nil {
		tokens = []store.APIToken{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"tokens": tokens})
	return nil
}

func (s *Server) createToken(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	// Tokens are minted from an interactive session only, so a leaked token
	// cannot be used to mint replacements.
	if p.Kind != "session" {
		return &apiError{http.StatusForbidden, "forbidden", "API tokens can only be created from a signed-in session"}
	}
	var in struct {
		Name          string `json:"name"`
		Role          string `json:"role"`
		ExpiresInDays int    `json:"expires_in_days"`
	}
	if err := decode(r, &in); err != nil {
		return err
	}
	if !validName(in.Name, 100) {
		return errBadRequest("name is required (max 100 characters)")
	}
	if in.Role != "viewer" && in.Role != "member" && in.Role != "admin" {
		return errBadRequest("role must be viewer, member or admin")
	}
	if !store.RoleAtLeast(p.Role, in.Role) {
		return &apiError{http.StatusForbidden, "forbidden", "a token cannot have a stronger role than its owner"}
	}
	if in.ExpiresInDays < 0 || in.ExpiresInDays > 366 {
		return errBadRequest("expires_in_days must be between 0 (no expiry) and 366")
	}
	var expires *time.Time
	if in.ExpiresInDays > 0 {
		t := time.Now().Add(time.Duration(in.ExpiresInDays) * 24 * time.Hour)
		expires = &t
	}
	secret, err := authn.NewToken(tokenPrefix)
	if err != nil {
		return err
	}
	token, err := s.Store.CreateToken(r.Context(), p.OrgID, p.UserID, in.Name, in.Role, authn.HashToken(secret), secret[:len(tokenPrefix)+6], expires)
	if err != nil {
		return err
	}
	s.audit(r, p, "token.created", "api_token", token.ID, "success", map[string]any{"role": in.Role})
	// The secret is returned exactly once and is not recoverable afterwards.
	writeJSON(w, http.StatusCreated, map[string]any{"token": token, "secret": secret})
	return nil
}

func (s *Server) revokeToken(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	if err := s.Store.RevokeToken(r.Context(), p.OrgID, p.UserID, id); err != nil {
		return err
	}
	s.audit(r, p, "token.revoked", "api_token", id, "success", nil)
	w.WriteHeader(http.StatusNoContent)
	return nil
}

func (s *Server) listAudit(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	limit, err := queryInt(r, "limit", 100, 1, 500)
	if err != nil {
		return err
	}
	before, err := queryInt(r, "before", 0, 0, 1<<62)
	if err != nil {
		return err
	}
	events, err := s.Store.ListAudit(r.Context(), p.OrgID, int64(before), limit)
	if err != nil {
		return err
	}
	if events == nil {
		events = []store.AuditEvent{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"events": events})
	return nil
}
