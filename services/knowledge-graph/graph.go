// Package graph is the knowledge-graph subsystem: the data model, the
// storage abstraction and its implementations.
//
// A graph snapshot belongs to exactly one repository version and is
// immutable once written. PostgreSQL is the system of record; the Store
// interface is what a graph-database projection would implement (ADR 0003).
package graph

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"strings"
)

// Node kinds.
const (
	KindRepository = "repository"
	KindDirectory  = "directory"
	KindFile       = "file"
	KindPackage    = "package"
)

// Relationship kinds.
const (
	RelContains     = "CONTAINS"
	RelImports      = "IMPORTS"
	RelExports      = "EXPORTS"
	RelCalls        = "CALLS"
	RelInstantiates = "INSTANTIATES"
	RelExtends      = "EXTENDS"
	RelImplements   = "IMPLEMENTS"
	RelReferences   = "REFERENCES"
	RelDependsOn    = "DEPENDS_ON"
)

var relKinds = map[string]bool{
	RelContains: true, RelImports: true, RelExports: true, RelCalls: true, RelInstantiates: true,
	RelExtends: true, RelImplements: true, RelReferences: true, RelDependsOn: true,
}

// Confidence levels, strongest first. See docs/specifications/semantic-resolution.md.
const (
	Confirmed = "confirmed"
	Probable  = "probable"
	Inferred  = "inferred"
	Unknown   = "unknown"
)

var confidenceRank = map[string]int{Confirmed: 0, Probable: 1, Inferred: 2, Unknown: 3}

// ConfidencesAtLeast lists the levels that are at least as strong as min.
// An empty or unrecognized min means no filtering.
func ConfidencesAtLeast(min string) []string {
	rank, ok := confidenceRank[min]
	if !ok {
		rank = 3
	}
	var out []string
	for _, c := range []string{Confirmed, Probable, Inferred, Unknown} {
		if confidenceRank[c] <= rank {
			out = append(out, c)
		}
	}
	return out
}

type Node struct {
	// ID is local to the snapshot. Key is stable across versions.
	ID            int32          `json:"id"`
	Key           string         `json:"key"`
	Kind          string         `json:"kind"`
	Name          string         `json:"name"`
	QualifiedName string         `json:"qualified_name,omitempty"`
	Path          string         `json:"path,omitempty"`
	StartLine     int32          `json:"start_line,omitempty"`
	EndLine       int32          `json:"end_line,omitempty"`
	Signature     string         `json:"signature,omitempty"`
	Doc           string         `json:"doc,omitempty"`
	Exported      bool           `json:"exported,omitempty"`
	Attrs         map[string]any `json:"attrs,omitempty"`
}

type Edge struct {
	Src          int32  `json:"src"`
	Dst          int32  `json:"dst"`
	Kind         string `json:"kind"`
	Confidence   string `json:"confidence"`
	Rule         string `json:"rule"`
	EvidencePath string `json:"evidence_path,omitempty"`
	EvidenceLine int32  `json:"evidence_line,omitempty"`
	Occurrences  int32  `json:"occurrences"`
}

// Scope identifies a snapshot and the tenant that owns it. Every store
// operation is scoped; a version id alone is never sufficient.
type Scope struct {
	OrganizationID string
	VersionID      string
}

type Stats struct {
	Nodes             int64            `json:"nodes"`
	Edges             int64            `json:"edges"`
	NodesByKind       map[string]int64 `json:"nodes_by_kind"`
	EdgesByKind       map[string]int64 `json:"edges_by_kind"`
	EdgesByConfidence map[string]int64 `json:"edges_by_confidence"`
}

type SearchQuery struct {
	// Text matches a case-insensitive substring of the name or qualified name.
	Text       string
	Kinds      []string
	PathPrefix string
	Limit      int
	Offset     int
}

type Direction string

const (
	Out  Direction = "out"
	In   Direction = "in"
	Both Direction = "both"
)

type EdgeFilter struct {
	Kinds         []string
	MinConfidence string
}

type NeighborhoodQuery struct {
	Depth     int
	Direction Direction
	Filter    EdgeFilter
	// MaxNodes bounds the result. When reached, Truncated is set.
	MaxNodes int
}

