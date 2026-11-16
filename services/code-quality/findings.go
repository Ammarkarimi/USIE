package quality

import (
	"crypto/sha256"
	"encoding/hex"
	"strconv"
)

// Raw is a rule match before it is classified and fingerprinted.
type Raw struct {
	Rule      string `json:"rule"`
	StartLine int    `json:"start_line"`
	StartCol  int    `json:"start_col"`
	EndLine   int    `json:"end_line"`
	EndCol    int    `json:"end_col"`
	Message   string `json:"message"`
	Function  string `json:"function,omitempty"`
	// Normalized matched code (or a hash of it). Never a secret's value.
	Context string `json:"context"`
}

// Finding is a classified rule match, ready to store.
type Finding struct {
	Rule        string
	Category    string
	Severity    string
	Path        string
	StartLine   int
	StartCol    int
	EndLine     int
	EndCol      int
	Message     string
	Function    string
	Fingerprint string
	IsNew       bool
	InTest      bool
}

// Assemble classifies the matches of one file and gives each a fingerprint
// that stays the same while the matched code is unchanged, wherever it
// moves in the file. Matches of rules missing from the catalog are dropped.
// In test files maintainability findings are dropped: test code is not
// held to the same structure rules.
func Assemble(path string, inTest bool, raws []Raw, rules map[string]Rule) []Finding {
	out := make([]Finding, 0, len(raws))
	seen := map[string]int{}
	for _, r := range raws {
		rule, ok := rules[r.Rule]
		if !ok {
			continue
		}
		if inTest && rule.Category == "smell" {
			continue
		}
		key := r.Rule + "\x00" + r.Context
		n := seen[key]
		seen[key] = n + 1
		sum := sha256.Sum256([]byte(r.Rule + "\x00" + path + "\x00" + r.Context + "\x00" + strconv.Itoa(n)))
		end := r.EndLine
		if end < r.StartLine {
			end = r.StartLine
		}
		out = append(out, Finding{
			Rule: r.Rule, Category: rule.Category, Severity: rule.Severity, Path: path,
			StartLine: r.StartLine, StartCol: r.StartCol, EndLine: end, EndCol: r.EndCol,
			Message: r.Message, Function: r.Function, Fingerprint: hex.EncodeToString(sum[:16]), InTest: inTest,
		})
	}
	return out
}
