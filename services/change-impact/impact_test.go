package impact

import (
	"context"
	"reflect"
	"strings"
	"testing"

	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
)

func TestParseUnifiedDiff(t *testing.T) {
	diff := `diff --git a/pkg/a.py b/pkg/a.py
index 1..2 100644
--- a/pkg/a.py
+++ b/pkg/a.py
@@ -3,4 +3,5 @@ def f():
 x = 1
-y = 2
+y = 3
+z = 4
 w = 5
 v = 6
@@ -20,0 +22,2 @@
+extra()
+extra()
--- /dev/null
+++ b/new.py
@@ -0,0 +1 @@
+print(1)
--- a/gone.py
+++ /dev/null
@@ -1,2 +0,0 @@
-a
-b
`
	got, err := ParseUnifiedDiff(diff)
	if err != nil {
		t.Fatal(err)
	}
	want := []FileChange{
		{Path: "pkg/a.py", Status: "modified", Ranges: []LineRange{{4, 4}, {21, 21}}},
		{Path: "new.py", Status: "added", Ranges: []LineRange{{1, 1}}},
		{Path: "gone.py", Status: "deleted", Ranges: []LineRange{{1, 2}}},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("got %+v\nwant %+v", got, want)
	}
	if _, err := ParseUnifiedDiff("not a diff"); err == nil {
		t.Error("text without headers must be rejected")
	}
	if _, err := ParseUnifiedDiff("--- a/x\n+++ b/x\n@@ nonsense @@\n"); err == nil {
		t.Error("malformed hunk header must be rejected")
	}
}

func TestChangedLines(t *testing.T) {
	base := "a\nb\nc\nd\ne\n"
	head := "a\nB\nc\nd\nnew\ne\n"
	got, ok := ChangedLines(base, head)
	if !ok || !reflect.DeepEqual(got, []LineRange{{2, 2}, {5, 5}}) {
		t.Errorf("changed lines: %v %v", got, ok)
	}
	// A deletion marks the head line at the deletion point.
	got, _ = ChangedLines("a\nb\nc\n", "a\nc\n")
	if !reflect.DeepEqual(got, []LineRange{{2, 2}}) {
		t.Errorf("deletion: %v", got)
	}
	if got, ok := ChangedLines("same\n", "same\n"); !ok || len(got) != 0 {
		t.Errorf("identical: %v", got)
	}
}

// fixture: lib.py defines helper (lines 1-3) and Service.run (5-9);
// app.py:main calls Service.run (confirmed); test_app.py:test_main calls
// main; tools.py:cli calls helper only through an inferred relationship.
func fixture(t *testing.T) (*Analyzer, context.Context) {
	b := graph.NewBuilder()
	b.AddFileTree("repo", []graph.Node{
		{Key: "file:lib.py", Kind: graph.KindFile, Name: "lib.py", Path: "lib.py", Attrs: map[string]any{"category": "source"}},
		{Key: "file:app.py", Kind: graph.KindFile, Name: "app.py", Path: "app.py", Attrs: map[string]any{"category": "source"}},
		{Key: "file:tests/test_app.py", Kind: graph.KindFile, Name: "test_app.py", Path: "tests/test_app.py", Attrs: map[string]any{"category": "test"}},
		{Key: "file:tools.py", Kind: graph.KindFile, Name: "tools.py", Path: "tools.py", Attrs: map[string]any{"category": "source"}},
	})
	sym := func(key, kind, name, path string, start, end int32, exported bool) {
		b.AddNode(graph.Node{Key: key, Kind: kind, Name: name, Path: path, StartLine: start, EndLine: end, Exported: exported})
		b.AddEdge("file:"+path, key, graph.Edge{Kind: graph.RelContains, Confidence: graph.Confirmed, Rule: "structure.declaration"})
	}
	sym("sym:lib.py#helper", "function", "helper", "lib.py", 1, 3, false)
	sym("sym:lib.py#Service", "class", "Service", "lib.py", 5, 9, true)
	b.AddNode(graph.Node{Key: "sym:lib.py#Service.run", Kind: "method", Name: "run", Path: "lib.py", StartLine: 6, EndLine: 9, Exported: true})
	b.AddEdge("sym:lib.py#Service", "sym:lib.py#Service.run", graph.Edge{Kind: graph.RelContains, Confidence: graph.Confirmed, Rule: "structure.declaration"})
	sym("sym:app.py#main", "function", "main", "app.py", 3, 6, true)
	sym("sym:tests/test_app.py#test_main", "function", "test_main", "tests/test_app.py", 2, 3, false)
	sym("sym:tools.py#cli", "function", "cli", "tools.py", 1, 2, false)
	edge := func(src, dst, kind, conf string) {
		if !b.AddEdge(src, dst, graph.Edge{Kind: kind, Confidence: conf, Rule: "test.rule", EvidencePath: "x", EvidenceLine: 1}) {
			t.Fatalf("edge %s -> %s", src, dst)
		}
	}
	edge("sym:app.py#main", "sym:lib.py#Service.run", graph.RelCalls, graph.Confirmed)
	edge("file:app.py", "sym:lib.py#Service", graph.RelImports, graph.Confirmed)
	edge("sym:tests/test_app.py#test_main", "sym:app.py#main", graph.RelCalls, graph.Probable)
	edge("sym:tools.py#cli", "sym:lib.py#helper", graph.RelCalls, graph.Inferred)
	nodes, edges, err := b.Build()
	if err != nil {
		t.Fatal(err)
	}
	store := graph.NewMemory()
	scope := graph.Scope{OrganizationID: "org", VersionID: "v1"}
	ctx := context.Background()
	if err := store.ReplaceSnapshot(ctx, scope, nodes, edges); err != nil {
		t.Fatal(err)
	}
	return &Analyzer{Graph: store, Scope: scope}, ctx
}

