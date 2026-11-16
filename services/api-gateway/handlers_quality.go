package api

import (
	"encoding/json"
	"math"
	"net/http"
	"regexp"
	"strings"

	"github.com/ammarkarimi/usie/internal/pipeline"
	"github.com/ammarkarimi/usie/internal/store"
	quality "github.com/ammarkarimi/usie/services/code-quality"
	analysis "github.com/ammarkarimi/usie/services/semantic-analysis"
)

var fingerprintPattern = regexp.MustCompile(`^[0-9a-f]{32}$`)

// catalog is the rule catalog this API process knows. Without the engine
// binary only the text rules are described; stored findings keep their
// category and severity either way.
func (s *Server) catalog() []quality.Rule {
	info := analysis.Info{}
	if s.Engine != nil {
		info = *s.Engine
	}
	return pipeline.RuleCatalog(info)
}

func (s *Server) listRules(w http.ResponseWriter, r *http.Request, _ *store.Principal) error {
	writeJSON(w, http.StatusOK, map[string]any{
		"rules":                  s.catalog(),
		"metrics":                quality.Metrics,
		"engine_rules_available": s.Engine != nil,
	})
	return nil
}

// qualityStats is the part of a version's stats written by the quality step.
type qualityStats struct {
	Functions          int     `json:"functions"`
	Complexity         int     `json:"complexity"`
	MaxComplexity      int     `json:"max_complexity"`
	ComparedLines      int     `json:"duplication_compared_lines"`
	DuplicatedLines    int     `json:"duplicated_lines"`
	DuplicatedBlocks   int     `json:"duplicated_blocks"`
	DuplicationSkipped bool    `json:"duplication_skipped"`
	BaselineVersionID  *string `json:"baseline_version_id"`
}

type categorySummary struct {
	Open       int            `json:"open"`
	New        int            `json:"new"`
	Reviewed   int            `json:"reviewed"`
	BySeverity map[string]int `json:"by_severity"`
}

type ruleCount struct {
	Rule     string `json:"rule"`
	Name     string `json:"name"`
	Category string `json:"category"`
	Severity string `json:"severity"`
	Count    int    `json:"count"`
}

func (s *Server) gateFor(r *http.Request, orgID, repoID string) ([]quality.Condition, bool, error) {
	raw, err := s.Store.QualityGate(r.Context(), orgID, repoID)
	if err != nil {
		return nil, false, err
	}
	if raw == nil {
		return quality.DefaultGate(), true, nil
	}
	var conds []quality.Condition
	if err := json.Unmarshal(raw, &conds); err != nil {
		return nil, false, err
	}
	return conds, false, nil
}