type Subgraph struct {
	Nodes     []Node `json:"nodes"`
	Edges     []Edge `json:"edges"`
	Truncated bool   `json:"truncated"`
}

// Diff describes how the head snapshot differs from the base snapshot.
// Nodes and relationships are matched by their stable keys.
type Diff struct {
	NodesAdded   int64      `json:"nodes_added"`
	NodesRemoved int64      `json:"nodes_removed"`
	NodesChanged int64      `json:"nodes_changed"`
	EdgesAdded   int64      `json:"edges_added"`
	EdgesRemoved int64      `json:"edges_removed"`
	Added        []Node     `json:"added"`
	Removed      []Node     `json:"removed"`
	Changed      []Node     `json:"changed"`
	AddedEdges   []EdgeKeys `json:"added_edges"`
	RemovedEdges []EdgeKeys `json:"removed_edges"`
	// Truncated is set when the sample lists were capped; counts are exact.
	Truncated bool `json:"truncated"`
}

type EdgeKeys struct {
	Src        string `json:"src"`
	Dst        string `json:"dst"`
	Kind       string `json:"kind"`
	Confidence string `json:"confidence"`
}

var ErrNotFound = errors.New("graph: not found")

// Store is the graph storage abstraction.
type Store interface {
	// ReplaceSnapshot atomically replaces the snapshot of a version.
	ReplaceSnapshot(ctx context.Context, s Scope, nodes []Node, edges []Edge) error
	Stats(ctx context.Context, s Scope) (Stats, error)
	NodeByKey(ctx context.Context, s Scope, key string) (Node, error)
	Search(ctx context.Context, s Scope, q SearchQuery) (nodes []Node, total int64, err error)
	// Neighborhood explores breadth-first from a node.
	Neighborhood(ctx context.Context, s Scope, key string, q NeighborhoodQuery) (Subgraph, error)
	// Subgraph returns all edges of the given kinds between nodes of the given kinds.
	Subgraph(ctx context.Context, s Scope, nodeKinds []string, f EdgeFilter, maxEdges int) (Subgraph, error)
	Diff(ctx context.Context, base, head Scope, sampleLimit int) (Diff, error)
}

const (
	MaxDepth    = 6
	MaxMaxNodes = 2000
)

// Normalize clamps a query to the supported bounds.
func (q NeighborhoodQuery) Normalize() NeighborhoodQuery {
	if q.Depth < 1 {
		q.Depth = 1
	}
	if q.Depth > MaxDepth {
		q.Depth = MaxDepth
	}
	if q.MaxNodes < 1 || q.MaxNodes > MaxMaxNodes {
		q.MaxNodes = 200
	}
	if q.Direction != In && q.Direction != Out {
		q.Direction = Both
	}
	return q
}

// Validate checks the structural invariants of a snapshot before it is
// stored: dense unique ids, unique keys, no dangling or duplicate
// relationships, known relationship kinds and confidence levels, and
// provenance on every relationship.
func Validate(nodes []Node, edges []Edge) error {
	keys := make(map[string]struct{}, len(nodes))
	for i, n := range nodes {
		if n.ID != int32(i) {
			return fmt.Errorf("graph: node %d has id %d (ids must be dense and ordered)", i, n.ID)
		}
		if n.Key == "" || n.Kind == "" {
			return fmt.Errorf("graph: node %d has an empty key or kind", i)
		}
		if _, dup := keys[n.Key]; dup {
			return fmt.Errorf("graph: duplicate node key %q", n.Key)
		}
		keys[n.Key] = struct{}{}
	}
	type edgeID struct {
		src, dst int32
		kind     string
	}
	seen := make(map[edgeID]struct{}, len(edges))
	count := int32(len(nodes))
	for _, e := range edges {
		if e.Src < 0 || e.Src >= count || e.Dst < 0 || e.Dst >= count {
			return fmt.Errorf("graph: relationship %s references a missing node (%d -> %d)", e.Kind, e.Src, e.Dst)
		}
		if !relKinds[e.Kind] {
			return fmt.Errorf("graph: unknown relationship kind %q", e.Kind)
		}
		if _, ok := confidenceRank[e.Confidence]; !ok {
			return fmt.Errorf("graph: unknown confidence %q", e.Confidence)
		}
		if e.Rule == "" {
			return fmt.Errorf("graph: relationship %s %d -> %d has no provenance rule", e.Kind, e.Src, e.Dst)
		}
		id := edgeID{e.Src, e.Dst, e.Kind}
		if _, dup := seen[id]; dup {
			return fmt.Errorf("graph: duplicate relationship %s %d -> %d", e.Kind, e.Src, e.Dst)
		}
		seen[id] = struct{}{}
	}
	return nil
}

