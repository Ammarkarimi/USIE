package pipeline

import (
	"archive/zip"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/ammarkarimi/usie/database"
	"github.com/ammarkarimi/usie/internal/platform/blob"
	"github.com/ammarkarimi/usie/internal/platform/config"
	"github.com/ammarkarimi/usie/internal/store"
	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
	analysis "github.com/ammarkarimi/usie/services/semantic-analysis"
)

type env struct {
	t      *testing.T
	ctx    context.Context
	pool   *pgxpool.Pool
	st     *store.Store
	queue  *orchestrator.Queue
	graph  *graph.Postgres
	blobs  *blob.FS
	pipe   *Pipeline
	orgID  string
	userID string
	wsID   string
	root   string // allowed local root
}

func engineBin(t *testing.T) string {
	if bin := os.Getenv("USIE_ENGINE_BIN"); bin != "" {
		return bin
	}
	for _, profile := range []string{"release", "debug"} {
		bin, _ := filepath.Abs(filepath.Join("..", "..", "engines", "target", profile, "usie-engine"))
		if _, err := os.Stat(bin); err == nil {
			return bin
		}
	}
	t.Skip("usie-engine binary not built")
	return ""
}

func setup(t *testing.T) *env {
	t.Helper()
	url := os.Getenv("USIE_TEST_DATABASE_URL")
	if url == "" {
		t.Skip("USIE_TEST_DATABASE_URL not set")
	}
	ctx := context.Background()
	pool, err := pgxpool.New(ctx, url)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(pool.Close)
	if _, err := database.Migrate(ctx, pool); err != nil {
		t.Fatal(err)
	}
	e := &env{t: t, ctx: ctx, pool: pool, st: store.New(pool), queue: orchestrator.NewQueue(pool), graph: graph.NewPostgres(pool)}
	// The queue is global: make sure nothing from other tests is runnable.
	pool.Exec(ctx, `UPDATE analysis_jobs SET status = 'cancelled' WHERE status IN ('queued', 'running')`)

	name := fmt.Sprintf("pipe-%d-%d", os.Getpid(), time.Now().UnixNano())
	e.userID, e.orgID, err = e.st.Register(ctx, "open", name+"@example.test", "Tester", "x", name)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		pool.Exec(ctx, `DELETE FROM organizations WHERE id = $1`, e.orgID)
		pool.Exec(ctx, `DELETE FROM users WHERE id = $1`, e.userID)
	})
	ws, _ := e.st.ListWorkspaces(ctx, e.orgID)
	e.wsID = ws[0].ID

	data := t.TempDir()
	e.root, _ = filepath.EvalSymlinks(t.TempDir())
	e.blobs, err = blob.NewFS(filepath.Join(data, "blobs"))
	if err != nil {
		t.Fatal(err)
	}
	cfg := config.Config{
		DataDir: data, AllowedLocalRoots: []string{e.root}, MaxRepoFiles: 1000, MaxRepoBytes: 50 << 20, MaxFileBytes: 1 << 20,
		GitTimeout: time.Minute, EngineTimeout: time.Minute, ParseTimeout: 5 * time.Second,
	}
	engine := analysis.Engine{Bin: engineBin(t), ParseTimeout: cfg.ParseTimeout, MaxFileBytes: cfg.MaxFileBytes}
	info, err := engine.Info(ctx)
	if err != nil {
		t.Fatal(err)
	}
	e.pipe = &Pipeline{Pool: pool, Blobs: e.blobs, Engine: engine, Info: info, Cfg: cfg, Log: slog.New(slog.NewTextHandler(io.Discard, nil))}
	return e
}

func (e *env) write(repo string, files map[string]string) {
	for name, content := range files {
		p := filepath.Join(e.root, repo, filepath.FromSlash(name))
		os.MkdirAll(filepath.Dir(p), 0o755)
		if err := os.WriteFile(p, []byte(content), 0o644); err != nil {
			e.t.Fatal(err)
		}
	}
}

