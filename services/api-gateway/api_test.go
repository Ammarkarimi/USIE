package api

import (
	"archive/zip"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"gopkg.in/yaml.v3"

	"github.com/ammarkarimi/usie/internal/pipeline"
	"github.com/ammarkarimi/usie/internal/platform/blob"
	"github.com/ammarkarimi/usie/internal/platform/config"
	"github.com/ammarkarimi/usie/internal/store"
	"github.com/ammarkarimi/usie/internal/testdb"
	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
	analysis "github.com/ammarkarimi/usie/services/semantic-analysis"
)

// TestOpenAPICoversEveryRoute keeps the contract and the implementation in
// lockstep, including the minimum role of each operation.
func TestOpenAPICoversEveryRoute(t *testing.T) {
	var doc struct {
		Paths map[string]map[string]struct {
			MinRole  string           `yaml:"x-min-role"`
			Security []map[string]any `yaml:"security"`
		} `yaml:"paths"`
	}
	if err := yaml.Unmarshal(openAPI, &doc); err != nil {
		t.Fatal(err)
	}
	seen := map[string]bool{}
	for _, rt := range routes() {
		path := strings.TrimPrefix(rt.Path, "/api/v1")
		op, ok := doc.Paths[path][strings.ToLower(rt.Method)]
		if !ok {
			t.Errorf("%s %s is not documented in openapi.yaml", rt.Method, rt.Path)
			continue
		}
		seen[rt.Method+" "+path] = true
		if op.MinRole != rt.MinRole {
			t.Errorf("%s %s: documented role %q, implemented %q", rt.Method, path, op.MinRole, rt.MinRole)
		}
		if (rt.MinRole == "") != (len(op.Security) == 0) {
			t.Errorf("%s %s: security requirement does not match", rt.Method, path)
		}
	}
	for path, methods := range doc.Paths {
		for method := range methods {
			if !seen[strings.ToUpper(method)+" "+path] {
				t.Errorf("%s %s is documented but not implemented", strings.ToUpper(method), path)
			}
		}
	}
}

func TestRateLimiter(t *testing.T) {
	l := newRateLimiter(3)
	for i := 0; i < 3; i++ {
		if !l.allow("a") {
			t.Fatalf("request %d should pass", i)
		}
	}
	if l.allow("a") {
		t.Fatal("4th request should be limited")
	}
	if !l.allow("b") {
		t.Fatal("limits are per key")
	}
}

func TestClientIP(t *testing.T) {
	req := func(xff ...string) *http.Request {
		r := httptest.NewRequest("GET", "/", nil)
		r.RemoteAddr = "10.0.0.9:1234"
		for _, v := range xff {
			r.Header.Add("X-Forwarded-For", v)
		}
		return r
	}
	direct := &Server{}
	if got := direct.clientIP(req("1.2.3.4")); got != "10.0.0.9" {
		t.Errorf("without trusted proxies the header must be ignored: %s", got)
	}
	// Two trusted proxies: Vercel sets the client, Fly appends Vercel's address.
	proxied := &Server{Cfg: config.Config{TrustedProxyHops: 2}}
	if got := proxied.clientIP(req("203.0.113.7, 198.51.100.1")); got != "203.0.113.7" {
		t.Errorf("client behind two proxies: %s", got)
	}
	// A client-supplied entry to the left of the trusted ones is ignored.
	if got := proxied.clientIP(req("6.6.6.6", "203.0.113.7, 198.51.100.1")); got != "203.0.113.7" {
		t.Errorf("forged leftmost entry was used: %s", got)
	}
	if got := proxied.clientIP(req()); got != "10.0.0.9" {
		t.Errorf("missing header: %s", got)
	}
	if got := proxied.clientIP(req("not-an-ip, 198.51.100.1")); got != "10.0.0.9" {
		t.Errorf("garbage entry: %s", got)
	}
}

