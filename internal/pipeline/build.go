package pipeline

import (
	"context"
	"encoding/json"
	"path"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
	ingestion "github.com/ammarkarimi/usie/services/repository-ingestion"
	analysis "github.com/ammarkarimi/usie/services/semantic-analysis"
)

// clean removes NUL characters, which PostgreSQL text cannot store. Source
// files may legitimately contain them inside string literals.
func clean(s string) string {
	if strings.IndexByte(s, 0) < 0 {
		return s
	}
	return strings.ReplaceAll(s, "\x00", "")
}

func cleanValue(v any) any {
	switch t := v.(type) {
	case string:
		return clean(t)
	case []any:
		for i := range t {
			t[i] = cleanValue(t[i])
		}
	case map[string]any:
		for k, x := range t {
			t[k] = cleanValue(x)
		}
	}
	return v
}

// normalizePyPI applies PEP 503 normalization.
func normalizePyPI(name string) string {
	var b strings.Builder
	prevSep := false
	for _, r := range strings.ToLower(name) {
		if r == '-' || r == '_' || r == '.' {
			if !prevSep {
				b.WriteByte('-')
			}
			prevSep = true
			continue
		}
		b.WriteRune(r)
		prevSep = false
	}
	return b.String()
}

// buildGraph merges the file inventory, manifest facts and engine output
// into one validated snapshot. dropped counts engine relationships whose
// endpoints do not exist; it is expected to be zero and is reported if not.
func buildGraph(repoName string, files []*fileState, facts ingestion.ManifestFacts, res *analysis.Resolution) ([]graph.Node, []graph.Edge, int, error) {
	b := graph.NewBuilder()
	fileNodes := make([]graph.Node, 0, len(files))
	for _, f := range files {
		attrs := map[string]any{"category": f.Category, "size_bytes": f.Size, "parse_status": f.parseStatus}
		if f.Language != "" {
			attrs["language"] = f.Language
		}
		if f.Generated {
			attrs["generated"] = true
		}
		if f.Vendored {
			attrs["vendored"] = true
		}
		fileNodes = append(fileNodes, graph.Node{Name: path.Base(f.Path), Path: f.Path, EndLine: int32(f.Lines), Attrs: attrs})
	}
	b.AddFileTree(repoName, fileNodes)

	for _, n := range res.Nodes {
		node := graph.Node{
			Key: n.Key, Kind: n.Kind, Name: clean(n.Name), QualifiedName: clean(n.QualifiedName), Path: n.Path,
			Signature: clean(n.Signature), Doc: clean(n.Doc), Exported: n.Exported,
		}
		if n.Span != nil {
			node.StartLine, node.EndLine = n.Span.StartLine, n.Span.EndLine
		}
		if len(n.Attrs) > 0 {
			node.Attrs, _ = cleanValue(n.Attrs).(map[string]any)
		}
		b.AddNode(node)
	}
	dropped := 0
	for _, e := range res.Edges {
		edge := graph.Edge{Kind: e.Kind, Confidence: e.Confidence, Rule: e.Rule, Occurrences: e.Count}
		if e.Evidence != nil {
			edge.EvidencePath, edge.EvidenceLine = e.Evidence.Path, e.Evidence.Line
		}
		if !b.AddEdge(e.Src, e.Dst, edge) {
			dropped++
		}
	}

	// Declared dependencies: repository -> package.
	for _, d := range facts.Dependencies {
		key := "ext:" + d.Ecosystem + ":" + d.Name
		if d.Ecosystem == "pypi" {
			// The resolver names Python packages by import name. Link to that
			// node only when the names agree after normalization; otherwise
			// the distribution gets its own node rather than a guessed match.
			importKey := "ext:python:" + strings.ReplaceAll(normalizePyPI(d.Name), "-", "_")
			if b.Has(importKey) {
				key = importKey
			} else {
				key = "ext:pypi:" + normalizePyPI(d.Name)
			}
		}
		if !b.Has(key) {
			b.AddNode(graph.Node{Key: key, Kind: graph.KindPackage, Name: d.Name, Attrs: map[string]any{"ecosystem": d.Ecosystem, "declared": true, "imported": false}})
		}
		b.AddEdge("repo", key, graph.Edge{Kind: graph.RelDependsOn, Confidence: graph.Confirmed, Rule: "manifest.declared-dependency", EvidencePath: d.ManifestPath})
	}
	nodes, edges, err := b.Build()
	return nodes, edges, dropped, err
}

