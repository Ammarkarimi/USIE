package graph

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

// Postgres is the system-of-record Store.
type Postgres struct{ pool *pgxpool.Pool }

func NewPostgres(pool *pgxpool.Pool) *Postgres { return &Postgres{pool: pool} }

const nodeColumns = `id, key, kind, name, qualified_name, path, start_line, end_line, signature, doc, exported, attrs`

func scanNode(row pgx.Row) (Node, error) {
	var n Node
	var attrs []byte
	if err := row.Scan(&n.ID, &n.Key, &n.Kind, &n.Name, &n.QualifiedName, &n.Path, &n.StartLine, &n.EndLine, &n.Signature, &n.Doc, &n.Exported, &attrs); err != nil {
		return n, err
	}
	if len(attrs) > 2 { // anything but "{}"
		if err := json.Unmarshal(attrs, &n.Attrs); err != nil {
			return n, fmt.Errorf("graph: node %s has invalid attrs: %w", n.Key, err)
		}
	}
	return n, nil
}

func collectNodes(rows pgx.Rows) ([]Node, error) {
	defer rows.Close()
	var out []Node
	for rows.Next() {
		n, err := scanNode(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, n)
	}
	return out, rows.Err()
}

func collectEdges(rows pgx.Rows) ([]Edge, error) {
	defer rows.Close()
	var out []Edge
	for rows.Next() {
		var e Edge
		if err := rows.Scan(&e.Src, &e.Dst, &e.Kind, &e.Confidence, &e.Rule, &e.EvidencePath, &e.EvidenceLine, &e.Occurrences); err != nil {
			return nil, err
		}
		out = append(out, e)
	}
	return out, rows.Err()
}

func (p *Postgres) ReplaceSnapshot(ctx context.Context, s Scope, nodes []Node, edges []Edge) error {
	return pgx.BeginFunc(ctx, p.pool, func(tx pgx.Tx) error { return WriteSnapshot(ctx, tx, s, nodes, edges) })
}

// WriteSnapshot replaces a snapshot inside the caller's transaction, so a
// version and its graph become visible atomically.
func WriteSnapshot(ctx context.Context, tx pgx.Tx, s Scope, nodes []Node, edges []Edge) error {
	if err := Validate(nodes, edges); err != nil {
		return err
	}
	if _, err := tx.Exec(ctx, `DELETE FROM graph_nodes WHERE organization_id = $1 AND version_id = $2`, s.OrganizationID, s.VersionID); err != nil {
		return err
	}
	_, err := tx.CopyFrom(ctx, pgx.Identifier{"graph_nodes"},
		[]string{"organization_id", "version_id", "id", "key", "kind", "name", "qualified_name", "path", "start_line", "end_line", "signature", "doc", "exported", "attrs"},
		pgx.CopyFromSlice(len(nodes), func(i int) ([]any, error) {
			n := nodes[i]
			attrs := []byte("{}")
			if len(n.Attrs) > 0 {
				var err error
				if attrs, err = json.Marshal(n.Attrs); err != nil {
					return nil, err
				}
			}
			return []any{s.OrganizationID, s.VersionID, n.ID, n.Key, n.Kind, n.Name, n.QualifiedName, n.Path, n.StartLine, n.EndLine, n.Signature, n.Doc, n.Exported, attrs}, nil
		}))
	if err != nil {
		return fmt.Errorf("graph: write nodes: %w", err)
	}
	_, err = tx.CopyFrom(ctx, pgx.Identifier{"graph_relationships"},
		[]string{"organization_id", "version_id", "src", "dst", "kind", "confidence", "rule", "evidence_path", "evidence_line", "occurrences"},
		pgx.CopyFromSlice(len(edges), func(i int) ([]any, error) {
			e := edges[i]
			return []any{s.OrganizationID, s.VersionID, e.Src, e.Dst, e.Kind, e.Confidence, e.Rule, e.EvidencePath, e.EvidenceLine, e.Occurrences}, nil
		}))
	if err != nil {
		return fmt.Errorf("graph: write relationships: %w", err)
	}
	return nil
}

