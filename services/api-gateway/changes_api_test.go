package api

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/ammarkarimi/usie/internal/store"
	ai "github.com/ammarkarimi/usie/services/ai-reasoning"
	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
	transform "github.com/ammarkarimi/usie/services/transformation"
	verify "github.com/ammarkarimi/usie/services/verification"
)

// proposer returns queued proposals in order.
type proposer struct {
	mu      sync.Mutex
	outputs []string
	prompts []ai.Prompt
}

func (p *proposer) Name() string  { return "scripted" }
func (p *proposer) Model() string { return "scripted-model" }
func (p *proposer) Generate(_ context.Context, pr ai.Prompt) (*ai.Generation, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.prompts = append(p.prompts, pr)
	out := p.outputs[0]
	p.outputs = p.outputs[1:]
	return &ai.Generation{JSON: json.RawMessage(out), Model: "scripted-model", Usage: ai.Usage{InputTokens: 2000, OutputTokens: 300}}, nil
}

func proposalJSON(edits []map[string]string) string {
	b, _ := json.Marshal(map[string]any{
		"feasible": true, "summary": "Fix add.", "rationale": "add subtracts [C1].", "edits": edits,
		"risks": []string{"callers relying on the old result"}, "test_plan": []string{"run test_calc"}, "limitations": []string{},
	})
	return string(b)
}

func (c *client) waitChange(id string) map[string]any {
	c.h.t.Helper()
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		out := c.must(200, "GET", "/api/v1/changes/"+id, nil)
		ch := out["change"].(map[string]any)
		if ch["status"] != "pending" {
			return out
		}
		time.Sleep(50 * time.Millisecond)
	}
	c.h.t.Fatal("proposal still pending")
	return nil
}

