// Package impact computes the impact of a change on an analyzed version:
// which symbols the change touches, what depends on them (reverse
// traversal of the knowledge graph), which tests reach them, and a risk
// score whose every factor is reported.
package impact

import (
	"bufio"
	"fmt"
	"strconv"
	"strings"
)

// LineRange is an inclusive range of 1-based line numbers. An insertion
// point between lines is the range {N, N} of the line before which text
// was inserted (or the last line, for an append).
type LineRange struct {
	Start int `json:"start"`
	End   int `json:"end"`
}

// FileChange is how one file differs between the analyzed version and the
// change, in terms of the analyzed version's lines.
type FileChange struct {
	Path string `json:"path"`
	// Status is modified, added (the file does not exist in the analyzed
	// version), deleted (the change removes it) or whole (every
	// declaration of the file counts as changed, for Reason).
	Status string      `json:"status"`
	Ranges []LineRange `json:"ranges,omitempty"`
	Reason string      `json:"reason,omitempty"`
}

const maxDiffFiles = 2000

// ParseUnifiedDiff reads a unified diff (`git diff`, `diff -u`) and
// returns, per file, the analyzed version's line ranges that the diff
// changes. The diff is taken to apply to the analyzed version: removed
// lines and the positions of inserted lines are on its side.
func ParseUnifiedDiff(text string) ([]FileChange, error) {
	var out []FileChange
	var cur *FileChange
	var oldPath, newPath string
	sc := bufio.NewScanner(strings.NewReader(text))
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	oldLine, oldLeft, newLeft := 0, 0, 0
	pendingStart := 0
	flush := func(end int) {
		if cur != nil && pendingStart > 0 {
			if end < pendingStart {
				end = pendingStart
			}
			cur.Ranges = appendRange(cur.Ranges, LineRange{pendingStart, end})
		}
		pendingStart = 0
	}
	for sc.Scan() {
		line := sc.Text()
		inHunk := oldLeft > 0 || newLeft > 0
		switch {
		case inHunk && (strings.HasPrefix(line, "-") || strings.HasPrefix(line, "+") || strings.HasPrefix(line, " ") || line == ""):
			switch {
			case strings.HasPrefix(line, "-"):
				if pendingStart == 0 {
					pendingStart = oldLine
				}
				oldLine++
				oldLeft--
			case strings.HasPrefix(line, "+"):
				// An insertion before old line `oldLine`.
				if pendingStart == 0 {
					pendingStart = max(oldLine, 1)
				}
				newLeft--
			default:
				flush(oldLine - 1)
				oldLine++
				oldLeft--
				newLeft--
			}
			if oldLeft <= 0 && newLeft <= 0 {
				flush(oldLine - 1)
				oldLeft, newLeft = 0, 0
			}
		case strings.HasPrefix(line, `\ No newline`):
		case strings.HasPrefix(line, "--- "):
			flush(oldLine - 1)
			oldPath = diffPath(line[4:])
		case strings.HasPrefix(line, "+++ "):
			newPath = diffPath(line[4:])
			status, path := "modified", oldPath
			switch {
			case oldPath == "":
				status, path = "added", newPath
			case newPath == "":
				status = "deleted"
			}
			if path == "" {
				return nil, fmt.Errorf("diff: file header without a path")
			}
			if len(out) >= maxDiffFiles {
				return nil, fmt.Errorf("diff: more than %d files", maxDiffFiles)
			}
			out = append(out, FileChange{Path: path, Status: status})
			cur = &out[len(out)-1]
		case strings.HasPrefix(line, "@@ "):
			if cur == nil {
				return nil, fmt.Errorf("diff: hunk before a file header")
			}
			o, ol, n, nl, err := parseHunkHeader(line)
			if err != nil {
				return nil, err
			}
			_ = n
			oldLine, oldLeft, newLeft = o, ol, nl
			if ol == 0 {
				// Pure insertion after line o: on the old side it sits
				// before line o+1.
				oldLine = o + 1
			}
		}
	}
	if err := sc.Err(); err != nil {
		return nil, fmt.Errorf("diff: %w", err)
	}
	flush(oldLine - 1)
	if len(out) == 0 && strings.TrimSpace(text) != "" {
		return nil, fmt.Errorf("diff: no file headers (`--- a/path` / `+++ b/path`) found")
	}
	return out, nil
}