func TestRepoPathValidation(t *testing.T) {
	for _, p := range []string{"a.py", "src/a/b.ts", "x"} {
		if _, err := repoPath(p, false); err != nil {
			t.Errorf("%q: %v", p, err)
		}
	}
	for _, p := range []string{"", "/etc/passwd", "../a", "a/../../b", "a/./b", "a//b", "a/", "a\\b", "a\x00b"} {
		if _, err := repoPath(p, false); err == nil {
			t.Errorf("%q should be rejected", p)
		}
	}
}

// ---- integration ---------------------------------------------------------

type harness struct {
	t      *testing.T
	srv    *httptest.Server
	pool   *pgxpool.Pool
	worker func(ctx context.Context)
	root   string
}

func newHarness(t *testing.T, signup string) *harness {
	t.Helper()
	pool := testdb.New(t)
	data := t.TempDir()
	root, _ := filepath.EvalSymlinks(t.TempDir())
	blobs, err := blob.NewFS(filepath.Join(data, "blobs"))
	if err != nil {
		t.Fatal(err)
	}
	cfg := config.Config{
		SignupMode: signup, SessionTTL: time.Hour, AllowedLocalRoots: []string{root}, DataDir: data,
		MaxUploadBytes: 1 << 20, MaxRepoFiles: 1000, MaxRepoBytes: 50 << 20, MaxFileBytes: 1 << 20,
		RateLimitPerMinute: 10000, AuthRateLimitPerMinute: 1000, GitTimeout: time.Minute, EngineTimeout: time.Minute, ParseTimeout: 5 * time.Second,
	}
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	queue := orchestrator.NewQueue(pool)
	s := &Server{Cfg: cfg, Store: store.New(pool), Queue: queue, Graph: graph.NewPostgres(pool), Blobs: blobs, Log: log, Version: "test"}
	h := &harness{t: t, srv: httptest.NewServer(s.Handler()), pool: pool, root: root}
	t.Cleanup(h.srv.Close)

	h.worker = func(ctx context.Context) {
		bin := os.Getenv("USIE_ENGINE_BIN")
		if bin == "" {
			bin, _ = filepath.Abs("../../engines/target/release/usie-engine")
		}
		if _, err := os.Stat(bin); err != nil {
			t.Skip("usie-engine binary not built")
		}
		engine := analysis.Engine{Bin: bin, ParseTimeout: cfg.ParseTimeout, MaxFileBytes: cfg.MaxFileBytes}
		info, err := engine.Info(ctx)
		if err != nil {
			t.Fatal(err)
		}
		p := &pipeline.Pipeline{Pool: pool, Blobs: blobs, Engine: engine, Info: info, Cfg: cfg, Log: log}
		ctx, cancel := context.WithCancel(ctx)
		w := &orchestrator.Worker{Queue: queue, Handler: p.Handle, Lease: 10 * time.Second, PollInterval: 50 * time.Millisecond, Log: log,
			OnFinish: func(*orchestrator.Job, string, time.Duration) { cancel() }}
		if err := w.Run(ctx); err != nil {
			t.Fatal(err)
		}
	}
	return h
}

// client is one browser-like user agent with its own cookie jar.
type client struct {
	h     *harness
	http  *http.Client
	csrf  string
	token string
}

func (h *harness) client() *client {
	jar, _ := cookiejar.New(nil)
	return &client{h: h, http: &http.Client{Jar: jar}}
}

