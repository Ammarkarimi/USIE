package api

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"path"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/ammarkarimi/usie/internal/pipeline"
	"github.com/ammarkarimi/usie/internal/platform/blob"
	"github.com/ammarkarimi/usie/internal/store"
	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
	ingestion "github.com/ammarkarimi/usie/services/repository-ingestion"
)

func (s *Server) listWorkspaces(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	ws, err := s.Store.ListWorkspaces(r.Context(), p.OrgID)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, map[string]any{"workspaces": ws})
	return nil
}

func (s *Server) createWorkspace(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	var in struct {
		Name string `json:"name"`
	}
	if err := decode(r, &in); err != nil {
		return err
	}
	if !validName(in.Name, 100) {
		return errBadRequest("name is required (max 100 characters)")
	}
	ws, err := s.Store.CreateWorkspace(r.Context(), p.OrgID, in.Name)
	if err != nil {
		return err
	}
	s.audit(r, p, "workspace.created", "workspace", ws.ID, "success", nil)
	writeJSON(w, http.StatusCreated, ws)
	return nil
}

func (s *Server) listRepositories(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	repos, err := s.Store.ListRepositories(r.Context(), p.OrgID)
	if err != nil {
		return err
	}
	if repos == nil {
		repos = []store.Repository{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"repositories": repos})
	return nil
}

func (s *Server) createRepository(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	var in struct {
		WorkspaceID    string `json:"workspace_id"`
		Name           string `json:"name"`
		SourceKind     string `json:"source_kind"`
		SourceLocation string `json:"source_location"`
		DefaultRef     string `json:"default_ref"`
	}
	if err := decode(r, &in); err != nil {
		return err
	}
	if !validName(in.Name, 200) {
		return errBadRequest("name is required (max 200 characters)")
	}
	if in.DefaultRef != "" && !ingestion.ValidRef(in.DefaultRef) {
		return errBadRequest("default_ref is not a valid branch, tag or commit name")
	}
	switch in.SourceKind {
	case "git":
		if _, err := ingestion.ValidateGitURL(in.SourceLocation); err != nil {
			return errBadRequest("%s", err)
		}
	case "local":
		// Pointing the service at server directories is an administrative act.
		if !store.RoleAtLeast(p.Role, "admin") {
			return errForbidden
		}
		resolved, err := ingestion.ResolveLocalPath(in.SourceLocation, s.Cfg.AllowedLocalRoots)
		if err != nil {
			return errBadRequest("%s", err)
		}
		in.SourceLocation, in.DefaultRef = resolved, ""
	case "upload":
		in.SourceLocation, in.DefaultRef = "", ""
	default:
		return errBadRequest("source_kind must be git, local or upload")
	}
	if in.WorkspaceID == "" {
		ws, err := s.Store.ListWorkspaces(r.Context(), p.OrgID)
		if err != nil {
			return err
		}
		if len(ws) == 0 {
			return errBadRequest("the organization has no workspace")
		}
		in.WorkspaceID = ws[0].ID
	} else if !store.ValidID(in.WorkspaceID) {
		return errBadRequest("workspace_id is not valid")
	}
	repo, err := s.Store.CreateRepository(r.Context(), p.OrgID, p.UserID, store.Repository{
		WorkspaceID: in.WorkspaceID, Name: in.Name, SourceKind: in.SourceKind, SourceLocation: in.SourceLocation, DefaultRef: in.DefaultRef,
	})
	if errors.Is(err, store.ErrNotFound) {
		return errBadRequest("workspace not found")
	}
	if err != nil {
		return err
	}
	s.audit(r, p, "repository.created", "repository", repo.ID, "success", map[string]any{"source_kind": repo.SourceKind})
	writeJSON(w, http.StatusCreated, repo)
	return nil
}

func (s *Server) getRepository(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	repo, err := s.Store.GetRepository(r.Context(), p.OrgID, id)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, repo)
	return nil
}

func (s *Server) deleteRepository(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	if err := s.Store.DeleteRepository(r.Context(), p.OrgID, id); err != nil {
		return err
	}
	s.audit(r, p, "repository.deleted", "repository", id, "success", nil)
	w.WriteHeader(http.StatusNoContent)
	return nil
}

