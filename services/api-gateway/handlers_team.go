package api

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/ammarkarimi/usie/internal/platform/authn"
	"github.com/ammarkarimi/usie/internal/store"
)

const (
	invitationTTL    = 7 * 24 * time.Hour
	resetTTLByEmail  = time.Hour
	resetTTLByAdmin  = 24 * time.Hour
	maxTeamTokenSize = 200
)

func teamError(err error) error {
	var re *store.RuleError
	switch {
	case errors.As(err, &re):
		return &apiError{http.StatusConflict, "team_rule", re.Message}
	case errors.Is(err, store.ErrNotFound):
		return errNotFound
	}
	return err
}

func validRole(role string) bool {
	switch role {
	case "owner", "admin", "member", "viewer":
		return true
	}
	return false
}

// invitationPath and resetPath carry the token in the fragment, which
// browsers do not send to servers or in Referer headers.
func invitationPath(token string) string { return "/accept-invite#token=" + token }
func resetPath(token string) string      { return "/reset-password#token=" + token }

func (s *Server) listMembers(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	members, err := s.Store.ListMembers(r.Context(), p.OrgID)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, map[string]any{"members": members})
	return nil
}

func (s *Server) updateMember(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	userID, err := pathID(r, "user_id")
	if err != nil {
		return err
	}
	var in struct {
		Role string `json:"role"`
	}
	if err := decode(r, &in); err != nil {
		return err
	}
	if !validRole(in.Role) {
		return errBadRequest("role must be owner, admin, member or viewer")
	}
	if err := s.Store.SetRole(r.Context(), p.OrgID, p.UserID, p.Role, userID, in.Role); err != nil {
		s.audit(r, p, "member.role_change", "user", userID, "denied", map[string]any{"role": in.Role})
		return teamError(err)
	}
	s.audit(r, p, "member.role_change", "user", userID, "success", map[string]any{"role": in.Role})
	w.WriteHeader(http.StatusNoContent)
	return nil
}

func (s *Server) removeMember(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	userID, err := pathID(r, "user_id")
	if err != nil {
		return err
	}
	if userID == p.UserID && p.Kind != "session" {
		return &apiError{http.StatusForbidden, "session_required", "leave an organization from a browser session"}
	}
	if userID != p.UserID && !store.RoleAtLeast(p.Role, "admin") {
		return errForbidden
	}
	if err := s.Store.RemoveMember(r.Context(), p.OrgID, p.UserID, p.Role, userID); err != nil {
		s.audit(r, p, "member.remove", "user", userID, "denied", nil)
		return teamError(err)
	}
	s.audit(r, p, "member.remove", "user", userID, "success", map[string]any{"self": userID == p.UserID})
	w.WriteHeader(http.StatusNoContent)
	return nil
}

// issueMemberReset gives an administrator a one-time reset link for a
// member who belongs to no other organization (installations without
// email).
func (s *Server) issueMemberReset(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	userID, err := pathID(r, "user_id")
	if err != nil {
		return err
	}
	if err := s.Store.AdminResetAllowed(r.Context(), p.OrgID, p.UserID, p.Role, userID); err != nil {
		s.audit(r, p, "password.reset_issue", "user", userID, "denied", nil)
		return teamError(err)
	}
	token, err := authn.NewToken("")
	if err != nil {
		return err
	}
	if err := s.Store.CreatePasswordReset(r.Context(), userID, p.UserID, authn.HashToken(token), resetTTLByAdmin); err != nil {
		return err
	}
	s.audit(r, p, "password.reset_issue", "user", userID, "success", nil)
	writeJSON(w, http.StatusCreated, map[string]any{"reset_path": resetPath(token), "expires_at": time.Now().Add(resetTTLByAdmin).UTC()})
	return nil
}

func (s *Server) listInvitations(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	list, err := s.Store.ListInvitations(r.Context(), p.OrgID)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, map[string]any{"invitations": list, "email_enabled": s.Mail != nil})
	return nil
}