func (c *client) do(method, path string, body any, headers ...string) (int, map[string]any) {
	c.h.t.Helper()
	var reader io.Reader
	ctype := "application/json"
	switch b := body.(type) {
	case nil:
	case []byte:
		reader, ctype = bytes.NewReader(b), "application/zip"
	default:
		data, _ := json.Marshal(b)
		reader = bytes.NewReader(data)
	}
	req, _ := http.NewRequest(method, c.h.srv.URL+path, reader)
	if reader != nil {
		req.Header.Set("Content-Type", ctype)
	}
	if c.csrf != "" {
		req.Header.Set(csrfHeader, c.csrf)
	}
	if c.token != "" {
		req.Header.Set("Authorization", "Bearer "+c.token)
	}
	for i := 0; i+1 < len(headers); i += 2 {
		req.Header.Set(headers[i], headers[i+1])
	}
	resp, err := c.http.Do(req)
	if err != nil {
		c.h.t.Fatal(err)
	}
	defer resp.Body.Close()
	out := map[string]any{}
	data, _ := io.ReadAll(resp.Body)
	json.Unmarshal(data, &out) //nolint:errcheck
	return resp.StatusCode, out
}

func (c *client) must(want int, method, path string, body any) map[string]any {
	c.h.t.Helper()
	status, out := c.do(method, path, body)
	if status != want {
		c.h.t.Fatalf("%s %s: status %d, want %d: %v", method, path, status, want, out)
	}
	return out
}

func (c *client) register(email, org string) map[string]any {
	out := c.must(200, "POST", "/api/v1/auth/register", map[string]string{"email": email, "password": "correct horse battery", "display_name": "Test User", "organization": org})
	c.csrf = out["csrf_token"].(string)
	return out["principal"].(map[string]any)
}

func uniq(prefix string) string {
	return fmt.Sprintf("%s-%d@example.test", prefix, time.Now().UnixNano())
}

