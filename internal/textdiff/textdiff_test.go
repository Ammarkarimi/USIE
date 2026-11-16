package textdiff

import (
	"math/rand"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func apply(a, b []string, ops []Op) []string {
	var out []string
	for _, op := range ops {
		switch op.Kind {
		case Equal:
			out = append(out, a[op.A])
		case Insert:
			out = append(out, b[op.B])
		}
	}
	return out
}

func TestEditsReconstructRandomTexts(t *testing.T) {
	r := rand.New(rand.NewSource(1))
	words := []string{"a", "b", "c", "d", ""}
	gen := func() []string {
		n := r.Intn(30)
		out := make([]string, n)
		for i := range out {
			out[i] = words[r.Intn(len(words))]
		}
		return out
	}
	for i := 0; i < 2000; i++ {
		a, b := gen(), gen()
		ops, ok := Edits(a, b, 0)
		if !ok {
			t.Fatal("no script")
		}
		if got := apply(a, b, ops); strings.Join(got, "\n") != strings.Join(b, "\n") || len(got) != len(b) {
			t.Fatalf("case %d: %q -> %q gave %q", i, a, b, got)
		}
		// Old-side lines appear in order, each exactly once.
		next := 0
		for _, op := range ops {
			if op.Kind != Insert {
				if op.A != next {
					t.Fatalf("case %d: old index %d, want %d", i, op.A, next)
				}
				next++
			}
		}
		if next != len(a) {
			t.Fatalf("case %d: consumed %d of %d old lines", i, next, len(a))
		}
	}
}

func TestEditsBound(t *testing.T) {
	if _, ok := Edits([]string{"a", "b", "c"}, []string{"x", "y", "z"}, 2); ok {
		t.Error("bound not enforced")
	}
}

func TestUnified(t *testing.T) {
	old := "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n"
	new := "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\n"
	got, err := Unified("x.txt", "x.txt", old, new, 3)
	if err != nil {
		t.Fatal(err)
	}
	want := "--- a/x.txt\n+++ b/x.txt\n@@ -1,5 +1,5 @@\n one\n-two\n+TWO\n three\n four\n five\n@@ -8,3 +8,4 @@\n eight\n nine\n ten\n+eleven\n"
	if got != want {
		t.Errorf("got\n%s\nwant\n%s", got, want)
	}
	created, _ := Unified("/dev/null", "n.txt", "", "hello\n", 3)
	if created != "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+hello\n" {
		t.Errorf("created:\n%s", created)
	}
	deleted, _ := Unified("d.txt", "/dev/null", "a\nb\n", "", 3)
	if deleted != "--- a/d.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n" {
		t.Errorf("deleted:\n%s", deleted)
	}
}

// The output applies with the standard patch tool.
func TestUnifiedAppliesWithPatch(t *testing.T) {
	patchBin, err := exec.LookPath("patch")
	if err != nil {
		t.Skip("patch not installed")
	}
	r := rand.New(rand.NewSource(7))
	for i := 0; i < 50; i++ {
		var a, b []string
		for j := 0; j < 40; j++ {
			line := []string{"x", "y", "func f() {", "}", "return 1"}[r.Intn(5)]
			if r.Intn(4) > 0 {
				a = append(a, line)
			}
			if r.Intn(4) > 0 {
				b = append(b, line)
			}
		}
		oldText, newText := strings.Join(a, "\n")+"\n", strings.Join(b, "\n")+"\n"
		diff, err := Unified("f.txt", "f.txt", oldText, newText, 3)
		if err != nil {
			t.Fatal(err)
		}
		dir := t.TempDir()
		os.WriteFile(filepath.Join(dir, "f.txt"), []byte(oldText), 0o644)
		cmd := exec.Command(patchBin, "-p1", "--quiet")
		cmd.Dir = dir
		cmd.Stdin = strings.NewReader(diff)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("case %d: patch failed: %v %s\n%s", i, err, out, diff)
		}
		got, _ := os.ReadFile(filepath.Join(dir, "f.txt"))
		if string(got) != newText {
			t.Fatalf("case %d: patched text differs", i)
		}
	}
}