// persist writes every result of the run and marks the version ready, in
// one transaction: readers see either the previous state or the complete
// new one.
func (p *Pipeline) persist(ctx context.Context, tx pgx.Tx, job *orchestrator.Job, files []*fileState, facts ingestion.ManifestFacts,
	res *analysis.Resolution, nodes []graph.Node, edges []graph.Edge, git *ingestion.GitMetadata, commitSHA, branch, treeHash string,
	parentID *string, stats Stats) error {
	org, version := job.OrgID, job.VersionID
	for _, table := range []string{"source_files", "dependencies", "symbol_references"} {
		if _, err := tx.Exec(ctx, `DELETE FROM `+table+` WHERE organization_id = $1 AND version_id = $2`, org, version); err != nil {
			return err
		}
	}
	now := time.Now()
	_, err := tx.CopyFrom(ctx, pgx.Identifier{"source_files"},
		[]string{"organization_id", "version_id", "path", "content_hash", "size_bytes", "language", "category", "encoding", "is_generated", "is_vendored",
			"content_stored", "parse_status", "parse_detail", "change", "line_count", "symbol_count", "diagnostic_count", "analyzed_at"},
		pgx.CopyFromSlice(len(files), func(i int) ([]any, error) {
			f := files[i]
			return []any{org, version, f.Path, f.ContentHash, f.Size, f.Language, f.Category, f.Encoding, f.Generated, f.Vendored,
				f.stored, f.parseStatus, f.parseDetail, f.change, int32(f.Lines), int32(f.symbols), int32(f.diagnostics), now}, nil
		}))
	if err != nil {
		return err
	}
	_, err = tx.CopyFrom(ctx, pgx.Identifier{"dependencies"},
		[]string{"organization_id", "version_id", "manifest_path", "ecosystem", "name", "requirement", "scope"},
		pgx.CopyFromSlice(len(facts.Dependencies), func(i int) ([]any, error) {
			d := facts.Dependencies[i]
			return []any{org, version, d.ManifestPath, d.Ecosystem, clean(d.Name), clean(d.Requirement), d.Scope}, nil
		}))
	if err != nil {
		return err
	}
	_, err = tx.CopyFrom(ctx, pgx.Identifier{"symbol_references"},
		[]string{"organization_id", "version_id", "seq", "path", "line", "col", "kind", "text", "source_key", "outcome", "confidence", "target_key", "rule", "reason"},
		pgx.CopyFromSlice(len(res.References), func(i int) ([]any, error) {
			r := res.References[i]
			return []any{org, version, int32(i), r.Path, r.Line, r.Col, r.Kind, clean(r.Text), clean(r.Source), r.Outcome, r.Confidence, clean(r.Target), r.Rule, clean(r.Reason)}, nil
		}))
	if err != nil {
		return err
	}
	if err := graph.WriteSnapshot(ctx, tx, graph.Scope{OrganizationID: org, VersionID: version}, nodes, edges); err != nil {
		return err
	}

	if git != nil {
		batch := &pgx.Batch{}
		for _, c := range git.Commits {
			parents := c.Parents
			if parents == nil {
				parents = []string{}
			}
			batch.Queue(`INSERT INTO commits (organization_id, repository_id, sha, parents, author_name, author_email, authored_at, committed_at, subject)
				VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (repository_id, sha) DO NOTHING`,
				org, job.RepositoryID, c.SHA, parents, clean(c.AuthorName), clean(c.AuthorEmail), c.AuthoredAt, c.CommittedAt, clean(c.Subject))
		}
		if len(git.Branches) > 0 {
			batch.Queue(`DELETE FROM branches WHERE organization_id = $1 AND repository_id = $2`, org, job.RepositoryID)
			for _, br := range git.Branches {
				batch.Queue(`INSERT INTO branches (organization_id, repository_id, name, head_sha, is_default) VALUES ($1, $2, $3, $4, $5)
					ON CONFLICT (repository_id, name) DO UPDATE SET head_sha = EXCLUDED.head_sha, is_default = EXCLUDED.is_default, updated_at = now()`,
					org, job.RepositoryID, clean(br.Name), br.HeadSHA, br.Default)
			}
		}
		if batch.Len() > 0 {
			if err := tx.SendBatch(ctx, batch).Close(); err != nil {
				return err
			}
		}
	}

	statsJSON, err := json.Marshal(stats)
	if err != nil {
		return err
	}
	// Only a version still being analyzed may become ready: a cancelled or
	// superseded run must not publish results.
	tag, err := tx.Exec(ctx, `UPDATE repository_versions SET status = 'ready', commit_sha = $3, branch = $4, tree_hash = $5,
		parent_version_id = $6, stats = $7, error = '', analyzed_at = now()
		WHERE organization_id = $1 AND id = $2 AND status = 'analyzing'`, org, version, commitSHA, branch, treeHash, parentID, statsJSON)
	if err != nil {
		return err
	}
	if tag.RowsAffected() != 1 {
		return orchestrator.Permanent(context.Canceled)
	}
	return nil
}