func TestAuthenticationAndSessions(t *testing.T) {
	h := newHarness(t, "first-user")
	a := h.client()

	if status, out := a.do("GET", "/api/v1/auth/me", nil); status != 401 || out["error"].(map[string]any)["code"] != "unauthenticated" {
		t.Fatalf("anonymous me: %d %v", status, out)
	}
	sys := a.must(200, "GET", "/api/v1/system", nil)
	if sys["signup_allowed"] != true {
		t.Fatal("first user should be allowed to register")
	}

	// Validation.
	for _, body := range []map[string]string{
		{"email": "not-an-email", "password": "correct horse battery", "display_name": "x", "organization": "o"},
		{"email": "a@b.test", "password": "short", "display_name": "x", "organization": "o"},
		{"email": "a@b.test", "password": "correct horse battery", "display_name": "", "organization": "o"},
	} {
		if status, _ := a.do("POST", "/api/v1/auth/register", body); status != 400 {
			t.Errorf("invalid registration accepted: %v", body)
		}
	}
	if status, _ := a.do("POST", "/api/v1/auth/register", map[string]any{"email": "a@b.test", "extra": 1}); status != 400 {
		t.Error("unknown fields must be rejected")
	}

	email := uniq("owner")
	p := a.register(email, "Acme")
	if p["role"] != "owner" || p["organization_name"] != "Acme" {
		t.Fatalf("principal: %v", p)
	}
	if me := a.must(200, "GET", "/api/v1/auth/me", nil); me["csrf_token"] != a.csrf {
		t.Fatal("me must return the session CSRF token")
	}

	// first-user mode: nobody else may register now.
	b := h.client()
	if status, out := b.do("POST", "/api/v1/auth/register", map[string]string{"email": uniq("second"), "password": "correct horse battery", "display_name": "B", "organization": "B"}); status != 403 || out["error"].(map[string]any)["code"] != "signup_closed" {
		t.Fatalf("second registration: %d %v", status, out)
	}

	// Login: wrong password and unknown user give the same answer.
	if status, out := b.do("POST", "/api/v1/auth/login", map[string]string{"email": email, "password": "wrong password!"}); status != 401 || out["error"].(map[string]any)["code"] != "invalid_credentials" {
		t.Fatalf("wrong password: %d %v", status, out)
	}
	if status, _ := b.do("POST", "/api/v1/auth/login", map[string]string{"email": "nobody@example.test", "password": "wrong password!"}); status != 401 {
		t.Fatal("unknown user")
	}
	out := b.must(200, "POST", "/api/v1/auth/login", map[string]string{"email": strings.ToUpper(email), "password": "correct horse battery"})
	b.csrf = out["csrf_token"].(string)

	// CSRF: cookie-authenticated writes need the token.
	saved := b.csrf
	b.csrf = ""
	if status, out := b.do("POST", "/api/v1/workspaces", map[string]string{"name": "x"}); status != 403 || out["error"].(map[string]any)["code"] != "csrf" {
		t.Fatalf("missing CSRF: %d %v", status, out)
	}
	b.csrf = "wrong"
	if status, _ := b.do("POST", "/api/v1/workspaces", map[string]string{"name": "x"}); status != 403 {
		t.Fatal("wrong CSRF token accepted")
	}
	b.csrf = saved
	b.must(201, "POST", "/api/v1/workspaces", map[string]string{"name": "Second"})
	if status, _ := b.do("POST", "/api/v1/workspaces", map[string]string{"name": "Second"}); status != 409 {
		t.Error("duplicate workspace should conflict")
	}

	// API tokens: created from a session, used as bearer, cannot exceed the owner's role.
	tok := b.must(201, "POST", "/api/v1/tokens", map[string]any{"name": "ci", "role": "viewer", "expires_in_days": 30})
	secret := tok["secret"].(string)
	if !strings.HasPrefix(secret, "usie_") {
		t.Fatalf("secret: %q", secret)
	}
	bearer := &client{h: h, http: &http.Client{}, token: secret}
	me := bearer.must(200, "GET", "/api/v1/auth/me", nil)
	if p := me["principal"].(map[string]any); p["role"] != "viewer" || p["kind"] != "token" || me["csrf_token"] != nil {
		t.Fatalf("token principal: %v", me)
	}
	// Viewer token cannot write; bearer requests need no CSRF token.
	if status, _ := bearer.do("POST", "/api/v1/repositories", map[string]string{"name": "r", "source_kind": "upload"}); status != 403 {
		t.Error("viewer token must not create repositories")
	}
	if status, _ := bearer.do("POST", "/api/v1/tokens", map[string]string{"name": "x", "role": "viewer"}); status != 403 {
		t.Error("tokens must not mint tokens")
	}
	list := b.must(200, "GET", "/api/v1/tokens", nil)["tokens"].([]any)
	if len(list) != 1 || strings.Contains(fmt.Sprint(list), secret) {
		t.Fatalf("token list leaks or is wrong: %v", list)
	}
	tokenID := tok["token"].(map[string]any)["id"].(string)
	if status, _ := b.do("DELETE", "/api/v1/tokens/"+tokenID, nil); status != 204 {
		t.Fatal("revoke failed")
	}
	if status, _ := bearer.do("GET", "/api/v1/auth/me", nil); status != 401 {
		t.Error("revoked token still works")
	}
	if status, _ := (&client{h: h, http: &http.Client{}, token: "usie_forged"}).do("GET", "/api/v1/auth/me", nil); status != 401 {
		t.Error("forged token accepted")
	}

	// Logout invalidates the session server-side.
	b.must(204, "POST", "/api/v1/auth/logout", nil)
	if status, _ := b.do("GET", "/api/v1/auth/me", nil); status != 401 {
		t.Error("session survived logout")
	}

	// Audit trail records the security-relevant events.
	events := a.must(200, "GET", "/api/v1/audit-events", nil)["events"].([]any)
	var actions []string
	for _, e := range events {
		actions = append(actions, e.(map[string]any)["action"].(string))
	}
	joined := strings.Join(actions, ",")
	for _, want := range []string{"user.registered", "auth.login", "workspace.created", "token.created", "token.revoked", "auth.logout"} {
		if !strings.Contains(joined, want) {
			t.Errorf("audit trail lacks %s: %s", want, joined)
		}
	}
}

