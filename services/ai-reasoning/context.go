package ai

import (
	"context"
	"errors"
	"fmt"
	"regexp"
	"sort"
	"strings"

	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
)

// ContextItem is one piece of evidence sent to the model.
type ContextItem struct {
	ID        string `json:"id"`
	Kind      string `json:"kind"` // overview | readme | file_index | symbol | file
	Key       string `json:"key,omitempty"`
	Path      string `json:"path,omitempty"`
	StartLine int    `json:"start_line,omitempty"`
	EndLine   int    `json:"end_line,omitempty"`
	Title     string `json:"title"`
	Content   string `json:"content"`
	Truncated bool   `json:"truncated,omitempty"`
}

// ContextPack is the full evidence for one question.
type ContextPack struct {
	Items []ContextItem `json:"items"`
	Focus string        `json:"focus,omitempty"`
	// Terms are the search terms extracted from the question.
	Terms []string `json:"terms"`
}

func (p *ContextPack) IDs() map[string]bool {
	ids := make(map[string]bool, len(p.Items))
	for _, it := range p.Items {
		ids[it.ID] = true
	}
	return ids
}

// SourceReader returns the text of a file in the version being asked about.
// It returns ErrNoSource when content is not available (binary, oversize).
type SourceReader func(ctx context.Context, path string) (string, error)

var ErrNoSource = errors.New("ai: source not available")

// RetrievalLimits bound the evidence for one question.
type RetrievalLimits struct {
	MaxChars        int // total characters of evidence
	MaxItemChars    int
	MaxSymbols      int
	MaxExcerptLines int
}

func DefaultRetrievalLimits() RetrievalLimits {
	return RetrievalLimits{MaxChars: 120_000, MaxItemChars: 12_000, MaxSymbols: 14, MaxExcerptLines: 160}
}

type Retriever struct {
	Graph  graph.Store
	Read   SourceReader
	Limits RetrievalLimits
}

var symbolKinds = []string{"function", "method", "class", "interface", "type", "variable", "namespace"}

var relationshipKinds = []string{graph.RelCalls, graph.RelInstantiates, graph.RelExtends, graph.RelImplements, graph.RelReferences, graph.RelImports}

// Retrieve collects evidence for a question: an overview of the repository,
// the focus node and its neighbours (when given), symbols and files whose
// names match terms in the question, and, if nothing matched, the README and
// a file index.
func (r *Retriever) Retrieve(ctx context.Context, scope graph.Scope, question, focusKey string) (*ContextPack, error) {
	lim := r.Limits
	if lim.MaxChars == 0 {
		lim = DefaultRetrievalLimits()
	}
	b := &packBuilder{limits: lim, pack: &ContextPack{Items: []ContextItem{}, Focus: focusKey}, seen: map[string]bool{}}

	stats, err := r.Graph.Stats(ctx, scope)
	if err != nil {
		return nil, err
	}
	b.add(ContextItem{Kind: "overview", Title: "Repository overview", Content: overview(stats)})

	var selected []graph.Node
	if focusKey != "" {
		node, err := r.Graph.NodeByKey(ctx, scope, focusKey)
		if err != nil {
			return nil, fmt.Errorf("focus %q: %w", focusKey, err)
		}
		selected = append(selected, node)
		// The focus node's direct neighbours are the most relevant evidence.
		sub, err := r.Graph.Neighborhood(ctx, scope, focusKey, graph.NeighborhoodQuery{Depth: 1, Direction: graph.Both, Filter: graph.EdgeFilter{Kinds: append([]string{graph.RelContains}, relationshipKinds...)}, MaxNodes: 40})
		if err != nil {
			return nil, err
		}
		for _, n := range sub.Nodes[1:] {
			if isSymbol(n.Kind) && len(selected) < lim.MaxSymbols/2+1 {
				selected = append(selected, n)
			}
		}
	}

	terms := extractTerms(question)
	b.pack.Terms = terms
	matched, err := r.searchTerms(ctx, scope, terms, lim.MaxSymbols)
	if err != nil {
		return nil, err
	}
	for _, n := range matched {
		if len(selected) >= lim.MaxSymbols {
			break
		}
		if !containsKey(selected, n.Key) {
			selected = append(selected, n)
		}
	}

	for _, n := range selected {
		if b.full() {
			break
		}
		item, err := r.describe(ctx, scope, n, lim)
		if err != nil {
			return nil, err
		}
		b.add(item)
	}

	// Broad questions ("what does this project do?") match no symbol: give
	// the README and the file layout instead.
	if len(matched) == 0 && focusKey == "" {
		if err := r.addOrientation(ctx, scope, b); err != nil {
			return nil, err
		}
	}
	return b.pack, nil
}

