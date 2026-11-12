package graph

import (
	"context"
	"errors"
	"fmt"
	"os"
	"reflect"
	"strings"
	"testing"

	"github.com/ammarkarimi/usie/database"
	"github.com/jackc/pgx/v5/pgxpool"
)

func TestValidate(t *testing.T) {
	nodes := []Node{{ID: 0, Key: "a", Kind: "file"}, {ID: 1, Key: "b", Kind: "file"}}
	ok := Edge{Src: 0, Dst: 1, Kind: RelImports, Confidence: Confirmed, Rule: "r", Occurrences: 1}
	if err := Validate(nodes, []Edge{ok}); err != nil {
		t.Fatal(err)
	}
	bad := map[string]func() ([]Node, []Edge){
		"sparse ids": func() ([]Node, []Edge) { return []Node{{ID: 1, Key: "a", Kind: "file"}}, nil },
		"duplicate key": func() ([]Node, []Edge) {
			return []Node{{ID: 0, Key: "a", Kind: "f"}, {ID: 1, Key: "a", Kind: "f"}}, nil
		},
		"empty key":          func() ([]Node, []Edge) { return []Node{{ID: 0, Kind: "f"}}, nil },
		"dangling":           func() ([]Node, []Edge) { e := ok; e.Dst = 9; return nodes, []Edge{e} },
		"negative":           func() ([]Node, []Edge) { e := ok; e.Src = -1; return nodes, []Edge{e} },
		"unknown kind":       func() ([]Node, []Edge) { e := ok; e.Kind = "LIKES"; return nodes, []Edge{e} },
		"unknown confidence": func() ([]Node, []Edge) { e := ok; e.Confidence = "certain"; return nodes, []Edge{e} },
		"no provenance":      func() ([]Node, []Edge) { e := ok; e.Rule = ""; return nodes, []Edge{e} },
		"duplicate edge":     func() ([]Node, []Edge) { return nodes, []Edge{ok, ok} },
	}
	for name, build := range bad {
		if err := Validate(build()); err == nil {
			t.Errorf("%s: expected an error", name)
		}
	}
}

func TestBuilder(t *testing.T) {
	b := NewBuilder()
	b.AddFileTree("demo", []Node{{Path: "src/app/main.py", Name: "main.py"}, {Path: "README.md", Name: "README.md"}, {Path: "src/util.py", Name: "util.py"}})
	b.AddNode(Node{Key: "sym:src/util.py#f", Kind: "function", Name: "f", Path: "src/util.py"})
	if !b.AddEdge("file:src/util.py", "sym:src/util.py#f", Edge{Kind: RelContains, Confidence: Confirmed, Rule: "structure.declaration"}) {
		t.Fatal("edge should be added")
	}
	if b.AddEdge("file:src/util.py", "sym:missing", Edge{Kind: RelCalls, Confidence: Confirmed, Rule: "r"}) {
		t.Fatal("edge to a missing node must be refused")
	}
	// Merging keeps the strongest evidence and sums occurrences.
	b.AddEdge("file:src/app/main.py", "sym:src/util.py#f", Edge{Kind: RelCalls, Confidence: Inferred, Rule: "weak", Occurrences: 2})
	b.AddEdge("file:src/app/main.py", "sym:src/util.py#f", Edge{Kind: RelCalls, Confidence: Confirmed, Rule: "strong", Occurrences: 3})
	nodes, edges, err := b.Build()
	if err != nil {
		t.Fatal(err)
	}
	var keys []string
	for _, n := range nodes {
		keys = append(keys, n.Key)
	}
	want := "repo file:README.md dir:src dir:src/app file:src/app/main.py file:src/util.py sym:src/util.py#f"
	if got := strings.Join(keys, " "); got != want {
		t.Errorf("nodes:\n got %s\nwant %s", got, want)
	}
	var call Edge
	contains := 0
	for _, e := range edges {
		switch e.Kind {
		case RelCalls:
			call = e
		case RelContains:
			contains++
		}
	}
	if call.Confidence != Confirmed || call.Rule != "strong" || call.Occurrences != 5 {
		t.Errorf("merged edge: %+v", call)
	}
	if contains != 6 {
		t.Errorf("CONTAINS edges = %d", contains)
	}
}