func (s *Server) createInvitation(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	var in struct {
		Email string `json:"email"`
		Role  string `json:"role"`
	}
	if err := decode(r, &in); err != nil {
		return err
	}
	in.Email = strings.TrimSpace(in.Email)
	if !validEmail(in.Email) {
		return errBadRequest("a valid email address is required")
	}
	if !validRole(in.Role) {
		return errBadRequest("role must be owner, admin, member or viewer")
	}
	token, err := authn.NewToken("")
	if err != nil {
		return err
	}
	inv, err := s.Store.CreateInvitation(r.Context(), p.OrgID, p.UserID, p.Role, in.Email, in.Role, authn.HashToken(token), invitationTTL)
	if err != nil {
		s.audit(r, p, "invitation.create", "invitation", "", "denied", map[string]any{"role": in.Role})
		return teamError(err)
	}
	out := map[string]any{"invitation": inv, "emailed": false}
	if s.Mail != nil {
		body := fmt.Sprintf("%s invited you to join %s on USIE as %s.\n\nAccept the invitation (valid for 7 days):\n%s%s\n\nIf you did not expect this, ignore this message.\n",
			p.DisplayName, p.OrgName, in.Role, s.Cfg.PublicURL, invitationPath(token))
		if err := s.Mail.Send(r.Context(), in.Email, "Invitation to "+p.OrgName+" on USIE", body); err != nil {
			s.Log.Warn("invitation email failed", "error", err.Error())
			out["email_error"] = "the email could not be sent; share the link instead"
			out["accept_path"] = invitationPath(token)
		} else {
			out["emailed"] = true
		}
	} else {
		// Shown once: only its hash is stored.
		out["accept_path"] = invitationPath(token)
	}
	s.audit(r, p, "invitation.create", "invitation", inv.ID, "success", map[string]any{"role": in.Role, "emailed": out["emailed"]})
	writeJSON(w, http.StatusCreated, out)
	return nil
}

func (s *Server) revokeInvitation(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	if err := s.Store.RevokeInvitation(r.Context(), p.OrgID, id); err != nil {
		return teamError(err)
	}
	s.audit(r, p, "invitation.revoke", "invitation", id, "success", nil)
	w.WriteHeader(http.StatusNoContent)
	return nil
}

type tokenBody struct {
	Token       string `json:"token"`
	Password    string `json:"password"`
	DisplayName string `json:"display_name"`
}

func readToken(t string) ([]byte, error) {
	if t == "" || len(t) > maxTeamTokenSize {
		return nil, errBadRequest("token is required")
	}
	return authn.HashToken(t), nil
}

var errInvalidLink = &apiError{http.StatusNotFound, "invalid_link", "this link is invalid, expired or already used"}

func (s *Server) previewInvitation(w http.ResponseWriter, r *http.Request, _ *store.Principal) error {
	var in tokenBody
	if err := decode(r, &in); err != nil {
		return err
	}
	hash, err := readToken(in.Token)
	if err != nil {
		return err
	}
	inv, err := s.Store.OpenInvitation(r.Context(), hash)
	if errors.Is(err, store.ErrNotFound) {
		return errInvalidLink
	}
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, inv)
	return nil
}