func keys(list []Affected) string {
	var out []string
	for _, a := range list {
		out = append(out, a.Node.Key+"@"+a.Confidence)
	}
	return strings.Join(out, " ")
}

func TestDiffMapsToInnermostSymbols(t *testing.T) {
	a, ctx := fixture(t)
	changed, unmatched, err := a.SymbolsForChanges(ctx, []FileChange{
		{Path: "lib.py", Status: "modified", Ranges: []LineRange{{7, 8}, {4, 4}}},
		{Path: "missing.py", Status: "modified", Ranges: []LineRange{{1, 1}}},
	})
	if err != nil {
		t.Fatal(err)
	}
	var got []string
	for _, c := range changed {
		got = append(got, c.Node.Key+" ("+c.Reason+")")
	}
	// Line 4 is between declarations: the file itself changes.
	want := []string{"file:lib.py (lines outside any declaration changed)", "sym:lib.py#Service.run (lines 7–8 changed)"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("changed: %v", got)
	}
	if len(unmatched) != 1 || !strings.Contains(unmatched[0], "missing.py") {
		t.Errorf("unmatched: %v", unmatched)
	}
}

func TestAnalyzeFollowsDependentsAndFindsTests(t *testing.T) {
	a, ctx := fixture(t)
	run, _ := a.Graph.NodeByKey(ctx, a.Scope, "sym:lib.py#Service.run")
	res, err := a.Analyze(ctx, []Changed{{Node: run, Reason: "edited"}}, nil, Options{Depth: 3})
	if err != nil {
		t.Fatal(err)
	}
	// main calls run (confirmed); the test reaches it through main
	// (probable: the weakest link).
	if got := keys(res.Affected); got != "sym:app.py#main@confirmed sym:tests/test_app.py#test_main@probable" {
		t.Errorf("affected: %s", got)
	}
	if got := keys(res.Tests); got != "sym:tests/test_app.py#test_main@probable" {
		t.Errorf("tests: %s", got)
	}
	test := res.Tests[0]
	if strings.Join(test.Via, " > ") != "sym:tests/test_app.py#test_main > sym:app.py#main > sym:lib.py#Service.run" ||
		strings.Join(test.Kinds, ",") != "CALLS,CALLS" || test.Distance != 2 {
		t.Errorf("path: %+v", test)
	}
	if len(res.UntestedChanges) != 0 {
		t.Errorf("untested: %v", res.UntestedChanges)
	}
	if res.Risk.Level == "" || len(res.Risk.Factors) != 6 {
		t.Errorf("risk: %+v", res.Risk)
	}

	// Depth 1 stops before the test; the change is then untested.
	res, _ = a.Analyze(ctx, []Changed{{Node: run}}, nil, Options{Depth: 1})
	if keys(res.Affected) != "sym:app.py#main@confirmed" || len(res.UntestedChanges) != 1 {
		t.Errorf("depth 1: %s untested=%v", keys(res.Affected), res.UntestedChanges)
	}

	// Inferred relationships are followed only on request.
	helper, _ := a.Graph.NodeByKey(ctx, a.Scope, "sym:lib.py#helper")
	res, _ = a.Analyze(ctx, []Changed{{Node: helper}}, nil, Options{})
	if len(res.Affected) != 0 {
		t.Errorf("inferred followed by default: %s", keys(res.Affected))
	}
	res, _ = a.Analyze(ctx, []Changed{{Node: helper}}, nil, Options{MinConfidence: graph.Inferred})
	if keys(res.Affected) != "sym:tools.py#cli@inferred" {
		t.Errorf("inferred: %s", keys(res.Affected))
	}
}

func TestRiskIsExplained(t *testing.T) {
	a, ctx := fixture(t)
	svc, _ := a.Graph.NodeByKey(ctx, a.Scope, "sym:lib.py#Service")
	res, _ := a.Analyze(ctx, []Changed{{Node: svc}}, nil, Options{})
	total := 0.0
	for _, f := range res.Risk.Factors {
		if f.Value < 0 || f.Value > 1 || f.Explanation == "" {
			t.Errorf("factor %+v", f)
		}
		total += f.Points
	}
	if diff := total - float64(res.Risk.Score); diff > 1 || diff < -1 {
		t.Errorf("score %d does not match its factors (%.1f)", res.Risk.Score, total)
	}
	// Service is imported by app.py (a file) and exported but no test
	// reaches it.
	if len(res.UntestedChanges) != 1 || keys(res.Affected) != "file:app.py@confirmed" {
		t.Errorf("service: %s %v", keys(res.Affected), res.UntestedChanges)
	}
}
