package transform

import (
	"strings"
	"testing"
)

func TestApply(t *testing.T) {
	editable := map[string]string{
		"src/a.py": "def f():\n    return 1\n\n\ndef g():\n    return 1\n",
		"src/b.py": "x = 1\n",
	}
	exists := func(p string) bool { return p == "src/a.py" || p == "src/b.py" || p == "README.md" }
	lim := DefaultLimits()

	applied, err := Apply([]Edit{
		{Path: "src/a.py", Action: "modify", Search: "def f():\n    return 1", Replace: "def f():\n    return 2"},
		{Path: "src/new.py", Action: "create", Replace: "y = 2\n"},
		{Path: "src/b.py", Action: "delete"},
	}, editable, exists, lim)
	if err != nil {
		t.Fatal(err)
	}
	if len(applied.Files) != 3 || applied.Files[0].Content != "def f():\n    return 2\n\n\ndef g():\n    return 1\n" ||
		applied.Files[1].Status != "created" || applied.Files[2].Status != "deleted" || applied.Files[2].Content != "" {
		t.Errorf("files: %+v", applied.Files)
	}
	for _, want := range []string{"--- a/src/a.py\n+++ b/src/a.py\n", "-    return 2\n"[1:], "--- /dev/null\n+++ b/src/new.py\n", "--- a/src/b.py\n+++ /dev/null\n"} {
		if !strings.Contains(applied.Diff, want) {
			t.Errorf("diff lacks %q:\n%s", want, applied.Diff)
		}
	}
	if applied.LinesAdded != 2 || applied.LinesRemoved != 2 {
		t.Errorf("counts: +%d -%d", applied.LinesAdded, applied.LinesRemoved)
	}

	reject := []struct {
		name  string
		edits []Edit
		want  string
	}{
		{"ambiguous search", []Edit{{Path: "src/a.py", Action: "modify", Search: "return 1", Replace: "return 3"}}, "occurs 2 times"},
		{"missing search", []Edit{{Path: "src/a.py", Action: "modify", Search: "nope", Replace: "x"}}, "does not occur"},
		{"file not shown", []Edit{{Path: "README.md", Action: "modify", Search: "a", Replace: "b"}}, "not provided in full"},
		{"path escape", []Edit{{Path: "../etc/passwd", Action: "create", Replace: "x"}}, "not a valid repository path"},
		{"absolute path", []Edit{{Path: "/etc/passwd", Action: "create", Replace: "x"}}, "not a valid repository path"},
		{"git internals", []Edit{{Path: ".git/hooks/pre-commit", Action: "create", Replace: "x"}}, "not a valid repository path"},
		{"create existing", []Edit{{Path: "README.md", Action: "create", Replace: "x"}}, "already exists"},
		{"delete unseen", []Edit{{Path: "README.md", Action: "delete"}}, "only files provided in full"},
		{"no-op", []Edit{{Path: "src/b.py", Action: "modify", Search: "x = 1", Replace: "x = 1"}}, "unchanged"},
		{"unknown action", []Edit{{Path: "src/b.py", Action: "chmod"}}, "unknown action"},
		{"empty", nil, "no edits"},
	}
	for _, c := range reject {
		if _, err := Apply(c.edits, editable, exists, lim); err == nil || !strings.Contains(err.Error(), c.want) {
			t.Errorf("%s: %v", c.name, err)
		}
	}
	lim.MaxEdits = 1
	if _, err := Apply([]Edit{{Path: "a", Action: "create"}, {Path: "b", Action: "create"}}, editable, exists, lim); err == nil {
		t.Error("edit limit not enforced")
	}
}

func TestParseProposal(t *testing.T) {
	p, err := ParseProposal([]byte(`{"feasible":true,"summary":"s","rationale":"r [C1]","edits":[],"risks":["a",""],"test_plan":[],"limitations":[]}`))
	if err != nil || !p.Feasible || len(p.Risks) != 1 {
		t.Errorf("%+v %v", p, err)
	}
	if _, err := ParseProposal([]byte(`{"feasible":true,"extra":1}`)); err == nil {
		t.Error("unknown fields accepted")
	}
}
