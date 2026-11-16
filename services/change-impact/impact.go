package impact

import (
	"context"
	"fmt"
	"math"
	"path"
	"sort"
	"strings"

	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
)

// Relationship kinds followed backwards from a changed node: whoever calls,
// instantiates, references, extends, implements or imports it is affected.
var impactKinds = []string{
	graph.RelCalls, graph.RelInstantiates, graph.RelReferences,
	graph.RelExtends, graph.RelImplements, graph.RelImports,
}

const (
	DefaultDepth = 3
	MaxDepth     = 6
	maxAffected  = 2000
	maxSeeds     = 500
	maxEdgesStep = 20000
)

// Changed is a node the change touches directly.
type Changed struct {
	Node   graph.Node `json:"node"`
	Reason string     `json:"reason"`
}

// Affected is a node that depends, directly or transitively, on a changed
// node.
type Affected struct {
	Node     graph.Node `json:"node"`
	Distance int        `json:"distance"`
	// Confidence is the weakest confidence along the best path.
	Confidence string `json:"confidence"`
	// Via is the path from this node to the changed node it depends on,
	// as node keys; Kinds are the relationship kinds along it.
	Via   []string `json:"via"`
	Kinds []string `json:"kinds"`
	Test  bool     `json:"test"`
}

// Factor is one term of the risk score.
type Factor struct {
	ID          string  `json:"id"`
	Label       string  `json:"label"`
	Value       float64 `json:"value"` // 0..1
	Weight      float64 `json:"weight"`
	Points      float64 `json:"points"` // Value × Weight
	Explanation string  `json:"explanation"`
}

type Risk struct {
	Score   int      `json:"score"` // 0..100
	Level   string   `json:"level"` // low | medium | high
	Factors []Factor `json:"factors"`
	Note    string   `json:"note"`
}

type Options struct {
	Depth         int
	MinConfidence string
}

// Result of an impact analysis. Counts are exact up to the stated limits;
// Truncated reports that a limit was reached.
type Result struct {
	Changed   []Changed  `json:"changed"`
	Unmatched []string   `json:"unmatched"`
	Affected  []Affected `json:"affected"`
	Tests     []Affected `json:"tests"`
	// UntestedChanges lists changed nodes that no affected test reaches.
	UntestedChanges []string `json:"untested_changes"`
	Files           int      `json:"affected_files"`
	Risk            Risk     `json:"risk"`
	Depth           int      `json:"depth"`
	MinConfidence   string   `json:"min_confidence"`
	Truncated       bool     `json:"truncated"`
	Limitations     []string `json:"limitations"`
}

// Analyzer reads one snapshot of the knowledge graph.
type Analyzer struct {
	Graph graph.Store
	Scope graph.Scope
}

// SymbolsForChanges maps line ranges to the innermost symbols they touch.
// Changes outside any symbol (module level) touch the file itself. Files
// that the analyzed version does not contain are reported in unmatched.
func (a *Analyzer) SymbolsForChanges(ctx context.Context, changes []FileChange) ([]Changed, []string, error) {
	var out []Changed
	var unmatched []string
	for _, fc := range changes {
		file, err := a.Graph.NodeByKey(ctx, a.Scope, "file:"+fc.Path)
		if err != nil {
			if fc.Status == "added" {
				unmatched = append(unmatched, fmt.Sprintf("%s: new file, not part of the analyzed version", fc.Path))
			} else {
				unmatched = append(unmatched, fmt.Sprintf("%s: not in the analyzed version", fc.Path))
			}
			continue
		}
		if fc.Status == "added" {
			unmatched = append(unmatched, fmt.Sprintf("%s: the diff adds a file that already exists in the analyzed version; is it the right version?", fc.Path))
			continue
		}
		syms, _, err := a.Graph.Search(ctx, a.Scope, graph.SearchQuery{PathPrefix: fc.Path, Limit: 5000,
			Kinds: []string{"function", "method", "class", "interface", "type", "variable", "namespace"}})
		if err != nil {
			return nil, nil, err
		}
		inFile := syms[:0]
		for _, s := range syms {
			if s.Path == fc.Path && s.StartLine > 0 {
				inFile = append(inFile, s)
			}
		}
		if fc.Status == "deleted" || fc.Status == "whole" {
			reason := fc.Reason
			if reason == "" {
				reason = "file deleted"
			}
			out = append(out, Changed{Node: file, Reason: reason})
			for _, s := range inFile {
				if isTopLevel(s, inFile) {
					out = append(out, Changed{Node: s, Reason: reason})
				}
			}
			continue
		}
		fileLevel := false
		hits := map[string]*Changed{}
		var order []string
		for _, r := range fc.Ranges {
			inner := innermost(inFile, r)
			if len(inner) == 0 {
				fileLevel = true
				continue
			}
			for _, s := range inner {
				if c, ok := hits[s.Key]; ok {
					c.Reason = mergeReason(c.Reason, r)
					continue
				}
				hits[s.Key] = &Changed{Node: s, Reason: describe(r)}
				order = append(order, s.Key)
			}
		}
		if fileLevel {
			out = append(out, Changed{Node: file, Reason: "lines outside any declaration changed"})
		}
		for _, k := range order {
			out = append(out, *hits[k])
		}
	}
	return out, unmatched, nil
}

