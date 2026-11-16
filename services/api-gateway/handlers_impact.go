package api

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net/http"
	"sort"
	"strings"

	"github.com/ammarkarimi/usie/internal/pipeline"
	"github.com/ammarkarimi/usie/internal/platform/blob"
	"github.com/ammarkarimi/usie/internal/store"
	impact "github.com/ammarkarimi/usie/services/change-impact"
	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
)

type impactRequest struct {
	// Symbols are node keys treated as changed.
	Symbols []string `json:"symbols"`
	// Files are paths whose every declaration is treated as changed.
	Files []string `json:"files"`
	// Diff is a unified diff that applies to this version.
	Diff string `json:"diff"`
	// BaseVersionID compares this version with an earlier analyzed
	// version of the same repository: what changed since base, and what in
	// this version depends on it.
	BaseVersionID string `json:"base_version_id"`
	Depth         int    `json:"depth"`
	MinConfidence string `json:"min_confidence"`
}

const (
	maxImpactSymbols      = 500
	maxImpactCompareFiles = 1000
)

func (s *Server) analyzeImpact(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	v, scope, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	var req impactRequest
	if err := decode(r, &req); err != nil {
		return err
	}
	if len(req.Symbols) == 0 && len(req.Files) == 0 && strings.TrimSpace(req.Diff) == "" && req.BaseVersionID == "" {
		return errBadRequest("give symbols, files, a diff or a base_version_id")
	}
	if len(req.Symbols) > maxImpactSymbols || len(req.Files) > maxImpactSymbols {
		return errBadRequest("at most %d symbols and %d files", maxImpactSymbols, maxImpactSymbols)
	}
	if req.Depth < 0 || req.Depth > impact.MaxDepth {
		return errBadRequest("depth must be between 1 and %d", impact.MaxDepth)
	}
	switch req.MinConfidence {
	case "", graph.Confirmed, graph.Probable, graph.Inferred:
	default:
		return errBadRequest("min_confidence must be confirmed, probable or inferred")
	}
	ctx := r.Context()
	a := &impact.Analyzer{Graph: s.Graph, Scope: scope}
	var changed []impact.Changed
	unmatched := []string{}

	if len(req.Symbols) > 0 {
		nodes, err := s.Graph.NodesByKeys(ctx, scope, req.Symbols)
		if err != nil {
			return err
		}
		found := map[string]bool{}
		for _, n := range nodes {
			found[n.Key] = true
			changed = append(changed, impact.Changed{Node: n, Reason: "selected"})
		}
		for _, k := range req.Symbols {
			if !found[k] {
				unmatched = append(unmatched, fmt.Sprintf("%s: no such node in this version", k))
			}
		}
	}
	var changes []impact.FileChange
	for _, f := range req.Files {
		fp, err := repoPath(f, false)
		if err != nil {
			return err
		}
		changes = append(changes, impact.FileChange{Path: fp, Status: "whole", Reason: "file selected"})
	}
	if strings.TrimSpace(req.Diff) != "" {
		parsed, err := impact.ParseUnifiedDiff(req.Diff)
		if err != nil {
			return errBadRequest("%s", err)
		}
		for _, fc := range parsed {
			if _, err := repoPath(fc.Path, false); err != nil {
				unmatched = append(unmatched, fmt.Sprintf("%q: not a repository-relative path", fc.Path))
				continue
			}
			changes = append(changes, fc)
		}
	}
	var base *store.Version
	if req.BaseVersionID != "" {
		b, compare, notes, err := s.compareVersions(r, p, v, req.BaseVersionID)
		if err != nil {
			return err
		}
		base = &b
		changes = append(changes, compare...)
		unmatched = append(unmatched, notes...)
	}
	mapped, notes, err := a.SymbolsForChanges(ctx, changes)
	if err != nil {
		return err
	}
	changed = append(changed, mapped...)
	unmatched = append(unmatched, notes...)

	res, err := a.Analyze(ctx, changed, unmatched, impact.Options{Depth: req.Depth, MinConfidence: req.MinConfidence})
	if err != nil {
		return err
	}
	out := map[string]any{"version_id": v.ID, "impact": res}
	if base != nil {
		out["base_version_id"] = base.ID
	}
	writeJSON(w, http.StatusOK, out)
	return nil
}