func (e *env) repo(name, kind, location string) string {
	r, err := e.st.CreateRepository(e.ctx, e.orgID, e.userID, store.Repository{WorkspaceID: e.wsID, Name: name, SourceKind: kind, SourceLocation: location})
	if err != nil {
		e.t.Fatal(err)
	}
	return r.ID
}

// analyze queues a version and runs a worker until that job is terminal.
func (e *env) analyze(repoID, uploadKey string) (store.Version, orchestrator.Job) {
	e.t.Helper()
	versionID, jobID, err := e.st.CreateVersionAndJob(e.ctx, e.orgID, e.userID, repoID, "", uploadKey)
	if err != nil {
		e.t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(e.ctx, 60*time.Second)
	defer cancel()
	w := &orchestrator.Worker{Queue: e.queue, Handler: e.pipe.Handle, Lease: 10 * time.Second, PollInterval: 50 * time.Millisecond,
		Log: slog.New(slog.NewTextHandler(io.Discard, nil)), OnFinish: func(*orchestrator.Job, string, time.Duration) { cancel() }}
	if err := w.Run(ctx); err != nil {
		e.t.Fatal(err)
	}
	v, err := e.st.GetVersion(e.ctx, e.orgID, versionID)
	if err != nil {
		e.t.Fatal(err)
	}
	job, err := e.queue.Get(e.ctx, e.orgID, jobID)
	if err != nil {
		e.t.Fatal(err)
	}
	return v, job
}

func TestAnalyzeLocalRepositoryIncrementally(t *testing.T) {
	e := setup(t)
	e.write("shop", map[string]string{
		"pyproject.toml":          "[project]\nname = \"shop\"\ndependencies = [\"requests>=2\", \"PyYAML\"]\n",
		"shop/__init__.py":        "",
		"shop/cart.py":            "import requests\nfrom .pricing import total\n\nclass Cart:\n    def checkout(self, items):\n        requests.post('u')\n        return total(items)\n",
		"shop/pricing.py":         "def total(items):\n    return sum(items)\n\ndef unused():\n    pass\n",
		"web/package.json":        `{"name":"web","dependencies":{"react":"^18"}}`,
		"web/src/App.tsx":         "import React from 'react';\nimport { fmt } from './fmt';\nexport const App = () => <b>{fmt(1)}</b>;\n",
		"web/src/fmt.ts":          "export function fmt(n: number) { return String(n); }\n",
		"web/src/broken.ts":       "export function ok() {}\nconst x = ;\n",
		"node_modules/x/index.js": "module.exports = 1;\n",
		"vendor/lib/v.py":         "def vendored(): pass\n",
		"gen/api_pb2.py":          "# Generated by the protocol buffer compiler.  DO NOT EDIT!\nx = 1\n",
		"README.md":               "# Shop\n",
		"main.go":                 "package main\n\nfunc main() {}\n",
		"logo.png":                "\x89PNG\x00\x00\x00",
	})
	repoID := e.repo("shop", "local", filepath.Join(e.root, "shop"))

	v1, job := e.analyze(repoID, "")
	if v1.Status != "ready" || job.Status != "succeeded" {
		t.Fatalf("v1: status=%s job=%s error=%q %q", v1.Status, job.Status, v1.Error, job.Error)
	}
	var stats Stats
	if err := json.Unmarshal(v1.Stats, &stats); err != nil {
		t.Fatal(err)
	}
	if stats.Files != 13 || stats.ByLanguage["python"] != 5 || stats.ByLanguage["typescript"] != 3 || stats.ByLanguage["go"] != 1 {
		t.Errorf("inventory: files=%d by_language=%v", stats.Files, stats.ByLanguage)
	}
	if stats.ByParseStatus["parsed"] != 5 || stats.ByParseStatus["parsed_with_errors"] != 1 || stats.ByParseStatus["skipped"] != 2 || stats.ByParseStatus["unsupported"] != 5 {
		t.Errorf("parse statuses: %v", stats.ByParseStatus)
	}
	if stats.ParseCacheMisses != 6 || stats.ParseCacheHits != 0 || stats.Changes["added"] != 13 || stats.DroppedEdges != 0 {
		t.Errorf("first run: misses=%d hits=%d changes=%v dropped=%d", stats.ParseCacheMisses, stats.ParseCacheHits, stats.Changes, stats.DroppedEdges)
	}
	if strings.Join(stats.BuildSystems, ",") != "npm,python" || stats.Dependencies != 3 || len(v1.TreeHash) != 64 {
		t.Errorf("manifests: %v deps=%d", stats.BuildSystems, stats.Dependencies)
	}

	scope := graph.Scope{OrganizationID: e.orgID, VersionID: v1.ID}
	hood := func(s graph.Scope, key, kind string, dir graph.Direction) map[string]graph.Edge {
		sub, err := e.graph.Neighborhood(e.ctx, s, key, graph.NeighborhoodQuery{Depth: 1, Direction: dir, Filter: graph.EdgeFilter{Kinds: []string{kind}}})
		if err != nil {
			t.Fatalf("neighborhood %s: %v", key, err)
		}
		byID := map[int32]string{}
		for _, n := range sub.Nodes {
			byID[n.ID] = n.Key
		}
		out := map[string]graph.Edge{}
		for _, ed := range sub.Edges {
			out[byID[ed.Src]+" -> "+byID[ed.Dst]] = ed
		}
		return out
	}
	calls := hood(scope, "sym:shop/cart.py#Cart.checkout", graph.RelCalls, graph.Out)
	if ed := calls["sym:shop/cart.py#Cart.checkout -> sym:shop/pricing.py#total"]; ed.Confidence != graph.Confirmed || ed.EvidencePath != "shop/cart.py" || ed.EvidenceLine != 7 {
		t.Errorf("call edge: %+v (all: %v)", ed, calls)
	}
	if ed := calls["sym:shop/cart.py#Cart.checkout -> ext:python:requests"]; ed.Confidence != graph.Confirmed || ed.Rule != "py.import.declared-dependency" {
		t.Errorf("external call: %+v", ed)
	}
	if refs := hood(scope, "sym:web/src/App.tsx#App", graph.RelCalls, graph.Out); refs["sym:web/src/App.tsx#App -> sym:web/src/fmt.ts#fmt"].Confidence != graph.Confirmed {
		t.Errorf("tsx call: %v", refs)
	}
	deps := hood(scope, "repo", graph.RelDependsOn, graph.Out)
	// PyYAML is declared but imported as `yaml` nowhere: it gets a distribution node, not a guessed import match.
	for _, want := range []string{"repo -> ext:python:requests", "repo -> ext:pypi:pyyaml", "repo -> ext:npm:react"} {
		if deps[want].Rule != "manifest.declared-dependency" {
			t.Errorf("missing dependency edge %s in %v", want, deps)
		}
	}
	if _, err := e.graph.NodeByKey(e.ctx, scope, "sym:vendor/lib/v.py#vendored"); err == nil {
		t.Error("vendored code must not be analyzed")
	}
	if n, err := e.graph.NodeByKey(e.ctx, scope, "file:main.go"); err != nil || n.Attrs["parse_status"] != "unsupported" || n.Attrs["language"] != "go" {
		t.Errorf("unsupported language file node: %+v %v", n, err)
	}

	f, err := e.st.GetFile(e.ctx, e.orgID, v1.ID, "web/src/broken.ts")
	if err != nil || f.ParseStatus != "parsed_with_errors" || f.DiagnosticCount == 0 || f.SymbolCount != 2 || !f.ContentStored {
		t.Errorf("broken.ts: %+v %v", f, err)
	}
	if f, _ := e.st.GetFile(e.ctx, e.orgID, v1.ID, "main.go"); f.ParseStatus != "unsupported" || f.ParseDetail != "no parser adapter for go" {
		t.Errorf("main.go: %+v", f)
	}
	if f, _ := e.st.GetFile(e.ctx, e.orgID, v1.ID, "logo.png"); f.ContentStored || f.Encoding != "binary" {
		t.Errorf("logo.png: %+v", f)
	}
	dirs, rootFiles, err := e.st.ListDir(e.ctx, e.orgID, v1.ID, "")
	if err != nil || len(dirs) != 4 || dirs[0].Name != "gen" || dirs[1].Files != 3 || len(rootFiles) != 4 {
		t.Errorf("ListDir: dirs=%+v files=%d err=%v", dirs, len(rootFiles), err)
	}
	if content, err := blob.GetBytes(e.ctx, e.blobs, SourceKey(e.orgID, f.ContentHash), 1<<20); err != nil || !strings.Contains(string(content), "const x = ;") {
		t.Errorf("stored content: %q %v", content, err)
	}
	unresolved, total, err := e.st.ListReferences(e.ctx, e.orgID, v1.ID, store.ReferenceQuery{Outcome: "unresolved", Limit: 50})
	if err != nil || total != int64(len(unresolved)) {
		t.Fatal(err)
	}
	for _, r := range unresolved {
		if r.Reason == "" || r.TargetKey != "" {
			t.Errorf("unresolved reference without reason: %+v", r)
		}
	}

	// ---- second version: one file modified, one added, one removed ----
	e.write("shop", map[string]string{
		"shop/pricing.py": "def total(items, tax=0):\n    return sum(items) + tax\n",
		"shop/tax.py":     "from .pricing import total\n\ndef with_tax(items):\n    return total(items, 1)\n",
	})
	os.Remove(filepath.Join(e.root, "shop", "README.md"))
	v2, job := e.analyze(repoID, "")
	if v2.Status != "ready" {
		t.Fatalf("v2: %s %q", v2.Status, job.Error)
	}
	json.Unmarshal(v2.Stats, &stats)
	if stats.ParseCacheMisses != 2 || stats.ParseCacheHits != 5 {
		t.Errorf("incremental parse: misses=%d hits=%d", stats.ParseCacheMisses, stats.ParseCacheHits)
	}
	if stats.Changes["modified"] != 1 || stats.Changes["added"] != 1 || stats.Changes["removed"] != 1 || stats.Changes["unchanged"] != 11 {
		t.Errorf("changes: %v", stats.Changes)
	}
	if v2.ParentVersionID == nil || *v2.ParentVersionID != v1.ID || v2.Sequence != 2 || v2.TreeHash == v1.TreeHash {
		t.Errorf("v2 lineage: %+v", v2)
	}
	if f, _ := e.st.GetFile(e.ctx, e.orgID, v2.ID, "shop/pricing.py"); f.Change != "modified" {
		t.Errorf("pricing.py change: %s", f.Change)
	}
	d, err := e.graph.Diff(e.ctx, scope, graph.Scope{OrganizationID: e.orgID, VersionID: v2.ID}, 100)
	if err != nil {
		t.Fatal(err)
	}
	keys := func(nodes []graph.Node) string {
		var out []string
		for _, n := range nodes {
			out = append(out, n.Key)
		}
		return strings.Join(out, " ")
	}
	if keys(d.Added) != "file:shop/tax.py sym:shop/tax.py#with_tax" || keys(d.Removed) != "file:README.md sym:shop/pricing.py#unused" || keys(d.Changed) != "sym:shop/pricing.py#total" {
		t.Errorf("diff: +[%s] -[%s] ~[%s]", keys(d.Added), keys(d.Removed), keys(d.Changed))
	}
	// The first snapshot is immutable: still queryable and unchanged.
	if _, err := e.graph.NodeByKey(e.ctx, scope, "sym:shop/pricing.py#unused"); err != nil {
		t.Errorf("v1 snapshot changed: %v", err)
	}

	// Re-running a finished version's job is idempotent (at-least-once delivery).
	v3, _ := e.analyze(repoID, "")
	json.Unmarshal(v3.Stats, &stats)
	if v3.TreeHash != v2.TreeHash || stats.ParseCacheMisses != 0 || stats.Changes["unchanged"] != 13 {
		t.Errorf("unchanged re-analysis: misses=%d changes=%v", stats.ParseCacheMisses, stats.Changes)
	}
}

func TestAnalyzeUploadAndFailures(t *testing.T) {
	e := setup(t)
	upload := func(build func(w *zip.Writer)) string {
		var buf bytes.Buffer
		w := zip.NewWriter(&buf)
		build(w)
		w.Close()
		key := UploadKey(e.orgID, fmt.Sprintf("u%d", time.Now().UnixNano()))
		if err := blob.PutBytes(e.ctx, e.blobs, key, buf.Bytes()); err != nil {
			t.Fatal(err)
		}
		return key
	}
	repoID := e.repo("uploaded", "upload", "")
	good := upload(func(w *zip.Writer) {
		f, _ := w.Create("proj-main/app.py")
		f.Write([]byte("def main():\n    helper()\n\ndef helper():\n    pass\n"))
	})
	v, job := e.analyze(repoID, good)
	if v.Status != "ready" {
		t.Fatalf("upload: %s %q", v.Status, job.Error)
	}
	if _, err := e.graph.NodeByKey(e.ctx, graph.Scope{OrganizationID: e.orgID, VersionID: v.ID}, "sym:app.py#helper"); err != nil {
		t.Errorf("uploaded source not analyzed: %v", err)
	}

	// A hostile archive fails permanently (no retries) with a clear error and no partial data.
	evil := upload(func(w *zip.Writer) {
		f, _ := w.Create("../../escape.py")
		f.Write([]byte("x = 1\n"))
	})
	v, job = e.analyze(repoID, evil)
	if v.Status != "failed" || job.Status != "failed" || job.Attempts != 1 || !strings.Contains(v.Error, "unsafe path") {
		t.Errorf("hostile archive: version=%s job=%s attempts=%d error=%q", v.Status, job.Status, job.Attempts, v.Error)
	}
	if files, total, _ := e.st.ListFiles(e.ctx, e.orgID, v.ID, store.FileQuery{Limit: 10}); total != 0 || len(files) != 0 {
		t.Errorf("failed version has %d files", total)
	}
	if _, err := e.graph.Stats(e.ctx, graph.Scope{OrganizationID: e.orgID, VersionID: v.ID}); err == nil {
		t.Error("failed version has a graph snapshot")
	}

	// Local paths outside the allowed roots are refused.
	outside := e.repo("outside", "local", t.TempDir())
	if v, _ := e.analyze(outside, ""); v.Status != "failed" || !strings.Contains(v.Error, "outside the allowed roots") {
		t.Errorf("outside root: %s %q", v.Status, v.Error)
	}

	// Cancelling a queued job cancels its version without running it.
	versionID, jobID, _ := e.st.CreateVersionAndJob(e.ctx, e.orgID, e.userID, repoID, "", good)
	if j, err := e.queue.RequestCancel(e.ctx, e.orgID, jobID); err != nil || j.Status != "cancelled" {
		t.Fatalf("cancel: %+v %v", j, err)
	}
	if v, _ := e.st.GetVersion(e.ctx, e.orgID, versionID); v.Status != "cancelled" {
		t.Errorf("cancelled version status: %s", v.Status)
	}
	if j, _ := e.queue.Claim(e.ctx, "00000000-0000-0000-0000-000000000000", time.Second); j != nil {
		t.Errorf("cancelled job was claimed: %+v", j)
	}
	// Another organization cannot see or cancel the job.
	if _, err := e.queue.RequestCancel(e.ctx, "00000000-0000-0000-0000-000000000001", jobID); err != orchestrator.ErrNotFound {
		t.Errorf("cross-tenant cancel: %v", err)
	}
}
