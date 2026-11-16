// Package quality turns rule matches into stored findings and evaluates
// quality gates.
//
// Two sources produce findings: the analysis engine's syntax-tree rules
// (one file at a time, see engines/parser/src/checks.rs) and the text rules
// in this package (hard-coded secrets and duplicated code), which also cover
// files the engine cannot parse. Every rule is described in the catalog,
// including what it cannot see.
package quality

import "sort"

// Rule describes one rule of the catalog.
type Rule struct {
	ID          string   `json:"id"`
	Name        string   `json:"name"`
	Category    string   `json:"category"`
	Severity    string   `json:"severity"`
	Languages   []string `json:"languages"`
	Description string   `json:"description"`
	Remediation string   `json:"remediation"`
	// engine: syntax-tree rule of the analysis engine. text: line-based rule
	// of this package.
	Source string `json:"source"`
}

// Categories in display order.
var Categories = []string{"bug", "vulnerability", "hotspot", "smell"}

// Severities from most to least severe.
var Severities = []string{"blocker", "critical", "major", "minor", "info"}

const (
	RuleSecret     = "hardcoded-secret"
	RulePassword   = "hardcoded-password"
	RuleDuplicated = "duplicated-block"
)

// DuplicationWindow is the number of consecutive code lines that must be
// identical for code to count as duplicated.
const DuplicationWindow = 10

var textRules = []Rule{
	{
		ID: RuleSecret, Name: "Credentials should not be committed", Category: "vulnerability", Severity: "blocker",
		Languages: []string{"any"}, Source: "text",
		Description: "A value has the exact format of a known credential: AWS access key ID, GitHub, GitLab, npm, Slack, Stripe live, SendGrid, Google API, OpenAI or Anthropic key, Slack webhook URL, or a private key block (a PEM header followed by key data). " +
			"Checked in every text file that is not vendored or generated, tests included. Values containing EXAMPLE or xxxx are treated as documentation. The value is never stored; findings show only its first characters.",
		Remediation: "Revoke the credential now, since it is in the repository history, then load it from a secret manager or environment variable.",
	},
	{
		ID: RulePassword, Name: "Passwords and keys should not be hard-coded", Category: "vulnerability", Severity: "critical",
		Languages: []string{"any"}, Source: "text",
		Description: "A quoted literal of 6 or more characters is assigned to a name containing password, passwd, secret, api_key, access_key, auth_token, access_token, client_secret or private_key " +
			"(and, in .env, properties, ini, conf and YAML files, an unquoted value). Placeholders (example, changeme, your…, ${…}, <…>), constant names in capitals, and low-entropy values are ignored. " +
			"Test files and documentation are not checked.",
		Remediation: "Load the value from a secret manager or environment variable. If it was ever a real credential, rotate it.",
	},
	{
		ID: RuleDuplicated, Name: "Source code should not be duplicated", Category: "smell", Severity: "minor",
		Languages: []string{"any parsed language"}, Source: "text",
		Description: "At least 10 consecutive code lines are identical (after trimming whitespace) to lines elsewhere in the repository. Blank lines, comments, braces and import lines are ignored. " +
			"Only non-test source files of languages the engine parses are compared. Renamed variables hide duplication from this rule.",
		Remediation: "Extract the shared code into a function or module used by both places.",
	},
}

// Catalog returns the engine rules followed by the text rules.
func Catalog(engine []Rule) []Rule {
	out := make([]Rule, 0, len(engine)+len(textRules))
	for _, r := range engine {
		r.Source = "engine"
		out = append(out, r)
	}
	out = append(out, textRules...)
	return out
}

// Index maps rule ids to rules.
func Index(rules []Rule) map[string]Rule {
	m := make(map[string]Rule, len(rules))
	for _, r := range rules {
		m[r.ID] = r
	}
	return m
}

func severityRank(s string) int {
	for i, v := range Severities {
		if v == s {
			return i
		}
	}
	return len(Severities)
}

// SortFindings orders findings by severity, then path and line.
func SortFindings(fs []Finding) {
	sort.SliceStable(fs, func(i, j int) bool {
		a, b := fs[i], fs[j]
		if ra, rb := severityRank(a.Severity), severityRank(b.Severity); ra != rb {
			return ra < rb
		}
		if a.Path != b.Path {
			return a.Path < b.Path
		}
		return a.StartLine < b.StartLine
	})
}
