package api

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/ammarkarimi/usie/internal/store"
	transform "github.com/ammarkarimi/usie/services/transformation"
	verify "github.com/ammarkarimi/usie/services/verification"
)

func (s *Server) changeError(err error) error {
	switch {
	case errors.Is(err, transform.ErrNotFound):
		return errNotFound
	case errors.Is(err, transform.ErrNotReady):
		return &apiError{http.StatusConflict, "version_not_ready", "analysis of this version has not completed"}
	case errors.Is(err, transform.ErrBudget):
		return &apiError{http.StatusTooManyRequests, "ai_budget_exhausted", "the organization's daily AI budget is used up; it resets at 00:00 UTC"}
	case errors.Is(err, transform.ErrUserRate):
		return &apiError{http.StatusTooManyRequests, "ai_rate_limited", "you have reached the hourly limit of change proposals; try again later"}
	case errors.Is(err, transform.ErrInvalidInput):
		return errBadRequest("%s", strings.TrimPrefix(err.Error(), "transform: invalid input: "))
	case errors.Is(err, transform.ErrNotPending):
		return &apiError{http.StatusConflict, "invalid_state", "the proposal cannot be changed in its current state"}
	}
	return err
}

func (s *Server) requireChanges() error {
	if s.Changes == nil || !s.Changes.Enabled() {
		return &apiError{http.StatusServiceUnavailable, "ai_disabled", "AI change proposals need an AI provider, which is not configured on this installation"}
	}
	return nil
}

type proposeBody struct {
	VersionID string `json:"version_id"`
	Request   string `json:"request"`
	FocusKey  string `json:"focus_key"`
}

func (s *Server) proposeChange(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if err := s.requireChanges(); err != nil {
		return err
	}
	var in proposeBody
	if err := decode(r, &in); err != nil {
		return err
	}
	if !store.ValidID(in.VersionID) {
		return errBadRequest("version_id must be a version id")
	}
	c, err := s.Changes.Propose(r.Context(), transform.ProposeInput{OrgID: p.OrgID, UserID: p.UserID, VersionID: strings.ToLower(in.VersionID), Request: in.Request, FocusKey: in.FocusKey})
	if err != nil {
		return s.changeError(err)
	}
	// The request text is not recorded in the audit trail, only its size.
	s.audit(r, p, "change.propose", "code_change", c.ID, "success", map[string]any{"version_id": c.VersionID, "request_chars": len(in.Request)})
	writeJSON(w, http.StatusAccepted, c)
	return nil
}

func (s *Server) listChanges(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if s.Changes == nil {
		writeJSON(w, http.StatusOK, map[string]any{"changes": []any{}})
		return nil
	}
	repoID := r.URL.Query().Get("repository_id")
	if repoID != "" && !store.ValidID(repoID) {
		return errBadRequest("repository_id must be a repository id")
	}
	limit, err := queryInt(r, "limit", 50, 1, 200)
	if err != nil {
		return err
	}
	list, err := s.Changes.List(r.Context(), p.OrgID, strings.ToLower(repoID), limit)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, map[string]any{"changes": list})
	return nil
}

func (s *Server) getChange(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if s.Changes == nil {
		return errNotFound
	}
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	c, err := s.Changes.Get(r.Context(), p.OrgID, id)
	if err != nil {
		return s.changeError(err)
	}
	runs, err := s.listRuns(r.Context(), p.OrgID, id)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, map[string]any{"change": c, "verifications": runs})
	return nil
}

func (s *Server) discardChange(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if s.Changes == nil {
		return errNotFound
	}
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	c, err := s.Changes.Discard(r.Context(), p.OrgID, id, p.UserID, store.RoleAtLeast(p.Role, "admin"))
	if err != nil {
		return s.changeError(err)
	}
	s.audit(r, p, "change.discard", "code_change", id, "success", nil)
	writeJSON(w, http.StatusOK, c)
	return nil
}