func isTopLevel(s graph.Node, all []graph.Node) bool {
	for _, o := range all {
		if o.Key != s.Key && o.StartLine <= s.StartLine && o.EndLine >= s.EndLine &&
			(o.StartLine < s.StartLine || o.EndLine > s.EndLine) && o.Kind != "variable" {
			return false
		}
	}
	return true
}

// innermost returns the symbols overlapping r that contain no other symbol
// overlapping r.
func innermost(syms []graph.Node, r LineRange) []graph.Node {
	var over []graph.Node
	for _, s := range syms {
		end := s.EndLine
		if end < s.StartLine {
			end = s.StartLine
		}
		if int(s.StartLine) <= r.End && int(end) >= r.Start {
			over = append(over, s)
		}
	}
	var out []graph.Node
	for _, s := range over {
		contains := false
		for _, o := range over {
			if o.Key != s.Key && o.StartLine >= s.StartLine && o.EndLine <= s.EndLine &&
				(o.StartLine > s.StartLine || o.EndLine < s.EndLine) {
				contains = true
				break
			}
		}
		if !contains {
			out = append(out, s)
		}
	}
	return out
}

func describe(r LineRange) string {
	if r.Start == r.End {
		return fmt.Sprintf("line %d changed", r.Start)
	}
	return fmt.Sprintf("lines %d–%d changed", r.Start, r.End)
}

func mergeReason(prev string, r LineRange) string {
	if strings.Count(prev, ";") >= 4 {
		return prev
	}
	return prev + "; " + describe(r)
}

type visit struct {
	dist   int
	rank   int // confidence rank (0 = confirmed)
	parent int32
	kind   string
	seed   int32
}

var confidenceRank = map[string]int{graph.Confirmed: 0, graph.Probable: 1, graph.Inferred: 2}
var rankName = []string{graph.Confirmed, graph.Probable, graph.Inferred}

