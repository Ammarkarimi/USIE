package verify

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/ammarkarimi/usie/internal/pipeline"
	"github.com/ammarkarimi/usie/internal/platform/blob"
	"github.com/ammarkarimi/usie/internal/store"
	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
)

// FileChange is the resulting content of one file touched by a change.
type FileChange struct {
	Path    string `json:"path"`
	Status  string `json:"status"` // modified | created | deleted
	Content string `json:"content,omitempty"`
}

// StepResult is one executed step.
type StepResult struct {
	Name     string       `json:"name"`
	Argv     []string     `json:"argv"`
	ExitCode int          `json:"exit_code"`
	TimedOut bool         `json:"timed_out"`
	Duration float64      `json:"duration_seconds"`
	Output   string       `json:"output"`
	Clipped  bool         `json:"output_clipped"`
	Tests    *TestSummary `json:"tests,omitempty"`
}

// TreeResult is a profile run on one tree.
type TreeResult struct {
	Steps  []StepResult `json:"steps"`
	Passed bool         `json:"passed"`
	// Skipped names steps not run because an earlier step failed.
	Skipped []string `json:"skipped,omitempty"`
}

// RunResult is stored in verification_runs.result.
type RunResult struct {
	Sandbox      string      `json:"sandbox"`
	Network      bool        `json:"network"`
	Profile      Profile     `json:"profile"`
	MissingFiles int         `json:"missing_files"`
	Baseline     *TreeResult `json:"baseline,omitempty"`
	Change       *TreeResult `json:"change,omitempty"`
	// NewlyFailing are tests that fail with the change and passed (or did
	// not fail) without it; Fixed the reverse. Only when both runs report
	// test names.
	NewlyFailing []string `json:"newly_failing,omitempty"`
	Fixed        []string `json:"fixed,omitempty"`
	Notes        []string `json:"notes"`
}

// Runner executes verify_change jobs.
type Runner struct {
	Pool    *pgxpool.Pool
	Store   *store.Store
	Blobs   blob.Store
	Sandbox Sandbox // nil: verification is disabled
	Log     *slog.Logger
	// MaxFileBytes bounds each file read from the blob store.
	MaxFileBytes int64
	// TempDir holds job work trees ("" for the system default).
	TempDir string
}