// compareVersions lists the files that differ between base and head and,
// for modified text files, the head lines that changed.
func (s *Server) compareVersions(r *http.Request, p *store.Principal, head store.Version, baseID string) (store.Version, []impact.FileChange, []string, error) {
	if !store.ValidID(baseID) {
		return store.Version{}, nil, nil, errBadRequest("base_version_id must be a version id")
	}
	ctx := r.Context()
	base, err := s.Store.GetVersion(ctx, p.OrgID, strings.ToLower(baseID))
	if err != nil {
		return store.Version{}, nil, nil, err
	}
	if base.RepositoryID != head.RepositoryID {
		return store.Version{}, nil, nil, errBadRequest("base must be a version of the same repository")
	}
	if base.ID == head.ID {
		return store.Version{}, nil, nil, errBadRequest("base must differ from the analyzed version")
	}
	if base.Status != "ready" {
		return store.Version{}, nil, nil, &apiError{http.StatusConflict, "version_not_ready", "the base version has not been analyzed"}
	}
	baseFiles, err := s.allFiles(r, p.OrgID, base.ID)
	if err != nil {
		return store.Version{}, nil, nil, err
	}
	headFiles, err := s.allFiles(r, p.OrgID, head.ID)
	if err != nil {
		return store.Version{}, nil, nil, err
	}
	var changes []impact.FileChange
	var notes []string
	compared := 0
	for path, hf := range headFiles {
		bf, inBase := baseFiles[path]
		switch {
		case !inBase:
			changes = append(changes, impact.FileChange{Path: path, Status: "whole", Reason: "file added since the base version"})
		case bf.ContentHash == hf.ContentHash:
		default:
			compared++
			if compared > maxImpactCompareFiles {
				notes = append(notes, fmt.Sprintf("%s: not compared (more than %d modified files)", path, maxImpactCompareFiles))
				continue
			}
			ranges, ok := s.changedLines(r, p.OrgID, bf, hf)
			if !ok {
				changes = append(changes, impact.FileChange{Path: path, Status: "whole", Reason: "file modified (content not comparable line by line)"})
				continue
			}
			if len(ranges) > 0 {
				changes = append(changes, impact.FileChange{Path: path, Status: "modified", Ranges: ranges})
			}
		}
	}
	for path := range baseFiles {
		if _, ok := headFiles[path]; !ok {
			notes = append(notes, fmt.Sprintf("%s: deleted since the base version; references to it that remain appear as unresolved in this version", path))
		}
	}
	sort.Slice(changes, func(i, j int) bool { return changes[i].Path < changes[j].Path })
	sort.Strings(notes)
	return base, changes, notes, nil
}

func (s *Server) allFiles(r *http.Request, orgID, versionID string) (map[string]store.SourceFile, error) {
	return s.allFilesCtx(r.Context(), orgID, versionID)
}

func (s *Server) allFilesCtx(ctx context.Context, orgID, versionID string) (map[string]store.SourceFile, error) {
	out := map[string]store.SourceFile{}
	const page = 5000
	for offset := 0; ; offset += page {
		files, total, err := s.Store.ListFiles(ctx, orgID, versionID, store.FileQuery{Recursive: true, Limit: page, Offset: offset})
		if err != nil {
			return nil, err
		}
		for _, f := range files {
			out[f.Path] = f
		}
		if int64(offset+page) >= total || len(files) == 0 {
			return out, nil
		}
	}
}

func (s *Server) changedLines(r *http.Request, orgID string, base, head store.SourceFile) ([]impact.LineRange, bool) {
	read := func(f store.SourceFile) (string, bool) {
		if !f.ContentStored || (f.Encoding != "utf-8" && f.Encoding != "utf-8-bom") {
			return "", false
		}
		data, err := blob.GetBytes(r.Context(), s.Blobs, pipeline.SourceKey(orgID, f.ContentHash), s.Cfg.MaxFileBytes)
		if err != nil {
			if !errors.Is(err, blob.ErrNotFound) {
				s.Log.Warn("reading content for impact comparison", "error", err)
			}
			return "", false
		}
		return string(bytes.TrimPrefix(data, []byte{0xEF, 0xBB, 0xBF})), true
	}
	a, ok := read(base)
	if !ok {
		return nil, false
	}
	b, ok := read(head)
	if !ok {
		return nil, false
	}
	return impact.ChangedLines(a, b)
}