// Analyze follows the knowledge graph backwards from the changed nodes.
func (a *Analyzer) Analyze(ctx context.Context, changed []Changed, unmatched []string, opt Options) (*Result, error) {
	if opt.Depth <= 0 {
		opt.Depth = DefaultDepth
	}
	if opt.Depth > MaxDepth {
		opt.Depth = MaxDepth
	}
	if _, ok := confidenceRank[opt.MinConfidence]; !ok {
		opt.MinConfidence = graph.Probable
	}
	res := &Result{Changed: dedupeChanged(changed), Unmatched: unmatched, Depth: opt.Depth, MinConfidence: opt.MinConfidence,
		Affected: []Affected{}, Tests: []Affected{}, UntestedChanges: []string{}}
	if res.Unmatched == nil {
		res.Unmatched = []string{}
	}
	if len(res.Changed) > maxSeeds {
		res.Changed = res.Changed[:maxSeeds]
		res.Truncated = true
	}

	nodes := map[int32]graph.Node{}
	visits := map[int32]*visit{}
	var frontier []int32
	for _, c := range res.Changed {
		nodes[c.Node.ID] = c.Node
		visits[c.Node.ID] = &visit{dist: 0, rank: 0, parent: -1, seed: c.Node.ID}
		frontier = append(frontier, c.Node.ID)
	}
	filter := graph.EdgeFilter{Kinds: impactKinds, MinConfidence: opt.MinConfidence}
	for depth := 1; depth <= opt.Depth && len(frontier) > 0; depth++ {
		edges, sources, truncated, err := a.Graph.Incoming(ctx, a.Scope, frontier, filter, maxEdgesStep)
		if err != nil {
			return nil, err
		}
		if truncated {
			res.Truncated = true
		}
		for _, n := range sources {
			nodes[n.ID] = n
		}
		var next []int32
		for _, e := range edges {
			from := visits[e.Dst]
			if from == nil {
				continue
			}
			rank := max(from.rank, confidenceRank[e.Confidence])
			cur, seen := visits[e.Src]
			switch {
			case !seen:
				if len(visits) >= maxAffected+len(res.Changed) {
					res.Truncated = true
					continue
				}
				visits[e.Src] = &visit{dist: depth, rank: rank, parent: e.Dst, kind: e.Kind, seed: from.seed}
				next = append(next, e.Src)
			case cur.dist == depth && rank < cur.rank:
				// A stronger path of the same length.
				cur.rank, cur.parent, cur.kind, cur.seed = rank, e.Dst, e.Kind, from.seed
			}
		}
		sort.Slice(next, func(i, j int) bool { return next[i] < next[j] })
		frontier = next
	}

	// Which changed nodes each affected test reaches (through any path we
	// kept), and which files are tests.
	testFiles, err := a.testFiles(ctx, nodes)
	if err != nil {
		return nil, err
	}
	covered := map[int32]bool{}
	files := map[string]bool{}
	for id, v := range visits {
		if v.dist == 0 {
			continue
		}
		n := nodes[id]
		var via, kinds []string
		for cur := id; cur >= 0; {
			cv := visits[cur]
			via = append(via, nodes[cur].Key)
			if cv.parent < 0 {
				break
			}
			kinds = append(kinds, cv.kind)
			cur = cv.parent
		}
		af := Affected{Node: n, Distance: v.dist, Confidence: rankName[v.rank], Via: via, Kinds: kinds, Test: isTest(n, testFiles)}
		res.Affected = append(res.Affected, af)
		if af.Test {
			res.Tests = append(res.Tests, af)
			covered[v.seed] = true
		} else if n.Path != "" {
			files[n.Path] = true
		}
	}
	// A changed node inside a test file covers itself.
	for _, c := range res.Changed {
		if isTest(c.Node, testFiles) {
			covered[c.Node.ID] = true
		}
	}
	sort.Slice(res.Affected, func(i, j int) bool {
		a, b := res.Affected[i], res.Affected[j]
		if a.Distance != b.Distance {
			return a.Distance < b.Distance
		}
		if a.Confidence != b.Confidence {
			return confidenceRank[a.Confidence] < confidenceRank[b.Confidence]
		}
		return a.Node.Key < b.Node.Key
	})
	sort.Slice(res.Tests, func(i, j int) bool { return res.Tests[i].Node.Key < res.Tests[j].Node.Key })
	for _, c := range res.Changed {
		if !covered[c.Node.ID] {
			res.UntestedChanges = append(res.UntestedChanges, c.Node.Key)
		}
	}
	res.Files = len(files)
	res.Risk = score(res, visits)
	res.Limitations = []string{
		"Dependencies are found through the relationships USIE resolved. Calls through values of unknown type, reflection, dependency injection, configuration and code generation are not followed, so the affected set can be incomplete.",
		fmt.Sprintf("Relationships weaker than %q are not followed; dependents are followed to depth %d.", opt.MinConfidence, opt.Depth),
		"Tests are identified by their location (test directories and file naming conventions); a test that reaches the change only through an unresolved call is not found.",
	}
	return res, nil
}

func dedupeChanged(in []Changed) []Changed {
	seen := map[string]bool{}
	out := []Changed{}
	for _, c := range in {
		if seen[c.Node.Key] {
			continue
		}
		seen[c.Node.Key] = true
		out = append(out, c)
	}
	return out
}

// testFiles reports which of the nodes' files are tests, from the file
// nodes' category.
func (a *Analyzer) testFiles(ctx context.Context, nodes map[int32]graph.Node) (map[string]bool, error) {
	var keys []string
	seen := map[string]bool{}
	for _, n := range nodes {
		if n.Path != "" && !seen[n.Path] {
			seen[n.Path] = true
			keys = append(keys, "file:"+n.Path)
		}
	}
	out := map[string]bool{}
	for start := 0; start < len(keys); start += 1000 {
		end := min(start+1000, len(keys))
		files, err := a.Graph.NodesByKeys(ctx, a.Scope, keys[start:end])
		if err != nil {
			return nil, err
		}
		for _, f := range files {
			if f.Attrs["category"] == "test" || looksLikeTest(f.Path) {
				out[f.Path] = true
			}
		}
	}
	return out, nil
}