func (s *Server) analyzeRepository(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	var in struct {
		Ref string `json:"ref"`
	}
	if r.ContentLength != 0 {
		if err := decode(r, &in); err != nil {
			return err
		}
	}
	repo, err := s.Store.GetRepository(r.Context(), p.OrgID, id)
	if err != nil {
		return err
	}
	switch {
	case repo.SourceKind == "upload":
		return errBadRequest("upload repositories are analyzed by uploading an archive")
	case in.Ref != "" && repo.SourceKind != "git":
		return errBadRequest("ref applies to git repositories only")
	case in.Ref != "" && !ingestion.ValidRef(in.Ref):
		return errBadRequest("ref is not a valid branch, tag or commit name")
	}
	return s.enqueue(w, r, p, repo.ID, in.Ref, "")
}

func (s *Server) enqueue(w http.ResponseWriter, r *http.Request, p *store.Principal, repoID, ref, uploadKey string) error {
	versionID, jobID, err := s.Store.CreateVersionAndJob(r.Context(), p.OrgID, p.UserID, repoID, ref, uploadKey)
	if err != nil {
		return err
	}
	s.audit(r, p, "analysis.requested", "repository", repoID, "success", map[string]any{"version_id": versionID, "job_id": jobID})
	writeJSON(w, http.StatusAccepted, map[string]string{"version_id": versionID, "job_id": jobID})
	return nil
}

func (s *Server) uploadRepository(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	repo, err := s.Store.GetRepository(r.Context(), p.OrgID, id)
	if err != nil {
		return err
	}
	if repo.SourceKind != "upload" {
		return errBadRequest("this repository does not accept uploads")
	}
	if ct := r.Header.Get("Content-Type"); ct != "application/zip" && ct != "application/octet-stream" {
		return &apiError{http.StatusUnsupportedMediaType, "unsupported_media_type", "Content-Type must be application/zip"}
	}
	body := http.MaxBytesReader(w, r.Body, s.Cfg.MaxUploadBytes)
	magic := make([]byte, 4)
	if _, err := io.ReadFull(body, magic); err != nil || !bytes.Equal(magic[:2], []byte("PK")) {
		return errBadRequest("the uploaded file is not a ZIP archive")
	}
	random := make([]byte, 16)
	rand.Read(random) //nolint:errcheck
	key := pipeline.UploadKey(p.OrgID, hex.EncodeToString(random))
	if _, err := s.Blobs.Put(r.Context(), key, io.MultiReader(bytes.NewReader(magic), body)); err != nil {
		s.Blobs.Delete(context.WithoutCancel(r.Context()), key) //nolint:errcheck
		return err
	}
	if err := s.enqueue(w, r, p, repo.ID, "", key); err != nil {
		s.Blobs.Delete(context.WithoutCancel(r.Context()), key) //nolint:errcheck
		return err
	}
	return nil
}

func (s *Server) listVersions(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	if _, err := s.Store.GetRepository(r.Context(), p.OrgID, id); err != nil {
		return err
	}
	versions, err := s.Store.ListVersions(r.Context(), p.OrgID, id, 100)
	if err != nil {
		return err
	}
	if versions == nil {
		versions = []store.Version{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"versions": versions})
	return nil
}

func (s *Server) listCommits(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	if _, err := s.Store.GetRepository(r.Context(), p.OrgID, id); err != nil {
		return err
	}
	limit, err := queryInt(r, "limit", 100, 1, 500)
	if err != nil {
		return err
	}
	commits, err := s.Store.ListCommits(r.Context(), p.OrgID, id, limit)
	if err != nil {
		return err
	}
	if commits == nil {
		commits = []store.Commit{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"commits": commits})
	return nil
}

func (s *Server) listBranches(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	if _, err := s.Store.GetRepository(r.Context(), p.OrgID, id); err != nil {
		return err
	}
	branches, err := s.Store.ListBranches(r.Context(), p.OrgID, id)
	if err != nil {
		return err
	}
	if branches == nil {
		branches = []store.Branch{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"branches": branches})
	return nil
}

// version loads the version named in the path, enforcing tenant ownership.
func (s *Server) version(r *http.Request, p *store.Principal) (store.Version, error) {
	id, err := pathID(r, "id")
	if err != nil {
		return store.Version{}, err
	}
	return s.Store.GetVersion(r.Context(), p.OrgID, id)
}

// readyVersion additionally requires analysis to have completed.
func (s *Server) readyVersion(r *http.Request, p *store.Principal) (store.Version, graph.Scope, error) {
	v, err := s.version(r, p)
	if err != nil {
		return v, graph.Scope{}, err
	}
	if v.Status != "ready" {
		return v, graph.Scope{}, &apiError{http.StatusConflict, "version_not_ready", "analysis of this version has not completed (status: " + v.Status + ")"}
	}
	return v, graph.Scope{OrganizationID: p.OrgID, VersionID: v.ID}, nil
}

