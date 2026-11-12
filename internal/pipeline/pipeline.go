// Package pipeline implements the analyze_version job: acquire a repository
// revision, inventory it, parse what changed, resolve relationships, and
// store the resulting knowledge-graph snapshot atomically.
//
// The job is idempotent: re-running it for the same version replaces any
// partial results, so at-least-once delivery from the queue is safe.
package pipeline

import (
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/ammarkarimi/usie/internal/platform/blob"
	"github.com/ammarkarimi/usie/internal/platform/config"
	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
	ingestion "github.com/ammarkarimi/usie/services/repository-ingestion"
	analysis "github.com/ammarkarimi/usie/services/semantic-analysis"
)

type Pipeline struct {
	Pool   *pgxpool.Pool
	Blobs  *blob.FS
	Engine analysis.Engine
	Info   analysis.Info
	Cfg    config.Config
	Log    *slog.Logger
}

// SourceKey is the blob key of a source file's content.
func SourceKey(orgID, contentHash string) string {
	return "org/" + orgID + "/src/" + contentHash[:2] + "/" + contentHash
}

// UploadKey is the blob key of an uploaded archive.
func UploadKey(orgID, id string) string { return "org/" + orgID + "/uploads/" + id + ".zip" }

func irKey(orgID, fingerprint, contentHash string) string {
	sum := sha256.Sum256([]byte(fingerprint))
	return "org/" + orgID + "/ir/" + hex.EncodeToString(sum[:8]) + "/" + contentHash[:2] + "/" + contentHash
}

type fileState struct {
	ingestion.File
	stored      bool
	parserLang  string
	parseStatus string
	parseDetail string
	change      string
	symbols     int
	diagnostics int
	irKey       string
}

// Stats is stored on the version and shown in the UI. Every number is
// measured during the run; nothing is estimated.
type Stats struct {
	Files            int              `json:"files"`
	Bytes            int64            `json:"bytes"`
	Lines            int64            `json:"lines"`
	ByLanguage       map[string]int   `json:"files_by_language"`
	ByCategory       map[string]int   `json:"files_by_category"`
	ByParseStatus    map[string]int   `json:"files_by_parse_status"`
	Changes          map[string]int   `json:"changes"`
	ParseCacheHits   int              `json:"parse_cache_hits"`
	ParseCacheMisses int              `json:"parse_cache_misses"`
	Symbols          int64            `json:"symbols"`
	Graph            graphStats       `json:"graph"`
	References       map[string]int64 `json:"references_by_outcome"`
	DroppedEdges     int              `json:"dropped_edges"`
	BuildSystems     []string         `json:"build_systems"`
	Dependencies     int              `json:"declared_dependencies"`
	ManifestProblems any              `json:"manifest_problems"`
	SkippedSymlinks  int              `json:"skipped_symlinks"`
	SkippedIgnored   int              `json:"skipped_ignored"`
	EngineVersion    string           `json:"engine_version"`
	DurationsMS      map[string]int64 `json:"durations_ms"`
}

type graphStats struct {
	Nodes             int            `json:"nodes"`
	Edges             int            `json:"edges"`
	EdgesByKind       map[string]int `json:"edges_by_kind"`
	EdgesByConfidence map[string]int `json:"edges_by_confidence"`
}

