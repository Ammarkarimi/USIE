package ai

import (
	"encoding/json"
	"fmt"
	"regexp"
	"strings"
	"unicode/utf8"
)

// answerSchema is the structured output the model must produce. Claims are
// separate from the summary so each can carry its own basis and citations.
var answerSchema = json.RawMessage(`{
  "type": "object",
  "additionalProperties": false,
  "required": ["answerable", "summary", "claims", "limitations", "followups"],
  "properties": {
    "answerable": {"type": "string", "enum": ["yes", "partially", "no"]},
    "summary": {"type": "string"},
    "claims": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["statement", "basis", "citations"],
        "properties": {
          "statement": {"type": "string"},
          "basis": {"type": "string", "enum": ["evidence", "inference"]},
          "citations": {"type": "array", "items": {"type": "string"}}
        }
      }
    },
    "limitations": {"type": "array", "items": {"type": "string"}},
    "followups": {"type": "array", "items": {"type": "string"}}
  }
}`)

// AnswerSchema returns the JSON schema of model output.
func AnswerSchema() json.RawMessage { return answerSchema }

type rawAnswer struct {
	Answerable  string     `json:"answerable"`
	Summary     string     `json:"summary"`
	Claims      []rawClaim `json:"claims"`
	Limitations []string   `json:"limitations"`
	Followups   []string   `json:"followups"`
}

type rawClaim struct {
	Statement string   `json:"statement"`
	Basis     string   `json:"basis"`
	Citations []string `json:"citations"`
}

// Support classifies a claim after validation.
const (
	// SupportEvidence: the model says the cited evidence shows it, and every
	// citation refers to evidence that was actually provided.
	SupportEvidence = "evidence"
	// SupportInference: the model marked it as its own inference.
	SupportInference = "inference"
	// SupportUnsupported: claimed as evidence-based but no valid citation.
	SupportUnsupported = "unsupported"
)

type Claim struct {
	Statement string   `json:"statement"`
	Support   string   `json:"support"`
	Citations []string `json:"citations"`
	// InvalidCitations lists identifiers that were not in the evidence.
	InvalidCitations []string `json:"invalid_citations,omitempty"`
}

// Answer is what is stored and shown: the model's output after validation.
type Answer struct {
	Answerable  string   `json:"answerable"`
	Summary     string   `json:"summary"`
	Claims      []Claim  `json:"claims"`
	Limitations []string `json:"limitations"`
	Followups   []string `json:"followups"`
	// ValidationNotes records what the validator changed or found.
	ValidationNotes []string `json:"validation_notes"`
}

const (
	maxSummary   = 8000
	maxStatement = 2000
	maxClaims    = 30
	maxListItem  = 500
	maxListItems = 10
)

var citationRef = regexp.MustCompile(`\[(C\d{1,3})\]`)

// ValidateAnswer parses model output and checks it against the evidence
// identifiers that were sent. It never upgrades a claim: it can only
// downgrade evidence claims whose citations do not check out.
func ValidateAnswer(raw json.RawMessage, evidenceIDs map[string]bool) (*Answer, error) {
	var in rawAnswer
	if err := json.Unmarshal(raw, &in); err != nil {
		return nil, fmt.Errorf("ai: the provider's answer is not valid JSON: %w", err)
	}
	out := &Answer{Answerable: in.Answerable, Summary: clip(strings.TrimSpace(in.Summary), maxSummary), Claims: []Claim{}, Limitations: []string{}, Followups: []string{}, ValidationNotes: []string{}}
	switch out.Answerable {
	case "yes", "partially", "no":
	default:
		return nil, fmt.Errorf("ai: the provider's answer has an invalid answerable value %q", in.Answerable)
	}
	if len(in.Claims) > maxClaims {
		out.ValidationNotes = append(out.ValidationNotes, fmt.Sprintf("%d claims were returned; only the first %d are kept", len(in.Claims), maxClaims))
		in.Claims = in.Claims[:maxClaims]
	}
	unsupported := 0
	for _, c := range in.Claims {
		statement := clip(strings.TrimSpace(c.Statement), maxStatement)
		if statement == "" {
			continue
		}
		claim := Claim{Statement: statement, Citations: []string{}}
		seen := map[string]bool{}
		for _, id := range c.Citations {
			id = strings.Trim(strings.TrimSpace(id), "[]")
			if seen[id] {
				continue
			}
			seen[id] = true
			if evidenceIDs[id] {
				claim.Citations = append(claim.Citations, id)
			} else {
				claim.InvalidCitations = append(claim.InvalidCitations, clip(id, 40))
			}
		}
		switch {
		case c.Basis == "inference":
			claim.Support = SupportInference
		case len(claim.Citations) > 0:
			claim.Support = SupportEvidence
		default:
			claim.Support = SupportUnsupported
			unsupported++
		}
		out.Claims = append(out.Claims, claim)
	}
	if unsupported > 0 {
		out.ValidationNotes = append(out.ValidationNotes, fmt.Sprintf("%d claim(s) were presented as evidence-based but cite no evidence that was provided; they are marked unsupported", unsupported))
	}
	var bad []string
	for _, m := range citationRef.FindAllStringSubmatch(out.Summary, -1) {
		if !evidenceIDs[m[1]] {
			bad = append(bad, m[1])
		}
	}
	if len(bad) > 0 {
		out.ValidationNotes = append(out.ValidationNotes, "the summary refers to evidence that was not provided: "+strings.Join(uniq(bad), ", "))
	}
	out.Limitations = clipList(in.Limitations)
	out.Followups = clipList(in.Followups)
	return out, nil
}

// PlainText renders an answer for use as conversation history.
func (a *Answer) PlainText() string {
	var b strings.Builder
	b.WriteString(a.Summary)
	for _, c := range a.Claims {
		fmt.Fprintf(&b, "\n- (%s) %s", c.Support, c.Statement)
	}
	if len(a.Limitations) > 0 {
		b.WriteString("\nLimitations: " + strings.Join(a.Limitations, "; "))
	}
	return b.String()
}

func clipList(items []string) []string {
	out := []string{}
	for _, s := range items {
		if s = strings.TrimSpace(s); s != "" && len(out) < maxListItems {
			out = append(out, clip(s, maxListItem))
		}
	}
	return out
}

func clip(s string, max int) string {
	if len(s) <= max {
		return s
	}
	for max > 0 && !utf8.RuneStart(s[max]) {
		max--
	}
	return s[:max] + "…"
}

func uniq(in []string) []string {
	seen := map[string]bool{}
	var out []string
	for _, s := range in {
		if !seen[s] {
			seen[s] = true
			out = append(out, s)
		}
	}
	return out
}