func (p *Postgres) exists(ctx context.Context, s Scope) error {
	var ok bool
	err := p.pool.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM graph_nodes WHERE organization_id = $1 AND version_id = $2)`, s.OrganizationID, s.VersionID).Scan(&ok)
	if err != nil {
		return err
	}
	if !ok {
		return ErrNotFound
	}
	return nil
}

func (p *Postgres) Stats(ctx context.Context, s Scope) (Stats, error) {
	st := Stats{NodesByKind: map[string]int64{}, EdgesByKind: map[string]int64{}, EdgesByConfidence: map[string]int64{}}
	if err := p.exists(ctx, s); err != nil {
		return st, err
	}
	group := func(sql string, into map[string]int64, total *int64) error {
		rows, err := p.pool.Query(ctx, sql, s.OrganizationID, s.VersionID)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var k string
			var n int64
			if err := rows.Scan(&k, &n); err != nil {
				return err
			}
			into[k] = n
			if total != nil {
				*total += n
			}
		}
		return rows.Err()
	}
	if err := group(`SELECT kind, count(*) FROM graph_nodes WHERE organization_id = $1 AND version_id = $2 GROUP BY kind`, st.NodesByKind, &st.Nodes); err != nil {
		return st, err
	}
	if err := group(`SELECT kind, count(*) FROM graph_relationships WHERE organization_id = $1 AND version_id = $2 GROUP BY kind`, st.EdgesByKind, &st.Edges); err != nil {
		return st, err
	}
	err := group(`SELECT confidence, count(*) FROM graph_relationships WHERE organization_id = $1 AND version_id = $2 GROUP BY confidence`, st.EdgesByConfidence, nil)
	return st, err
}

func (p *Postgres) NodeByKey(ctx context.Context, s Scope, key string) (Node, error) {
	n, err := scanNode(p.pool.QueryRow(ctx, `SELECT `+nodeColumns+` FROM graph_nodes WHERE organization_id = $1 AND version_id = $2 AND key = $3`, s.OrganizationID, s.VersionID, key))
	if errors.Is(err, pgx.ErrNoRows) {
		return n, ErrNotFound
	}
	return n, err
}

func (p *Postgres) Search(ctx context.Context, s Scope, q SearchQuery) ([]Node, int64, error) {
	if err := p.exists(ctx, s); err != nil {
		return nil, 0, err
	}
	// strpos/starts_with take the text literally: no LIKE pattern escaping.
	const where = ` FROM graph_nodes WHERE organization_id = $1 AND version_id = $2
		AND (cardinality($3::text[]) = 0 OR kind = ANY($3))
		AND ($4 = '' OR starts_with(path, $4))
		AND ($5 = '' OR strpos(lower(name), lower($5)) > 0 OR strpos(lower(qualified_name), lower($5)) > 0)`
	kinds := q.Kinds
	if kinds == nil {
		kinds = []string{}
	}
	var total int64
	if err := p.pool.QueryRow(ctx, `SELECT count(*)`+where, s.OrganizationID, s.VersionID, kinds, q.PathPrefix, q.Text).Scan(&total); err != nil {
		return nil, 0, err
	}
	limit := q.Limit
	if limit <= 0 {
		limit = 1 << 30
	}
	rows, err := p.pool.Query(ctx, `SELECT `+nodeColumns+where+`
		ORDER BY CASE WHEN $5 <> '' AND lower(name) = lower($5) THEN 0
		              WHEN $5 <> '' AND starts_with(lower(name), lower($5)) THEN 1 ELSE 2 END,
		         lower(name) COLLATE "C", path COLLATE "C", start_line, key COLLATE "C"
		LIMIT $6 OFFSET $7`, s.OrganizationID, s.VersionID, kinds, q.PathPrefix, q.Text, limit, q.Offset)
	if err != nil {
		return nil, 0, err
	}
	nodes, err := collectNodes(rows)
	return nodes, total, err
}

func (p *Postgres) Neighborhood(ctx context.Context, s Scope, key string, q NeighborhoodQuery) (Subgraph, error) {
	start, err := p.NodeByKey(ctx, s, key)
	if err != nil {
		return Subgraph{}, err
	}
	q = q.Normalize()
	kinds := q.Filter.Kinds
	if kinds == nil {
		kinds = []string{}
	}
	confidences := ConfidencesAtLeast(q.Filter.MinConfidence)

	visited := map[int32]bool{start.ID: true}
	order := []int32{start.ID}
	frontier := []int32{start.ID}
	truncated := false
	// One bounded query per level: the work done is proportional to the
	// frontier, and the node cap is enforced between levels.
	for depth := 0; depth < q.Depth && len(frontier) > 0 && !truncated; depth++ {
		rows, err := p.pool.Query(ctx, `
			SELECT DISTINCT n FROM (
				SELECT dst AS n FROM graph_relationships
				 WHERE $6 AND organization_id = $1 AND version_id = $2 AND src = ANY($3)
				   AND (cardinality($4::text[]) = 0 OR kind = ANY($4)) AND confidence = ANY($5)
				UNION ALL
				SELECT src AS n FROM graph_relationships
				 WHERE $7 AND organization_id = $1 AND version_id = $2 AND dst = ANY($3)
				   AND (cardinality($4::text[]) = 0 OR kind = ANY($4)) AND confidence = ANY($5)
			) c ORDER BY n`,
			s.OrganizationID, s.VersionID, frontier, kinds, confidences, q.Direction != In, q.Direction != Out)
		if err != nil {
			return Subgraph{}, err
		}
		candidates, err := pgx.CollectRows(rows, pgx.RowTo[int32])
		if err != nil {
			return Subgraph{}, err
		}
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

	rows, err := p.pool.Query(ctx, `SELECT `+nodeColumns+` FROM graph_nodes WHERE organization_id = $1 AND version_id = $2 AND id = ANY($3)`, s.OrganizationID, s.VersionID, order)
	if err != nil {
		return Subgraph{}, err
	}
	fetched, err := collectNodes(rows)
	if err != nil {
		return Subgraph{}, err
	}
	byID := make(map[int32]Node, len(fetched))
	for _, n := range fetched {
		byID[n.ID] = n
	}
	sub := Subgraph{Truncated: truncated}
	for _, id := range order {
		sub.Nodes = append(sub.Nodes, byID[id])
	}
	rows, err = p.pool.Query(ctx, `
		SELECT src, dst, kind, confidence, rule, evidence_path, evidence_line, occurrences FROM graph_relationships
		 WHERE organization_id = $1 AND version_id = $2 AND src = ANY($3) AND dst = ANY($3)
		   AND (cardinality($4::text[]) = 0 OR kind = ANY($4)) AND confidence = ANY($5)
		 ORDER BY src, dst, kind COLLATE "C"`, s.OrganizationID, s.VersionID, order, kinds, confidences)
	if err != nil {
		return Subgraph{}, err
	}
	sub.Edges, err = collectEdges(rows)
	return sub, err
}

func (p *Postgres) Subgraph(ctx context.Context, s Scope, nodeKinds []string, f EdgeFilter, maxEdges int) (Subgraph, error) {
	if err := p.exists(ctx, s); err != nil {
		return Subgraph{}, err
	}
	kinds, nk := f.Kinds, nodeKinds
	if kinds == nil {
		kinds = []string{}
	}
	if nk == nil {
		nk = []string{}
	}
	limit := maxEdges
	if limit <= 0 {
		limit = 1 << 30
	}
	rows, err := p.pool.Query(ctx, `
		SELECT e.src, e.dst, e.kind, e.confidence, e.rule, e.evidence_path, e.evidence_line, e.occurrences
		  FROM graph_relationships e
		  JOIN graph_nodes a ON a.version_id = e.version_id AND a.id = e.src
		  JOIN graph_nodes b ON b.version_id = e.version_id AND b.id = e.dst
		 WHERE e.organization_id = $1 AND e.version_id = $2
		   AND (cardinality($3::text[]) = 0 OR e.kind = ANY($3)) AND e.confidence = ANY($4)
		   AND (cardinality($5::text[]) = 0 OR (a.kind = ANY($5) AND b.kind = ANY($5)))
		 ORDER BY e.src, e.dst, e.kind COLLATE "C" LIMIT $6`,
		s.OrganizationID, s.VersionID, kinds, ConfidencesAtLeast(f.MinConfidence), nk, int64(limit)+1)
	if err != nil {
		return Subgraph{}, err
	}
	edges, err := collectEdges(rows)
	if err != nil {
		return Subgraph{}, err
	}
	sub := Subgraph{}
	if maxEdges > 0 && len(edges) > maxEdges {
		edges, sub.Truncated = edges[:maxEdges], true
	}
	sub.Edges = edges
	ids := make([]int32, 0, len(edges)*2)
	seen := map[int32]bool{}
	for _, e := range edges {
		for _, id := range []int32{e.Src, e.Dst} {
			if !seen[id] {
				seen[id] = true
				ids = append(ids, id)
			}
		}
	}
	nrows, err := p.pool.Query(ctx, `SELECT `+nodeColumns+` FROM graph_nodes WHERE organization_id = $1 AND version_id = $2 AND id = ANY($3) ORDER BY id`, s.OrganizationID, s.VersionID, ids)
	if err != nil {
		return Subgraph{}, err
	}
	sub.Nodes, err = collectNodes(nrows)
	return sub, err
}

func (p *Postgres) Diff(ctx context.Context, base, head Scope, sampleLimit int) (Diff, error) {
	var d Diff
	for _, s := range []Scope{base, head} {
		if err := p.exists(ctx, s); err != nil {
			return d, err
		}
	}
	args := []any{base.OrganizationID, base.VersionID, head.OrganizationID, head.VersionID}
	const prefixed = `h.id, h.key, h.kind, h.name, h.qualified_name, h.path, h.start_line, h.end_line, h.signature, h.doc, h.exported, h.attrs`
	const (
		onlyIn = ` FROM graph_nodes h WHERE h.organization_id = $3 AND h.version_id = $4 AND NOT EXISTS (
			SELECT 1 FROM graph_nodes b WHERE b.organization_id = $1 AND b.version_id = $2 AND b.key = h.key)`
		changed = ` FROM graph_nodes h JOIN graph_nodes b ON b.key = h.key
			WHERE h.organization_id = $3 AND h.version_id = $4 AND b.organization_id = $1 AND b.version_id = $2
			  AND (b.kind <> h.kind OR b.signature <> h.signature OR b.exported <> h.exported)`
	)
	nodeSet := func(from string, a []any, count *int64, list *[]Node) error {
		if err := p.pool.QueryRow(ctx, `SELECT count(*)`+from, a...).Scan(count); err != nil {
			return err
		}
		rows, err := p.pool.Query(ctx, `SELECT `+prefixed+from+` ORDER BY h.key COLLATE "C" LIMIT `+fmt.Sprint(sampleLimit), a...)
		if err != nil {
			return err
		}
		*list, err = collectNodes(rows)
		if *count > int64(len(*list)) {
			d.Truncated = true
		}
		return err
	}
	if err := nodeSet(onlyIn, args, &d.NodesAdded, &d.Added); err != nil {
		return d, err
	}
	swapped := []any{head.OrganizationID, head.VersionID, base.OrganizationID, base.VersionID}
	if err := nodeSet(onlyIn, swapped, &d.NodesRemoved, &d.Removed); err != nil {
		return d, err
	}
	if err := nodeSet(changed, args, &d.NodesChanged, &d.Changed); err != nil {
		return d, err
	}

	// Relationships compared by (source key, target key, kind).
	const edgeDiff = `
		WITH present AS (
			SELECT s.key AS src, t.key AS dst, e.kind, e.confidence
			  FROM graph_relationships e
			  JOIN graph_nodes s ON s.version_id = e.version_id AND s.id = e.src
			  JOIN graph_nodes t ON t.version_id = e.version_id AND t.id = e.dst
			 WHERE e.organization_id = $3 AND e.version_id = $4
		), other AS (
			SELECT s.key AS src, t.key AS dst, e.kind
			  FROM graph_relationships e
			  JOIN graph_nodes s ON s.version_id = e.version_id AND s.id = e.src
			  JOIN graph_nodes t ON t.version_id = e.version_id AND t.id = e.dst
			 WHERE e.organization_id = $1 AND e.version_id = $2
		), missing AS (
			SELECT p.* FROM present p WHERE NOT EXISTS (
				SELECT 1 FROM other o WHERE o.src = p.src AND o.dst = p.dst AND o.kind = p.kind)
		)
		SELECT src, dst, kind, confidence, count(*) OVER () FROM missing
		 ORDER BY src COLLATE "C", dst COLLATE "C", kind COLLATE "C" LIMIT `
	edgeSet := func(a []any, count *int64, list *[]EdgeKeys) error {
		rows, err := p.pool.Query(ctx, edgeDiff+fmt.Sprint(sampleLimit), a...)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var e EdgeKeys
			if err := rows.Scan(&e.Src, &e.Dst, &e.Kind, &e.Confidence, count); err != nil {
				return err
			}
			*list = append(*list, e)
		}
		if *count > int64(len(*list)) {
			d.Truncated = true
		}
		return rows.Err()
	}
	if sampleLimit < 1 {
		return d, fmt.Errorf("graph: sampleLimit must be positive")
	}
	if err := edgeSet(args, &d.EdgesAdded, &d.AddedEdges); err != nil {
		return d, err
	}
	err := edgeSet(swapped, &d.EdgesRemoved, &d.RemovedEdges)
	return d, err
}