func (s *Server) getVersion(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	v, err := s.version(r, p)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, v)
	return nil
}

// repoPath validates a repository-relative path from a query parameter.
func repoPath(raw string, allowEmpty bool) (string, error) {
	if raw == "" && allowEmpty {
		return "", nil
	}
	if raw == "" || len(raw) > 1024 || !utf8.ValidString(raw) || path.Clean(raw) != raw || strings.HasPrefix(raw, "/") || raw == ".." || strings.HasPrefix(raw, "../") || strings.ContainsAny(raw, "\x00\\") {
		return "", errBadRequest("path is not a valid repository-relative path")
	}
	return raw, nil
}

func (s *Server) getTree(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	v, _, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	dir, err := repoPath(r.URL.Query().Get("dir"), true)
	if err != nil {
		return err
	}
	dirs, files, err := s.Store.ListDir(r.Context(), p.OrgID, v.ID, dir)
	if err != nil {
		return err
	}
	if dirs == nil {
		dirs = []store.DirEntry{}
	}
	if files == nil {
		files = []store.SourceFile{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"dir": dir, "directories": dirs, "files": files})
	return nil
}

func (s *Server) listFiles(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	v, _, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	q := r.URL.Query()
	dir, err := repoPath(q.Get("dir"), true)
	if err != nil {
		return err
	}
	limit, err := queryInt(r, "limit", 100, 1, 1000)
	if err != nil {
		return err
	}
	offset, err := queryInt(r, "offset", 0, 0, 10_000_000)
	if err != nil {
		return err
	}
	files, total, err := s.Store.ListFiles(r.Context(), p.OrgID, v.ID, store.FileQuery{
		Dir: dir, Language: q.Get("language"), Category: q.Get("category"), ParseStatus: q.Get("parse_status"), Limit: limit, Offset: offset,
	})
	if err != nil {
		return err
	}
	if files == nil {
		files = []store.SourceFile{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"files": files, "total": total})
	return nil
}

// getFile returns a file's metadata, its text content when retained, and
// the symbols declared in it.
func (s *Server) getFile(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	v, scope, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	filePath, err := repoPath(r.URL.Query().Get("path"), false)
	if err != nil {
		return err
	}
	file, err := s.Store.GetFile(r.Context(), p.OrgID, v.ID, filePath)
	if err != nil {
		return err
	}
	out := map[string]any{"file": file, "content": nil, "content_unavailable_reason": ""}
	switch {
	case !file.ContentStored:
		out["content_unavailable_reason"] = "content is not retained for binary or oversize files"
	case file.Encoding != "utf-8" && file.Encoding != "utf-8-bom":
		out["content_unavailable_reason"] = "file is not UTF-8 text (" + file.Encoding + ")"
	default:
		data, err := blob.GetBytes(r.Context(), s.Blobs, pipeline.SourceKey(p.OrgID, file.ContentHash), s.Cfg.MaxFileBytes)
		if errors.Is(err, blob.ErrNotFound) {
			out["content_unavailable_reason"] = "stored content is missing"
		} else if err != nil {
			return err
		} else {
			// Returned inside JSON, so repository content is never
			// interpreted by the browser as markup.
			out["content"] = string(bytes.TrimPrefix(data, []byte{0xEF, 0xBB, 0xBF}))
		}
	}
	symbols, _, err := s.Graph.Search(r.Context(), scope, graph.SearchQuery{PathPrefix: filePath, Limit: 5000,
		Kinds: []string{"function", "method", "class", "interface", "type", "variable", "namespace"}})
	if err != nil {
		return err
	}
	outline := make([]graph.Node, 0, len(symbols))
	for _, n := range symbols {
		if n.Path == filePath {
			outline = append(outline, n)
		}
	}
	sort.SliceStable(outline, func(i, j int) bool { return outline[i].StartLine < outline[j].StartLine })
	out["symbols"] = outline
	writeJSON(w, http.StatusOK, out)
	return nil
}

func (s *Server) listDependencies(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	v, _, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	deps, err := s.Store.ListDependencies(r.Context(), p.OrgID, v.ID)
	if err != nil {
		return err
	}
	if deps == nil {
		deps = []store.Dependency{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"dependencies": deps})
	return nil
}