type packBuilder struct {
	limits RetrievalLimits
	pack   *ContextPack
	used   int
	seen   map[string]bool
}

func (b *packBuilder) full() bool { return b.used >= b.limits.MaxChars }

func (b *packBuilder) add(item ContextItem) {
	if item.Key != "" {
		if b.seen[item.Key] {
			return
		}
		b.seen[item.Key] = true
	}
	room := b.limits.MaxChars - b.used
	if room <= 200 {
		return
	}
	max := b.limits.MaxItemChars
	if room < max {
		max = room
	}
	if len(item.Content) > max {
		item.Content = clip(item.Content, max)
		item.Truncated = true
	}
	item.ID = fmt.Sprintf("C%d", len(b.pack.Items)+1)
	b.used += len(item.Content)
	b.pack.Items = append(b.pack.Items, item)
}

func overview(s graph.Stats) string {
	var b strings.Builder
	fmt.Fprintf(&b, "Knowledge graph: %d nodes, %d relationships.\n", s.Nodes, s.Edges)
	b.WriteString("Nodes by kind: " + formatCounts(s.NodesByKind) + "\n")
	b.WriteString("Relationships by kind: " + formatCounts(s.EdgesByKind) + "\n")
	b.WriteString("Relationships by confidence: " + formatCounts(s.EdgesByConfidence) + "\n")
	b.WriteString("Only Python, JavaScript and TypeScript files are parsed; other files are listed but their contents are not analyzed.")
	return b.String()
}

func formatCounts(m map[string]int64) string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	parts := make([]string, 0, len(keys))
	for _, k := range keys {
		parts = append(parts, fmt.Sprintf("%s %d", k, m[k]))
	}
	return strings.Join(parts, ", ")
}

func isSymbol(kind string) bool {
	for _, k := range symbolKinds {
		if k == kind {
			return true
		}
	}
	return false
}

func containsKey(nodes []graph.Node, key string) bool {
	for _, n := range nodes {
		if n.Key == key {
			return true
		}
	}
	return false
}

var termPattern = regexp.MustCompile(`[A-Za-z_][A-Za-z0-9_]*(?:[./][A-Za-z0-9_]+)*`)

// stopwords are common English words that are never useful as code search
// terms.
var stopwords = map[string]bool{}

func init() {
	for _, w := range strings.Fields(`the and for are but not you all any can had her was one our out day get has him his how man new now old see two way who boy did its let put say she too use
		what when where which while with without would could should does doing done this that these those there their them they then than from into onto over under about above after again
		against between both each few more most other some such only own same very just also because before being below during further here once only same until why will your yours
		function functions method methods class classes file files code repository repo project module modules work works working call calls called calling used uses using defined define
		implemented implement implementation explain explains describe show tell find list happen happens does handle handled handles where whats what's please help example`) {
		stopwords[w] = true
	}
}

// extractTerms returns identifiers and paths mentioned in a question, most
// code-like first.
func extractTerms(question string) []string {
	type term struct {
		text  string
		score int
	}
	seen := map[string]bool{}
	var terms []term
	for _, m := range termPattern.FindAllString(question, -1) {
		lower := strings.ToLower(m)
		if len(m) < 3 || stopwords[lower] || seen[lower] {
			continue
		}
		seen[lower] = true
		score := 0
		if strings.ContainsAny(m, "_./") {
			score += 2
		}
		if m != lower && m != strings.ToUpper(m) { // camelCase or PascalCase
			score += 2
		}
		if strings.Contains(question, "`"+m) {
			score += 3
		}
		terms = append(terms, term{m, score})
	}
	sort.SliceStable(terms, func(i, j int) bool { return terms[i].score > terms[j].score })
	out := make([]string, 0, len(terms))
	for _, t := range terms {
		if len(out) == 12 {
			break
		}
		out = append(out, t.text)
	}
	return out
}