func (s *Server) acceptInvitation(w http.ResponseWriter, r *http.Request, _ *store.Principal) error {
	var in tokenBody
	if err := decode(r, &in); err != nil {
		return err
	}
	hash, err := readToken(in.Token)
	if err != nil {
		return err
	}
	inv, err := s.Store.OpenInvitation(r.Context(), hash)
	if errors.Is(err, store.ErrNotFound) {
		return errInvalidLink
	}
	if err != nil {
		return err
	}
	if !s.authLim.allow("account:" + strings.ToLower(inv.Email)) {
		w.Header().Set("Retry-After", "60")
		return &apiError{http.StatusTooManyRequests, "rate_limited", "too many attempts for this account; retry later"}
	}
	if len(in.Password) > authn.MaxPasswordLength {
		return errBadRequest("password is too long")
	}
	newHash := ""
	if !inv.ExistingAccount {
		if len(in.Password) < authn.MinPasswordLength {
			return errBadRequest("password must be between %d and %d characters", authn.MinPasswordLength, authn.MaxPasswordLength)
		}
		if !validName(in.DisplayName, 100) {
			return errBadRequest("display_name is required (max 100 characters)")
		}
		if newHash, err = authn.HashPassword(in.Password); err != nil {
			return err
		}
	}
	userID, orgID, err := s.Store.AcceptInvitation(r.Context(), hash, in.DisplayName, newHash, func(h string) bool {
		ok, _ := authn.VerifyPassword(h, in.Password)
		return ok
	})
	switch {
	case errors.Is(err, store.ErrNotFound):
		return errInvalidLink
	case errors.Is(err, store.ErrInvalidCredentials):
		s.audit(r, nil, "invitation.accept", "invitation", "", "failure", nil)
		return &apiError{http.StatusUnauthorized, "invalid_credentials", "the password of the existing account is not correct"}
	case err != nil:
		return teamError(err)
	}
	s.audit(r, &store.Principal{UserID: userID, OrgID: orgID, Kind: "session"}, "invitation.accept", "organization", orgID, "success", map[string]any{"new_account": !inv.ExistingAccount})
	return s.startSession(w, r, userID, orgID)
}

// requestPasswordReset emails a reset link when email is configured. The
// response is the same whether or not the address has an account.
func (s *Server) requestPasswordReset(w http.ResponseWriter, r *http.Request, _ *store.Principal) error {
	var in struct {
		Email string `json:"email"`
	}
	if err := decode(r, &in); err != nil {
		return err
	}
	in.Email = strings.TrimSpace(in.Email)
	if !validEmail(in.Email) {
		return errBadRequest("a valid email address is required")
	}
	if s.Mail == nil {
		return &apiError{http.StatusServiceUnavailable, "email_disabled", "this installation cannot send email; ask an administrator of your organization for a reset link"}
	}
	if !s.authLim.allow("reset:" + strings.ToLower(in.Email)) {
		w.Header().Set("Retry-After", "60")
		return &apiError{http.StatusTooManyRequests, "rate_limited", "too many reset requests for this address; retry later"}
	}
	user, err := s.Store.UserByEmail(r.Context(), in.Email)
	switch {
	case err == nil && !user.Disabled:
		token, err := authn.NewToken("")
		if err != nil {
			return err
		}
		if err := s.Store.CreatePasswordReset(r.Context(), user.ID, "", authn.HashToken(token), resetTTLByEmail); err != nil {
			return err
		}
		body := fmt.Sprintf("Someone asked to reset the password of your USIE account.\n\nChoose a new password (valid for 1 hour):\n%s%s\n\nIf it was not you, ignore this message; your password is unchanged.\n",
			s.Cfg.PublicURL, resetPath(token))
		// Sent in the background so timing does not reveal whether the
		// address has an account.
		go func() {
			ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			defer cancel()
			if err := s.Mail.Send(ctx, user.Email, "Reset your USIE password", body); err != nil {
				s.Log.Warn("password reset email failed", "error", err.Error())
			}
		}()
		s.audit(r, &store.Principal{UserID: user.ID, Kind: "session"}, "password.reset_request", "user", user.ID, "success", nil)
	case err != nil && !errors.Is(err, store.ErrNotFound):
		return err
	}
	writeJSON(w, http.StatusAccepted, map[string]any{"message": "If the address has an account, a reset link is on its way."})
	return nil
}