func (s *Server) versionQuality(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	v, _, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	var stats struct {
		Quality *qualityStats `json:"quality"`
	}
	if len(v.Stats) > 0 {
		if err := json.Unmarshal(v.Stats, &stats); err != nil {
			return err
		}
	}
	if stats.Quality == nil {
		writeJSON(w, http.StatusOK, map[string]any{
			"version_id": v.ID, "available": false,
			"reason": "This version was analyzed before code quality checks existed. Analyze the repository again to check it.",
		})
		return nil
	}
	q := stats.Quality
	counts, err := s.Store.FindingCounts(r.Context(), p.OrgID, v.ID)
	if err != nil {
		return err
	}
	rules := quality.Index(s.catalog())
	cats := map[string]*categorySummary{}
	for _, c := range quality.Categories {
		cats[c] = &categorySummary{BySeverity: map[string]int{}}
	}
	measures := map[string]float64{}
	byRule := map[string]int{}
	for _, c := range counts {
		cs := cats[c.Category]
		if cs == nil {
			continue
		}
		if c.Status != "open" {
			cs.Reviewed += c.Count
			continue
		}
		cs.Open += c.Count
		cs.BySeverity[c.Severity] += c.Count
		byRule[c.Rule] += c.Count
		if c.IsNew {
			cs.New += c.Count
		}
		switch c.Severity {
		case "blocker":
			measures["blocker_findings"] += float64(c.Count)
		case "critical":
			measures["critical_findings"] += float64(c.Count)
		}
	}
	plural := map[string]string{"bug": "bugs", "vulnerability": "vulnerabilities", "hotspot": "hotspots", "smell": "smells"}
	for c, cs := range cats {
		measures[plural[c]] = float64(cs.Open)
		measures["new_"+plural[c]] = float64(cs.New)
	}
	var dupPercent *float64
	if q.ComparedLines > 0 && !q.DuplicationSkipped {
		pct := math.Round(float64(q.DuplicatedLines)/float64(q.ComparedLines)*1000) / 10
		dupPercent = &pct
		measures["duplicated_lines_percent"] = pct
	}
	conds, isDefault, err := s.gateFor(r, p.OrgID, v.RepositoryID)
	if err != nil {
		return err
	}
	gate := quality.Evaluate(conds, measures, q.BaselineVersionID != nil)
	gate.Default = isDefault

	top := make([]ruleCount, 0, len(byRule))
	for id, n := range byRule {
		rc := ruleCount{Rule: id, Count: n}
		if rule, ok := rules[id]; ok {
			rc.Name, rc.Category, rc.Severity = rule.Name, rule.Category, rule.Severity
		}
		top = append(top, rc)
	}
	sortRuleCounts(top)

	hot := cats["hotspot"]
	var reviewedPct *float64
	if total := hot.Open + hot.Reviewed; total > 0 {
		pct := math.Round(float64(hot.Reviewed)/float64(total)*1000) / 10
		reviewedPct = &pct
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"version_id":          v.ID,
		"available":           true,
		"baseline_version_id": q.BaselineVersionID,
		"gate":                gate,
		"categories":          cats,
		"grades": map[string]string{
			"reliability":     quality.SeverityGrade(cats["bug"].BySeverity),
			"security":        quality.SeverityGrade(cats["vulnerability"].BySeverity),
			"security_review": quality.ReviewGrade(hot.Reviewed, hot.Open+hot.Reviewed),
		},
		"hotspots_reviewed_percent": reviewedPct,
		"metrics": map[string]any{
			"functions": q.Functions, "complexity": q.Complexity, "max_complexity": q.MaxComplexity,
			"duplication_compared_lines": q.ComparedLines, "duplicated_lines": q.DuplicatedLines,
			"duplicated_blocks": q.DuplicatedBlocks, "duplicated_lines_percent": dupPercent,
			"duplication_skipped": q.DuplicationSkipped,
		},
		"rules": top,
	})
	return nil
}

func sortRuleCounts(rc []ruleCount) {
	for i := 1; i < len(rc); i++ {
		for j := i; j > 0 && (rc[j].Count > rc[j-1].Count || (rc[j].Count == rc[j-1].Count && rc[j].Rule < rc[j-1].Rule)); j-- {
			rc[j], rc[j-1] = rc[j-1], rc[j]
		}
	}
}

var findingStatuses = map[string]bool{"open": true, "reviewed": true, "accepted": true, "false_positive": true, "safe": true, "all": true}

func oneOf(values []string, v string) bool {
	for _, x := range values {
		if x == v {
			return true
		}
	}
	return false
}

func (s *Server) listFindings(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	v, _, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	q := r.URL.Query()
	f := store.FindingFilter{
		Category: q.Get("category"), Severity: q.Get("severity"), Rule: q.Get("rule"), Status: q.Get("status"),
		NewOnly: q.Get("new") == "true",
	}
	if f.Category != "" && !oneOf(quality.Categories, f.Category) {
		return errBadRequest("unknown category %q", f.Category)
	}
	if f.Severity != "" && !oneOf(quality.Severities, f.Severity) {
		return errBadRequest("unknown severity %q", f.Severity)
	}
	if f.Status != "" && !findingStatuses[f.Status] {
		return errBadRequest("unknown status %q", f.Status)
	}
	if len(f.Rule) > 100 {
		return errBadRequest("rule is too long")
	}
	if raw := q.Get("path"); raw != "" {
		dir := strings.HasSuffix(raw, "/")
		clean, err := repoPath(strings.TrimSuffix(raw, "/"), false)
		if err != nil {
			return err
		}
		if dir {
			clean += "/"
		}
		f.Path = clean
	}
	if f.Limit, err = queryInt(r, "limit", 50, 1, 500); err != nil {
		return err
	}
	if f.Offset, err = queryInt(r, "offset", 0, 0, 1_000_000); err != nil {
		return err
	}
	rows, total, err := s.Store.ListFindings(r.Context(), p.OrgID, v.ID, f)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, map[string]any{"findings": rows, "total": total})
	return nil
}