// Handle is the orchestrator.Handler for analyze_version jobs.
func (p *Pipeline) Handle(ctx context.Context, job *orchestrator.Job, report func(string, any)) error {
	var (
		repoName, sourceKind, location, defaultRef string
		requestedRef, uploadKey                    string
		sequence                                   int
	)
	err := p.Pool.QueryRow(ctx, `
		SELECT r.name, r.source_kind, r.source_location, r.default_ref, v.requested_ref, v.upload_key, v.sequence
		  FROM repository_versions v JOIN repositories r ON r.id = v.repository_id
		 WHERE v.organization_id = $1 AND v.id = $2`, job.OrgID, job.VersionID).
		Scan(&repoName, &sourceKind, &location, &defaultRef, &requestedRef, &uploadKey, &sequence)
	if err != nil {
		return orchestrator.Permanent(fmt.Errorf("load version: %w", err))
	}
	if _, err := p.Pool.Exec(ctx, `UPDATE repository_versions SET status = 'analyzing', error = '' WHERE id = $1 AND status IN ('pending', 'analyzing')`, job.VersionID); err != nil {
		return err
	}

	durations := map[string]int64{}
	stage := func(name string) func() {
		start := time.Now()
		report(name, map[string]any{})
		return func() { durations[name] = time.Since(start).Milliseconds() }
	}

	work := filepath.Join(p.Cfg.WorkDir(), job.ID+"-"+fmt.Sprint(job.Attempts))
	if err := os.MkdirAll(work, 0o700); err != nil {
		return err
	}
	defer os.RemoveAll(work)
	limits := ingestion.ScanLimits{MaxFiles: p.Cfg.MaxRepoFiles, MaxBytes: p.Cfg.MaxRepoBytes, MaxFileBytes: p.Cfg.MaxFileBytes}

	// ---- acquire --------------------------------------------------------
	done := stage("fetching")
	var root string
	var git *ingestion.GitMetadata
	commitSHA, branch := "", ""
	switch sourceKind {
	case "git":
		ref := requestedRef
		if ref == "" {
			ref = defaultRef
		}
		root = filepath.Join(work, "src")
		gitCtx, cancel := context.WithTimeout(ctx, p.Cfg.GitTimeout)
		git, err = ingestion.Clone(gitCtx, location, ref, root, ingestion.GitOptions{AllowPrivateHosts: p.Cfg.GitAllowPrivateHosts})
		cancel()
		if err != nil {
			if errors.Is(err, ingestion.ErrInvalidGitURL) || strings.Contains(err.Error(), "invalid ref") {
				return orchestrator.Permanent(err)
			}
			return fmt.Errorf("clone: %w", err)
		}
		commitSHA, branch = git.HeadSHA, git.Branch
	case "upload":
		if uploadKey == "" {
			return orchestrator.Permanent(errors.New("version has no uploaded archive"))
		}
		root = filepath.Join(work, "src")
		if err := os.MkdirAll(root, 0o700); err != nil {
			return err
		}
		if err := p.extractUpload(ctx, uploadKey, work, root, limits); err != nil {
			return orchestrator.Permanent(err)
		}
	case "local":
		if root, err = ingestion.ResolveLocalPath(location, p.Cfg.AllowedLocalRoots); err != nil {
			return orchestrator.Permanent(err)
		}
		commitSHA, branch = ingestion.ReadLocalHead(root)
	default:
		return orchestrator.Permanent(fmt.Errorf("unknown source kind %q", sourceKind))
	}
	done()

	// ---- inventory ------------------------------------------------------
	done = stage("scanning")
	manifests := ingestion.NewManifestCollector()
	var files []*fileState
	scan, err := ingestion.Scan(ctx, root, limits, func(f ingestion.File, content []byte) error {
		st := &fileState{File: f}
		if content != nil && f.Encoding != "binary" {
			key := SourceKey(job.OrgID, f.ContentHash)
			exists, err := p.Blobs.Exists(ctx, key)
			if err != nil {
				return err
			}
			if !exists {
				if err := blob.PutBytes(ctx, p.Blobs, key, content); err != nil {
					return err
				}
			}
			st.stored = true
			if !f.Vendored && ingestion.IsManifest(f.Path) {
				manifests.Add(f.Path, content)
			}
		}
		files = append(files, st)
		return nil
	})
	if err != nil {
		if errors.Is(err, ingestion.ErrLimit) {
			return orchestrator.Permanent(err)
		}
		return fmt.Errorf("scan: %w", err)
	}
	sort.Slice(files, func(i, j int) bool { return files[i].Path < files[j].Path })
	facts := manifests.Facts()
	done()

	// ---- incremental comparison with the previous ready version ---------
	parentID, parentFiles, err := p.parentVersion(ctx, job, sequence)
	if err != nil {
		return err
	}
	stats := Stats{
		Files: len(files), Bytes: scan.TotalBytes, ByLanguage: map[string]int{}, ByCategory: map[string]int{}, ByParseStatus: map[string]int{},
		Changes: map[string]int{"added": 0, "modified": 0, "unchanged": 0, "removed": 0}, BuildSystems: facts.BuildSystems,
		Dependencies: len(facts.Dependencies), ManifestProblems: facts.Problems, SkippedSymlinks: scan.SkippedSymlinks,
		SkippedIgnored: scan.SkippedIgnored, EngineVersion: p.Info.Version, DurationsMS: durations,
	}
	if facts.Problems == nil {
		stats.ManifestProblems = []any{}
	}
	if stats.BuildSystems == nil {
		stats.BuildSystems = []string{}
	}
	for _, f := range files {
		prev, existed := parentFiles[f.Path]
		switch {
		case !existed:
			f.change = "added"
		case prev == f.ContentHash:
			f.change = "unchanged"
		default:
			f.change = "modified"
		}
		delete(parentFiles, f.Path)
		stats.Changes[f.change]++
		stats.Lines += int64(f.Lines)
		lang := f.Language
		if lang == "" {
			lang = "other"
		}
		stats.ByLanguage[lang]++
		stats.ByCategory[f.Category]++
	}
	stats.Changes["removed"] = len(parentFiles)

	// ---- parse ----------------------------------------------------------
	done = stage("parsing")
	if err := p.parse(ctx, job, files, &stats, report); err != nil {
		return err
	}
	for _, f := range files {
		stats.ByParseStatus[f.parseStatus]++
	}
	done()

	// ---- resolve --------------------------------------------------------
	done = stage("resolving")
	cfg := analysis.ResolveConfig{TsConfigs: facts.TsConfigs, Packages: facts.Packages}
	var inputs []analysis.ResolveFile
	for _, f := range files {
		cfg.Paths = append(cfg.Paths, f.Path)
		if f.irKey != "" {
			key := f.irKey
			inputs = append(inputs, analysis.ResolveFile{Path: f.Path, Open: func() ([]byte, error) { return p.readIR(ctx, key) }})
		}
	}
	seenDep := map[analysis.DeclaredDependency]bool{}
	for _, d := range facts.Dependencies {
		if d.Ecosystem == "npm" || d.Ecosystem == "pypi" {
			dep := analysis.DeclaredDependency{Ecosystem: d.Ecosystem, Name: d.Name}
			if !seenDep[dep] {
				seenDep[dep] = true
				cfg.Dependencies = append(cfg.Dependencies, dep)
			}
		}
	}
	engineCtx, cancel := context.WithTimeout(ctx, p.Cfg.EngineTimeout)
	resolution, err := p.Engine.Resolve(engineCtx, cfg, inputs)
	cancel()
	if err != nil {
		return fmt.Errorf("resolve: %w", err)
	}
	done()

	// ---- build and store ------------------------------------------------
	done = stage("storing")
	nodes, edges, dropped, err := buildGraph(repoName, files, facts, resolution)
	if err != nil {
		return orchestrator.Permanent(fmt.Errorf("graph validation failed: %w", err))
	}
	if dropped > 0 {
		p.Log.Warn("relationships referenced unknown nodes and were dropped", "job_id", job.ID, "count", dropped)
	}
	stats.DroppedEdges = dropped
	stats.Symbols = resolution.Stats.Symbols
	stats.References = resolution.Stats.ReferencesByOutcome
	if stats.References == nil {
		stats.References = map[string]int64{}
	}
	stats.Graph = graphStats{Nodes: len(nodes), Edges: len(edges), EdgesByKind: map[string]int{}, EdgesByConfidence: map[string]int{}}
	for _, e := range edges {
		stats.Graph.EdgesByKind[e.Kind]++
		stats.Graph.EdgesByConfidence[e.Confidence]++
	}
	storeStart := time.Now()
	durations["storing"] = 0
	persist := func(tx pgx.Tx) error {
		durations["storing"] = time.Since(storeStart).Milliseconds()
		return p.persist(ctx, tx, job, files, facts, resolution, nodes, edges, git, commitSHA, branch, scan.TreeHash, parentID, stats)
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	if err := pgx.BeginFunc(ctx, p.Pool, persist); err != nil {
		return fmt.Errorf("store results: %w", err)
	}
	done()
	report("complete", map[string]any{"files": stats.Files, "symbols": stats.Symbols, "relationships": len(edges)})
	return nil
}

func (p *Pipeline) extractUpload(ctx context.Context, key, work, dest string, limits ingestion.ScanLimits) error {
	src, err := p.Blobs.Get(ctx, key)
	if err != nil {
		return fmt.Errorf("uploaded archive is not available: %w", err)
	}
	defer src.Close()
	// zip needs random access; blobs are local files, so use the handle directly.
	file, ok := src.(*os.File)
	if !ok {
		return errors.New("blob store did not return a seekable archive")
	}
	info, err := file.Stat()
	if err != nil {
		return err
	}
	_, err = ingestion.ExtractZip(ctx, file, info.Size(), dest, limits)
	return err
}

func (p *Pipeline) parentVersion(ctx context.Context, job *orchestrator.Job, sequence int) (*string, map[string]string, error) {
	files := map[string]string{}
	var id string
	err := p.Pool.QueryRow(ctx, `SELECT id FROM repository_versions
		WHERE organization_id = $1 AND repository_id = $2 AND status = 'ready' AND sequence < $3 ORDER BY sequence DESC LIMIT 1`,
		job.OrgID, job.RepositoryID, sequence).Scan(&id)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, files, nil
	}
	if err != nil {
		return nil, nil, err
	}
	rows, err := p.Pool.Query(ctx, `SELECT path, content_hash FROM source_files WHERE organization_id = $1 AND version_id = $2`, job.OrgID, id)
	if err != nil {
		return nil, nil, err
	}
	defer rows.Close()
	for rows.Next() {
		var path, hash string
		if err := rows.Scan(&path, &hash); err != nil {
			return nil, nil, err
		}
		files[path] = hash
	}
	return &id, files, rows.Err()
}