// changePatch returns the proposal as a patch file for `git apply`.
func (s *Server) changePatch(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if s.Changes == nil {
		return errNotFound
	}
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	c, err := s.Changes.Get(r.Context(), p.OrgID, id)
	if err != nil {
		return s.changeError(err)
	}
	if c.Diff == "" {
		return &apiError{http.StatusConflict, "no_diff", "this proposal has no diff"}
	}
	w.Header().Set("Content-Type", "text/x-diff; charset=utf-8")
	w.Header().Set("Content-Disposition", fmt.Sprintf(`attachment; filename="usie-change-%s.patch"`, c.ID[:8]))
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.WriteHeader(http.StatusOK)
	_, err = w.Write([]byte(c.Diff))
	return err
}

// verificationCapability is the sandbox of the most recently started online
// worker that has one.
func (s *Server) verificationCapability(ctx context.Context) (*verify.Capability, error) {
	workers, err := s.Queue.ListWorkers(ctx)
	if err != nil {
		return nil, err
	}
	for _, wk := range workers {
		if !wk.Online {
			continue
		}
		var caps struct {
			Verification *verify.Capability `json:"verification"`
		}
		if json.Unmarshal(wk.Capabilities, &caps) == nil && caps.Verification != nil && caps.Verification.Sandbox != "none" {
			return caps.Verification, nil
		}
	}
	return nil, nil
}

func (s *Server) facts(ctx context.Context, orgID, versionID string) (verify.Facts, error) {
	var f verify.Facts
	files, err := s.allFilesCtx(ctx, orgID, versionID)
	if err != nil {
		return f, err
	}
	for path := range files {
		f.Paths = append(f.Paths, path)
	}
	v, err := s.Store.GetVersion(ctx, orgID, versionID)
	if err != nil {
		return f, err
	}
	var stats struct {
		BuildSystems []string `json:"build_systems"`
	}
	_ = json.Unmarshal(v.Stats, &stats)
	f.BuildSystems = stats.BuildSystems
	if pkg, ok := files["package.json"]; ok && pkg.ContentStored {
		if text, err := AISources(s.Store, s.Blobs, s.Cfg.MaxFileBytes)(orgID, versionID)(ctx, pkg.Path); err == nil {
			f.PackageJSONTest = verify.PackageJSONHasTest([]byte(text))
		}
	}
	return f, nil
}

func (s *Server) verificationProfiles(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	c, err := s.changeForVerification(r, p)
	if err != nil {
		return err
	}
	capability, err := s.verificationCapability(r.Context())
	if err != nil {
		return err
	}
	facts, err := s.facts(r.Context(), p.OrgID, c.VersionID)
	if err != nil {
		return err
	}
	var sb verify.Sandbox
	if capability != nil {
		sb = verify.Remote{Cap: *capability}
	}
	writeJSON(w, http.StatusOK, map[string]any{"sandbox": capability, "profiles": verify.Profiles(facts, sb)})
	return nil
}

func (s *Server) changeForVerification(r *http.Request, p *store.Principal) (transform.Change, error) {
	if s.Changes == nil {
		return transform.Change{}, errNotFound
	}
	id, err := pathID(r, "id")
	if err != nil {
		return transform.Change{}, err
	}
	c, err := s.Changes.Get(r.Context(), p.OrgID, id)
	if err != nil {
		return c, s.changeError(err)
	}
	return c, nil
}

type verifyBody struct {
	Profile  string `json:"profile"`
	Baseline *bool  `json:"baseline"`
}

const maxActiveRunsPerOrg = 3