func (s *Server) findingTarget(r *http.Request, p *store.Principal) (repoID, fingerprint, category string, err error) {
	if repoID, err = pathID(r, "id"); err != nil {
		return
	}
	fingerprint = r.PathValue("fingerprint")
	if !fingerprintPattern.MatchString(fingerprint) {
		err = errBadRequest("invalid finding fingerprint")
		return
	}
	category, err = s.Store.FindingCategory(r.Context(), p.OrgID, repoID, fingerprint)
	return
}

func (s *Server) reviewFinding(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	repoID, fp, category, err := s.findingTarget(r, p)
	if err != nil {
		return err
	}
	var body struct {
		Status  string `json:"status"`
		Comment string `json:"comment"`
	}
	if err := decode(r, &body); err != nil {
		return err
	}
	body.Comment = strings.TrimSpace(body.Comment)
	switch {
	case category == "hotspot" && body.Status != "safe" && body.Status != "accepted":
		return errBadRequest("a security hotspot is reviewed as safe or accepted")
	case category != "hotspot" && body.Status != "accepted" && body.Status != "false_positive":
		return errBadRequest("a finding is reviewed as accepted or false_positive")
	case len(body.Comment) > 2000:
		return errBadRequest("comment is longer than 2000 characters")
	case body.Status == "false_positive" && body.Comment == "":
		return errBadRequest("explain in a comment why this is a false positive")
	}
	if err := s.Store.ReviewFinding(r.Context(), p.OrgID, repoID, fp, body.Status, body.Comment, p.UserID); err != nil {
		return err
	}
	s.audit(r, p, "finding.reviewed", "finding", fp, "success", map[string]any{"repository_id": repoID, "status": body.Status})
	w.WriteHeader(http.StatusNoContent)
	return nil
}

func (s *Server) reopenFinding(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	repoID, fp, _, err := s.findingTarget(r, p)
	if err != nil {
		return err
	}
	existed, err := s.Store.ReopenFinding(r.Context(), p.OrgID, repoID, fp)
	if err != nil {
		return err
	}
	if existed {
		s.audit(r, p, "finding.reopened", "finding", fp, "success", map[string]any{"repository_id": repoID})
	}
	w.WriteHeader(http.StatusNoContent)
	return nil
}

func (s *Server) getQualityGate(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	repoID, err := pathID(r, "id")
	if err != nil {
		return err
	}
	conds, isDefault, err := s.gateFor(r, p.OrgID, repoID)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, map[string]any{"conditions": conds, "default": isDefault, "metrics": quality.Metrics})
	return nil
}

func (s *Server) setQualityGate(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	repoID, err := pathID(r, "id")
	if err != nil {
		return err
	}
	var body struct {
		Conditions []quality.Condition `json:"conditions"`
		Default    bool                `json:"default"`
	}
	if err := decode(r, &body); err != nil {
		return err
	}
	var raw json.RawMessage
	if !body.Default {
		if body.Conditions == nil {
			return errBadRequest("send conditions, or default: true to use the default gate")
		}
		if err := quality.ValidateGate(body.Conditions); err != nil {
			return errBadRequest("%s", err.Error())
		}
		if raw, err = json.Marshal(body.Conditions); err != nil {
			return err
		}
	}
	if err := s.Store.SetQualityGate(r.Context(), p.OrgID, repoID, raw); err != nil {
		return err
	}
	s.audit(r, p, "quality_gate.updated", "repository", repoID, "success", map[string]any{"default": body.Default, "conditions": len(body.Conditions)})
	return s.getQualityGate(w, r, p)
}
