package analysis

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// engineBin locates a built engine. These are contract tests between the Go
// services and the Rust engine, so they need the real binary.
func engineBin(t *testing.T) string {
	t.Helper()
	if bin := os.Getenv("USIE_ENGINE_BIN"); bin != "" {
		return bin
	}
	for _, profile := range []string{"release", "debug"} {
		bin, _ := filepath.Abs(filepath.Join("..", "..", "engines", "target", profile, "usie-engine"))
		if _, err := os.Stat(bin); err == nil {
			return bin
		}
	}
	t.Skip("usie-engine binary not built (run `cargo build --release` in engines/)")
	return ""
}

func TestEngineContract(t *testing.T) {
	ctx := context.Background()
	e := Engine{Bin: engineBin(t), ParseTimeout: 5 * time.Second, MaxFileBytes: 1 << 20}

	info, err := e.Info(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if info.IRSchemaVersion != 1 || info.Fingerprint("python") == "" || info.Fingerprint("tsx") == "" || info.Fingerprint("cobol") != "" {
		t.Fatalf("info: %+v", info)
	}

	root := t.TempDir()
	files := map[string]string{
		"pkg/__init__.py": "",
		"pkg/a.py":        "from .b import helper\n\nclass Service:\n    def run(self):\n        helper()\n        self.missing()\n",
		"pkg/b.py":        "def helper():\n    \"\"\"Help.\"\"\"\n",
		"web/app.ts":      "import { helper } from './lib';\nimport React from 'react';\nexport const run = () => { helper(); React.use(); };\n",
		"web/lib.ts":      "export function helper(): void {}\n",
		"broken.py":       "def broken(:\n",
	}
	var requests []ParseRequest
	var paths []string
	for _, p := range []string{"broken.py", "pkg/__init__.py", "pkg/a.py", "pkg/b.py", "web/app.ts", "web/lib.ts"} {
		full := filepath.Join(root, filepath.FromSlash(p))
		os.MkdirAll(filepath.Dir(full), 0o755)
		os.WriteFile(full, []byte(files[p]), 0o644)
		lang := "python"
		if strings.HasSuffix(p, ".ts") {
			lang = "typescript"
		}
		requests = append(requests, ParseRequest{ID: p, Path: p, Language: lang})
		paths = append(paths, p)
	}
	requests = append(requests, ParseRequest{ID: "escape", Path: "../outside.py", Language: "python"})

	irs := map[string][]byte{}
	var order []string
	err = e.Parse(ctx, root, requests, func(r ParseResult) error {
		order = append(order, r.ID)
		if r.ID == "escape" {
			if r.Status != "error" || r.Code != "path_escape" {
				t.Errorf("escape: %+v", r)
			}
			return nil
		}
		if r.Status != "ok" {
			t.Errorf("%s: %s %s", r.Path, r.Code, r.Message)
		}
		irs[r.Path] = r.IR
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(order) != len(requests) || order[0] != "broken.py" || order[len(order)-1] != "escape" {
		t.Fatalf("result order: %v", order)
	}

	var resolveFiles []ResolveFile
	for _, p := range paths {
		ir := irs[p]
		resolveFiles = append(resolveFiles, ResolveFile{Path: p, Open: func() ([]byte, error) { return ir, nil }})
	}
	res, err := e.Resolve(ctx, ResolveConfig{Paths: paths, Dependencies: []DeclaredDependency{{Ecosystem: "npm", Name: "react"}}}, resolveFiles)
	if err != nil {
		t.Fatal(err)
	}
	edge := func(src, kind, dst string) *EdgeRecord {
		for i, e := range res.Edges {
			if e.Src == src && e.Kind == kind && e.Dst == dst {
				return &res.Edges[i]
			}
		}
		t.Errorf("missing edge %s -%s-> %s", src, kind, dst)
		return &EdgeRecord{}
	}
	if e := edge("sym:pkg/a.py#Service.run", "CALLS", "sym:pkg/b.py#helper"); e.Confidence != "confirmed" || e.Rule == "" || e.Evidence == nil || e.Evidence.Line != 5 {
		t.Errorf("python call edge: %+v", e)
	}
	edge("sym:web/app.ts#run", "CALLS", "sym:web/lib.ts#helper")
	if e := edge("sym:web/app.ts#run", "CALLS", "ext:npm:react"); e.Confidence != "confirmed" {
		t.Errorf("declared dependency: %+v", e)
	}
	edge("file:pkg/a.py", "IMPORTS", "file:pkg/b.py")

	var helper *NodeRecord
	for i, n := range res.Nodes {
		if n.Key == "sym:pkg/b.py#helper" {
			helper = &res.Nodes[i]
		}
	}
	if helper == nil || helper.Kind != "function" || helper.Doc != "Help." || helper.Span == nil || helper.Span.StartLine != 1 || helper.Attrs["language"] != "python" {
		t.Errorf("node record: %+v", helper)
	}
	unresolved := 0
	for _, r := range res.References {
		if r.Outcome == "unresolved" {
			unresolved++
			if r.Reason == "" || r.Target != "" || r.Confidence != "unknown" {
				t.Errorf("unresolved reference must carry a reason and no target: %+v", r)
			}
		}
	}
	if unresolved != 1 || res.Stats.Files != 6 || res.Stats.ReferencesByOutcome["unresolved"] != 1 {
		t.Errorf("unresolved=%d stats=%+v", unresolved, res.Stats)
	}
}

func TestEngineFailuresAreErrors(t *testing.T) {
	ctx := context.Background()
	e := Engine{Bin: engineBin(t)}

	// Malformed IR is rejected by the engine with a structured error.
	_, err := e.Resolve(ctx, ResolveConfig{}, []ResolveFile{{Path: "a.py", Open: func() ([]byte, error) { return []byte(`{"bogus":true}`), nil }}})
	if err == nil || !strings.Contains(err.Error(), "invalid_input") {
		t.Errorf("malformed IR: %v", err)
	}
	if err := e.Parse(ctx, filepath.Join(t.TempDir(), "missing"), []ParseRequest{{ID: "1", Path: "a.py", Language: "python"}}, func(ParseResult) error { return nil }); err == nil {
		t.Error("missing root should fail")
	}
	cancelled, cancel := context.WithCancel(ctx)
	cancel()
	if _, err := e.Info(cancelled); err == nil {
		t.Error("cancelled context should fail")
	}
	if _, err := (Engine{Bin: filepath.Join(t.TempDir(), "nope")}).Info(ctx); err == nil {
		t.Error("missing binary should fail")
	}
}