// diffPath strips the `a/` / `b/` prefixes and timestamps; `/dev/null` is "".
func diffPath(s string) string {
	if i := strings.IndexByte(s, '\t'); i >= 0 {
		s = s[:i]
	}
	s = strings.TrimSpace(s)
	if s == "/dev/null" {
		return ""
	}
	if strings.HasPrefix(s, `"`) {
		if u, err := strconv.Unquote(s); err == nil {
			s = u
		}
	}
	for _, p := range []string{"a/", "b/"} {
		if strings.HasPrefix(s, p) {
			return s[len(p):]
		}
	}
	return s
}

// parseHunkHeader reads `@@ -o,ol +n,nl @@`.
func parseHunkHeader(line string) (o, ol, n, nl int, err error) {
	fields := strings.Fields(line)
	if len(fields) < 3 || !strings.HasPrefix(fields[1], "-") || !strings.HasPrefix(fields[2], "+") {
		return 0, 0, 0, 0, fmt.Errorf("diff: malformed hunk header %q", line)
	}
	parse := func(s string) (int, int, error) {
		start, count, found := strings.Cut(s, ",")
		a, err := strconv.Atoi(start)
		if err != nil {
			return 0, 0, err
		}
		b := 1
		if found {
			if b, err = strconv.Atoi(count); err != nil {
				return 0, 0, err
			}
		}
		return a, b, nil
	}
	if o, ol, err = parse(fields[1][1:]); err != nil {
		return 0, 0, 0, 0, fmt.Errorf("diff: malformed hunk header %q", line)
	}
	if n, nl, err = parse(fields[2][1:]); err != nil {
		return 0, 0, 0, 0, fmt.Errorf("diff: malformed hunk header %q", line)
	}
	return o, ol, n, nl, nil
}

func appendRange(rs []LineRange, r LineRange) []LineRange {
	if n := len(rs); n > 0 && r.Start <= rs[n-1].End+1 {
		if r.End > rs[n-1].End {
			rs[n-1].End = r.End
		}
		return rs
	}
	return append(rs, r)
}

// maxEditDistance bounds the line diff; larger differences are reported
// as a change of the whole file.
const maxEditDistance = 4000

// ChangedLines compares two texts line by line (Myers' algorithm) and
// returns the ranges of `head` lines that were added or modified, plus
// insertion points for pure deletions. ok is false when the difference
// exceeds the bound, in which case the whole file should be treated as
// changed.
func ChangedLines(base, head string) (ranges []LineRange, ok bool) {
	a := splitLines(base)
	b := splitLines(head)
	n, m := len(a), len(b)
	maxD := n + m
	if maxD > maxEditDistance {
		maxD = maxEditDistance
	}
	off := maxD + 1
	v := make([]int, 2*maxD+3)
	var trace [][]int
	found := false
	for d := 0; d <= maxD && !found; d++ {
		snapshot := make([]int, len(v))
		copy(snapshot, v)
		trace = append(trace, snapshot)
		for k := -d; k <= d; k += 2 {
			var x int
			if k == -d || (k != d && v[off+k-1] < v[off+k+1]) {
				x = v[off+k+1]
			} else {
				x = v[off+k-1] + 1
			}
			y := x - k
			for x < n && y < m && a[x] == b[y] {
				x++
				y++
			}
			v[off+k] = x
			if x >= n && y >= m {
				found = true
				break
			}
		}
	}
	if !found {
		return nil, false
	}
	// Backtrack to mark changed head lines.
	changed := make([]bool, m+1)
	x, y := n, m
	for d := len(trace) - 1; d > 0; d-- {
		vv := trace[d]
		k := x - y
		var prevK int
		if k == -d || (k != d && vv[off+k-1] < vv[off+k+1]) {
			prevK = k + 1
		} else {
			prevK = k - 1
		}
		prevX := vv[off+prevK]
		prevY := prevX - prevK
		for x > prevX && y > prevY {
			x--
			y--
		}
		if x == prevX {
			// Insertion of b[prevY].
			if prevY < m {
				changed[prevY] = true
			}
		} else {
			// Deletion of a[prevX]: mark the head line at the deletion point.
			mark := y
			if mark >= m {
				mark = m - 1
			}
			if mark >= 0 {
				changed[mark] = true
			}
		}
		x, y = prevX, prevY
	}
	for i := 0; i < m; i++ {
		if changed[i] {
			ranges = appendRange(ranges, LineRange{i + 1, i + 1})
		}
	}
	return ranges, true
}

func splitLines(s string) []string {
	if s == "" {
		return nil
	}
	s = strings.TrimSuffix(s, "\n")
	return strings.Split(s, "\n")
}
