package graph

import (
	"context"
	"sort"
	"strings"
	"sync"
)

// Memory is an in-process Store. It is the reference implementation of the
// Store contract, used by tests and suitable for embedding.
type Memory struct {
	mu        sync.RWMutex
	snapshots map[Scope]*memSnapshot
}

type memSnapshot struct {
	nodes []Node
	edges []Edge
	byKey map[string]int32
	out   map[int32][]int // node id -> indexes into edges
	in    map[int32][]int
}

func NewMemory() *Memory { return &Memory{snapshots: map[Scope]*memSnapshot{}} }

func (m *Memory) ReplaceSnapshot(_ context.Context, s Scope, nodes []Node, edges []Edge) error {
	if err := Validate(nodes, edges); err != nil {
		return err
	}
	snap := &memSnapshot{
		nodes: append([]Node(nil), nodes...), edges: append([]Edge(nil), edges...),
		byKey: make(map[string]int32, len(nodes)), out: map[int32][]int{}, in: map[int32][]int{},
	}
	for _, n := range nodes {
		snap.byKey[n.Key] = n.ID
	}
	for i, e := range edges {
		snap.out[e.Src] = append(snap.out[e.Src], i)
		snap.in[e.Dst] = append(snap.in[e.Dst], i)
	}
	m.mu.Lock()
	m.snapshots[s] = snap
	m.mu.Unlock()
	return nil
}

func (m *Memory) get(s Scope) (*memSnapshot, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	snap, ok := m.snapshots[s]
	if !ok {
		return nil, ErrNotFound
	}
	return snap, nil
}

func (m *Memory) Stats(_ context.Context, s Scope) (Stats, error) {
	snap, err := m.get(s)
	if err != nil {
		return Stats{}, err
	}
	st := Stats{Nodes: int64(len(snap.nodes)), Edges: int64(len(snap.edges)), NodesByKind: map[string]int64{}, EdgesByKind: map[string]int64{}, EdgesByConfidence: map[string]int64{}}
	for _, n := range snap.nodes {
		st.NodesByKind[n.Kind]++
	}
	for _, e := range snap.edges {
		st.EdgesByKind[e.Kind]++
		st.EdgesByConfidence[e.Confidence]++
	}
	return st, nil
}

func (m *Memory) NodeByKey(_ context.Context, s Scope, key string) (Node, error) {
	snap, err := m.get(s)
	if err != nil {
		return Node{}, err
	}
	id, ok := snap.byKey[key]
	if !ok {
		return Node{}, ErrNotFound
	}
	return snap.nodes[id], nil
}

func (m *Memory) Search(_ context.Context, s Scope, q SearchQuery) ([]Node, int64, error) {
	snap, err := m.get(s)
	if err != nil {
		return nil, 0, err
	}
	kinds := toSet(q.Kinds)
	text := strings.ToLower(q.Text)
	var hits []Node
	for _, n := range snap.nodes {
		if len(kinds) > 0 && !kinds[n.Kind] {
			continue
		}
		if q.PathPrefix != "" && !strings.HasPrefix(n.Path, q.PathPrefix) {
			continue
		}
		if text != "" && !strings.Contains(strings.ToLower(n.Name), text) && !strings.Contains(strings.ToLower(n.QualifiedName), text) {
			continue
		}
		hits = append(hits, n)
	}
	sortSearch(hits, text)
	total := int64(len(hits))
	if q.Offset >= len(hits) {
		return nil, total, nil
	}
	hits = hits[q.Offset:]
	if q.Limit > 0 && len(hits) > q.Limit {
		hits = hits[:q.Limit]
	}
	return hits, total, nil
}

// sortSearch orders results: exact name, then prefix, then the rest; ties by
// name, path and line. Both stores implement this ordering.
func sortSearch(nodes []Node, lowerText string) {
	rank := func(n Node) int {
		name := strings.ToLower(n.Name)
		switch {
		case lowerText != "" && name == lowerText:
			return 0
		case lowerText != "" && strings.HasPrefix(name, lowerText):
			return 1
		}
		return 2
	}
	sort.SliceStable(nodes, func(i, j int) bool {
		a, b := nodes[i], nodes[j]
		if ra, rb := rank(a), rank(b); ra != rb {
			return ra < rb
		}
		if la, lb := strings.ToLower(a.Name), strings.ToLower(b.Name); la != lb {
			return la < lb
		}
		if a.Path != b.Path {
			return a.Path < b.Path
		}
		if a.StartLine != b.StartLine {
			return a.StartLine < b.StartLine
		}
		return a.Key < b.Key
	})
}

func edgeAllowed(e Edge, kinds map[string]bool, maxRank int) bool {
	return (len(kinds) == 0 || kinds[e.Kind]) && confidenceRank[e.Confidence] <= maxRank
}

func filterRank(f EdgeFilter) int {
	if r, ok := confidenceRank[f.MinConfidence]; ok {
		return r
	}
	return 3
}

