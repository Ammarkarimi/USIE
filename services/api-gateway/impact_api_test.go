package api

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestImpactAnalysis(t *testing.T) {
	h := newHarness(t, "open")
	alice := h.client()
	alice.register(uniq("impact"), "Impact Co")
	dir := filepath.Join(h.root, "shop")
	os.MkdirAll(filepath.Join(dir, "shop"), 0o755)
	os.MkdirAll(filepath.Join(dir, "tests"), 0o755)
	write := func(name, content string) {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	write("shop/__init__.py", "")
	write("shop/pricing.py", "def total(items):\n    return sum(items)\n\n\ndef discount(x):\n    return x * 0.9\n")
	write("shop/cart.py", "from .pricing import total\n\n\nclass Cart:\n    def checkout(self, items):\n        return total(items)\n")
	write("tests/test_cart.py", "from shop.cart import Cart\n\n\ndef test_checkout():\n    assert Cart().checkout([1]) == 1\n")
	repo := alice.must(201, "POST", "/api/v1/repositories", map[string]string{"name": "shop", "source_kind": "local", "source_location": dir})
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
	v1 := analyze()
	impactOf := func(version string, body map[string]any) map[string]any {
		return alice.must(200, "POST", "/api/v1/versions/"+version+"/impact", body)["impact"].(map[string]any)
	}
	keys := func(list any) string {
		var out []string
		for _, it := range list.([]any) {
			m := it.(map[string]any)
			n := m["node"].(map[string]any)
			out = append(out, n["key"].(string))
		}
		return strings.Join(out, " ")
	}

	// A diff that edits `total`: Cart.checkout calls it, the test reaches it.
	diff := "--- a/shop/pricing.py\n+++ b/shop/pricing.py\n@@ -1,2 +1,2 @@\n def total(items):\n-    return sum(items)\n+    return sum(items) + 0\n"
	res := impactOf(v1, map[string]any{"diff": diff})
	if got := keys(res["changed"]); got != "sym:shop/pricing.py#total" {
		t.Errorf("changed: %s", got)
	}
	if got := keys(res["affected"]); got != "sym:shop/cart.py#Cart.checkout" {
		t.Errorf("affected: %s", got)
	}
	// The test calls `Cart().checkout(...)`: a method of a computed receiver
	// in Python, matched only by name (inferred), which is not followed by
	// default. The change is reported as untested rather than guessed.
	if len(res["tests"].([]any)) != 0 || len(res["untested_changes"].([]any)) != 1 {
		t.Errorf("default tests: %s untested %v", keys(res["tests"]), res["untested_changes"])
	}
	res = impactOf(v1, map[string]any{"diff": diff, "min_confidence": "inferred"})
	if keys(res["tests"]) != "sym:tests/test_cart.py#test_checkout" || len(res["untested_changes"].([]any)) != 0 {
		t.Errorf("tests with inferred: %s untested %v", keys(res["tests"]), res["untested_changes"])
	}
	if tests := res["tests"].([]any); len(tests) == 1 && tests[0].(map[string]any)["confidence"] != "inferred" {
		t.Errorf("test path confidence: %v", tests[0])
	}
	risk := res["risk"].(map[string]any)
	if len(risk["factors"].([]any)) != 6 || risk["note"] == "" {
		t.Errorf("risk: %v", risk)
	}

	// `discount` has no dependents and no test.
	res = impactOf(v1, map[string]any{"symbols": []string{"sym:shop/pricing.py#discount", "sym:nope"}})
	if len(res["affected"].([]any)) != 0 || len(res["untested_changes"].([]any)) != 1 || !strings.Contains(res["unmatched"].([]any)[0].(string), "sym:nope") {
		t.Errorf("discount: %v", res)
	}

	// Comparing two analyzed versions.
	write("shop/pricing.py", "def total(items):\n    return sum(items)\n\n\ndef discount(x):\n    return x * 0.8\n")
	write("shop/extra.py", "from .pricing import discount\n\n\ndef sale(x):\n    return discount(x)\n")
	v2 := analyze()
	out := alice.must(200, "POST", "/api/v1/versions/"+v2+"/impact", map[string]any{"base_version_id": v1})
	res = out["impact"].(map[string]any)
	changed := keys(res["changed"])
	if !strings.Contains(changed, "sym:shop/pricing.py#discount") || strings.Contains(changed, "#total") || !strings.Contains(changed, "sym:shop/extra.py#sale") {
		t.Errorf("compare changed: %s", changed)
	}
	if out["base_version_id"] != v1 {
		t.Errorf("base: %v", out["base_version_id"])
	}

	// Validation and isolation.
	for _, body := range []map[string]any{{}, {"diff": "not a diff"}, {"depth": 9, "files": []string{"x"}}, {"files": []string{"../etc"}}, {"base_version_id": v2}, {"min_confidence": "maybe", "files": []string{"a"}}} {
		if status, out := alice.do("POST", "/api/v1/versions/"+v2+"/impact", body); status != 400 {
			t.Errorf("accepted %v: %d %v", body, status, out)
		}
	}
	bob := h.client()
	bob.register(uniq("bob"), "Bob Co")
	if status, _ := bob.do("POST", "/api/v1/versions/"+v1+"/impact", map[string]any{"files": []string{"shop/cart.py"}}); status != 404 {
		t.Errorf("cross-tenant impact: %d", status)
	}
}