func TestTenantIsolationAndAnalysisFlow(t *testing.T) {
	h := newHarness(t, "open")
	alice, bob := h.client(), h.client()
	alice.register(uniq("alice"), "Alice Co")
	bob.register(uniq("bob"), "Bob Co")

	// Response headers.
	resp, err := http.Get(h.srv.URL + "/api/v1/system")
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	for _, hdr := range []string{"X-Request-Id", "Content-Security-Policy", "X-Content-Type-Options", "X-Frame-Options"} {
		if resp.Header.Get(hdr) == "" {
			t.Errorf("missing header %s", hdr)
		}
	}

	// Repository creation rules.
	for _, body := range []map[string]string{
		{"name": "r", "source_kind": "git", "source_location": "http://github.com/a/b"},
		{"name": "r", "source_kind": "git", "source_location": "https://user:pw@github.com/a/b"},
		{"name": "r", "source_kind": "git", "source_location": "file:///etc"},
		{"name": "r", "source_kind": "local", "source_location": "/etc"},
		{"name": "r", "source_kind": "svn"},
		{"name": "", "source_kind": "upload"},
	} {
		if status, out := alice.do("POST", "/api/v1/repositories", body); status != 400 {
			t.Errorf("accepted %v: %d %v", body, status, out)
		}
	}

	// A local repository under the allowed root, analyzed through the API.
	dir := filepath.Join(h.root, "svc")
	os.MkdirAll(filepath.Join(dir, "pkg"), 0o755)
	os.WriteFile(filepath.Join(dir, "pkg", "__init__.py"), nil, 0o644)
	os.WriteFile(filepath.Join(dir, "pkg", "core.py"), []byte("from .util import helper\n\ndef run():\n    return helper()\n"), 0o644)
	os.WriteFile(filepath.Join(dir, "pkg", "util.py"), []byte("def helper():\n    \"\"\"Help.\"\"\"\n    return 1\n"), 0o644)
	repo := alice.must(201, "POST", "/api/v1/repositories", map[string]string{"name": "svc", "source_kind": "local", "source_location": dir})
	repoID := repo["id"].(string)
	queued := alice.must(202, "POST", "/api/v1/repositories/"+repoID+"/analyze", nil)
	versionID, jobID := queued["version_id"].(string), queued["job_id"].(string)

	// Before analysis completes, graph endpoints say so explicitly.
	if status, out := alice.do("GET", "/api/v1/versions/"+versionID+"/graph/stats", nil); status != 409 || out["error"].(map[string]any)["code"] != "version_not_ready" {
		t.Fatalf("not-ready version: %d %v", status, out)
	}

	h.worker(context.Background())
	job := alice.must(200, "GET", "/api/v1/jobs/"+jobID, nil)
	if job["status"] != "succeeded" {
		t.Fatalf("job: %v", job)
	}
	v := alice.must(200, "GET", "/api/v1/versions/"+versionID, nil)
	if v["status"] != "ready" {
		t.Fatalf("version: %v", v)
	}

	// SSE stream of a finished job delivers one event and ends.
	req, _ := http.NewRequest("GET", h.srv.URL+"/api/v1/jobs/"+jobID+"/events", nil)
	resp, err = alice.http.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	stream, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.Header.Get("Content-Type") != "text/event-stream" || !strings.HasPrefix(string(stream), "event: job\ndata: {") || !strings.Contains(string(stream), `"status":"succeeded"`) {
		t.Errorf("event stream: %q", stream)
	}

	base := "/api/v1/versions/" + versionID
	tree := alice.must(200, "GET", base+"/tree?dir=pkg", nil)
	if files := tree["files"].([]any); len(files) != 3 {
		t.Errorf("tree: %v", tree)
	}
	file := alice.must(200, "GET", base+"/file?path=pkg/util.py", nil)
	if !strings.Contains(file["content"].(string), "def helper") || len(file["symbols"].([]any)) != 1 {
		t.Errorf("file: %v", file)
	}
	if status, _ := alice.do("GET", base+"/file?path=../../etc/passwd", nil); status != 400 {
		t.Error("path traversal in file endpoint")
	}
	search := alice.must(200, "GET", base+"/graph/search?q=help&kind=function", nil)
	if search["total"].(float64) != 1 {
		t.Errorf("search: %v", search)
	}
	hood := alice.must(200, "GET", base+"/graph/neighborhood?key="+urlEscape("sym:pkg/util.py#helper")+"&direction=in&kinds=CALLS", nil)
	nodes := hood["nodes"].([]any)
	if len(nodes) != 2 || nodes[1].(map[string]any)["key"] != "sym:pkg/core.py#run" {
		t.Errorf("callers of helper: %v", hood)
	}
	edge := hood["edges"].([]any)[0].(map[string]any)
	if edge["confidence"] != "confirmed" || edge["rule"] == "" || edge["evidence_path"] != "pkg/core.py" {
		t.Errorf("edge provenance: %v", edge)
	}
	if status, _ := alice.do("GET", base+"/graph/neighborhood?key=x&depth=99", nil); status != 400 {
		t.Error("depth bound not enforced")
	}
	files := alice.must(200, "GET", base+"/graph/files", nil)
	if len(files["edges"].([]any)) != 1 {
		t.Errorf("file graph: %v", files)
	}

	// ---- Bob, in another organization, sees none of it -------------------
	for _, path := range []string{
		"/api/v1/repositories/" + repoID, "/api/v1/repositories/" + repoID + "/versions", "/api/v1/versions/" + versionID,
		base + "/graph/stats", base + "/file?path=pkg/util.py", "/api/v1/jobs/" + jobID, "/api/v1/jobs/" + jobID + "/events",
	} {
		if status, _ := bob.do("GET", path, nil); status != 404 {
			t.Errorf("bob GET %s: %d", path, status)
		}
	}
	for _, path := range []string{"/api/v1/repositories/" + repoID + "/analyze", "/api/v1/jobs/" + jobID + "/cancel"} {
		if status, _ := bob.do("POST", path, nil); status != 404 {
			t.Errorf("bob POST %s: %d", path, status)
		}
	}
	if status, _ := bob.do("DELETE", "/api/v1/repositories/"+repoID, nil); status != 404 {
		t.Error("bob deleted alice's repository")
	}
	if repos := bob.must(200, "GET", "/api/v1/repositories", nil)["repositories"].([]any); len(repos) != 0 {
		t.Errorf("bob sees repositories: %v", repos)
	}
	if jobs := bob.must(200, "GET", "/api/v1/jobs", nil)["jobs"].([]any); len(jobs) != 0 {
		t.Errorf("bob sees jobs: %v", jobs)
	}
	// Malformed ids are 404s, not server errors.
	if status, _ := bob.do("GET", "/api/v1/repositories/not-a-uuid", nil); status != 404 {
		t.Error("malformed id")
	}

	// ---- uploads ---------------------------------------------------------
	up := alice.must(201, "POST", "/api/v1/repositories", map[string]string{"name": "zip", "source_kind": "upload"})
	upID := up["id"].(string)
	var archive bytes.Buffer
	zw := zip.NewWriter(&archive)
	f, _ := zw.Create("p/main.ts")
	f.Write([]byte("export function main() { return 1; }\n"))
	zw.Close()
	if status, _ := alice.do("POST", "/api/v1/repositories/"+upID+"/upload", []byte("not a zip file")); status != 400 {
		t.Error("non-zip upload accepted")
	}
	if status, _ := alice.do("POST", "/api/v1/repositories/"+upID+"/upload", bytes.Repeat([]byte("PK"), 1<<20)); status != 413 {
		t.Error("oversize upload accepted")
	}
	queued = alice.must(202, "POST", "/api/v1/repositories/"+upID+"/upload", archive.Bytes())
	h.worker(context.Background())
	v = alice.must(200, "GET", "/api/v1/versions/"+queued["version_id"].(string), nil)
	if v["status"] != "ready" {
		t.Fatalf("upload version: %v", v)
	}
	if status, _ := alice.do("POST", "/api/v1/repositories/"+repoID+"/upload", archive.Bytes()); status != 400 {
		t.Error("upload to a local repository accepted")
	}

	// Deleting the repository removes its data.
	alice.must(204, "DELETE", "/api/v1/repositories/"+repoID, nil)
	if status, _ := alice.do("GET", "/api/v1/versions/"+versionID, nil); status != 404 {
		t.Error("version survived repository deletion")
	}

	// Metrics and health.
	for _, path := range []string{"/healthz", "/readyz", "/metrics"} {
		resp, err := http.Get(h.srv.URL + path)
		if err != nil || resp.StatusCode != 200 {
			t.Errorf("%s: %v %v", path, err, resp.StatusCode)
		}
		body, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		if path == "/metrics" && !strings.Contains(string(body), "usie_http_requests_total") {
			t.Error("metrics missing request counter")
		}
	}
	if status, _ := alice.do("GET", "/api/v1/nope", nil); status != 404 {
		t.Error("unknown endpoint")
	}
}

