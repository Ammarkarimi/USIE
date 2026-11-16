// Package transform produces AI-proposed code changes for an analyzed
// version: a model returns search-and-replace edits for files it was shown
// in full, and the server validates and applies them to the stored sources
// to produce a unified diff. Nothing is written to the repository, and a
// proposal is not verified until a verification run passes.
package transform

import (
	"encoding/json"
	"errors"
	"fmt"
	"path"
	"strings"
	"unicode/utf8"

	"github.com/ammarkarimi/usie/internal/textdiff"
	verify "github.com/ammarkarimi/usie/services/verification"
)

// proposalSchema is the structure the model must return (strict JSON schema:
// every property required, no others).
var proposalSchema = json.RawMessage(`{
  "type": "object",
  "additionalProperties": false,
  "required": ["feasible", "summary", "rationale", "edits", "risks", "test_plan", "limitations"],
  "properties": {
    "feasible": {"type": "boolean", "description": "false when the request cannot be done safely with the files provided"},
    "summary": {"type": "string", "description": "one or two sentences: what the change does"},
    "rationale": {"type": "string", "description": "why these edits implement the request; reference evidence ids such as [C3]"},
    "edits": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["path", "action", "search", "replace", "reason"],
        "properties": {
          "path": {"type": "string"},
          "action": {"type": "string", "enum": ["modify", "create", "delete"]},
          "search": {"type": "string", "description": "modify: exact text copied from the editable file, occurring exactly once; empty otherwise"},
          "replace": {"type": "string", "description": "modify: the new text; create: the whole file; delete: empty"},
          "reason": {"type": "string"}
        }
      }
    },
    "risks": {"type": "array", "items": {"type": "string"}},
    "test_plan": {"type": "array", "items": {"type": "string"}},
    "limitations": {"type": "array", "items": {"type": "string"}}
  }
}`)

// ProposalSchema returns the JSON schema of a proposal.
func ProposalSchema() json.RawMessage { return proposalSchema }

// Edit is one edit as returned by the model.
type Edit struct {
	Path    string `json:"path"`
	Action  string `json:"action"`
	Search  string `json:"search"`
	Replace string `json:"replace"`
	Reason  string `json:"reason"`
}

// Proposal is the model's structured output.
type Proposal struct {
	Feasible    bool     `json:"feasible"`
	Summary     string   `json:"summary"`
	Rationale   string   `json:"rationale"`
	Edits       []Edit   `json:"edits"`
	Risks       []string `json:"risks"`
	TestPlan    []string `json:"test_plan"`
	Limitations []string `json:"limitations"`
}

// Limits bound what a proposal may change.
type Limits struct {
	MaxEdits        int
	MaxFiles        int
	MaxCreatedFiles int
	MaxFileChars    int
}

func DefaultLimits() Limits {
	return Limits{MaxEdits: 40, MaxFiles: 10, MaxCreatedFiles: 5, MaxFileChars: 200_000}
}

// Applied is the result of applying a proposal's edits.
type Applied struct {
	Files        []verify.FileChange
	Diff         string
	LinesAdded   int
	LinesRemoved int
}

// ParseProposal decodes and bounds the model output.
func ParseProposal(raw []byte) (*Proposal, error) {
	var p Proposal
	dec := json.NewDecoder(strings.NewReader(string(raw)))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&p); err != nil {
		return nil, fmt.Errorf("the AI provider returned output that does not match the proposal format: %w", err)
	}
	p.Summary = clip(p.Summary, 2000)
	p.Rationale = clip(p.Rationale, 8000)
	p.Risks = clipList(p.Risks, 10, 500)
	p.TestPlan = clipList(p.TestPlan, 10, 500)
	p.Limitations = clipList(p.Limitations, 10, 500)
	for i := range p.Edits {
		p.Edits[i].Reason = clip(p.Edits[i].Reason, 500)
	}
	return &p, nil
}

// ValidPath reports whether p is a clean repository-relative path that a
// change may touch.
func ValidPath(p string) bool {
	if p == "" || len(p) > 1024 || !utf8.ValidString(p) || path.Clean(p) != p || path.IsAbs(p) ||
		p == ".." || strings.HasPrefix(p, "../") || strings.ContainsAny(p, "\x00\\") {
		return false
	}
	first := strings.SplitN(p, "/", 2)[0]
	return first != ".git"
}

