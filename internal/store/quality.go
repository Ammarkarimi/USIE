package store

import (
	"context"
	"encoding/json"
	"strconv"
	"strings"
	"time"
)

// FindingRow is a stored finding with its review, if any.
type FindingRow struct {
	Seq         int     `json:"seq"`
	Rule        string  `json:"rule"`
	Category    string  `json:"category"`
	Severity    string  `json:"severity"`
	Path        string  `json:"path"`
	StartLine   int     `json:"start_line"`
	StartCol    int     `json:"start_col"`
	EndLine     int     `json:"end_line"`
	EndCol      int     `json:"end_col"`
	Message     string  `json:"message"`
	Function    string  `json:"function"`
	Fingerprint string  `json:"fingerprint"`
	IsNew       bool    `json:"is_new"`
	InTest      bool    `json:"in_test"`
	Status      string  `json:"status"`
	Review      *Review `json:"review"`
}

// Review is a person's decision about a finding.
type Review struct {
	Status     string    `json:"status"`
	Comment    string    `json:"comment"`
	ReviewedBy string    `json:"reviewed_by"`
	ReviewedAt time.Time `json:"reviewed_at"`
}

// FindingFilter narrows a finding list. Empty fields do not filter.
type FindingFilter struct {
	Category string
	Severity string
	Rule     string
	// open, reviewed, accepted, false_positive, safe or all (default open).
	Status  string
	NewOnly bool
	// Exact file path, or a directory prefix ending in "/".
	Path   string
	Limit  int
	Offset int
}

// FindingCount is one row of a grouped count.
type FindingCount struct {
	Category string
	Severity string
	Rule     string
	IsNew    bool
	Status   string
	Count    int
}

const reviewJoin = `findings f LEFT JOIN finding_reviews r ON r.organization_id = f.organization_id
	AND r.repository_id = f.repository_id AND r.fingerprint = f.fingerprint`