func TestRoles(t *testing.T) {
	h := newHarness(t, "open")
	owner := h.client()
	p := owner.register(uniq("owner"), "Roles Co")
	orgID := p["organization_id"].(string)

	// Add a member and a viewer directly (membership management is not in the API yet).
	ctx := context.Background()
	mk := func(role string) *client {
		c := h.client()
		email := uniq(role)
		c.register(email, "tmp-"+role)
		var userID string
		h.pool.QueryRow(ctx, `SELECT id FROM users WHERE email = $1`, email).Scan(&userID)
		h.pool.Exec(ctx, `DELETE FROM memberships WHERE user_id = $1`, userID)
		h.pool.Exec(ctx, `INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, $3)`, orgID, userID, role)
		out := c.must(200, "POST", "/api/v1/auth/login", map[string]string{"email": email, "password": "correct horse battery"})
		c.csrf = out["csrf_token"].(string)
		return c
	}
	member, viewer := mk("member"), mk("viewer")

	repo := member.must(201, "POST", "/api/v1/repositories", map[string]string{"name": "m", "source_kind": "upload"})
	if status, _ := viewer.do("POST", "/api/v1/repositories", map[string]string{"name": "v", "source_kind": "upload"}); status != 403 {
		t.Error("viewer created a repository")
	}
	if status, _ := member.do("POST", "/api/v1/repositories", map[string]string{"name": "l", "source_kind": "local", "source_location": h.root}); status != 403 {
		t.Error("member registered a local path")
	}
	if status, _ := member.do("DELETE", "/api/v1/repositories/"+repo["id"].(string), nil); status != 403 {
		t.Error("member deleted a repository")
	}
	if status, _ := member.do("GET", "/api/v1/audit-events", nil); status != 403 {
		t.Error("member read the audit trail")
	}
	if status, _ := member.do("POST", "/api/v1/tokens", map[string]string{"name": "x", "role": "admin"}); status != 403 {
		t.Error("member minted an admin token")
	}
	if repos := viewer.must(200, "GET", "/api/v1/repositories", nil)["repositories"].([]any); len(repos) != 1 {
		t.Error("viewer cannot read")
	}
	owner.must(204, "DELETE", "/api/v1/repositories/"+repo["id"].(string), nil)
	denied := owner.must(200, "GET", "/api/v1/audit-events", nil)["events"].([]any)
	found := false
	for _, e := range denied {
		if e.(map[string]any)["action"] == "authorization.denied" {
			found = true
		}
	}
	if !found {
		t.Error("denied actions are not audited")
	}
}

func urlEscape(s string) string {
	r := strings.NewReplacer("#", "%23", ":", "%3A", "/", "%2F")
	return r.Replace(s)
}