type analysisKey struct{ hash, language string }

type cached struct {
	ok          bool
	code, msg   string
	symbols     int
	diagnostics int
}

// parse decides, for every file, whether it is parsed, and parses only
// content that has no cached analysis for the current parser fingerprint.
func (p *Pipeline) parse(ctx context.Context, job *orchestrator.Job, files []*fileState, stats *Stats, report func(string, any)) error {
	wanted := map[analysisKey][]*fileState{}
	for _, f := range files {
		f.parserLang = ingestion.ParserLanguage(f.Path)
		switch {
		case f.parserLang == "":
			f.parseStatus = "unsupported"
			if f.Language != "" {
				f.parseDetail = "no parser adapter for " + f.Language
			}
		case f.Vendored:
			f.parseStatus, f.parseDetail = "skipped", "vendored third-party code"
		case f.Generated:
			f.parseStatus, f.parseDetail = "skipped", "generated file"
		case f.Oversize:
			f.parseStatus, f.parseDetail = "skipped", fmt.Sprintf("larger than %d bytes", p.Cfg.MaxFileBytes)
		case f.Encoding != "utf-8" && f.Encoding != "utf-8-bom":
			f.parseStatus, f.parseDetail = "skipped", "unsupported encoding: "+f.Encoding
		case !f.stored:
			f.parseStatus, f.parseDetail = "skipped", "content not retained"
		default:
			key := analysisKey{f.ContentHash, f.parserLang}
			wanted[key] = append(wanted[key], f)
		}
	}

	// Cache lookup.
	fingerprints := map[string]string{}
	var fpList, hashes []string
	for key := range wanted {
		if _, ok := fingerprints[key.language]; !ok {
			fp := p.Info.Fingerprint(key.language)
			if fp == "" {
				return orchestrator.Permanent(fmt.Errorf("engine has no parser for %s", key.language))
			}
			fingerprints[key.language] = fp
			fpList = append(fpList, fp)
		}
		hashes = append(hashes, key.hash)
	}
	known := map[analysisKey]cached{}
	if len(hashes) > 0 {
		rows, err := p.Pool.Query(ctx, `SELECT content_hash, language, parser_fingerprint, status, error_code, error_message, symbol_count, diagnostic_count
			FROM file_analyses WHERE organization_id = $1 AND content_hash = ANY($2) AND parser_fingerprint = ANY($3)`, job.OrgID, hashes, fpList)
		if err != nil {
			return err
		}
		for rows.Next() {
			var hash, lang, fp, status string
			var c cached
			if err := rows.Scan(&hash, &lang, &fp, &status, &c.code, &c.msg, &c.symbols, &c.diagnostics); err != nil {
				rows.Close()
				return err
			}
			if fingerprints[lang] == fp {
				c.ok = status == "ok"
				known[analysisKey{hash, lang}] = c
			}
		}
		rows.Close()
		if err := rows.Err(); err != nil {
			return err
		}
	}

	apply := func(key analysisKey, c cached) {
		for _, f := range wanted[key] {
			if c.ok {
				f.irKey = irKey(job.OrgID, fingerprints[key.language], key.hash)
				f.symbols, f.diagnostics = c.symbols, c.diagnostics
				f.parseStatus = "parsed"
				if c.diagnostics > 0 {
					f.parseStatus = "parsed_with_errors"
					f.parseDetail = fmt.Sprintf("%d syntax diagnostics", c.diagnostics)
				}
			} else {
				f.parseStatus, f.parseDetail = "failed", c.code+": "+c.msg
			}
		}
	}

	var requests []analysis.ParseRequest
	var requestKeys []analysisKey
	for key := range wanted {
		if c, hit := known[key]; hit {
			// A cache row without its IR blob is not a hit.
			exists := !c.ok
			if c.ok {
				var err error
				if exists, err = p.Blobs.Exists(ctx, irKey(job.OrgID, fingerprints[key.language], key.hash)); err != nil {
					return err
				}
			}
			if exists {
				stats.ParseCacheHits += len(wanted[key])
				apply(key, c)
				continue
			}
		}
		stats.ParseCacheMisses += len(wanted[key])
		requestKeys = append(requestKeys, key)
	}
	sort.Slice(requestKeys, func(i, j int) bool {
		if requestKeys[i].hash != requestKeys[j].hash {
			return requestKeys[i].hash < requestKeys[j].hash
		}
		return requestKeys[i].language < requestKeys[j].language
	})
	for i, key := range requestKeys {
		requests = append(requests, analysis.ParseRequest{
			ID: fmt.Sprint(i), Path: wanted[key][0].Path, Language: key.language, Source: key.hash[:2] + "/" + key.hash,
		})
	}
	if len(requests) == 0 {
		return nil
	}

	root := filepath.Join(p.Blobs.Root(), "org", job.OrgID, "src")
	engineCtx, cancel := context.WithTimeout(ctx, p.Cfg.EngineTimeout)
	defer cancel()
	received := 0
	lastReport := time.Now()
	err := p.Engine.Parse(engineCtx, root, requests, func(res analysis.ParseResult) error {
		key := requestKeys[received]
		received++
		c := cached{code: res.Code, msg: truncate(res.Message, 300)}
		if res.Status == "ok" {
			var summary analysis.IRSummary
			if err := json.Unmarshal(res.IR, &summary); err != nil {
				return fmt.Errorf("engine returned unreadable IR for %s: %w", res.Path, err)
			}
			if err := p.writeIR(ctx, irKey(job.OrgID, fingerprints[key.language], key.hash), res.IR); err != nil {
				return err
			}
			c.ok, c.symbols, c.diagnostics = true, len(summary.Symbols), summary.Stats.DiagnosticsTotal
		}
		apply(key, c)
		// Timeouts and read errors depend on load, not on content: do not cache them.
		if c.ok || (c.code != "timeout" && c.code != "read_error") {
			status := "error"
			if c.ok {
				status = "ok"
			}
			if _, err := p.Pool.Exec(ctx, `INSERT INTO file_analyses (organization_id, content_hash, language, parser_fingerprint, status, error_code, error_message, symbol_count, diagnostic_count)
				VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
				ON CONFLICT (organization_id, content_hash, language, parser_fingerprint) DO UPDATE
				SET status = EXCLUDED.status, error_code = EXCLUDED.error_code, error_message = EXCLUDED.error_message,
				    symbol_count = EXCLUDED.symbol_count, diagnostic_count = EXCLUDED.diagnostic_count`,
				job.OrgID, key.hash, key.language, fingerprints[key.language], status, c.code, c.msg, c.symbols, c.diagnostics); err != nil {
				return err
			}
		}
		if time.Since(lastReport) > 2*time.Second {
			lastReport = time.Now()
			report("parsing", map[string]any{"parsed": received, "to_parse": len(requests), "cache_hits": stats.ParseCacheHits})
		}
		return nil
	})
	if err != nil {
		return fmt.Errorf("parse: %w", err)
	}
	return nil
}

func (p *Pipeline) writeIR(ctx context.Context, key string, ir []byte) error {
	var buf bytes.Buffer
	zw := gzip.NewWriter(&buf)
	if _, err := zw.Write(ir); err != nil {
		return err
	}
	if err := zw.Close(); err != nil {
		return err
	}
	return blob.PutBytes(ctx, p.Blobs, key, buf.Bytes())
}

func (p *Pipeline) readIR(ctx context.Context, key string) ([]byte, error) {
	r, err := p.Blobs.Get(ctx, key)
	if err != nil {
		return nil, err
	}
	defer r.Close()
	zr, err := gzip.NewReader(r)
	if err != nil {
		return nil, err
	}
	// Bound decompression: stored IR is never larger than one protocol line.
	data, err := io.ReadAll(io.LimitReader(zr, 256<<20))
	if err != nil {
		return nil, err
	}
	return data, nil
}

func truncate(s string, max int) string {
	if len(s) <= max {
		return s
	}
	return s[:max]
}