func (s *Server) listReferences(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	v, _, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	q := r.URL.Query()
	filePath, err := repoPath(q.Get("path"), true)
	if err != nil {
		return err
	}
	outcome := q.Get("outcome")
	if outcome != "" && outcome != "resolved" && outcome != "external" && outcome != "builtin" && outcome != "unresolved" {
		return errBadRequest("outcome must be resolved, external, builtin or unresolved")
	}
	limit, err := queryInt(r, "limit", 100, 1, 1000)
	if err != nil {
		return err
	}
	offset, err := queryInt(r, "offset", 0, 0, 10_000_000)
	if err != nil {
		return err
	}
	refs, total, err := s.Store.ListReferences(r.Context(), p.OrgID, v.ID, store.ReferenceQuery{
		Path: filePath, Outcome: outcome, TargetKey: q.Get("target"), SourceKey: q.Get("source"), Limit: limit, Offset: offset,
	})
	if err != nil {
		return err
	}
	if refs == nil {
		refs = []store.Reference{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"references": refs, "total": total})
	return nil
}

// ---- graph ------------------------------------------------------------------

func (s *Server) graphStats(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	_, scope, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	stats, err := s.Graph.Stats(r.Context(), scope)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, stats)
	return nil
}

func (s *Server) graphSearch(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	_, scope, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	q := r.URL.Query()
	limit, err := queryInt(r, "limit", 50, 1, 500)
	if err != nil {
		return err
	}
	offset, err := queryInt(r, "offset", 0, 0, 10_000_000)
	if err != nil {
		return err
	}
	text := q.Get("q")
	if len(text) > 200 {
		return errBadRequest("q is too long")
	}
	nodes, total, err := s.Graph.Search(r.Context(), scope, graph.SearchQuery{Text: text, Kinds: queryList(r, "kind"), PathPrefix: q.Get("path_prefix"), Limit: limit, Offset: offset})
	if err != nil {
		return err
	}
	if nodes == nil {
		nodes = []graph.Node{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"nodes": nodes, "total": total})
	return nil
}

func nodeKey(r *http.Request) (string, error) {
	key := r.URL.Query().Get("key")
	if key == "" || len(key) > 2048 || !utf8.ValidString(key) {
		return "", errBadRequest("key is required")
	}
	return key, nil
}

func (s *Server) graphNode(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	_, scope, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	key, err := nodeKey(r)
	if err != nil {
		return err
	}
	node, err := s.Graph.NodeByKey(r.Context(), scope, key)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, node)
	return nil
}

func edgeFilter(r *http.Request) (graph.EdgeFilter, error) {
	f := graph.EdgeFilter{Kinds: queryList(r, "kinds"), MinConfidence: r.URL.Query().Get("min_confidence")}
	switch f.MinConfidence {
	case "", graph.Confirmed, graph.Probable, graph.Inferred, graph.Unknown:
	default:
		return f, errBadRequest("min_confidence must be confirmed, probable, inferred or unknown")
	}
	if len(f.Kinds) > 20 {
		return f, errBadRequest("too many relationship kinds")
	}
	return f, nil
}

func subgraphJSON(sub graph.Subgraph) map[string]any {
	if sub.Nodes == nil {
		sub.Nodes = []graph.Node{}
	}
	if sub.Edges == nil {
		sub.Edges = []graph.Edge{}
	}
	return map[string]any{"nodes": sub.Nodes, "edges": sub.Edges, "truncated": sub.Truncated}
}

func (s *Server) graphNeighborhood(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	_, scope, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	key, err := nodeKey(r)
	if err != nil {
		return err
	}
	filter, err := edgeFilter(r)
	if err != nil {
		return err
	}
	depth, err := queryInt(r, "depth", 1, 1, graph.MaxDepth)
	if err != nil {
		return err
	}
	maxNodes, err := queryInt(r, "max_nodes", 200, 1, graph.MaxMaxNodes)
	if err != nil {
		return err
	}
	direction := graph.Direction(r.URL.Query().Get("direction"))
	if direction == "" {
		direction = graph.Both
	}
	if direction != graph.In && direction != graph.Out && direction != graph.Both {
		return errBadRequest("direction must be in, out or both")
	}
	sub, err := s.Graph.Neighborhood(r.Context(), scope, key, graph.NeighborhoodQuery{Depth: depth, Direction: direction, Filter: filter, MaxNodes: maxNodes})
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, subgraphJSON(sub))
	return nil
}

// graphFiles returns the file-level import graph.
func (s *Server) graphFiles(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	_, scope, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	maxEdges, err := queryInt(r, "max_edges", 1500, 1, 5000)
	if err != nil {
		return err
	}
	min := r.URL.Query().Get("min_confidence")
	if _, err := edgeFilter(r); err != nil {
		return err
	}
	sub, err := s.Graph.Subgraph(r.Context(), scope, []string{graph.KindFile}, graph.EdgeFilter{Kinds: []string{graph.RelImports}, MinConfidence: min}, maxEdges)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, subgraphJSON(sub))
	return nil
}

