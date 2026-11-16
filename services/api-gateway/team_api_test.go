package api

import (
	"context"
	"strings"
	"sync"
	"testing"
	"time"
)

// memMail records messages instead of sending them.
type memMail struct {
	mu   sync.Mutex
	msgs []sentMail
	got  chan struct{}
}

type sentMail struct{ to, subject, body string }

func newMemMail() *memMail { return &memMail{got: make(chan struct{}, 16)} }

func (m *memMail) Send(_ context.Context, to, subject, body string) error {
	m.mu.Lock()
	m.msgs = append(m.msgs, sentMail{to, subject, body})
	m.mu.Unlock()
	m.got <- struct{}{}
	return nil
}

func (m *memMail) wait(t *testing.T) sentMail {
	t.Helper()
	select {
	case <-m.got:
	case <-time.After(5 * time.Second):
		t.Fatal("no email was sent")
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.msgs[len(m.msgs)-1]
}

func linkToken(t *testing.T, link string) string {
	t.Helper()
	i := strings.Index(link, "#token=")
	if i < 0 {
		t.Fatalf("no token in %q", link)
	}
	return strings.Fields(link[i+len("#token="):])[0]
}

func errCode(out map[string]any) any {
	if e, ok := out["error"].(map[string]any); ok {
		return e["code"]
	}
	return nil
}

func (c *client) login(email, password string) {
	c.h.t.Helper()
	out := c.must(200, "POST", "/api/v1/auth/login", map[string]string{"email": email, "password": password})
	c.csrf = out["csrf_token"].(string)
}

func (c *client) adopt(out map[string]any) map[string]any {
	c.csrf = out["csrf_token"].(string)
	return out["principal"].(map[string]any)
}

func TestTeamInvitationsRolesAndResets(t *testing.T) {
	h := newHarness(t, "open")
	const pw = "correct horse battery"

	owner := h.client()
	ownerEmail := uniq("owner")
	op := owner.register(ownerEmail, "Acme")
	ownerID := op["user_id"].(string)
	if sys := owner.must(200, "GET", "/api/v1/system", nil); sys["email"] != false {
		t.Fatalf("email should be off: %v", sys["email"])
	}

	// Invitation without email: the link is returned once.
	bobEmail := uniq("bob")
	inv := owner.must(201, "POST", "/api/v1/invitations", map[string]string{"email": bobEmail, "role": "member"})
	if inv["emailed"] != false || !strings.HasPrefix(inv["accept_path"].(string), "/accept-invite#token=") {
		t.Fatalf("invitation: %v", inv)
	}
	bobToken := linkToken(t, inv["accept_path"].(string))
	if status, out := owner.do("POST", "/api/v1/invitations", map[string]string{"email": strings.ToUpper(bobEmail), "role": "viewer"}); status != 409 {
		t.Fatalf("second open invitation for the same address: %d %v", status, out)
	}
	for _, body := range []map[string]string{{"email": "nope", "role": "member"}, {"email": uniq("x"), "role": "god"}} {
		if status, _ := owner.do("POST", "/api/v1/invitations", body); status != 400 {
			t.Errorf("invalid invitation accepted: %v", body)
		}
	}
	list := owner.must(200, "GET", "/api/v1/invitations", nil)["invitations"].([]any)
	if len(list) != 1 || list[0].(map[string]any)["status"] != "open" {
		t.Fatalf("invitations: %v", list)
	}

	// Accepting as a new account.
	bob := h.client()
	prev := bob.must(200, "POST", "/api/v1/invitations/preview", map[string]string{"token": bobToken})
	if prev["organization_name"] != "Acme" || prev["existing_account"] != false || prev["role"] != "member" {
		t.Fatalf("preview: %v", prev)
	}
	if status, out := bob.do("POST", "/api/v1/invitations/preview", map[string]string{"token": "usie_wrong"}); status != 404 || errCode(out) != "invalid_link" {
		t.Fatalf("bad token: %d %v", status, out)
	}
	if status, _ := bob.do("POST", "/api/v1/invitations/accept", map[string]string{"token": bobToken, "password": "short", "display_name": "Bob"}); status != 400 {
		t.Fatal("short password accepted")
	}
	bp := bob.adopt(bob.must(200, "POST", "/api/v1/invitations/accept", map[string]string{"token": bobToken, "password": pw, "display_name": "Bob"}))
	if bp["organization_name"] != "Acme" || bp["role"] != "member" {
		t.Fatalf("bob: %v", bp)
	}
	bobID := bp["user_id"].(string)
	if status, _ := h.client().do("POST", "/api/v1/invitations/accept", map[string]string{"token": bobToken, "password": pw, "display_name": "Bob"}); status != 404 {
		t.Fatal("an invitation must be usable once")
	}
	if got := owner.must(200, "GET", "/api/v1/invitations", nil)["invitations"].([]any)[0].(map[string]any)["status"]; got != "accepted" {
		t.Fatalf("status after accept: %v", got)
	}

	// Members and role rules.
	members := bob.must(200, "GET", "/api/v1/members", nil)["members"].([]any)
	if len(members) != 2 {
		t.Fatalf("members: %v", members)
	}
	if status, _ := bob.do("PATCH", "/api/v1/members/"+ownerID, map[string]string{"role": "viewer"}); status != 403 {
		t.Fatal("a member changed a role")
	}
	if status, _ := bob.do("POST", "/api/v1/invitations", map[string]string{"email": uniq("y"), "role": "member"}); status != 403 {
		t.Fatal("a member invited someone")
	}
	if status, _ := bob.do("DELETE", "/api/v1/members/"+ownerID, nil); status != 403 {
		t.Fatal("a member removed someone")
	}
	owner.must(204, "PATCH", "/api/v1/members/"+bobID, map[string]string{"role": "admin"})
	for _, c := range []struct {
		path, role string
	}{
		{bobID, "owner"},   // own role
		{ownerID, "admin"}, // an owner, by an admin
	} {
		if status, out := bob.do("PATCH", "/api/v1/members/"+c.path, map[string]string{"role": c.role}); status != 409 || errCode(out) != "team_rule" {
			t.Errorf("admin set %s to %s: %d %v", c.path, c.role, status, out)
		}
	}
	if status, _ := bob.do("POST", "/api/v1/invitations", map[string]string{"email": uniq("z"), "role": "owner"}); status != 409 {
		t.Error("an admin invited an owner")
	}
	if status, out := owner.do("DELETE", "/api/v1/members/"+ownerID, nil); status != 409 {
		t.Fatalf("the last owner left: %d %v", status, out)
	}
	if status, _ := owner.do("PATCH", "/api/v1/members/"+ownerID, map[string]string{"role": "member"}); status != 409 {
		t.Fatal("the owner changed their own role")
	}

	// An existing account from another organization joins.
	carol := h.client()
	carolEmail := uniq("carol")
	cp := carol.register(carolEmail, "Beta")
	carolID, betaID := cp["user_id"].(string), cp["organization_id"].(string)
	inv = bob.must(201, "POST", "/api/v1/invitations", map[string]string{"email": carolEmail, "role": "viewer"})
	carolToken := linkToken(t, inv["accept_path"].(string))
	if prev := h.client().must(200, "POST", "/api/v1/invitations/preview", map[string]string{"token": carolToken}); prev["existing_account"] != true {
		t.Fatalf("preview for an existing account: %v", prev)
	}
	stranger := h.client()
	if status, out := stranger.do("POST", "/api/v1/invitations/accept", map[string]string{"token": carolToken, "password": "not her password"}); status != 401 || errCode(out) != "invalid_credentials" {
		t.Fatalf("accept with the wrong password: %d %v", status, out)
	}
	carolAcme := h.client()
	ap := carolAcme.adopt(carolAcme.must(200, "POST", "/api/v1/invitations/accept", map[string]string{"token": carolToken, "password": pw}))
	if ap["user_id"] != carolID || ap["role"] != "viewer" || ap["organization_name"] != "Acme" {
		t.Fatalf("existing account joined as %v", ap)
	}
	orgs := carolAcme.must(200, "GET", "/api/v1/auth/organizations", nil)
	if len(orgs["organizations"].([]any)) != 2 {
		t.Fatalf("organizations: %v", orgs)
	}
	sw := carolAcme.must(200, "POST", "/api/v1/auth/switch", map[string]string{"organization_id": betaID})
	if sw["principal"].(map[string]any)["organization_name"] != "Beta" || sw["principal"].(map[string]any)["role"] != "owner" {
		t.Fatalf("switch: %v", sw)
	}
	// Organization isolation: Beta's owner cannot manage Acme's members.
	if status, _ := carolAcme.do("PATCH", "/api/v1/members/"+bobID, map[string]string{"role": "viewer"}); status != 404 {
		t.Fatal("a member of another organization was changed")
	}
	if status, _ := carolAcme.do("POST", "/api/v1/auth/switch", map[string]string{"organization_id": "00000000-0000-0000-0000-000000000001"}); status != 404 {
		t.Fatal("switched to an organization the user does not belong to")
	}
	carolAcme.must(200, "POST", "/api/v1/auth/switch", map[string]string{"organization_id": op["organization_id"].(string)})

	// Administrator-issued reset links: refused for people in other organizations.
	if status, out := owner.do("POST", "/api/v1/members/"+carolID+"/password-reset", nil); status != 409 {
		t.Fatalf("reset for a member of another organization: %d %v", status, out)
	}
	if status, _ := owner.do("POST", "/api/v1/members/"+ownerID+"/password-reset", nil); status != 409 {
		t.Fatal("reset link for oneself")
	}
	reset := owner.must(201, "POST", "/api/v1/members/"+bobID+"/password-reset", nil)
	resetToken := linkToken(t, reset["reset_path"].(string))
	anon := h.client()
	if status, _ := anon.do("POST", "/api/v1/auth/password-reset/confirm", map[string]string{"token": resetToken, "password": "short"}); status != 400 {
		t.Fatal("short password accepted")
	}
	anon.must(200, "POST", "/api/v1/auth/password-reset/confirm", map[string]string{"token": resetToken, "password": "a brand new passphrase"})
	if status, _ := bob.do("GET", "/api/v1/auth/me", nil); status != 401 {
		t.Fatal("a reset must end existing sessions")
	}
	if status, _ := anon.do("POST", "/api/v1/auth/password-reset/confirm", map[string]string{"token": resetToken, "password": "another passphrase"}); status != 404 {
		t.Fatal("a reset link must be usable once")
	}
	bob = h.client()
	bob.login(bobEmail, "a brand new passphrase")

	// Changing one's own password keeps this session and ends the others.
	bob2 := h.client()
	bob2.login(bobEmail, "a brand new passphrase")
	if status, out := bob.do("POST", "/api/v1/auth/password", map[string]string{"current_password": "wrong one!", "new_password": "third passphrase"}); status != 401 {
		t.Fatalf("wrong current password: %d %v", status, out)
	}
	bob.must(204, "POST", "/api/v1/auth/password", map[string]string{"current_password": "a brand new passphrase", "new_password": "third passphrase"})
	bob.must(200, "GET", "/api/v1/auth/me", nil)
	if status, _ := bob2.do("GET", "/api/v1/auth/me", nil); status != 401 {
		t.Fatal("other sessions should end after a password change")
	}

	// API tokens cannot leave on the user's behalf; removal ends access.
	tok := bob.must(201, "POST", "/api/v1/tokens", map[string]any{"name": "ci", "role": "viewer", "expires_in_days": 1})
	bearer := &client{h: h, http: bob.http, token: tok["secret"].(string)}
	bearer.http = h.client().http
	if status, _ := bearer.do("DELETE", "/api/v1/members/"+bobID, nil); status != 403 {
		t.Fatal("a token removed its user from the organization")
	}
	owner.must(204, "DELETE", "/api/v1/members/"+bobID, nil)
	if status, _ := bob.do("GET", "/api/v1/auth/me", nil); status != 401 {
		t.Fatal("a removed member's session still works")
	}
	if status, _ := bearer.do("GET", "/api/v1/auth/me", nil); status != 401 {
		t.Fatal("a removed member's token still works")
	}

	// Leaving: Carol leaves Acme from her session there and is back to one organization.
	carolAcme.must(204, "DELETE", "/api/v1/members/"+carolID, nil)
	carol.login(carolEmail, pw)
	if orgs := carol.must(200, "GET", "/api/v1/auth/organizations", nil)["organizations"].([]any); len(orgs) != 1 {
		t.Fatalf("after leaving: %v", orgs)
	}

	// Revoked invitations stop working.
	inv = owner.must(201, "POST", "/api/v1/invitations", map[string]string{"email": uniq("dan"), "role": "member"})
	danToken := linkToken(t, inv["accept_path"].(string))
	owner.must(204, "DELETE", "/api/v1/invitations/"+inv["invitation"].(map[string]any)["id"].(string), nil)
	if status, _ := anon.do("POST", "/api/v1/invitations/preview", map[string]string{"token": danToken}); status != 404 {
		t.Fatal("a revoked invitation still works")
	}

	// Self-service reset needs email.
	if status, out := anon.do("POST", "/api/v1/auth/password-reset", map[string]string{"email": ownerEmail}); status != 503 || errCode(out) != "email_disabled" {
		t.Fatalf("reset without email: %d %v", status, out)
	}

	// Audit trail.
	events := owner.must(200, "GET", "/api/v1/audit-events?limit=200", nil)["events"].([]any)
	seen := map[string]bool{}
	for _, e := range events {
		seen[e.(map[string]any)["action"].(string)] = true
	}
	for _, a := range []string{"invitation.create", "invitation.accept", "member.role_change", "member.remove", "password.reset_issue", "invitation.revoke"} {
		if !seen[a] {
			t.Errorf("audit event %s missing", a)
		}
	}
}

func TestTeamEmail(t *testing.T) {
	h := newHarness(t, "first-user")
	m := newMemMail()
	h.api.Mail = m
	h.api.Cfg.PublicURL = "https://usie.example.test"

	owner := h.client()
	ownerEmail := uniq("owner")
	owner.register(ownerEmail, "Acme")
	if sys := owner.must(200, "GET", "/api/v1/system", nil); sys["email"] != true {
		t.Fatal("email should be reported as enabled")
	}

	ana := uniq("ana")
	inv := owner.must(201, "POST", "/api/v1/invitations", map[string]string{"email": ana, "role": "viewer"})
	if inv["emailed"] != true || inv["accept_path"] != nil {
		t.Fatalf("an emailed invitation must not return the link: %v", inv)
	}
	msg := m.wait(t)
	if msg.to != ana || !strings.Contains(msg.subject, "Acme") || !strings.Contains(msg.body, "https://usie.example.test/accept-invite#token=") {
		t.Fatalf("invitation email: %+v", msg)
	}
	a := h.client()
	a.adopt(a.must(200, "POST", "/api/v1/invitations/accept", map[string]string{"token": linkToken(t, msg.body), "password": "correct horse battery", "display_name": "Ana"}))

	// Unknown addresses get the same answer and no email.
	anon := h.client()
	out := anon.must(202, "POST", "/api/v1/auth/password-reset", map[string]string{"email": uniq("nobody")})
	out2 := anon.must(202, "POST", "/api/v1/auth/password-reset", map[string]string{"email": ownerEmail})
	if out["message"] != out2["message"] {
		t.Fatal("the answer reveals whether an account exists")
	}
	msg = m.wait(t)
	if msg.to != ownerEmail || !strings.Contains(msg.body, "https://usie.example.test/reset-password#token=") {
		t.Fatalf("reset email: %+v", msg)
	}
	m.mu.Lock()
	if len(m.msgs) != 2 {
		t.Errorf("expected 2 emails, got %d", len(m.msgs))
	}
	m.mu.Unlock()

	// A second request invalidates the first link.
	first := linkToken(t, msg.body)
	anon.must(202, "POST", "/api/v1/auth/password-reset", map[string]string{"email": ownerEmail})
	second := linkToken(t, m.wait(t).body)
	if status, _ := anon.do("POST", "/api/v1/auth/password-reset/confirm", map[string]string{"token": first, "password": "new passphrase here"}); status != 404 {
		t.Fatal("an older reset link still works")
	}
	anon.must(200, "POST", "/api/v1/auth/password-reset/confirm", map[string]string{"token": second, "password": "new passphrase here"})
	if status, _ := owner.do("GET", "/api/v1/auth/me", nil); status != 401 {
		t.Fatal("a reset must end existing sessions")
	}
	owner = h.client()
	owner.login(ownerEmail, "new passphrase here")
}