// Handle is the orchestrator.Handler for verify_change jobs.
func (r *Runner) Handle(ctx context.Context, job *orchestrator.Job, report func(string, any)) error {
	if job.SubjectID == nil {
		return orchestrator.Permanent(errors.New("verify_change job without a verification run"))
	}
	runID := *job.SubjectID
	var changeID, profileID string
	var baseline bool
	var filesJSON []byte
	err := r.Pool.QueryRow(ctx, `
		SELECT r.change_id, r.profile, r.baseline, c.files FROM verification_runs r
		  JOIN code_changes c ON c.organization_id = r.organization_id AND c.id = r.change_id
		 WHERE r.organization_id = $1 AND r.id = $2`, job.OrgID, runID).Scan(&changeID, &profileID, &baseline, &filesJSON)
	if err != nil {
		return orchestrator.Permanent(fmt.Errorf("load verification run: %w", err))
	}
	fail := func(msg string) error {
		_, err := r.Pool.Exec(context.WithoutCancel(ctx), `UPDATE verification_runs SET status = 'error', error = $3, finished_at = now()
			WHERE organization_id = $1 AND id = $2`, job.OrgID, runID, msg)
		if err != nil {
			return err
		}
		return orchestrator.Permanent(errors.New(msg))
	}
	if r.Sandbox == nil {
		return fail("verification is disabled on this installation: no sandbox is configured")
	}
	var changed []FileChange
	if err := json.Unmarshal(filesJSON, &changed); err != nil {
		return fail("the change's files could not be read")
	}
	if _, err := r.Pool.Exec(ctx, `UPDATE verification_runs SET status = 'running', started_at = now(), error = '' WHERE organization_id = $1 AND id = $2`, job.OrgID, runID); err != nil {
		return err
	}

	report("preparing", map[string]any{"run_id": runID})
	sources, err := r.listSources(ctx, job.OrgID, job.VersionID)
	if err != nil {
		return err
	}
	facts := Facts{}
	for _, f := range sources {
		facts.Paths = append(facts.Paths, f.Path)
	}
	if stats, err := r.buildSystems(ctx, job.OrgID, job.VersionID); err == nil {
		facts.BuildSystems = stats
	}
	profile, ok := Find(facts, r.Sandbox, profileID)
	if !ok {
		return fail(fmt.Sprintf("unknown verification profile %q", profileID))
	}
	if !profile.Available {
		return fail(fmt.Sprintf("profile %s cannot run: %s", profile.Label, profile.Reason))
	}
	res := RunResult{Sandbox: r.Sandbox.Name(), Profile: profile, Notes: []string{}}
	if b, ok := r.Sandbox.(*Bubblewrap); ok {
		res.Network = b.Network
	}

	runTree := func(stage string, overrides []FileChange) (*TreeResult, error) {
		dir, err := os.MkdirTemp(r.TempDir, "usie-verify-")
		if err != nil {
			return nil, err
		}
		defer os.RemoveAll(dir)
		report(stage+": materializing sources", nil)
		missing, err := r.materialize(ctx, job.OrgID, sources, overrides, dir)
		if err != nil {
			return nil, err
		}
		res.MissingFiles = missing
		tr := &TreeResult{Passed: true}
		for i, step := range profile.Steps {
			if ctx.Err() != nil {
				return nil, ctx.Err()
			}
			report(fmt.Sprintf("%s: %s", stage, step.Name), nil)
			out, err := r.Sandbox.Run(ctx, Spec{WorkDir: dir, Argv: step.Argv, Env: step.Env})
			if err != nil {
				return nil, err
			}
			sr := StepResult{Name: step.Name, Argv: step.Argv, ExitCode: out.ExitCode, TimedOut: out.TimedOut,
				Duration: out.Duration.Seconds(), Output: joinOutput(out), Clipped: out.Truncated}
			if step.Tests && profile.parse != nil {
				sr.Tests = profile.parse(out.Stdout + "\n" + out.Stderr)
			}
			tr.Steps = append(tr.Steps, sr)
			if out.ExitCode != 0 {
				tr.Passed = false
				for _, rest := range profile.Steps[i+1:] {
					tr.Skipped = append(tr.Skipped, rest.Name)
				}
				break
			}
		}
		return tr, nil
	}

	started := time.Now()
	if baseline {
		if res.Baseline, err = runTree("baseline", nil); err != nil {
			return r.runError(ctx, job, runID, err)
		}
	}
	if res.Change, err = runTree("change", changed); err != nil {
		return r.runError(ctx, job, runID, err)
	}
	if res.Baseline != nil {
		res.NewlyFailing, res.Fixed = compare(res.Baseline, res.Change)
		if !res.Baseline.Passed && !res.Change.Passed {
			res.Notes = append(res.Notes, "The profile also fails without the change; compare the failing tests to see whether the change makes things worse.")
		}
	}
	if res.MissingFiles > 0 {
		res.Notes = append(res.Notes, fmt.Sprintf("%d files of the repository were not available in the sandbox (binary or oversize files are not retained by the analysis).", res.MissingFiles))
	}
	if !res.Network {
		res.Notes = append(res.Notes, "The sandbox has no network access: dependencies that are not part of the repository or the sandbox environment cannot be downloaded.")
	}
	status := "failed"
	if res.Change.Passed {
		status = "passed"
	}
	resultJSON, _ := json.Marshal(res)
	_, err = r.Pool.Exec(context.WithoutCancel(ctx), `UPDATE verification_runs SET status = $3, result = $4, finished_at = now()
		WHERE organization_id = $1 AND id = $2`, job.OrgID, runID, status, resultJSON)
	if err != nil {
		return err
	}
	r.Log.Info("verification finished", "run_id", runID, "change_id", changeID, "profile", profile.ID, "status", status, "seconds", time.Since(started).Seconds())
	return nil
}

func (r *Runner) runError(ctx context.Context, job *orchestrator.Job, runID string, err error) error {
	if ctx.Err() != nil {
		// Cancellation or lease loss: the queue records the outcome.
		return err
	}
	msg := "the sandbox could not run: " + err.Error()
	r.Pool.Exec(context.WithoutCancel(ctx), `UPDATE verification_runs SET status = 'error', error = $3, finished_at = now() WHERE organization_id = $1 AND id = $2`, job.OrgID, runID, msg) //nolint:errcheck
	return orchestrator.Permanent(errors.New(msg))
}