// fixture: a small call graph.
//
//	a.py: f -> g (confirmed), f -> h (probable), g -> h (inferred); b.py: h; a.py imports b.py
func fixture() ([]Node, []Edge) {
	b := NewBuilder()
	b.AddFileTree("demo", []Node{{Path: "a.py", Name: "a.py"}, {Path: "pkg/b.py", Name: "b.py"}})
	sym := func(path, name, kind, sig string, line int32) {
		key := "sym:" + path + "#" + name
		b.AddNode(Node{Key: key, Kind: kind, Name: name, QualifiedName: name, Path: path, StartLine: line, EndLine: line + 2, Signature: sig, Attrs: map[string]any{"language": "python"}})
		b.AddEdge("file:"+path, key, Edge{Kind: RelContains, Confidence: Confirmed, Rule: "structure.declaration"})
	}
	sym("a.py", "f", "function", "()", 1)
	sym("a.py", "g", "function", "(x)", 5)
	sym("pkg/b.py", "h", "function", "()", 1)
	sym("pkg/b.py", "Handler", "class", "", 9)
	b.AddEdge("sym:a.py#f", "sym:a.py#g", Edge{Kind: RelCalls, Confidence: Confirmed, Rule: "scope.module-symbol", EvidencePath: "a.py", EvidenceLine: 2})
	b.AddEdge("sym:a.py#f", "sym:pkg/b.py#h", Edge{Kind: RelCalls, Confidence: Probable, Rule: "py.import.wildcard", EvidencePath: "a.py", EvidenceLine: 3})
	b.AddEdge("sym:a.py#g", "sym:pkg/b.py#h", Edge{Kind: RelCalls, Confidence: Inferred, Rule: "heuristic.unique-method-name", EvidencePath: "a.py", EvidenceLine: 6})
	b.AddEdge("file:a.py", "file:pkg/b.py", Edge{Kind: RelImports, Confidence: Confirmed, Rule: "py.import.absolute", EvidencePath: "a.py", EvidenceLine: 1})
	nodes, edges, err := b.Build()
	if err != nil {
		panic(err)
	}
	return nodes, edges
}

func keysOf(nodes []Node) string {
	var keys []string
	for _, n := range nodes {
		keys = append(keys, n.Key)
	}
	return strings.Join(keys, " ")
}