// searchTerms finds nodes for each term and ranks them: exact name matches
// first, then prefix matches, then substring matches; definitions of types
// and functions before variables.
func (r *Retriever) searchTerms(ctx context.Context, scope graph.Scope, terms []string, limit int) ([]graph.Node, error) {
	type scored struct {
		node  graph.Node
		score int
		order int
	}
	best := map[string]*scored{}
	order := 0
	for ti, t := range terms {
		// A dotted term like Cart.checkout matches on its last segment.
		text := t
		if i := strings.LastIndexAny(t, "."); i >= 0 && !strings.Contains(t, "/") {
			text = t[i+1:]
		}
		kinds := append([]string{"file"}, symbolKinds...)
		if strings.Contains(t, "/") {
			kinds = []string{"file"}
		}
		nodes, _, err := r.Graph.Search(ctx, scope, graph.SearchQuery{Text: text, Kinds: kinds, Limit: 8})
		if err != nil {
			return nil, err
		}
		for _, n := range nodes {
			lowerName, lowerText := strings.ToLower(n.Name), strings.ToLower(text)
			score := 1
			switch {
			case lowerName == lowerText || strings.EqualFold(n.QualifiedName, t) || strings.HasSuffix(n.Path, t):
				score = 6
			case strings.HasPrefix(lowerName, lowerText):
				score = 3
			}
			switch n.Kind {
			case "class", "interface", "function", "method":
				score += 2
			case "file":
				if score < 6 {
					score -= 1
				}
			}
			score += (len(terms) - ti) / 3 // earlier (more code-like) terms weigh more
			if prev, ok := best[n.Key]; !ok || score > prev.score {
				order++
				best[n.Key] = &scored{n, score, order}
			}
		}
	}
	all := make([]*scored, 0, len(best))
	for _, s := range best {
		if s.score >= 3 { // drop weak substring-only matches
			all = append(all, s)
		}
	}
	sort.Slice(all, func(i, j int) bool {
		if all[i].score != all[j].score {
			return all[i].score > all[j].score
		}
		return all[i].order < all[j].order
	})
	out := make([]graph.Node, 0, limit)
	for _, s := range all {
		if len(out) == limit {
			break
		}
		out = append(out, s.node)
	}
	return out, nil
}

// describe renders one node: signature, docstring, relationships with their
// confidence, and the source excerpt.
func (r *Retriever) describe(ctx context.Context, scope graph.Scope, n graph.Node, lim RetrievalLimits) (ContextItem, error) {
	item := ContextItem{Key: n.Key, Path: n.Path, Kind: "symbol", Title: n.QualifiedName}
	if n.Kind == "file" {
		item.Kind, item.Title = "file", n.Path
	}
	var b strings.Builder
	if n.Kind == "file" {
		fmt.Fprintf(&b, "File %s\n", n.Path)
	} else {
		fmt.Fprintf(&b, "%s %s in %s", n.Kind, n.QualifiedName, n.Path)
		if n.StartLine > 0 {
			fmt.Fprintf(&b, " (lines %d-%d)", n.StartLine, n.EndLine)
		}
		b.WriteString("\n")
		if n.Signature != "" {
			fmt.Fprintf(&b, "Signature: %s\n", n.Signature)
		}
		if n.Exported {
			b.WriteString("Exported: yes\n")
		}
	}

	sub, err := r.Graph.Neighborhood(ctx, scope, n.Key, graph.NeighborhoodQuery{Depth: 1, Direction: graph.Both, Filter: graph.EdgeFilter{Kinds: relationshipKinds}, MaxNodes: 30})
	if err != nil {
		return item, err
	}
	byID := make(map[int32]graph.Node, len(sub.Nodes))
	for _, x := range sub.Nodes {
		byID[x.ID] = x
	}
	var outgoing, incoming []string
	for _, e := range sub.Edges {
		if e.Src == n.ID && e.Dst != n.ID {
			outgoing = append(outgoing, fmt.Sprintf("%s %s [%s]", e.Kind, label(byID[e.Dst]), e.Confidence))
		} else if e.Dst == n.ID && e.Src != n.ID {
			incoming = append(incoming, fmt.Sprintf("%s by %s [%s]", e.Kind, label(byID[e.Src]), e.Confidence))
		}
	}
	sort.Strings(outgoing)
	sort.Strings(incoming)
	if len(outgoing) > 0 {
		b.WriteString("Outgoing relationships:\n  " + strings.Join(outgoing, "\n  ") + "\n")
	}
	if len(incoming) > 0 {
		b.WriteString("Incoming relationships:\n  " + strings.Join(incoming, "\n  ") + "\n")
	}
	if sub.Truncated {
		b.WriteString("(relationship list truncated)\n")
	}

	if n.Path != "" && r.Read != nil {
		src, err := r.Read(ctx, n.Path)
		switch {
		case errors.Is(err, ErrNoSource):
			b.WriteString("Source: not available (binary or too large).\n")
		case err != nil:
			return item, err
		default:
			start, end := n.StartLine, n.EndLine
			if n.Kind == "file" || start == 0 {
				start, end = 1, 60
			}
			excerpt, s, e, cut := excerptLines(src, int(start), int(end), lim.MaxExcerptLines)
			if excerpt != "" {
				item.StartLine, item.EndLine = s, e
				fmt.Fprintf(&b, "Source %s lines %d-%d:\n%s", n.Path, s, e, excerpt)
				if cut {
					b.WriteString("\n(excerpt shortened)")
				}
			}
		}
	}
	item.Content = b.String()
	return item, nil
}