// Builder assembles a snapshot from keyed nodes and relationships.
type Builder struct {
	nodes []Node
	index map[string]int32
	edges []Edge
	edgeI map[[3]string]int
}

func NewBuilder() *Builder {
	return &Builder{index: map[string]int32{}, edgeI: map[[3]string]int{}}
}

// AddNode adds a node, or returns the existing id for its key.
func (b *Builder) AddNode(n Node) int32 {
	if id, ok := b.index[n.Key]; ok {
		return id
	}
	n.ID = int32(len(b.nodes))
	b.index[n.Key] = n.ID
	b.nodes = append(b.nodes, n)
	return n.ID
}

func (b *Builder) Has(key string) bool { _, ok := b.index[key]; return ok }

// AddEdge links two existing nodes by key. It reports false, adding
// nothing, when either endpoint is unknown, so callers can account for
// relationships that could not be represented.
func (b *Builder) AddEdge(srcKey, dstKey string, e Edge) bool {
	src, ok1 := b.index[srcKey]
	dst, ok2 := b.index[dstKey]
	if !ok1 || !ok2 {
		return false
	}
	id := [3]string{srcKey, dstKey, e.Kind}
	if i, dup := b.edgeI[id]; dup {
		b.edges[i].Occurrences += e.Occurrences
		if confidenceRank[e.Confidence] < confidenceRank[b.edges[i].Confidence] {
			b.edges[i].Confidence, b.edges[i].Rule = e.Confidence, e.Rule
			b.edges[i].EvidencePath, b.edges[i].EvidenceLine = e.EvidencePath, e.EvidenceLine
		}
		return true
	}
	e.Src, e.Dst = src, dst
	if e.Occurrences < 1 {
		e.Occurrences = 1
	}
	b.edgeI[id] = len(b.edges)
	b.edges = append(b.edges, e)
	return true
}

// AddFileTree adds the repository node, a directory node for every
// ancestor of the given file paths, a file node per path, and the
// CONTAINS relationships between them.
func (b *Builder) AddFileTree(repoName string, files []Node) {
	structural := Edge{Kind: RelContains, Confidence: Confirmed, Rule: "structure.filesystem"}
	b.AddNode(Node{Key: "repo", Kind: KindRepository, Name: repoName})
	var ensureDir func(dir string) string
	ensureDir = func(dir string) string {
		if dir == "" {
			return "repo"
		}
		key := "dir:" + dir
		if b.Has(key) {
			return key
		}
		parent, name := "", dir
		if i := strings.LastIndex(dir, "/"); i >= 0 {
			parent, name = dir[:i], dir[i+1:]
		}
		parentKey := ensureDir(parent)
		b.AddNode(Node{Key: key, Kind: KindDirectory, Name: name, Path: dir})
		b.AddEdge(parentKey, key, structural)
		return key
	}
	sorted := append([]Node(nil), files...)
	sort.Slice(sorted, func(i, j int) bool { return sorted[i].Path < sorted[j].Path })
	for _, f := range sorted {
		dir := ""
		if i := strings.LastIndex(f.Path, "/"); i >= 0 {
			dir = f.Path[:i]
		}
		parentKey := ensureDir(dir)
		f.Key, f.Kind = "file:"+f.Path, KindFile
		b.AddNode(f)
		b.AddEdge(parentKey, f.Key, structural)
	}
}

func (b *Builder) Build() ([]Node, []Edge, error) {
	if err := Validate(b.nodes, b.edges); err != nil {
		return nil, nil, err
	}
	return b.nodes, b.edges, nil
}
