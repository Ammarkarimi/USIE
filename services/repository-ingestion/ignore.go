package ingestion

import (
	"path"
	"strings"
)

// ignoreRule is one line of a .gitignore-style file.
type ignoreRule struct {
	base     string // directory the rule file lives in ("" for root)
	pattern  string
	negate   bool
	dirOnly  bool
	anchored bool // contains a slash: matches relative to base only
}

// IgnoreSet implements the commonly used subset of gitignore semantics:
// comments, negation, directory-only patterns, anchoring, `*`, `?`, `[…]`
// and `**`. Later rules override earlier ones.
type IgnoreSet struct{ rules []ignoreRule }

// Add parses rule-file content located in directory `base`.
func (s *IgnoreSet) Add(base string, content []byte) {
	for _, line := range strings.Split(string(content), "\n") {
		line = strings.TrimRight(line, "\r")
		if !strings.HasSuffix(line, "\\ ") {
			line = strings.TrimRight(line, " \t")
		}
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		rule := ignoreRule{base: base}
		if strings.HasPrefix(line, "!") {
			rule.negate = true
			line = line[1:]
		}
		line = strings.TrimPrefix(line, "\\")
		if strings.HasSuffix(line, "/") {
			rule.dirOnly = true
			line = strings.TrimSuffix(line, "/")
		}
		if strings.Contains(line, "/") {
			rule.anchored = true
			line = strings.TrimPrefix(line, "/")
		}
		if line == "" {
			continue
		}
		rule.pattern = line
		s.rules = append(s.rules, rule)
	}
}

// Match reports whether the repository-relative path is ignored.
func (s *IgnoreSet) Match(p string, isDir bool) bool {
	ignored := false
	for _, r := range s.rules {
		if r.dirOnly && !isDir {
			continue
		}
		rel := p
		if r.base != "" {
			if !strings.HasPrefix(p, r.base+"/") {
				continue
			}
			rel = p[len(r.base)+1:]
		}
		var hit bool
		if r.anchored {
			hit = globMatch(r.pattern, rel)
		} else {
			hit = globMatch(r.pattern, path.Base(rel))
		}
		if hit {
			ignored = !r.negate
		}
	}
	return ignored
}

// globMatch matches a gitignore glob against a slash-separated path.
func globMatch(pattern, name string) bool {
	return matchSegments(strings.Split(pattern, "/"), strings.Split(name, "/"))
}

func matchSegments(pat, segs []string) bool {
	for len(pat) > 0 {
		if pat[0] == "**" {
			if len(pat) == 1 {
				return true
			}
			for i := 0; i <= len(segs); i++ {
				if matchSegments(pat[1:], segs[i:]) {
					return true
				}
			}
			return false
		}
		if len(segs) == 0 {
			return false
		}
		if ok, err := path.Match(pat[0], segs[0]); err != nil || !ok {
			return false
		}
		pat, segs = pat[1:], segs[1:]
	}
	return len(segs) == 0
}
