package quality

import (
	"errors"
	"fmt"
	"math"
)

// Condition fails the gate when the measured value is greater than Threshold.
type Condition struct {
	Metric    string  `json:"metric"`
	Threshold float64 `json:"threshold"`
}

// MetricInfo describes a metric a gate condition can use.
type MetricInfo struct {
	Metric      string `json:"metric"`
	Description string `json:"description"`
	// Measured only against a baseline (the previous analyzed version).
	NewCode bool `json:"new_code"`
}

// Metrics lists the gate metrics. "Open" excludes findings reviewed as
// accepted, false positive or safe.
var Metrics = []MetricInfo{
	{"new_bugs", "Open bugs not present in the previous version", true},
	{"new_vulnerabilities", "Open vulnerabilities not present in the previous version", true},
	{"new_hotspots", "Unreviewed security hotspots not present in the previous version", true},
	{"new_smells", "Open code smells not present in the previous version", true},
	{"bugs", "Open bugs", false},
	{"vulnerabilities", "Open vulnerabilities", false},
	{"hotspots", "Unreviewed security hotspots", false},
	{"smells", "Open code smells", false},
	{"blocker_findings", "Open findings of blocker severity, any category", false},
	{"critical_findings", "Open findings of critical severity, any category", false},
	{"duplicated_lines_percent", "Duplicated code lines as a percentage of compared code lines", false},
}

// DefaultGate fails on any new bug, vulnerability or unreviewed hotspot,
// and on any open blocker (for example a committed credential).
func DefaultGate() []Condition {
	return []Condition{
		{"new_bugs", 0}, {"new_vulnerabilities", 0}, {"new_hotspots", 0}, {"blocker_findings", 0},
	}
}

// ValidateGate checks conditions submitted by a user.
func ValidateGate(conds []Condition) error {
	if len(conds) > 20 {
		return errors.New("a gate has at most 20 conditions")
	}
	seen := map[string]bool{}
	for _, c := range conds {
		known := false
		for _, m := range Metrics {
			known = known || m.Metric == c.Metric
		}
		if !known {
			return fmt.Errorf("unknown metric %q", c.Metric)
		}
		if seen[c.Metric] {
			return fmt.Errorf("metric %q appears twice", c.Metric)
		}
		seen[c.Metric] = true
		if c.Threshold < 0 || math.IsNaN(c.Threshold) || math.IsInf(c.Threshold, 0) || (c.Metric == "duplicated_lines_percent" && c.Threshold > 100) {
			return fmt.Errorf("threshold of %q is out of range", c.Metric)
		}
	}
	return nil
}

// ConditionResult is the outcome of one condition.
type ConditionResult struct {
	Metric    string   `json:"metric"`
	Threshold float64  `json:"threshold"`
	Actual    *float64 `json:"actual"`
	// passed, failed, or no_baseline (a new-code metric without a previous
	// version to compare with; it does not fail the gate).
	Status string `json:"status"`
}

// GateResult is the outcome of a gate.
type GateResult struct {
	// passed or failed.
	Status     string            `json:"status"`
	Conditions []ConditionResult `json:"conditions"`
	Default    bool              `json:"default"`
}

// Evaluate applies conditions to measures. New-code metrics are skipped
// when there is no baseline.
func Evaluate(conds []Condition, measures map[string]float64, baseline bool) GateResult {
	res := GateResult{Status: "passed", Conditions: []ConditionResult{}}
	newCode := map[string]bool{}
	for _, m := range Metrics {
		newCode[m.Metric] = m.NewCode
	}
	for _, c := range conds {
		cr := ConditionResult{Metric: c.Metric, Threshold: c.Threshold}
		if newCode[c.Metric] && !baseline {
			cr.Status = "no_baseline"
		} else {
			v := measures[c.Metric]
			cr.Actual = &v
			cr.Status = "passed"
			if v > c.Threshold {
				cr.Status = "failed"
				res.Status = "failed"
			}
		}
		res.Conditions = append(res.Conditions, cr)
	}
	return res
}

// SeverityGrade grades open findings of one category by the worst
// severity present: A none, B minor or info, C major, D critical, E blocker.
func SeverityGrade(bySeverity map[string]int) string {
	switch {
	case bySeverity["blocker"] > 0:
		return "E"
	case bySeverity["critical"] > 0:
		return "D"
	case bySeverity["major"] > 0:
		return "C"
	case bySeverity["minor"] > 0 || bySeverity["info"] > 0:
		return "B"
	}
	return "A"
}

// ReviewGrade grades security hotspots by the share that has been
// reviewed: A at least 80%, B 70%, C 50%, D 30%, E below. No hotspots is A.
func ReviewGrade(reviewed, total int) string {
	if total == 0 {
		return "A"
	}
	p := float64(reviewed) / float64(total) * 100
	switch {
	case p >= 80:
		return "A"
	case p >= 70:
		return "B"
	case p >= 50:
		return "C"
	case p >= 30:
		return "D"
	}
	return "E"
}