func (s *Server) confirmPasswordReset(w http.ResponseWriter, r *http.Request, _ *store.Principal) error {
	var in tokenBody
	if err := decode(r, &in); err != nil {
		return err
	}
	hash, err := readToken(in.Token)
	if err != nil {
		return err
	}
	if len(in.Password) < authn.MinPasswordLength || len(in.Password) > authn.MaxPasswordLength {
		return errBadRequest("password must be between %d and %d characters", authn.MinPasswordLength, authn.MaxPasswordLength)
	}
	newHash, err := authn.HashPassword(in.Password)
	if err != nil {
		return err
	}
	userID, err := s.Store.ResetPassword(r.Context(), hash, newHash)
	if errors.Is(err, store.ErrNotFound) {
		return errInvalidLink
	}
	if err != nil {
		return err
	}
	s.audit(r, &store.Principal{UserID: userID, Kind: "session"}, "password.reset", "user", userID, "success", nil)
	writeJSON(w, http.StatusOK, map[string]any{"message": "Your password was changed. Sign in with the new password; other sessions were signed out."})
	return nil
}

func (s *Server) changePassword(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if p.Kind != "session" {
		return &apiError{http.StatusForbidden, "session_required", "passwords can only be changed from a browser session"}
	}
	var in struct {
		CurrentPassword string `json:"current_password"`
		NewPassword     string `json:"new_password"`
	}
	if err := decode(r, &in); err != nil {
		return err
	}
	if len(in.NewPassword) < authn.MinPasswordLength || len(in.NewPassword) > authn.MaxPasswordLength || len(in.CurrentPassword) > authn.MaxPasswordLength {
		return errBadRequest("the new password must be between %d and %d characters", authn.MinPasswordLength, authn.MaxPasswordLength)
	}
	if !s.authLim.allow("account:" + strings.ToLower(p.Email)) {
		w.Header().Set("Retry-After", "60")
		return &apiError{http.StatusTooManyRequests, "rate_limited", "too many attempts; retry later"}
	}
	user, err := s.Store.UserByID(r.Context(), p.UserID)
	if err != nil {
		return err
	}
	if ok, _ := authn.VerifyPassword(user.PasswordHash, in.CurrentPassword); !ok {
		s.audit(r, p, "password.change", "user", p.UserID, "failure", nil)
		return &apiError{http.StatusUnauthorized, "invalid_credentials", "the current password is not correct"}
	}
	newHash, err := authn.HashPassword(in.NewPassword)
	if err != nil {
		return err
	}
	if err := s.Store.SetPassword(r.Context(), p.UserID, p.SessionID, newHash); err != nil {
		return err
	}
	s.audit(r, p, "password.change", "user", p.UserID, "success", nil)
	w.WriteHeader(http.StatusNoContent)
	return nil
}

func (s *Server) listMyOrganizations(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	orgs, err := s.Store.UserOrganizations(r.Context(), p.UserID)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, map[string]any{"organizations": orgs, "current": p.OrgID})
	return nil
}

func (s *Server) switchOrganization(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if p.Kind != "session" {
		return &apiError{http.StatusForbidden, "session_required", "API tokens belong to one organization"}
	}
	var in struct {
		OrganizationID string `json:"organization_id"`
	}
	if err := decode(r, &in); err != nil {
		return err
	}
	if !store.ValidID(in.OrganizationID) {
		return errBadRequest("organization_id must be an organization id")
	}
	if err := s.Store.SwitchSession(r.Context(), p.SessionID, p.UserID, strings.ToLower(in.OrganizationID)); err != nil {
		return teamError(err)
	}
	s.audit(r, p, "auth.switch_organization", "organization", in.OrganizationID, "success", nil)
	np, err := s.Store.PrincipalBySession(r.Context(), authn.HashToken(sessionToken(r)))
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, map[string]any{"principal": np, "csrf_token": np.CSRFToken})
	return nil
}

func sessionToken(r *http.Request) string {
	if c, err := r.Cookie(sessionCookie); err == nil {
		return c.Value
	}
	return ""
}
