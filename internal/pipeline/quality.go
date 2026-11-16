package pipeline

import (
	"context"
	"errors"

	"github.com/jackc/pgx/v5"

	quality "github.com/ammarkarimi/usie/services/code-quality"
	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
	ingestion "github.com/ammarkarimi/usie/services/repository-ingestion"
	analysis "github.com/ammarkarimi/usie/services/semantic-analysis"
)

// RuleCatalog is the full rule catalog: the engine's rules and the text rules.
func RuleCatalog(info analysis.Info) []quality.Rule {
	engine := make([]quality.Rule, 0, len(info.Rules))
	for _, r := range info.Rules {
		engine = append(engine, quality.Rule{
			ID: r.ID, Name: r.Name, Category: r.Category, Severity: r.Severity, Languages: r.Languages,
			Description: r.Description, Remediation: r.Remediation,
		})
	}
	return quality.Catalog(engine)
}

// qualityFindings classifies every rule match of the run, adds duplicated
// blocks, and marks findings whose fingerprint the baseline does not have.
func (p *Pipeline) qualityFindings(ctx context.Context, job *orchestrator.Job, parentID *string, files []*fileState,
	dup quality.DuplicationResult, stats *Stats) ([]quality.Finding, error) {
	rules := quality.Index(RuleCatalog(p.Info))
	var findings []quality.Finding
	q := &stats.Quality
	for _, f := range files {
		raws := make([]quality.Raw, 0, len(f.ruleMatches)+len(f.textMatches))
		raws = append(raws, f.ruleMatches...)
		raws = append(raws, f.textMatches...)
		if d, ok := dup.Files[f.Path]; ok {
			raws = append(raws, d.Blocks...)
		}
		findings = append(findings, quality.Assemble(f.Path, f.Category == ingestion.CategoryTest, raws, rules)...)
		if f.Category != ingestion.CategoryTest {
			q.Functions += f.metrics.Functions
			q.Complexity += f.metrics.Complexity
			q.MaxComplexity = max(q.MaxComplexity, f.metrics.MaxComplexity)
		}
	}
	q.ComparedLines, q.DuplicatedLines, q.DuplicatedBlocks, q.DuplicationSkipped = dup.CodeLines, dup.DuplicatedLines, dup.Blocks, dup.Skipped

	baseline, err := p.baselineFingerprints(ctx, job.OrgID, parentID)
	if err != nil {
		return nil, err
	}
	if baseline != nil {
		q.BaselineVersionID = parentID
		for i := range findings {
			if !baseline[findings[i].Fingerprint] {
				findings[i].IsNew = true
				q.NewFindings++
			}
		}
	}
	q.Findings = len(findings)
	quality.SortFindings(findings)
	return findings, nil
}

// baselineFingerprints returns the finding fingerprints of the parent
// version, or nil when there is no parent analyzed with quality checks.
func (p *Pipeline) baselineFingerprints(ctx context.Context, orgID string, parentID *string) (map[string]bool, error) {
	if parentID == nil {
		return nil, nil
	}
	var checked bool
	err := p.Pool.QueryRow(ctx, `SELECT stats ? 'quality' FROM repository_versions WHERE organization_id = $1 AND id = $2`, orgID, *parentID).Scan(&checked)
	if errors.Is(err, pgx.ErrNoRows) || (err == nil && !checked) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	rows, err := p.Pool.Query(ctx, `SELECT DISTINCT fingerprint FROM findings WHERE organization_id = $1 AND version_id = $2`, orgID, *parentID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	set := map[string]bool{}
	for rows.Next() {
		var fp string
		if err := rows.Scan(&fp); err != nil {
			return nil, err
		}
		set[fp] = true
	}
	return set, rows.Err()
}
