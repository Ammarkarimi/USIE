package quality

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"math"
	"path"
	"regexp"
	"strings"
)

// maxSecretLine bounds the part of a line that is scanned; minified files
// can have megabyte-long lines. RE2 matching is linear in the input.
const maxSecretLine = 4096

const maxSecretsPerFile = 100

type secretPattern struct {
	kind string
	re   *regexp.Regexp
}

var secretPatterns = []secretPattern{
	{"AWS access key ID", regexp.MustCompile(`\b(?:AKIA|ASIA)[0-9A-Z]{16}\b`)},
	{"GitHub token", regexp.MustCompile(`\bgh[pousr]_[A-Za-z0-9]{36,255}\b`)},
	{"GitHub fine-grained token", regexp.MustCompile(`\bgithub_pat_[A-Za-z0-9_]{22,255}\b`)},
	{"GitLab token", regexp.MustCompile(`\bglpat-[A-Za-z0-9_-]{20,}`)},
	{"npm token", regexp.MustCompile(`\bnpm_[A-Za-z0-9]{36}\b`)},
	{"Slack token", regexp.MustCompile(`\bxox[baprs]-[A-Za-z0-9-]{10,}`)},
	{"Slack webhook URL", regexp.MustCompile(`https://hooks\.slack\.com/services/T[A-Za-z0-9]+/B[A-Za-z0-9]+/[A-Za-z0-9]{16,}`)},
	{"Stripe live key", regexp.MustCompile(`\b(?:sk|rk)_live_[0-9a-zA-Z]{20,}\b`)},
	{"SendGrid key", regexp.MustCompile(`\bSG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}\b`)},
	{"Google API key", regexp.MustCompile(`\bAIza[0-9A-Za-z_-]{35}\b`)},
	{"OpenAI key", regexp.MustCompile(`\bsk-[A-Za-z0-9_-]*T3BlbkFJ[A-Za-z0-9_-]{10,}`)},
	{"Anthropic key", regexp.MustCompile(`\bsk-ant-(?:api|admin)\d\d-[A-Za-z0-9_-]{80,}`)},
	{"private key", regexp.MustCompile(`-----BEGIN (?:(?:RSA|EC|DSA|OPENSSH|ENCRYPTED|PGP) )?PRIVATE KEY(?: BLOCK)?-----`)},
}

var (
	// key = "value", key: 'value', "key": "value", key := "value", key => "value"
	quotedAssignment = regexp.MustCompile(`(?i)([a-z0-9_.\-]*(?:password|passwd|secret|api[_-]?key|access[_-]?key|auth[_-]?token|access[_-]?token|client[_-]?secret|private[_-]?key)[a-z0-9_.\-]*)["']?\s*(?::=|=>|=|:)\s*["']([^"'\s]{6,200})["']`)
	// KEY=value in .env, properties, ini and YAML files.
	bareAssignment = regexp.MustCompile(`(?i)^\s*(?:export\s+)?([a-z0-9_.\-]*(?:password|passwd|secret|api[_-]?key|access[_-]?key|token|client[_-]?secret)[a-z0-9_.\-]*)\s*[:=]\s*([^\s"'#]{6,200})\s*(?:#.*)?$`)
	constantName   = regexp.MustCompile(`^[A-Z0-9_]+$`)
)

// Names that contain a keyword but do not hold the credential itself.
var notCredentialKeys = []string{
	"hash", "file", "path", "name", "field", "label", "placeholder", "pattern", "regex", "header", "type", "policy",
	"length", "prompt", "message", "error", "url", "env", "param", "input", "hint", "rule", "reset", "confirm",
	"manager", "store", "provider", "ref", "_id", "kind", "mode", "required", "enabled", "valid",
}

// Values that are placeholders, references or descriptions.
var placeholderValues = []string{
	"example", "changeme", "change_me", "change-me", "your", "xxx", "***", "<", ">", "${", "{{", "$(", "%s", "%(",
	"placeholder", "dummy", "fake", "sample", "redacted", "secret", "password", "test", "todo", "none", "null",
	"undefined", "process.", "os.", "env", "...",
}

func configFile(p string) bool {
	base := strings.ToLower(path.Base(p))
	if strings.HasPrefix(base, ".env") {
		return true
	}
	switch path.Ext(base) {
	case ".env", ".properties", ".ini", ".cfg", ".conf", ".yaml", ".yml":
		return true
	}
	return false
}