func (m *Memory) Neighborhood(_ context.Context, s Scope, key string, q NeighborhoodQuery) (Subgraph, error) {
	snap, err := m.get(s)
	if err != nil {
		return Subgraph{}, err
	}
	start, ok := snap.byKey[key]
	if !ok {
		return Subgraph{}, ErrNotFound
	}
	q = q.Normalize()
	kinds, maxRank := toSet(q.Filter.Kinds), filterRank(q.Filter)
	visited := map[int32]bool{start: true}
	order := []int32{start}
	frontier := []int32{start}
	truncated := false
	for depth := 0; depth < q.Depth && len(frontier) > 0 && !truncated; depth++ {
		var candidates []int32
		for _, id := range frontier {
			if q.Direction != In {
				for _, ei := range snap.out[id] {
					if e := snap.edges[ei]; edgeAllowed(e, kinds, maxRank) {
						candidates = append(candidates, e.Dst)
					}
				}
			}
			if q.Direction != Out {
				for _, ei := range snap.in[id] {
					if e := snap.edges[ei]; edgeAllowed(e, kinds, maxRank) {
						candidates = append(candidates, e.Src)
					}
				}
			}
		}
		// Deterministic expansion order, shared with the Postgres store.
		sort.Slice(candidates, func(i, j int) bool { return candidates[i] < candidates[j] })
		var next []int32
		for _, id := range candidates {
			if visited[id] {
				continue
			}
			if len(order) >= q.MaxNodes {
				truncated = true
				break
			}
			visited[id] = true
			order = append(order, id)
			next = append(next, id)
		}
		frontier = next
	}
	sub := Subgraph{Truncated: truncated}
	for _, id := range order {
		sub.Nodes = append(sub.Nodes, snap.nodes[id])
	}
	for _, e := range snap.edges {
		if visited[e.Src] && visited[e.Dst] && edgeAllowed(e, kinds, maxRank) {
			sub.Edges = append(sub.Edges, e)
		}
	}
	sortEdges(sub.Edges)
	return sub, nil
}

func (m *Memory) Subgraph(_ context.Context, s Scope, nodeKinds []string, f EdgeFilter, maxEdges int) (Subgraph, error) {
	snap, err := m.get(s)
	if err != nil {
		return Subgraph{}, err
	}
	nk, kinds, maxRank := toSet(nodeKinds), toSet(f.Kinds), filterRank(f)
	var edges []Edge
	for _, e := range snap.edges {
		if edgeAllowed(e, kinds, maxRank) && (len(nk) == 0 || (nk[snap.nodes[e.Src].Kind] && nk[snap.nodes[e.Dst].Kind])) {
			edges = append(edges, e)
		}
	}
	sortEdges(edges)
	sub := Subgraph{}
	if maxEdges > 0 && len(edges) > maxEdges {
		edges, sub.Truncated = edges[:maxEdges], true
	}
	sub.Edges = edges
	used := map[int32]bool{}
	for _, e := range edges {
		used[e.Src], used[e.Dst] = true, true
	}
	for _, n := range snap.nodes {
		if used[n.ID] {
			sub.Nodes = append(sub.Nodes, n)
		}
	}
	return sub, nil
}

func (m *Memory) Diff(_ context.Context, base, head Scope, sampleLimit int) (Diff, error) {
	b, err := m.get(base)
	if err != nil {
		return Diff{}, err
	}
	h, err := m.get(head)
	if err != nil {
		return Diff{}, err
	}
	var d Diff
	sample := func(list *[]Node, n Node) {
		if len(*list) < sampleLimit {
			*list = append(*list, n)
		} else {
			d.Truncated = true
		}
	}
	for _, n := range sortedByKey(h.nodes) {
		id, ok := b.byKey[n.Key]
		switch {
		case !ok:
			d.NodesAdded++
			sample(&d.Added, n)
		case nodeChanged(b.nodes[id], n):
			d.NodesChanged++
			sample(&d.Changed, n)
		}
	}
	for _, n := range sortedByKey(b.nodes) {
		if _, ok := h.byKey[n.Key]; !ok {
			d.NodesRemoved++
			sample(&d.Removed, n)
		}
	}
	keysOf := func(s *memSnapshot) map[[3]string]string {
		out := make(map[[3]string]string, len(s.edges))
		for _, e := range s.edges {
			out[[3]string{s.nodes[e.Src].Key, s.nodes[e.Dst].Key, e.Kind}] = e.Confidence
		}
		return out
	}
	be, he := keysOf(b), keysOf(h)
	collect := func(from, other map[[3]string]string, count *int64, list *[]EdgeKeys) {
		var all []EdgeKeys
		for k, conf := range from {
			if _, ok := other[k]; !ok {
				all = append(all, EdgeKeys{Src: k[0], Dst: k[1], Kind: k[2], Confidence: conf})
			}
		}
		sort.Slice(all, func(i, j int) bool {
			if all[i].Src != all[j].Src {
				return all[i].Src < all[j].Src
			}
			if all[i].Dst != all[j].Dst {
				return all[i].Dst < all[j].Dst
			}
			return all[i].Kind < all[j].Kind
		})
		*count = int64(len(all))
		if len(all) > sampleLimit {
			all, d.Truncated = all[:sampleLimit], true
		}
		*list = all
	}
	collect(he, be, &d.EdgesAdded, &d.AddedEdges)
	collect(be, he, &d.EdgesRemoved, &d.RemovedEdges)
	return d, nil
}

// nodeChanged compares the properties that describe a symbol's interface.
// Line numbers are excluded: moving code is not a semantic change.
func nodeChanged(a, b Node) bool {
	return a.Kind != b.Kind || a.Signature != b.Signature || a.Exported != b.Exported
}

func sortedByKey(nodes []Node) []Node {
	out := append([]Node(nil), nodes...)
	sort.Slice(out, func(i, j int) bool { return out[i].Key < out[j].Key })
	return out
}

func sortEdges(edges []Edge) {
	sort.Slice(edges, func(i, j int) bool {
		a, b := edges[i], edges[j]
		if a.Src != b.Src {
			return a.Src < b.Src
		}
		if a.Dst != b.Dst {
			return a.Dst < b.Dst
		}
		return a.Kind < b.Kind
	})
}

func toSet(list []string) map[string]bool {
	if len(list) == 0 {
		return nil
	}
	set := make(map[string]bool, len(list))
	for _, s := range list {
		set[s] = true
	}
	return set
}