func isTest(n graph.Node, testFiles map[string]bool) bool {
	return n.Path != "" && testFiles[n.Path]
}

// looksLikeTest covers naming conventions the classifier may not have
// marked (it classifies by directory and file name).
func looksLikeTest(p string) bool {
	base := strings.ToLower(path.Base(p))
	return strings.HasSuffix(base, "_test.go") || strings.HasPrefix(base, "test_") ||
		strings.Contains(base, ".test.") || strings.Contains(base, ".spec.") ||
		strings.HasSuffix(strings.TrimSuffix(base, path.Ext(base)), "test") ||
		strings.HasSuffix(strings.TrimSuffix(base, path.Ext(base)), "tests")
}

// score combines named factors into a 0–100 risk score. The weights are a
// documented heuristic for ranking changes, not a probability of failure.
func score(res *Result, visits map[int32]*visit) Risk {
	log := func(n, cap float64) float64 {
		if n <= 0 {
			return 0
		}
		return math.Min(1, math.Log1p(n)/math.Log1p(cap))
	}
	nonTest := len(res.Affected) - len(res.Tests)
	weak := 0
	direct := map[int32]int{}
	for _, v := range visits {
		if v.dist == 0 {
			continue
		}
		if v.rank > 0 {
			weak++
		}
		if v.dist == 1 {
			direct[v.parent]++
		}
	}
	maxFanIn := 0
	for _, c := range direct {
		maxFanIn = max(maxFanIn, c)
	}
	exported := 0
	for _, c := range res.Changed {
		if c.Node.Exported || strings.HasPrefix(c.Node.Key, "file:") {
			exported++
		}
	}
	changedN := float64(max(len(res.Changed), 1))
	affectedN := float64(len(res.Affected))
	untested := float64(len(res.UntestedChanges)) / changedN
	uncertainty := 0.0
	if affectedN > 0 {
		uncertainty = float64(weak) / affectedN
	}
	factors := []Factor{
		{ID: "blast_radius", Label: "Dependents", Value: log(float64(max(nonTest, 0)), 200), Weight: 30,
			Explanation: fmt.Sprintf("%d non-test symbols and files depend on the change (scale saturates at 200)", max(nonTest, 0))},
		{ID: "untested", Label: "Changes no test reaches", Value: untested, Weight: 25,
			Explanation: fmt.Sprintf("%d of %d changed declarations are not reached by any test found through the graph", len(res.UntestedChanges), len(res.Changed))},
		{ID: "public_surface", Label: "Exported declarations changed", Value: float64(exported) / changedN, Weight: 15,
			Explanation: fmt.Sprintf("%d of %d changed declarations are exported or whole files", exported, len(res.Changed))},
		{ID: "fan_in", Label: "Most direct dependents of one change", Value: log(float64(maxFanIn), 50), Weight: 15,
			Explanation: fmt.Sprintf("the most-used changed declaration has %d direct dependents (scale saturates at 50)", maxFanIn)},
		{ID: "spread", Label: "Files affected", Value: log(float64(res.Files), 50), Weight: 10,
			Explanation: fmt.Sprintf("dependents are spread over %d non-test files (scale saturates at 50)", res.Files)},
		{ID: "uncertainty", Label: "Dependents found through weaker evidence", Value: uncertainty, Weight: 5,
			Explanation: fmt.Sprintf("%d of %d dependents are reached only through probable or inferred relationships", weak, len(res.Affected))},
	}
	total := 0.0
	for i := range factors {
		factors[i].Value = math.Round(factors[i].Value*1000) / 1000
		factors[i].Points = math.Round(factors[i].Value*factors[i].Weight*10) / 10
		total += factors[i].Value * factors[i].Weight
	}
	s := int(math.Round(total))
	level := "low"
	switch {
	case s >= 55:
		level = "high"
	case s >= 25:
		level = "medium"
	}
	return Risk{Score: s, Level: level, Factors: factors,
		Note: "A heuristic for ranking changes by how much depends on them and how well tests reach them. It is not a probability of failure."}
}
