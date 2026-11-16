package api

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	analysis "github.com/ammarkarimi/usie/services/semantic-analysis"
)

func TestCodeQuality(t *testing.T) {
	h := newHarness(t, "open")
	alice := h.client()
	alice.register(uniq("quality"), "Quality Co")
	dir := filepath.Join(h.root, "svc")
	for _, d := range []string{"app", "tests"} {
		if err := os.MkdirAll(filepath.Join(dir, d), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	write := func(name, content string) {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	token := "ghp_" + strings.Repeat("Zq7x", 9)
	var shared strings.Builder
	for i := 0; i < 12; i++ {
		shared.WriteString("    total = total + weight(item, " + string(rune('a'+i)) + ")\n")
	}
	write("app/db.py", "def find(cursor, uid):\n    cursor.execute(\"SELECT * FROM users WHERE id = \" + uid)\n\n\n"+
		"def run(expr):\n    return eval(expr)\n\n\nDB_PASSWORD = \"k8#Qz!v2Lm\"\n")
	write("app/a.py", "def a(item):\n    total = 0\n"+shared.String()+"    return total\n")
	write("app/b.py", "def b(item):\n    total = 1\n"+shared.String()+"    return total\n")
	write("app/settings.yaml", "github_token: "+token+"\n")
	write("tests/test_db.py", "import hashlib\n\n\ndef test_x(a, b, c, d, e, f, g, h):\n    # TODO tidy\n    return hashlib.md5(b'x')\n")

	repo := alice.must(201, "POST", "/api/v1/repositories", map[string]string{"name": "svc", "source_kind": "local", "source_location": dir})
	repoID := repo["id"].(string)
	analyze := func() string {
		queued := alice.must(202, "POST", "/api/v1/repositories/"+repoID+"/analyze", nil)
		h.worker(context.Background())
		v := alice.must(200, "GET", "/api/v1/versions/"+queued["version_id"].(string), nil)
		if v["status"] != "ready" {
			t.Fatalf("analysis: %v", v)
		}
		return v["id"].(string)
	}
	findings := func(version, query string) []map[string]any {
		res := alice.must(200, "GET", "/api/v1/versions/"+version+"/findings?limit=500&"+query, nil)
		var out []map[string]any
		for _, f := range res["findings"].([]any) {
			out = append(out, f.(map[string]any))
		}
		return out
	}
	ruleList := func(fs []map[string]any) string {
		var r []string
		for _, f := range fs {
			r = append(r, f["path"].(string)+":"+f["rule"].(string))
		}
		sort.Strings(r)
		return strings.Join(r, " ")
	}

	v1 := analyze()
	all := findings(v1, "status=all")
	want := "app/a.py:duplicated-block app/b.py:duplicated-block app/db.py:dynamic-code-execution app/db.py:hardcoded-password " +
		"app/db.py:sql-injection app/settings.yaml:hardcoded-secret tests/test_db.py:weak-hash"
	if got := ruleList(all); got != want {
		t.Fatalf("findings:\n got  %s\n want %s", got, want)
	}
	raw, _ := json.Marshal(all)
	if strings.Contains(string(raw), token[4:]) || strings.Contains(string(raw), "k8#Qz") {
		t.Fatal("a secret value is exposed by the API")
	}
	for _, f := range all {
		if f["is_new"] != false {
			t.Fatalf("first analysis has no baseline: %v", f)
		}
		if f["path"] == "tests/test_db.py" && f["in_test"] != true {
			t.Fatalf("in_test: %v", f)
		}
	}

	sum := alice.must(200, "GET", "/api/v1/versions/"+v1+"/quality", nil)
	gate := sum["gate"].(map[string]any)
	if sum["available"] != true || sum["baseline_version_id"] != nil || gate["status"] != "failed" || gate["default"] != true {
		t.Fatalf("summary: %v", sum)
	}
	for _, c := range gate["conditions"].([]any) {
		c := c.(map[string]any)
		wantStatus := "no_baseline"
		if c["metric"] == "blocker_findings" {
			wantStatus = "failed" // the committed token
		}
		if c["status"] != wantStatus {
			t.Errorf("condition %v", c)
		}
	}
	grades := sum["grades"].(map[string]any)
	if grades["security"] != "E" || grades["reliability"] != "A" || grades["security_review"] != "E" {
		t.Errorf("grades: %v", grades)
	}
	metrics := sum["metrics"].(map[string]any)
	if metrics["duplicated_lines"].(float64) != 26 || metrics["functions"].(float64) != 4 {
		t.Errorf("metrics: %v", metrics)
	}

	byRule := map[string]map[string]any{}
	for _, f := range all {
		byRule[f["rule"].(string)] = f
	}
	review := func(rule, status, comment string) (int, map[string]any) {
		return alice.do("PUT", "/api/v1/repositories/"+repoID+"/findings/"+byRule[rule]["fingerprint"].(string)+"/review",
			map[string]string{"status": status, "comment": comment})
	}
	if code, _ := review("hardcoded-password", "false_positive", ""); code != 400 {
		t.Errorf("false positive without a reason: %d", code)
	}
	if code, _ := review("dynamic-code-execution", "false_positive", "x"); code != 400 {
		t.Errorf("hotspots are reviewed as safe or accepted: %d", code)
	}
	if code, _ := review("hardcoded-password", "false_positive", "test fixture value"); code != 204 {
		t.Fatalf("review: %d", code)
	}
	if code, _ := review("dynamic-code-execution", "safe", "only evaluates arithmetic from the config file"); code != 204 {
		t.Fatalf("review hotspot: %d", code)
	}
	open := findings(v1, "")
	if strings.Contains(ruleList(open), "hardcoded-password") || strings.Contains(ruleList(open), "dynamic-code-execution") {
		t.Fatalf("reviewed findings are listed as open: %s", ruleList(open))
	}
	reviewed := findings(v1, "status=reviewed")
	if len(reviewed) != 2 || reviewed[0]["review"].(map[string]any)["reviewed_by"] == "" {
		t.Fatalf("reviewed: %v", reviewed)
	}
	sum = alice.must(200, "GET", "/api/v1/versions/"+v1+"/quality", nil)
	// Two hotspots (eval in app code, md5 in a test), one reviewed.
	if sum["grades"].(map[string]any)["security_review"] != "C" || sum["hotspots_reviewed_percent"].(float64) != 50 {
		t.Errorf("after review: %v %v", sum["grades"], sum["hotspots_reviewed_percent"])
	}

	// Another organization sees nothing.
	mallory := h.client()
	mallory.register(uniq("mallory"), "Other Co")
	if code, _ := mallory.do("GET", "/api/v1/versions/"+v1+"/findings", nil); code != 404 {
		t.Errorf("cross-tenant findings: %d", code)
	}
	if code, _ := mallory.do("PUT", "/api/v1/repositories/"+repoID+"/findings/"+byRule["sql-injection"]["fingerprint"].(string)+"/review",
		map[string]string{"status": "accepted"}); code != 404 {
		t.Errorf("cross-tenant review: %d", code)
	}

	// Second version: the token is removed and a new injection is added.
	// Moved code keeps its fingerprint, so only the new one is new.
	write("app/settings.yaml", "github_token: ${GITHUB_TOKEN}\n")
	write("app/db.py", "\n\ndef find(cursor, uid):\n    cursor.execute(\"SELECT * FROM users WHERE id = \" + uid)\n\n\n"+
		"def run(expr):\n    return eval(expr)\n\n\nDB_PASSWORD = \"k8#Qz!v2Lm\"\n\n\n"+
		"def remove(cursor, uid):\n    cursor.execute(f\"DELETE FROM users WHERE id = {uid}\")\n")
	v2 := analyze()
	fresh := findings(v2, "new=true&status=all")
	if len(fresh) != 1 || fresh[0]["rule"] != "sql-injection" || fresh[0]["start_line"].(float64) != 15 {
		t.Fatalf("new findings: %v", fresh)
	}
	if got := ruleList(findings(v2, "status=reviewed")); got != "app/db.py:dynamic-code-execution app/db.py:hardcoded-password" {
		t.Errorf("reviews carry over: %s", got)
	}
	sum = alice.must(200, "GET", "/api/v1/versions/"+v2+"/quality", nil)
	gate = sum["gate"].(map[string]any)
	if sum["baseline_version_id"] != v1 || gate["status"] != "failed" {
		t.Fatalf("v2 summary: %v", sum)
	}
	failed := []string{}
	for _, c := range gate["conditions"].([]any) {
		if c := c.(map[string]any); c["status"] == "failed" {
			failed = append(failed, c["metric"].(string))
		}
	}
	if strings.Join(failed, ",") != "new_vulnerabilities" {
		t.Errorf("failed conditions: %v", failed)
	}

	// A custom gate.
	if code, _ := alice.do("PUT", "/api/v1/repositories/"+repoID+"/quality-gate", map[string]any{"conditions": []map[string]any{{"metric": "nope", "threshold": 1}}}); code != 400 {
		t.Errorf("unknown metric: %d", code)
	}
	g := alice.must(200, "PUT", "/api/v1/repositories/"+repoID+"/quality-gate", map[string]any{"conditions": []map[string]any{{"metric": "critical_findings", "threshold": 5}}})
	if g["default"] != false || len(g["conditions"].([]any)) != 1 {
		t.Fatalf("gate: %v", g)
	}
	if st := alice.must(200, "GET", "/api/v1/versions/"+v2+"/quality", nil)["gate"].(map[string]any)["status"]; st != "passed" {
		t.Errorf("custom gate: %v", st)
	}
	if g := alice.must(200, "PUT", "/api/v1/repositories/"+repoID+"/quality-gate", map[string]any{"default": true}); g["default"] != true {
		t.Errorf("restore default: %v", g)
	}

	// Reopening restores the finding.
	alice.must(204, "DELETE", "/api/v1/repositories/"+repoID+"/findings/"+byRule["hardcoded-password"]["fingerprint"].(string)+"/review", nil)
	if !strings.Contains(ruleList(findings(v2, "")), "hardcoded-password") {
		t.Error("reopened finding is not open")
	}

	// Without the engine binary the API describes only its own text rules.
	rules := alice.must(200, "GET", "/api/v1/quality/rules", nil)
	if n := len(rules["rules"].([]any)); n != 3 || rules["engine_rules_available"] != false {
		t.Errorf("catalog without engine: %d rules", n)
	}
	info, err := analysis.Engine{Bin: os.Getenv("USIE_ENGINE_BIN")}.Info(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	h.api.Engine = &info
	rules = alice.must(200, "GET", "/api/v1/quality/rules", nil)
	if n := len(rules["rules"].([]any)); n < 20 || rules["engine_rules_available"] != true {
		t.Errorf("catalog: %d rules", n)
	}
}