// FindingCounts groups the findings of a version by category, severity,
// rule, new-ness and review status ("open" when unreviewed).
func (s *Store) FindingCounts(ctx context.Context, orgID, versionID string) ([]FindingCount, error) {
	rows, err := s.Pool.Query(ctx, `SELECT f.category, f.severity, f.rule, f.is_new, coalesce(r.status, 'open'), count(*)
		FROM `+reviewJoin+` WHERE f.organization_id = $1 AND f.version_id = $2 GROUP BY 1, 2, 3, 4, 5`, orgID, versionID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []FindingCount
	for rows.Next() {
		var c FindingCount
		if err := rows.Scan(&c.Category, &c.Severity, &c.Rule, &c.IsNew, &c.Status, &c.Count); err != nil {
			return nil, err
		}
		out = append(out, c)
	}
	return out, rows.Err()
}

// ListFindings returns one page of a version's findings and the total.
func (s *Store) ListFindings(ctx context.Context, orgID, versionID string, f FindingFilter) ([]FindingRow, int, error) {
	where := []string{"f.organization_id = $1", "f.version_id = $2"}
	args := []any{orgID, versionID}
	add := func(cond string, v any) {
		args = append(args, v)
		where = append(where, strings.ReplaceAll(cond, "?", "$"+strconv.Itoa(len(args))))
	}
	if f.Category != "" {
		add("f.category = ?", f.Category)
	}
	if f.Severity != "" {
		add("f.severity = ?", f.Severity)
	}
	if f.Rule != "" {
		add("f.rule = ?", f.Rule)
	}
	if f.NewOnly {
		where = append(where, "f.is_new")
	}
	if f.Path != "" {
		if strings.HasSuffix(f.Path, "/") {
			add("starts_with(f.path, ?)", f.Path)
		} else {
			add("f.path = ?", f.Path)
		}
	}
	switch f.Status {
	case "", "open":
		where = append(where, "r.status IS NULL")
	case "reviewed":
		where = append(where, "r.status IS NOT NULL")
	case "all":
	default:
		add("r.status = ?", f.Status)
	}
	cond := strings.Join(where, " AND ")
	var total int
	if err := s.Pool.QueryRow(ctx, `SELECT count(*) FROM `+reviewJoin+` WHERE `+cond, args...).Scan(&total); err != nil {
		return nil, 0, err
	}
	args = append(args, f.Limit, f.Offset)
	rows, err := s.Pool.Query(ctx, `SELECT f.seq, f.rule, f.category, f.severity, f.path, f.start_line, f.start_col, f.end_line, f.end_col,
			f.message, f.function_name, f.fingerprint, f.is_new, f.in_test, r.status, coalesce(r.comment, ''), coalesce(u.display_name, ''), r.reviewed_at
		FROM `+reviewJoin+` LEFT JOIN users u ON u.id = r.reviewed_by
		WHERE `+cond+` ORDER BY f.seq LIMIT $`+strconv.Itoa(len(args)-1)+` OFFSET $`+strconv.Itoa(len(args)), args...)
	if err != nil {
		return nil, 0, err
	}
	defer rows.Close()
	out := []FindingRow{}
	for rows.Next() {
		var x FindingRow
		var status *string
		var comment, by string
		var at *time.Time
		if err := rows.Scan(&x.Seq, &x.Rule, &x.Category, &x.Severity, &x.Path, &x.StartLine, &x.StartCol, &x.EndLine, &x.EndCol,
			&x.Message, &x.Function, &x.Fingerprint, &x.IsNew, &x.InTest, &status, &comment, &by, &at); err != nil {
			return nil, 0, err
		}
		x.Status = "open"
		if status != nil {
			x.Status = *status
			x.Review = &Review{Status: *status, Comment: comment, ReviewedBy: by}
			if at != nil {
				x.Review.ReviewedAt = *at
			}
		}
		out = append(out, x)
	}
	return out, total, rows.Err()
}

// FindingCategory returns the category of a fingerprint in a repository,
// or ErrNotFound when no analyzed version has it.
func (s *Store) FindingCategory(ctx context.Context, orgID, repoID, fingerprint string) (string, error) {
	var category string
	err := s.Pool.QueryRow(ctx, `SELECT category FROM findings WHERE organization_id = $1 AND repository_id = $2 AND fingerprint = $3 LIMIT 1`,
		orgID, repoID, fingerprint).Scan(&category)
	return category, mapErr(err)
}

// ReviewFinding records or replaces the review of a fingerprint.
func (s *Store) ReviewFinding(ctx context.Context, orgID, repoID, fingerprint, status, comment, userID string) error {
	_, err := s.Pool.Exec(ctx, `INSERT INTO finding_reviews (organization_id, repository_id, fingerprint, status, comment, reviewed_by)
		VALUES ($1, $2, $3, $4, $5, $6)
		ON CONFLICT (repository_id, fingerprint) DO UPDATE
		SET status = EXCLUDED.status, comment = EXCLUDED.comment, reviewed_by = EXCLUDED.reviewed_by, reviewed_at = now()`,
		orgID, repoID, fingerprint, status, comment, userID)
	return mapErr(err)
}

// ReopenFinding deletes a review. It reports whether one existed.
func (s *Store) ReopenFinding(ctx context.Context, orgID, repoID, fingerprint string) (bool, error) {
	tag, err := s.Pool.Exec(ctx, `DELETE FROM finding_reviews WHERE organization_id = $1 AND repository_id = $2 AND fingerprint = $3`,
		orgID, repoID, fingerprint)
	return tag.RowsAffected() == 1, err
}

// QualityGate returns a repository's stored gate conditions (raw JSON), or
// nil when it uses the default gate.
func (s *Store) QualityGate(ctx context.Context, orgID, repoID string) (json.RawMessage, error) {
	var gate []byte
	err := s.Pool.QueryRow(ctx, `SELECT quality_gate FROM repositories WHERE organization_id = $1 AND id = $2`, orgID, repoID).Scan(&gate)
	if err != nil {
		return nil, mapErr(err)
	}
	return gate, nil
}

// SetQualityGate stores gate conditions; nil restores the default gate.
func (s *Store) SetQualityGate(ctx context.Context, orgID, repoID string, gate json.RawMessage) error {
	var v any
	if gate != nil {
		v = string(gate)
	}
	tag, err := s.Pool.Exec(ctx, `UPDATE repositories SET quality_gate = $3 WHERE organization_id = $1 AND id = $2`, orgID, repoID, v)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return nil
}