// ScanSecrets finds credentials in one text file. category is the file's
// classification (source, test, documentation, ...): the password rule
// skips tests and documentation, which routinely contain fake credentials.
func ScanSecrets(p, category string, content []byte) []Raw {
	var out []Raw
	checkPasswords := category != "test" && category != "documentation"
	bare := configFile(p)
	line := 0
	for len(content) > 0 && len(out) < maxSecretsPerFile {
		line++
		var text []byte
		if i := bytes.IndexByte(content, '\n'); i >= 0 {
			text, content = content[:i], content[i+1:]
		} else {
			text, content = content, nil
		}
		if len(text) > maxSecretLine {
			text = text[:maxSecretLine]
		}
		s := string(text)
		for _, pat := range secretPatterns {
			for _, loc := range pat.re.FindAllStringIndex(s, 4) {
				value := s[loc[0]:loc[1]]
				upper := strings.ToUpper(value)
				if strings.Contains(upper, "EXAMPLE") || strings.Contains(upper, "XXXX") {
					continue
				}
				// A PEM header alone is often code that recognizes keys;
				// only a header followed by key material is a key.
				if pat.kind == "private key" && !keyMaterialFollows(content) {
					continue
				}
				out = append(out, Raw{
					Rule: RuleSecret, StartLine: line, StartCol: loc[0], EndLine: line, EndCol: loc[1],
					Message: fmt.Sprintf("A %s is committed in this file (%s…).", pat.kind, visiblePrefix(value)),
					Context: redactedContext(pat.kind, s, loc),
				})
			}
		}
		if !checkPasswords {
			continue
		}
		matches := quotedAssignment.FindAllStringSubmatchIndex(s, 4)
		if bare {
			matches = append(matches, bareAssignment.FindAllStringSubmatchIndex(s, 1)...)
		}
		for _, m := range matches {
			key, value := s[m[2]:m[3]], s[m[4]:m[5]]
			if !likelyCredential(key, value) {
				continue
			}
			out = append(out, Raw{
				Rule: RulePassword, StartLine: line, StartCol: m[4], EndLine: line, EndCol: m[5],
				Message: fmt.Sprintf("`%s` is assigned a hard-coded value.", key),
				Context: redactedContext(strings.ToLower(key), s, []int{m[4], m[5]}),
			})
		}
	}
	return out
}

var pemBody = regexp.MustCompile(`^\s*(?:[A-Za-z0-9+/]{40,}={0,2}|Proc-Type: 4,ENCRYPTED)\s*$`)

// keyMaterialFollows reports whether the next line holds base64 key data
// (or the header of an encrypted PEM block).
func keyMaterialFollows(rest []byte) bool {
	next := rest
	if i := bytes.IndexByte(rest, '\n'); i >= 0 {
		next = rest[:i]
	}
	if len(next) > maxSecretLine {
		return false
	}
	return pemBody.Match(next)
}

func likelyCredential(key, value string) bool {
	k := strings.ToLower(key)
	for _, w := range notCredentialKeys {
		if strings.Contains(k, w) {
			return false
		}
	}
	v := strings.ToLower(value)
	if v == k || constantName.MatchString(value) {
		return false
	}
	for _, w := range placeholderValues {
		if strings.Contains(v, w) {
			return false
		}
	}
	return entropy(value) >= 2.5
}

// entropy is the Shannon entropy of s in bits per character.
func entropy(s string) float64 {
	counts := map[rune]int{}
	n := 0
	for _, r := range s {
		counts[r]++
		n++
	}
	var h float64
	for _, c := range counts {
		p := float64(c) / float64(n)
		h -= p * math.Log2(p)
	}
	return h
}

func visiblePrefix(v string) string {
	if strings.HasPrefix(v, "-----") {
		return "-----BEGIN"
	}
	if len(v) > 4 {
		return v[:4]
	}
	return ""
}

// redactedContext identifies the line without containing the secret: the
// value is replaced before the line is hashed.
func redactedContext(kind, line string, loc []int) string {
	redacted := line[:loc[0]] + "<redacted>" + line[loc[1]:]
	sum := sha256.Sum256([]byte(kind + "\x00" + strings.Join(strings.Fields(redacted), " ")))
	return hex.EncodeToString(sum[:12])
}