func (s *Server) graphDiff(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	head, scope, err := s.readyVersion(r, p)
	if err != nil {
		return err
	}
	baseID := r.URL.Query().Get("base")
	if !store.ValidID(baseID) {
		return errBadRequest("base must be a version id")
	}
	base, err := s.Store.GetVersion(r.Context(), p.OrgID, strings.ToLower(baseID))
	if err != nil {
		return err
	}
	if base.RepositoryID != head.RepositoryID {
		return errBadRequest("base must be a version of the same repository")
	}
	if base.Status != "ready" {
		return &apiError{http.StatusConflict, "version_not_ready", "the base version has not been analyzed"}
	}
	limit, err := queryInt(r, "limit", 200, 1, 1000)
	if err != nil {
		return err
	}
	d, err := s.Graph.Diff(r.Context(), graph.Scope{OrganizationID: p.OrgID, VersionID: base.ID}, scope, limit)
	if err != nil {
		return err
	}
	for _, list := range []*[]graph.Node{&d.Added, &d.Removed, &d.Changed} {
		if *list == nil {
			*list = []graph.Node{}
		}
	}
	for _, list := range []*[]graph.EdgeKeys{&d.AddedEdges, &d.RemovedEdges} {
		if *list == nil {
			*list = []graph.EdgeKeys{}
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"base_version_id": base.ID, "head_version_id": head.ID, "diff": d})
	return nil
}

// ---- jobs and workers -------------------------------------------------------

func (s *Server) listJobs(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	limit, err := queryInt(r, "limit", 50, 1, 200)
	if err != nil {
		return err
	}
	status := r.URL.Query().Get("status")
	switch status {
	case "", "queued", "running", "succeeded", "failed", "cancelled":
	default:
		return errBadRequest("unknown status")
	}
	jobs, err := s.Queue.List(r.Context(), p.OrgID, status, limit)
	if err != nil {
		return err
	}
	if jobs == nil {
		jobs = []orchestrator.Job{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"jobs": jobs})
	return nil
}

func (s *Server) getJob(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	job, err := s.Queue.Get(r.Context(), p.OrgID, id)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, job)
	return nil
}

func (s *Server) cancelJob(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	job, err := s.Queue.RequestCancel(r.Context(), p.OrgID, id)
	if err != nil {
		return err
	}
	s.audit(r, p, "job.cancel_requested", "job", id, "success", nil)
	writeJSON(w, http.StatusOK, job)
	return nil
}

func terminal(status string) bool {
	return status == "succeeded" || status == "failed" || status == "cancelled"
}

// jobEvents streams job state as Server-Sent Events until the job reaches a
// terminal state. The stream is driven by polling the job row.
func (s *Server) jobEvents(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	job, err := s.Queue.Get(r.Context(), p.OrgID, id)
	if err != nil {
		return err
	}
	flusher, ok := w.(http.Flusher)
	if !ok {
		return errors.New("streaming unsupported")
	}
	h := w.Header()
	h.Set("Content-Type", "text/event-stream")
	h.Set("Cache-Control", "no-store")
	h.Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)

	send := func(j orchestrator.Job) {
		data, _ := json.Marshal(j)
		fmt.Fprintf(w, "event: job\ndata: %s\n\n", data)
		flusher.Flush()
	}
	send(job)
	last := job.UpdatedAt
	poll := time.NewTicker(time.Second)
	defer poll.Stop()
	keepAlive := time.NewTicker(20 * time.Second)
	defer keepAlive.Stop()
	deadline := time.NewTimer(30 * time.Minute)
	defer deadline.Stop()
	for !terminal(job.Status) {
		select {
		case <-r.Context().Done():
			return nil
		case <-deadline.C:
			return nil
		case <-keepAlive.C:
			fmt.Fprint(w, ": keep-alive\n\n")
			flusher.Flush()
		case <-poll.C:
			next, err := s.Queue.Get(r.Context(), p.OrgID, id)
			if err != nil {
				return nil // the response has started; just end the stream
			}
			if !next.UpdatedAt.Equal(last) || next.Status != job.Status {
				job, last = next, next.UpdatedAt
				send(job)
			}
		}
	}
	return nil
}

func (s *Server) listWorkers(w http.ResponseWriter, r *http.Request, _ *store.Principal) error {
	workers, err := s.Queue.ListWorkers(r.Context())
	if err != nil {
		return err
	}
	if workers == nil {
		workers = []orchestrator.WorkerInfo{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"workers": workers})
	return nil
}