// conformance is the Store contract. Every implementation must pass it.
func conformance(t *testing.T, store Store, scope func(i int) Scope) {
	ctx := context.Background()
	s := scope(1)
	nodes, edges := fixture()

	if _, err := store.Stats(ctx, s); !errors.Is(err, ErrNotFound) {
		t.Fatalf("stats of a missing snapshot: %v", err)
	}
	if err := store.ReplaceSnapshot(ctx, s, nodes, edges); err != nil {
		t.Fatal(err)
	}
	// Replacing is idempotent.
	if err := store.ReplaceSnapshot(ctx, s, nodes, edges); err != nil {
		t.Fatal(err)
	}
	if err := store.ReplaceSnapshot(ctx, scope(9), nodes, []Edge{{Src: 0, Dst: 999, Kind: RelCalls, Confidence: Confirmed, Rule: "r"}}); err == nil {
		t.Fatal("invalid snapshot accepted")
	}

	st, err := store.Stats(ctx, s)
	if err != nil {
		t.Fatal(err)
	}
	if st.Nodes != int64(len(nodes)) || st.Edges != int64(len(edges)) || st.NodesByKind["function"] != 3 || st.EdgesByKind[RelCalls] != 3 || st.EdgesByConfidence[Inferred] != 1 {
		t.Errorf("stats: %+v", st)
	}

	n, err := store.NodeByKey(ctx, s, "sym:a.py#g")
	if err != nil || n.Signature != "(x)" || n.StartLine != 5 || n.Attrs["language"] != "python" {
		t.Errorf("NodeByKey: %+v %v", n, err)
	}
	if _, err := store.NodeByKey(ctx, s, "sym:nope"); !errors.Is(err, ErrNotFound) {
		t.Errorf("missing node: %v", err)
	}

	// Tenant isolation: the same version id under another organization is invisible.
	other := Scope{OrganizationID: scope(2).OrganizationID, VersionID: s.VersionID}
	if _, err := store.NodeByKey(ctx, other, "sym:a.py#g"); !errors.Is(err, ErrNotFound) {
		t.Errorf("cross-tenant read: %v", err)
	}
	if _, _, err := store.Search(ctx, other, SearchQuery{}); !errors.Is(err, ErrNotFound) {
		t.Errorf("cross-tenant search: %v", err)
	}
	if _, err := store.Neighborhood(ctx, other, "sym:a.py#g", NeighborhoodQuery{}); !errors.Is(err, ErrNotFound) {
		t.Errorf("cross-tenant neighborhood: %v", err)
	}

	search := func(q SearchQuery) (string, int64) {
		got, total, err := store.Search(ctx, s, q)
		if err != nil {
			t.Fatal(err)
		}
		return keysOf(got), total
	}
	if got, total := search(SearchQuery{Text: "H", Kinds: []string{"function", "class"}}); got != "sym:pkg/b.py#h sym:pkg/b.py#Handler" || total != 2 {
		t.Errorf("search H: %q %d", got, total)
	}
	if got, total := search(SearchQuery{Kinds: []string{"function"}, Limit: 2, Offset: 1}); got != "sym:a.py#g sym:pkg/b.py#h" || total != 3 {
		t.Errorf("search paging: %q %d", got, total)
	}
	if got, _ := search(SearchQuery{PathPrefix: "pkg/", Kinds: []string{"file", "function"}}); got != "file:pkg/b.py sym:pkg/b.py#h" {
		t.Errorf("search prefix: %q", got)
	}
	if got, total := search(SearchQuery{Text: "%_"}); got != "" || total != 0 {
		t.Errorf("pattern characters must be literal: %q", got)
	}

	hood := func(key string, q NeighborhoodQuery) Subgraph {
		sub, err := store.Neighborhood(ctx, s, key, q)
		if err != nil {
			t.Fatal(err)
		}
		return sub
	}
	calls := EdgeFilter{Kinds: []string{RelCalls}}
	if sub := hood("sym:a.py#f", NeighborhoodQuery{Depth: 1, Direction: Out, Filter: calls}); keysOf(sub.Nodes) != "sym:a.py#f sym:a.py#g sym:pkg/b.py#h" || len(sub.Edges) != 3 {
		t.Errorf("out depth 1: %s (%d edges)", keysOf(sub.Nodes), len(sub.Edges))
	}
	if sub := hood("sym:pkg/b.py#h", NeighborhoodQuery{Depth: 1, Direction: In, Filter: calls}); keysOf(sub.Nodes) != "sym:pkg/b.py#h sym:a.py#f sym:a.py#g" {
		t.Errorf("in depth 1: %s", keysOf(sub.Nodes))
	}
	// Confidence filtering removes the inferred and probable calls into h.
	strict := EdgeFilter{Kinds: []string{RelCalls}, MinConfidence: Confirmed}
	if sub := hood("sym:pkg/b.py#h", NeighborhoodQuery{Depth: 3, Direction: In, Filter: strict}); keysOf(sub.Nodes) != "sym:pkg/b.py#h" || len(sub.Edges) != 0 {
		t.Errorf("confirmed only: %s", keysOf(sub.Nodes))
	}
	if sub := hood("sym:pkg/b.py#h", NeighborhoodQuery{Depth: 3, Direction: In, Filter: EdgeFilter{Kinds: []string{RelCalls}, MinConfidence: Probable}}); keysOf(sub.Nodes) != "sym:pkg/b.py#h sym:a.py#f" {
		t.Errorf("probable or better: %s", keysOf(sub.Nodes))
	}
	if sub := hood("sym:a.py#f", NeighborhoodQuery{Depth: 2, Direction: Both, MaxNodes: 3}); len(sub.Nodes) != 3 || !sub.Truncated {
		t.Errorf("cap: %d nodes truncated=%v", len(sub.Nodes), sub.Truncated)
	}
	if sub := hood("repo", NeighborhoodQuery{Depth: 6, Direction: Out}); len(sub.Nodes) != len(nodes) || sub.Truncated {
		t.Errorf("full traversal: %d nodes", len(sub.Nodes))
	}
	for _, e := range hood("sym:a.py#f", NeighborhoodQuery{Depth: 1, Direction: Out, Filter: calls}).Edges {
		if e.Rule == "" || e.EvidencePath != "a.py" || e.EvidenceLine == 0 {
			t.Errorf("edge lost provenance: %+v", e)
		}
	}

	sub, err := store.Subgraph(ctx, s, []string{KindFile}, EdgeFilter{Kinds: []string{RelImports}}, 10)
	if err != nil || keysOf(sub.Nodes) != "file:a.py file:pkg/b.py" || len(sub.Edges) != 1 || sub.Truncated {
		t.Errorf("file graph: %s %v", keysOf(sub.Nodes), err)
	}
	if sub, _ := store.Subgraph(ctx, s, nil, EdgeFilter{Kinds: []string{RelCalls}}, 2); len(sub.Edges) != 2 || !sub.Truncated {
		t.Errorf("subgraph cap: %d truncated=%v", len(sub.Edges), sub.Truncated)
	}

	// Diff against a modified head: g's signature changes, h is removed, k is added.
	hb := NewBuilder()
	hb.AddFileTree("demo", []Node{{Path: "a.py", Name: "a.py"}, {Path: "pkg/b.py", Name: "b.py"}})
	for _, n := range []Node{
		{Key: "sym:a.py#f", Kind: "function", Name: "f", Path: "a.py", Signature: "()", StartLine: 40},
		{Key: "sym:a.py#g", Kind: "function", Name: "g", Path: "a.py", Signature: "(x, y)"},
		{Key: "sym:a.py#k", Kind: "function", Name: "k", Path: "a.py", Signature: "()"},
		{Key: "sym:pkg/b.py#Handler", Kind: "class", Name: "Handler", Path: "pkg/b.py"},
	} {
		hb.AddNode(n)
	}
	hb.AddEdge("sym:a.py#f", "sym:a.py#g", Edge{Kind: RelCalls, Confidence: Confirmed, Rule: "r"})
	hb.AddEdge("sym:a.py#f", "sym:a.py#k", Edge{Kind: RelCalls, Confidence: Probable, Rule: "r"})
	hn, he, err := hb.Build()
	if err != nil {
		t.Fatal(err)
	}
	head := scope(3)
	if err := store.ReplaceSnapshot(ctx, head, hn, he); err != nil {
		t.Fatal(err)
	}
	d, err := store.Diff(ctx, s, head, 50)
	if err != nil {
		t.Fatal(err)
	}
	if keysOf(d.Added) != "sym:a.py#k" || keysOf(d.Removed) != "sym:pkg/b.py#h" || keysOf(d.Changed) != "sym:a.py#g" {
		t.Errorf("diff nodes: +%s -%s ~%s", keysOf(d.Added), keysOf(d.Removed), keysOf(d.Changed))
	}
	if d.NodesAdded != 1 || d.NodesRemoved != 1 || d.NodesChanged != 1 || d.EdgesAdded != 1 || d.Truncated {
		t.Errorf("diff counts: %+v", d)
	}
	if !reflect.DeepEqual(d.AddedEdges, []EdgeKeys{{Src: "sym:a.py#f", Dst: "sym:a.py#k", Kind: RelCalls, Confidence: Probable}}) {
		t.Errorf("added edges: %+v", d.AddedEdges)
	}
	// Removed: the import, 4 CONTAINS of symbols, and the two calls into h.
	if d.EdgesRemoved != 7 {
		t.Errorf("edges removed = %d: %+v", d.EdgesRemoved, d.RemovedEdges)
	}
	if small, _ := store.Diff(ctx, s, head, 2); len(small.RemovedEdges) != 2 || small.EdgesRemoved != 7 || !small.Truncated {
		t.Errorf("diff sampling: %d listed of %d, truncated=%v", len(small.RemovedEdges), small.EdgesRemoved, small.Truncated)
	}
	if _, err := store.Diff(ctx, s, scope(8), 10); !errors.Is(err, ErrNotFound) {
		t.Errorf("diff with missing snapshot: %v", err)
	}
}