func (s *Server) requestVerification(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	c, err := s.changeForVerification(r, p)
	if err != nil {
		return err
	}
	var in verifyBody
	if err := decode(r, &in); err != nil {
		return err
	}
	if c.Status != "proposed" {
		return &apiError{http.StatusConflict, "invalid_state", "only proposed changes can be verified"}
	}
	capability, err := s.verificationCapability(r.Context())
	if err != nil {
		return err
	}
	if capability == nil {
		return &apiError{http.StatusServiceUnavailable, "verification_disabled", "no online worker has a verification sandbox (USIE_SANDBOX is not set to bubblewrap on any worker)"}
	}
	facts, err := s.facts(r.Context(), p.OrgID, c.VersionID)
	if err != nil {
		return err
	}
	profile, ok := verify.Find(facts, verify.Remote{Cap: *capability}, in.Profile)
	if !ok {
		return errBadRequest("unknown profile %q", in.Profile)
	}
	if !profile.Available {
		return &apiError{http.StatusConflict, "profile_unavailable", fmt.Sprintf("%s cannot run: %s", profile.Label, profile.Reason)}
	}
	baseline := in.Baseline == nil || *in.Baseline
	var run verificationRun
	err = pgx.BeginFunc(r.Context(), s.Store.Pool, func(tx pgx.Tx) error {
		var active int
		if err := tx.QueryRow(r.Context(), `SELECT count(*) FROM verification_runs WHERE organization_id = $1 AND status IN ('queued', 'running')`, p.OrgID).Scan(&active); err != nil {
			return err
		}
		if active >= maxActiveRunsPerOrg {
			return &apiError{http.StatusTooManyRequests, "verification_busy", fmt.Sprintf("the organization already has %d verification runs queued or running", active)}
		}
		var err error
		run, err = scanRun(tx.QueryRow(r.Context(), `INSERT INTO verification_runs (organization_id, repository_id, version_id, change_id, profile, baseline, created_by)
			VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING `+runColumns, p.OrgID, c.RepositoryID, c.VersionID, c.ID, profile.ID, baseline, p.UserID))
		if err != nil {
			return err
		}
		_, err = tx.Exec(r.Context(), `INSERT INTO analysis_jobs (organization_id, repository_id, version_id, kind, subject_id, max_attempts, created_by)
			VALUES ($1, $2, $3, 'verify_change', $4, 2, $5)`, p.OrgID, c.RepositoryID, c.VersionID, run.ID, p.UserID)
		return err
	})
	if err != nil {
		return err
	}
	s.audit(r, p, "verification.request", "code_change", c.ID, "success", map[string]any{"profile": profile.ID, "run_id": run.ID, "baseline": baseline})
	writeJSON(w, http.StatusAccepted, run)
	return nil
}

type verificationRun struct {
	ID         string          `json:"id"`
	ChangeID   string          `json:"change_id"`
	VersionID  string          `json:"version_id"`
	Profile    string          `json:"profile"`
	Baseline   bool            `json:"baseline"`
	Status     string          `json:"status"`
	Result     json.RawMessage `json:"result"`
	Error      string          `json:"error"`
	CreatedBy  *string         `json:"created_by"`
	CreatedAt  time.Time       `json:"created_at"`
	StartedAt  *time.Time      `json:"started_at"`
	FinishedAt *time.Time      `json:"finished_at"`
}

const runColumns = `id, change_id, version_id, profile, baseline, status, result, error, created_by::text, created_at, started_at, finished_at`

func scanRun(row pgx.Row) (verificationRun, error) {
	var v verificationRun
	err := row.Scan(&v.ID, &v.ChangeID, &v.VersionID, &v.Profile, &v.Baseline, &v.Status, &v.Result, &v.Error, &v.CreatedBy, &v.CreatedAt, &v.StartedAt, &v.FinishedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return v, errNotFound
	}
	return v, err
}

func (s *Server) listRuns(ctx context.Context, orgID, changeID string) ([]verificationRun, error) {
	rows, err := s.Store.Pool.Query(ctx, `SELECT `+runColumns+` FROM verification_runs WHERE organization_id = $1 AND change_id = $2 ORDER BY created_at DESC LIMIT 50`, orgID, changeID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []verificationRun{}
	for rows.Next() {
		v, err := scanRun(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, v)
	}
	return out, rows.Err()
}

func (s *Server) getVerification(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	run, err := scanRun(s.Store.Pool.QueryRow(r.Context(), `SELECT `+runColumns+` FROM verification_runs WHERE organization_id = $1 AND id = $2`, p.OrgID, id))
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, run)
	return nil
}