// Apply validates the edits against the editable files (path → current
// content) and the set of existing paths, and applies them. Any invalid
// edit rejects the whole proposal: a partially applied change would not be
// what the model described.
func Apply(edits []Edit, editable map[string]string, exists func(string) bool, lim Limits) (*Applied, error) {
	if len(edits) == 0 {
		return nil, errors.New("the proposal contains no edits")
	}
	if len(edits) > lim.MaxEdits {
		return nil, fmt.Errorf("the proposal has %d edits; at most %d are accepted", len(edits), lim.MaxEdits)
	}
	content := map[string]string{}
	status := map[string]string{}
	var order []string
	touch := func(p, s string) {
		if _, ok := status[p]; !ok {
			order = append(order, p)
		}
		status[p] = s
	}
	created := 0
	for i, e := range edits {
		where := fmt.Sprintf("edit %d (%s)", i+1, e.Path)
		if !ValidPath(e.Path) {
			return nil, fmt.Errorf("%s: not a valid repository path", where)
		}
		switch e.Action {
		case "modify":
			cur, ok := content[e.Path]
			if !ok {
				orig, editableFile := editable[e.Path]
				if !editableFile {
					return nil, fmt.Errorf("%s: the file was not provided in full, so it cannot be edited", where)
				}
				cur = orig
			}
			if status[e.Path] == "deleted" {
				return nil, fmt.Errorf("%s: the file was deleted by an earlier edit", where)
			}
			if e.Search == "" {
				return nil, fmt.Errorf("%s: the text to replace is empty", where)
			}
			switch n := strings.Count(cur, e.Search); n {
			case 0:
				return nil, fmt.Errorf("%s: the text to replace does not occur in the file", where)
			case 1:
			default:
				return nil, fmt.Errorf("%s: the text to replace occurs %d times; it must be unique", where, n)
			}
			content[e.Path] = strings.Replace(cur, e.Search, e.Replace, 1)
			if status[e.Path] != "created" {
				touch(e.Path, "modified")
			}
		case "create":
			if exists(e.Path) || status[e.Path] != "" {
				return nil, fmt.Errorf("%s: the file already exists", where)
			}
			if e.Search != "" {
				return nil, fmt.Errorf("%s: a created file has no text to replace", where)
			}
			created++
			if created > lim.MaxCreatedFiles {
				return nil, fmt.Errorf("the proposal creates more than %d files", lim.MaxCreatedFiles)
			}
			content[e.Path] = e.Replace
			touch(e.Path, "created")
		case "delete":
			if _, ok := editable[e.Path]; !ok {
				return nil, fmt.Errorf("%s: only files provided in full can be deleted", where)
			}
			content[e.Path] = ""
			touch(e.Path, "deleted")
		default:
			return nil, fmt.Errorf("%s: unknown action %q", where, e.Action)
		}
		if len(content[e.Path]) > lim.MaxFileChars {
			return nil, fmt.Errorf("%s: the resulting file exceeds %d characters", where, lim.MaxFileChars)
		}
	}
	if len(order) > lim.MaxFiles {
		return nil, fmt.Errorf("the proposal changes %d files; at most %d are accepted", len(order), lim.MaxFiles)
	}
	out := &Applied{}
	var diff strings.Builder
	for _, p := range order {
		st := status[p]
		oldText, newText := editable[p], content[p]
		oldPath, newPath := p, p
		switch st {
		case "created":
			oldText, oldPath = "", "/dev/null"
		case "deleted":
			newText, newPath = "", "/dev/null"
		}
		if st == "modified" && oldText == newText {
			continue
		}
		d, err := textdiff.Unified(oldPath, newPath, oldText, newText, 3)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", p, err)
		}
		diff.WriteString(d)
		fc := verify.FileChange{Path: p, Status: st}
		if st != "deleted" {
			fc.Content = newText
		}
		out.Files = append(out.Files, fc)
	}
	if len(out.Files) == 0 {
		return nil, errors.New("the edits leave every file unchanged")
	}
	out.Diff = diff.String()
	for _, line := range strings.Split(out.Diff, "\n") {
		switch {
		case strings.HasPrefix(line, "+++ "), strings.HasPrefix(line, "--- "):
		case strings.HasPrefix(line, "+"):
			out.LinesAdded++
		case strings.HasPrefix(line, "-"):
			out.LinesRemoved++
		}
	}
	return out, nil
}

func clip(s string, max int) string {
	if len(s) <= max {
		return s
	}
	cut := max
	for cut > 0 && !utf8.RuneStart(s[cut]) {
		cut--
	}
	return s[:cut] + "…"
}

func clipList(items []string, n, max int) []string {
	out := []string{}
	for _, it := range items {
		if strings.TrimSpace(it) == "" {
			continue
		}
		if len(out) == n {
			break
		}
		out = append(out, clip(it, max))
	}
	return out
}