func TestMemoryConformance(t *testing.T) {
	conformance(t, NewMemory(), func(i int) Scope {
		return Scope{OrganizationID: fmt.Sprintf("org-%d", i), VersionID: fmt.Sprintf("version-%d", i)}
	})
}

// TestPostgresConformance runs when USIE_TEST_DATABASE_URL points at a
// disposable database (see scripts/test-db.sh).
func TestPostgresConformance(t *testing.T) {
	url := os.Getenv("USIE_TEST_DATABASE_URL")
	if url == "" {
		t.Skip("USIE_TEST_DATABASE_URL not set")
	}
	ctx := context.Background()
	pool, err := pgxpool.New(ctx, url)
	if err != nil {
		t.Fatal(err)
	}
	defer pool.Close()
	if _, err := database.Migrate(ctx, pool); err != nil {
		t.Fatal(err)
	}
	scopes := map[int]Scope{}
	scope := func(i int) Scope {
		if s, ok := scopes[i]; ok {
			return s
		}
		var s Scope
		slug := fmt.Sprintf("graph-test-%d-%d", os.Getpid(), i)
		err := pool.QueryRow(ctx, `
			WITH o AS (INSERT INTO organizations (name, slug) VALUES ($1, $1) RETURNING id),
			     w AS (INSERT INTO workspaces (organization_id, name) SELECT id, 'w' FROM o RETURNING id, organization_id),
			     r AS (INSERT INTO repositories (organization_id, workspace_id, name, source_kind)
			           SELECT organization_id, id, 'r', 'upload' FROM w RETURNING id, organization_id)
			INSERT INTO repository_versions (organization_id, repository_id, sequence)
			SELECT organization_id, id, 1 FROM r RETURNING organization_id::text, id::text`, slug).Scan(&s.OrganizationID, &s.VersionID)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { pool.Exec(ctx, `DELETE FROM organizations WHERE slug = $1`, slug) })
		scopes[i] = s
		return s
	}
	conformance(t, NewPostgres(pool), scope)
}