func label(n graph.Node) string {
	switch {
	case n.Key == "":
		return "(unknown)"
	case n.QualifiedName != "" && n.Path != "":
		return fmt.Sprintf("%s (%s)", n.QualifiedName, n.Path)
	case n.Kind == "package":
		return fmt.Sprintf("package %s", n.Name)
	default:
		return n.Key
	}
}

// excerptLines returns numbered lines [start, end], capped at max lines.
func excerptLines(src string, start, end, max int) (string, int, int, bool) {
	lines := strings.Split(src, "\n")
	if start < 1 {
		start = 1
	}
	if start > len(lines) {
		return "", 0, 0, false
	}
	if end < start || end > len(lines) {
		end = len(lines)
	}
	cut := false
	if end-start+1 > max {
		end, cut = start+max-1, true
	}
	var b strings.Builder
	for i := start; i <= end; i++ {
		fmt.Fprintf(&b, "%5d | %s\n", i, strings.TrimRight(lines[i-1], "\r"))
	}
	return strings.TrimRight(b.String(), "\n"), start, end, cut
}

func (r *Retriever) addOrientation(ctx context.Context, scope graph.Scope, b *packBuilder) error {
	files, total, err := r.Graph.Search(ctx, scope, graph.SearchQuery{Kinds: []string{"file"}, Limit: 400})
	if err != nil {
		return err
	}
	sort.Slice(files, func(i, j int) bool { return files[i].Path < files[j].Path })
	for _, f := range files {
		base := strings.ToLower(f.Path)
		if (base == "readme.md" || base == "readme.rst" || base == "readme.txt" || base == "readme") && r.Read != nil {
			src, err := r.Read(ctx, f.Path)
			if errors.Is(err, ErrNoSource) {
				continue
			}
			if err != nil {
				return err
			}
			excerpt, s, e, _ := excerptLines(src, 1, 150, 150)
			b.add(ContextItem{Kind: "readme", Key: f.Key, Path: f.Path, StartLine: s, EndLine: e, Title: f.Path, Content: excerpt})
			break
		}
	}
	var idx strings.Builder
	fmt.Fprintf(&idx, "%d files in the repository", total)
	if int(total) > len(files) {
		fmt.Fprintf(&idx, " (first %d by path shown)", len(files))
	}
	idx.WriteString(":\n")
	for _, f := range files {
		lang, _ := f.Attrs["language"].(string)
		status, _ := f.Attrs["parse_status"].(string)
		fmt.Fprintf(&idx, "%s  [%s, %s]\n", f.Path, orDash(lang), orDash(status))
	}
	b.add(ContextItem{Kind: "file_index", Title: "File index", Content: idx.String()})
	return nil
}

func orDash(s string) string {
	if s == "" {
		return "-"
	}
	return s
}