func joinOutput(r Result) string {
	switch {
	case r.Stdout == "":
		return r.Stderr
	case r.Stderr == "":
		return r.Stdout
	}
	return r.Stdout + "\n" + r.Stderr
}

// compare lists tests failing only with the change, and only without it.
func compare(base, change *TreeResult) (newly, fixed []string) {
	failing := func(t *TreeResult) map[string]bool {
		out := map[string]bool{}
		for _, s := range t.Steps {
			if s.Tests != nil {
				for _, f := range s.Tests.Failing {
					out[f] = true
				}
			}
		}
		return out
	}
	b, c := failing(base), failing(change)
	for f := range c {
		if !b[f] {
			newly = append(newly, f)
		}
	}
	for f := range b {
		if !c[f] {
			fixed = append(fixed, f)
		}
	}
	sort.Strings(newly)
	sort.Strings(fixed)
	return newly, fixed
}

func (r *Runner) listSources(ctx context.Context, orgID, versionID string) ([]store.SourceFile, error) {
	var out []store.SourceFile
	const page = 5000
	for offset := 0; ; offset += page {
		files, total, err := r.Store.ListFiles(ctx, orgID, versionID, store.FileQuery{Recursive: true, Limit: page, Offset: offset})
		if err != nil {
			return nil, err
		}
		out = append(out, files...)
		if int64(offset+page) >= total || len(files) == 0 {
			return out, nil
		}
	}
}

func (r *Runner) buildSystems(ctx context.Context, orgID, versionID string) ([]string, error) {
	var raw []byte
	if err := r.Pool.QueryRow(ctx, `SELECT stats FROM repository_versions WHERE organization_id = $1 AND id = $2`, orgID, versionID).Scan(&raw); err != nil {
		return nil, err
	}
	var stats struct {
		BuildSystems []string `json:"build_systems"`
	}
	_ = json.Unmarshal(raw, &stats)
	return stats.BuildSystems, nil
}

// materialize writes the version's stored sources, with the change's files
// replacing or removing theirs, under dir. It returns how many repository
// files had no stored content.
func (r *Runner) materialize(ctx context.Context, orgID string, sources []store.SourceFile, overrides []FileChange, dir string) (int, error) {
	over := map[string]*FileChange{}
	for i := range overrides {
		over[overrides[i].Path] = &overrides[i]
	}
	missing := 0
	write := func(rel string, data []byte) error {
		target, err := safeJoin(dir, rel)
		if err != nil {
			return err
		}
		if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
			return err
		}
		return os.WriteFile(target, data, 0o644)
	}
	for _, f := range sources {
		if ctx.Err() != nil {
			return 0, ctx.Err()
		}
		if o, ok := over[f.Path]; ok {
			if o.Status != "deleted" {
				if err := write(f.Path, []byte(o.Content)); err != nil {
					return 0, err
				}
			}
			continue
		}
		if !f.ContentStored {
			missing++
			continue
		}
		data, err := blob.GetBytes(ctx, r.Blobs, pipeline.SourceKey(orgID, f.ContentHash), r.MaxFileBytes)
		if err != nil {
			if errors.Is(err, blob.ErrNotFound) {
				missing++
				continue
			}
			return 0, err
		}
		if err := write(f.Path, data); err != nil {
			return 0, err
		}
	}
	for _, o := range overrides {
		if o.Status == "created" {
			if err := write(o.Path, []byte(o.Content)); err != nil {
				return 0, err
			}
		}
	}
	// Let the sandbox user (mapped to the invoking user) write everywhere.
	return missing, filepath.WalkDir(dir, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			return os.Chmod(p, 0o755)
		}
		return nil
	})
}

// safeJoin joins a repository-relative path under dir, refusing anything
// that would leave it.
func safeJoin(dir, rel string) (string, error) {
	clean := path.Clean(rel)
	if rel == "" || clean != rel || path.IsAbs(clean) || clean == ".." || strings.HasPrefix(clean, "../") || strings.ContainsRune(clean, 0) {
		return "", fmt.Errorf("verify: unsafe path %q", rel)
	}
	return filepath.Join(dir, filepath.FromSlash(clean)), nil
}