func TestAIChangesAndVerification(t *testing.T) {
	h := newHarness(t, "open")
	alice := h.client()
	alice.register(uniq("changes"), "Changes Co")
	dir := filepath.Join(h.root, "calc")
	os.MkdirAll(dir, 0o755)
	os.WriteFile(filepath.Join(dir, "calc.py"), []byte("def add(a, b):\n    return a - b\n"), 0o644)
	os.WriteFile(filepath.Join(dir, "test_calc.py"), []byte("import unittest\nfrom calc import add\n\n\nclass CalcTest(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\n\nif __name__ == '__main__':\n    unittest.main()\n"), 0o644)
	repo := alice.must(201, "POST", "/api/v1/repositories", map[string]string{"name": "calc", "source_kind": "local", "source_location": dir})
	queued := alice.must(202, "POST", "/api/v1/repositories/"+repo["id"].(string)+"/analyze", nil)
	h.worker(context.Background())
	versionID := queued["version_id"].(string)

	// Without an AI provider the feature says so.
	if status, out := alice.do("POST", "/api/v1/changes", map[string]any{"version_id": versionID, "request": "fix add"}); status != 503 {
		t.Fatalf("no provider: %d %v", status, out)
	}
	prov := &proposer{outputs: []string{
		proposalJSON([]map[string]string{{"path": "calc.py", "action": "modify", "search": "    return a - b", "replace": "    return a + b", "reason": "add must add"}}),
		proposalJSON([]map[string]string{{"path": "calc.py", "action": "modify", "search": "return a * b", "replace": "x", "reason": "wrong"}}),
	}}
	svc := &transform.Service{Pool: h.pool, Provider: prov, Graph: h.api.Graph, Sources: AISources(h.api.Store, h.api.Blobs, h.api.Cfg.MaxFileBytes),
		Log: slog.New(slog.NewTextHandler(io.Discard, nil)), Cfg: transform.Config{DailyTokenBudget: 1_000_000, UserRequestsPerHour: 10}}
	h.api.Changes = svc
	t.Cleanup(svc.Wait)

	created := alice.must(202, "POST", "/api/v1/changes", map[string]any{"version_id": versionID, "request": "add() subtracts; make it add", "focus_key": "sym:calc.py#add"})
	if created["status"] != "pending" {
		t.Fatalf("created: %v", created)
	}
	out := alice.waitChange(created["id"].(string))
	ch := out["change"].(map[string]any)
	if ch["status"] != "proposed" || ch["files_changed"].(float64) != 1 || ch["lines_added"].(float64) != 1 || ch["lines_removed"].(float64) != 1 {
		t.Fatalf("proposal: %v", ch)
	}
	if !strings.Contains(ch["diff"].(string), "-    return a - b\n+    return a + b\n") {
		t.Errorf("diff: %s", ch["diff"])
	}
	if imp := ch["impact"].(map[string]any); len(imp["changed"].([]any)) != 1 {
		t.Errorf("impact of the proposal: %v", imp)
	}
	// The model saw the editable file inside the evidence boundary; the
	// stored context does not keep file contents.
	user := prov.prompts[0].User
	if !strings.Contains(user, `kind="editable_file" path="calc.py"`) || !strings.Contains(user, "Change request:\nadd() subtracts") {
		t.Errorf("prompt: %s", user)
	}
	for _, item := range ch["context"].([]any) {
		if item.(map[string]any)["content"] != nil && item.(map[string]any)["content"] != "" {
			t.Errorf("stored context keeps content: %v", item)
		}
	}
	changeID := ch["id"].(string)

	// The patch is downloadable as-is.
	req, _ := http.NewRequest("GET", h.srv.URL+"/api/v1/changes/"+changeID+"/patch", nil)
	resp, err := alice.http.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	patch, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != 200 || resp.Header.Get("Content-Type") != "text/x-diff; charset=utf-8" || string(patch) != ch["diff"] {
		t.Errorf("patch: %d %q %q", resp.StatusCode, resp.Header.Get("Content-Type"), patch)
	}

	// An edit whose search text is not in the file fails the whole proposal.
	bad := alice.must(202, "POST", "/api/v1/changes", map[string]any{"version_id": versionID, "request": "make add multiply"})
	failed := alice.waitChange(bad["id"].(string))["change"].(map[string]any)
	if failed["status"] != "failed" || !strings.Contains(failed["error"].(string), "does not occur") || failed["diff"] != "" {
		t.Errorf("bad proposal: %v", failed)
	}

	// Verification needs a worker with a sandbox.
	if status, out := alice.do("POST", "/api/v1/changes/"+changeID+"/verifications", map[string]any{"profile": "unittest"}); status != 503 {
		t.Errorf("no sandbox: %d %v", status, out)
	}
	bwrap, e1 := exec.LookPath("bwrap")
	prlimit, e2 := exec.LookPath("prlimit")
	if e1 != nil || e2 != nil {
		t.Skip("bubblewrap not installed")
	}
	sb := &verify.Bubblewrap{Bwrap: bwrap, Prlimit: prlimit, Limits: verify.DefaultLimits()}
	if err := sb.Check(context.Background()); err != nil {
		t.Skipf("sandbox unavailable: %v", err)
	}
	if _, ok := sb.LookPath("python3"); !ok {
		t.Skip("python3 not visible in the sandbox")
	}
	ctx := context.Background()
	if _, err := h.api.Queue.RegisterWorker(ctx, "test-host", 1, "test", map[string]any{"verification": verify.Describe(sb)}); err != nil {
		t.Fatal(err)
	}
	profiles := alice.must(200, "GET", "/api/v1/changes/"+changeID+"/verification-profiles", nil)
	var unittestProfile map[string]any
	for _, p := range profiles["profiles"].([]any) {
		if p.(map[string]any)["id"] == "unittest" {
			unittestProfile = p.(map[string]any)
		}
	}
	if unittestProfile == nil || unittestProfile["available"] != true || unittestProfile["detected"] != true {
		t.Fatalf("profiles: %v", profiles)
	}
	if status, _ := alice.do("POST", "/api/v1/changes/"+changeID+"/verifications", map[string]any{"profile": "nope"}); status != 400 {
		t.Errorf("unknown profile: %d", status)
	}
	run := alice.must(202, "POST", "/api/v1/changes/"+changeID+"/verifications", map[string]any{"profile": "unittest"})

	// A worker runs the job in the sandbox.
	runner := &verify.Runner{Pool: h.pool, Store: store.New(h.pool), Blobs: h.api.Blobs, Sandbox: sb, Log: slog.New(slog.NewTextHandler(io.Discard, nil)), MaxFileBytes: 1 << 20}
	wctx, cancel := context.WithTimeout(ctx, 60*time.Second)
	w := &orchestrator.Worker{Queue: h.api.Queue, Handler: runner.Handle, Lease: 30 * time.Second, PollInterval: 50 * time.Millisecond,
		Log: slog.New(slog.NewTextHandler(io.Discard, nil)), OnFinish: func(*orchestrator.Job, string, time.Duration) { cancel() }}
	if err := w.Run(wctx); err != nil {
		t.Fatal(err)
	}
	cancel()
	done := alice.must(200, "GET", "/api/v1/verifications/"+run["id"].(string), nil)
	if done["status"] != "passed" {
		t.Fatalf("verification: %v", done)
	}
	result := done["result"].(map[string]any)
	baseline, change := result["baseline"].(map[string]any), result["change"].(map[string]any)
	if baseline["passed"] != false || change["passed"] != true || result["network"] != false || result["sandbox"] != "bubblewrap" {
		t.Errorf("result: %v", result)
	}
	steps := change["steps"].([]any)
	tests := steps[len(steps)-1].(map[string]any)["tests"].(map[string]any)
	if tests["passed"].(float64) != 1 || tests["failed"].(float64) != 0 {
		t.Errorf("tests with the change: %v", tests)
	}
	if fixed := result["fixed"].([]any); len(fixed) != 1 || !strings.Contains(fixed[0].(string), "test_add") {
		t.Errorf("fixed: %v", result["fixed"])
	}
	listed := alice.must(200, "GET", "/api/v1/changes/"+changeID, nil)["verifications"].([]any)
	if len(listed) != 1 {
		t.Errorf("runs listed: %v", listed)
	}

	// Roles and tenants.
	bob := h.client()
	bob.register(uniq("bob"), "Bob Co")
	for _, path := range []string{"/api/v1/changes/" + changeID, "/api/v1/changes/" + changeID + "/patch", "/api/v1/verifications/" + run["id"].(string)} {
		if status, _ := bob.do("GET", path, nil); status != 404 {
			t.Errorf("cross-tenant %s: %d", path, status)
		}
	}
	if list := bob.must(200, "GET", "/api/v1/changes", nil)["changes"].([]any); len(list) != 0 {
		t.Errorf("cross-tenant list: %v", list)
	}
	discarded := alice.must(200, "POST", "/api/v1/changes/"+changeID+"/discard", nil)
	if discarded["status"] != "discarded" {
		t.Errorf("discard: %v", discarded)
	}
	if status, _ := alice.do("POST", "/api/v1/changes/"+changeID+"/verifications", map[string]any{"profile": "unittest"}); status != 409 {
		t.Errorf("verification of a discarded change: %d", status)
	}
	// Proposal requests are audited by size only.
	var detail string
	h.pool.QueryRow(ctx, `SELECT detail::text FROM audit_events WHERE action = 'change.propose' ORDER BY id LIMIT 1`).Scan(&detail)
	if strings.Contains(detail, "subtracts") || !strings.Contains(detail, "request_chars") {
		t.Errorf("audit detail: %s", detail)
	}
}
