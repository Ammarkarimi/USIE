// Package textdiff computes line differences (Myers' O(ND) algorithm) and
// renders them as unified diffs.
package textdiff

import (
	"fmt"
	"strings"
)

type OpKind int

const (
	Equal OpKind = iota
	Delete
	Insert
)

// Op is one line of an edit script. A is the 0-based line index in the old
// text (Equal, Delete), B in the new text (Equal, Insert).
type Op struct {
	Kind OpKind
	A, B int
}

// Lines splits text into lines without their terminators. A final
// newline does not produce an empty last line.
func Lines(s string) []string {
	if s == "" {
		return nil
	}
	return strings.Split(strings.TrimSuffix(s, "\n"), "\n")
}

// Edits returns an edit script turning a into b. ok is false when the edit
// distance exceeds maxD (no script is computed then).
func Edits(a, b []string, maxD int) (ops []Op, ok bool) {
	n, m := len(a), len(b)
	if maxD <= 0 || maxD > n+m {
		maxD = n + m
	}
	off := maxD + 1
	v := make([]int, 2*maxD+3)
	var trace [][]int
	found := false
	for d := 0; d <= maxD && !found; d++ {
		trace = append(trace, append([]int(nil), v...))
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
	x, y := n, m
	var rev []Op
	for d := len(trace) - 1; d >= 0; d-- {
		vv := trace[d]
		k := x - y
		var prevK int
		if d == 0 {
			prevK = 0
		} else if k == -d || (k != d && vv[off+k-1] < vv[off+k+1]) {
			prevK = k + 1
		} else {
			prevK = k - 1
		}
		prevX := 0
		if d > 0 {
			prevX = vv[off+prevK]
		}
		prevY := prevX - prevK
		for x > prevX && y > prevY {
			x--
			y--
			rev = append(rev, Op{Equal, x, y})
		}
		if d == 0 {
			break
		}
		if x == prevX {
			rev = append(rev, Op{Insert, x, prevY})
		} else {
			rev = append(rev, Op{Delete, prevX, y})
		}
		x, y = prevX, prevY
	}
	for i, j := 0, len(rev)-1; i < j; i, j = i+1, j-1 {
		rev[i], rev[j] = rev[j], rev[i]
	}
	return rev, true
}

// Unified renders the difference between old and new content of one file
// as a unified diff with the given lines of context. Created files have
// old "" and oldPath "/dev/null"; deleted files the reverse.
func Unified(oldPath, newPath, oldText, newText string, context int) (string, error) {
	a, b := Lines(oldText), Lines(newText)
	ops, ok := Edits(a, b, 0)
	if !ok {
		return "", fmt.Errorf("textdiff: difference too large")
	}
	var sb strings.Builder
	label := func(p, prefix string) string {
		if p == "/dev/null" {
			return p
		}
		return prefix + p
	}
	fmt.Fprintf(&sb, "--- %s\n+++ %s\n", label(oldPath, "a/"), label(newPath, "b/"))
	// Group changes into hunks with context.
	i := 0
	for i < len(ops) {
		if ops[i].Kind == Equal {
			i++
			continue
		}
		start := max(i-context, 0)
		end := i
		for end < len(ops) {
			if ops[end].Kind != Equal {
				end++
				continue
			}
			// Run of equal lines: continue the hunk if another change
			// follows within 2*context lines.
			run := end
			for run < len(ops) && ops[run].Kind == Equal {
				run++
			}
			if run < len(ops) && run-end <= 2*context {
				end = run
				continue
			}
			end = min(end+context, len(ops))
			break
		}
		hunk := ops[start:end]
		oldStart, newStart, oldLen, newLen := -1, -1, 0, 0
		for _, op := range hunk {
			switch op.Kind {
			case Equal:
				if oldStart < 0 {
					oldStart = op.A
				}
				if newStart < 0 {
					newStart = op.B
				}
				oldLen++
				newLen++
			case Delete:
				if oldStart < 0 {
					oldStart = op.A
				}
				oldLen++
			case Insert:
				if newStart < 0 {
					newStart = op.B
				}
				newLen++
			}
		}
		// Hunk header line numbers are 1-based; an empty side reports the
		// line before the hunk.
		os, ns := oldStart+1, newStart+1
		if oldLen == 0 {
			os = firstIndex(hunk, true)
		}
		if newLen == 0 {
			ns = firstIndex(hunk, false)
		}
		fmt.Fprintf(&sb, "@@ -%s +%s @@\n", rangeText(os, oldLen), rangeText(ns, newLen))
		for _, op := range hunk {
			switch op.Kind {
			case Equal:
				sb.WriteString(" " + a[op.A] + "\n")
			case Delete:
				sb.WriteString("-" + a[op.A] + "\n")
			case Insert:
				sb.WriteString("+" + b[op.B] + "\n")
			}
		}
		i = end
	}
	return sb.String(), nil
}

// firstIndex is the 0-based position before which the empty side of a hunk
// sits, i.e. the 1-based number of the last line before it.
func firstIndex(hunk []Op, old bool) int {
	if len(hunk) == 0 {
		return 0
	}
	if old {
		return hunk[0].A
	}
	return hunk[0].B
}

func rangeText(start, n int) string {
	if n == 1 {
		return fmt.Sprintf("%d", start)
	}
	return fmt.Sprintf("%d,%d", start, n)
}
