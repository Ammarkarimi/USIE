package store

import (
	"context"
	"encoding/json"
	"time"

	"github.com/jackc/pgx/v5"
)

type Workspace struct {
	ID        string    `json:"id"`
	Name      string    `json:"name"`
	CreatedAt time.Time `json:"created_at"`
}

func (s *Store) ListWorkspaces(ctx context.Context, orgID string) ([]Workspace, error) {
	rows, err := s.Pool.Query(ctx, `SELECT id, name, created_at FROM workspaces WHERE organization_id = $1 ORDER BY created_at`, orgID)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, pgx.RowToStructByPos[Workspace])
}

func (s *Store) CreateWorkspace(ctx context.Context, orgID, name string) (Workspace, error) {
	w := Workspace{Name: name}
	err := s.Pool.QueryRow(ctx, `INSERT INTO workspaces (organization_id, name) VALUES ($1, $2) RETURNING id, created_at`, orgID, name).Scan(&w.ID, &w.CreatedAt)
	return w, mapErr(err)
}

type Repository struct {
	ID             string    `json:"id"`
	WorkspaceID    string    `json:"workspace_id"`
	Name           string    `json:"name"`
	SourceKind     string    `json:"source_kind"`
	SourceLocation string    `json:"source_location"`
	DefaultRef     string    `json:"default_ref"`
	CreatedAt      time.Time `json:"created_at"`
	// LatestVersion is the most recent version of any status, if one exists.
	LatestVersion *Version `json:"latest_version"`
}

type Version struct {
	ID              string          `json:"id"`
	RepositoryID    string          `json:"repository_id"`
	Sequence        int             `json:"sequence"`
	Status          string          `json:"status"`
	RequestedRef    string          `json:"requested_ref"`
	CommitSHA       string          `json:"commit_sha"`
	Branch          string          `json:"branch"`
	TreeHash        string          `json:"tree_hash"`
	ParentVersionID *string         `json:"parent_version_id"`
	UploadKey       string          `json:"-"`
	Stats           json.RawMessage `json:"stats"`
	Error           string          `json:"error"`
	CreatedAt       time.Time       `json:"created_at"`
	AnalyzedAt      *time.Time      `json:"analyzed_at"`
}

const versionColumns = `v.id, v.repository_id, v.sequence, v.status, v.requested_ref, v.commit_sha, v.branch, v.tree_hash,
	v.parent_version_id::text, v.upload_key, v.stats, v.error, v.created_at, v.analyzed_at`

func scanVersion(row pgx.Row, v *Version) error {
	return row.Scan(&v.ID, &v.RepositoryID, &v.Sequence, &v.Status, &v.RequestedRef, &v.CommitSHA, &v.Branch, &v.TreeHash,
		&v.ParentVersionID, &v.UploadKey, &v.Stats, &v.Error, &v.CreatedAt, &v.AnalyzedAt)
}

func (s *Store) CreateRepository(ctx context.Context, orgID, userID string, r Repository) (Repository, error) {
	err := s.Pool.QueryRow(ctx, `
		INSERT INTO repositories (organization_id, workspace_id, name, source_kind, source_location, default_ref, created_by)
		SELECT $1, w.id, $3, $4, $5, $6, $7 FROM workspaces w WHERE w.organization_id = $1 AND w.id = $2
		RETURNING id, created_at`, orgID, r.WorkspaceID, r.Name, r.SourceKind, r.SourceLocation, r.DefaultRef, userID).Scan(&r.ID, &r.CreatedAt)
	return r, mapErr(err)
}

const repositoryColumns = `r.id, r.workspace_id, r.name, r.source_kind, r.source_location, r.default_ref, r.created_at`

func (s *Store) ListRepositories(ctx context.Context, orgID string) ([]Repository, error) {
	rows, err := s.Pool.Query(ctx, `
		SELECT `+repositoryColumns+`, v.id IS NOT NULL, `+versionColumnsNullable+`
		  FROM repositories r
		  LEFT JOIN LATERAL (SELECT * FROM repository_versions x WHERE x.repository_id = r.id ORDER BY x.sequence DESC LIMIT 1) v ON true
		 WHERE r.organization_id = $1 ORDER BY r.created_at DESC`, orgID)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, scanRepository)
}

// versionColumnsNullable is versionColumns with defaults for a missing row.
const versionColumnsNullable = `coalesce(v.id::text, ''), coalesce(v.repository_id::text, ''), coalesce(v.sequence, 0), coalesce(v.status, ''),
	coalesce(v.requested_ref, ''), coalesce(v.commit_sha, ''), coalesce(v.branch, ''), coalesce(v.tree_hash, ''),
	v.parent_version_id::text, coalesce(v.upload_key, ''), coalesce(v.stats, '{}'), coalesce(v.error, ''), coalesce(v.created_at, now()), v.analyzed_at`

func scanRepository(row pgx.CollectableRow) (Repository, error) {
	var r Repository
	var has bool
	var v Version
	err := row.Scan(&r.ID, &r.WorkspaceID, &r.Name, &r.SourceKind, &r.SourceLocation, &r.DefaultRef, &r.CreatedAt, &has,
		&v.ID, &v.RepositoryID, &v.Sequence, &v.Status, &v.RequestedRef, &v.CommitSHA, &v.Branch, &v.TreeHash,
		&v.ParentVersionID, &v.UploadKey, &v.Stats, &v.Error, &v.CreatedAt, &v.AnalyzedAt)
	if has {
		r.LatestVersion = &v
	}
	return r, err
}

func (s *Store) GetRepository(ctx context.Context, orgID, id string) (Repository, error) {
	rows, err := s.Pool.Query(ctx, `
		SELECT `+repositoryColumns+`, v.id IS NOT NULL, `+versionColumnsNullable+`
		  FROM repositories r
		  LEFT JOIN LATERAL (SELECT * FROM repository_versions x WHERE x.repository_id = r.id ORDER BY x.sequence DESC LIMIT 1) v ON true
		 WHERE r.organization_id = $1 AND r.id = $2`, orgID, id)
	if err != nil {
		return Repository{}, err
	}
	r, err := pgx.CollectExactlyOneRow(rows, scanRepository)
	return r, mapErr(err)
}

func (s *Store) DeleteRepository(ctx context.Context, orgID, id string) error {
	tag, err := s.Pool.Exec(ctx, `DELETE FROM repositories WHERE organization_id = $1 AND id = $2`, orgID, id)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

// CreateVersionAndJob records a new version of a repository and queues its
// analysis in one transaction: a version never exists without a job.
func (s *Store) CreateVersionAndJob(ctx context.Context, orgID, userID, repoID, ref, uploadKey string) (versionID, jobID string, err error) {
	err = pgx.BeginFunc(ctx, s.Pool, func(tx pgx.Tx) error {
		// Lock the repository row so concurrent requests get distinct sequences.
		var locked string
		if err := tx.QueryRow(ctx, `SELECT id FROM repositories WHERE organization_id = $1 AND id = $2 FOR UPDATE`, orgID, repoID).Scan(&locked); err != nil {
			return err
		}
		if err := tx.QueryRow(ctx, `
			INSERT INTO repository_versions (organization_id, repository_id, sequence, requested_ref, upload_key, created_by)
			SELECT $1, $2, coalesce(max(sequence), 0) + 1, $3, $4, $5 FROM repository_versions WHERE repository_id = $2
			RETURNING id`, orgID, repoID, ref, uploadKey, userID).Scan(&versionID); err != nil {
			return err
		}
		return tx.QueryRow(ctx, `INSERT INTO analysis_jobs (organization_id, repository_id, version_id, kind, created_by)
			VALUES ($1, $2, $3, 'analyze_version', $4) RETURNING id`, orgID, repoID, versionID, userID).Scan(&jobID)
	})
	return versionID, jobID, mapErr(err)
}

func (s *Store) GetVersion(ctx context.Context, orgID, id string) (Version, error) {
	var v Version
	err := scanVersion(s.Pool.QueryRow(ctx, `SELECT `+versionColumns+` FROM repository_versions v WHERE v.organization_id = $1 AND v.id = $2`, orgID, id), &v)
	return v, mapErr(err)
}

func (s *Store) ListVersions(ctx context.Context, orgID, repoID string, limit int) ([]Version, error) {
	rows, err := s.Pool.Query(ctx, `SELECT `+versionColumns+` FROM repository_versions v
		WHERE v.organization_id = $1 AND v.repository_id = $2 ORDER BY v.sequence DESC LIMIT $3`, orgID, repoID, limit)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, func(row pgx.CollectableRow) (Version, error) {
		var v Version
		return v, scanVersion(row, &v)
	})
}

type SourceFile struct {
	Path            string     `json:"path"`
	ContentHash     string     `json:"content_hash"`
	SizeBytes       int64      `json:"size_bytes"`
	Language        string     `json:"language"`
	Category        string     `json:"category"`
	Encoding        string     `json:"encoding"`
	IsGenerated     bool       `json:"is_generated"`
	IsVendored      bool       `json:"is_vendored"`
	ContentStored   bool       `json:"content_stored"`
	ParseStatus     string     `json:"parse_status"`
	ParseDetail     string     `json:"parse_detail"`
	Change          string     `json:"change"`
	LineCount       int        `json:"line_count"`
	SymbolCount     int        `json:"symbol_count"`
	DiagnosticCount int        `json:"diagnostic_count"`
	AnalyzedAt      *time.Time `json:"analyzed_at"`
}

const fileColumns = `path, content_hash, size_bytes, language, category, encoding, is_generated, is_vendored, content_stored,
	parse_status, parse_detail, change, line_count, symbol_count, diagnostic_count, analyzed_at`

func (s *Store) GetFile(ctx context.Context, orgID, versionID, path string) (SourceFile, error) {
	rows, err := s.Pool.Query(ctx, `SELECT `+fileColumns+` FROM source_files WHERE organization_id = $1 AND version_id = $2 AND path = $3`, orgID, versionID, path)
	if err != nil {
		return SourceFile{}, err
	}
	f, err := pgx.CollectExactlyOneRow(rows, pgx.RowToStructByPos[SourceFile])
	return f, mapErr(err)
}

type FileQuery struct {
	// Dir lists the direct children of a directory ("" for the root).
	// When Recursive is set, every file below Dir is returned instead.
	Dir         string
	Recursive   bool
	Language    string
	Category    string
	ParseStatus string
	Limit       int
	Offset      int
}

type DirEntry struct {
	Name string `json:"name"`
	Path string `json:"path"`
	// Files is the number of files below this directory.
	Files int64 `json:"files"`
}

// ListDir returns the sub-directories and files directly inside a directory.
func (s *Store) ListDir(ctx context.Context, orgID, versionID, dir string) ([]DirEntry, []SourceFile, error) {
	prefix := ""
	if dir != "" {
		prefix = dir + "/"
	}
	rows, err := s.Pool.Query(ctx, `
		SELECT name, files FROM (
			SELECT split_part(substr(path, length($3) + 1), '/', 1) AS name, count(*) AS files
			  FROM source_files
			 WHERE organization_id = $1 AND version_id = $2 AND starts_with(path, $3) AND strpos(substr(path, length($3) + 1), '/') > 0
			 GROUP BY 1) d
		 ORDER BY name COLLATE "C"`, orgID, versionID, prefix)
	if err != nil {
		return nil, nil, err
	}
	dirs, err := pgx.CollectRows(rows, func(row pgx.CollectableRow) (DirEntry, error) {
		var d DirEntry
		err := row.Scan(&d.Name, &d.Files)
		d.Path = prefix + d.Name
		return d, err
	})
	if err != nil {
		return nil, nil, err
	}
	rows, err = s.Pool.Query(ctx, `SELECT `+fileColumns+` FROM source_files
		WHERE organization_id = $1 AND version_id = $2 AND starts_with(path, $3) AND strpos(substr(path, length($3) + 1), '/') = 0
		ORDER BY path COLLATE "C" LIMIT 5000`, orgID, versionID, prefix)
	if err != nil {
		return nil, nil, err
	}
	files, err := pgx.CollectRows(rows, pgx.RowToStructByPos[SourceFile])
	return dirs, files, err
}

// ListFiles returns a filtered, paged flat listing.
func (s *Store) ListFiles(ctx context.Context, orgID, versionID string, q FileQuery) ([]SourceFile, int64, error) {
	prefix := ""
	if q.Dir != "" {
		prefix = q.Dir + "/"
	}
	const where = ` FROM source_files WHERE organization_id = $1 AND version_id = $2 AND starts_with(path, $3)
		AND ($4 = '' OR language = $4) AND ($5 = '' OR category = $5) AND ($6 = '' OR parse_status = $6)`
	args := []any{orgID, versionID, prefix, q.Language, q.Category, q.ParseStatus}
	var total int64
	if err := s.Pool.QueryRow(ctx, `SELECT count(*)`+where, args...).Scan(&total); err != nil {
		return nil, 0, err
	}
	rows, err := s.Pool.Query(ctx, `SELECT `+fileColumns+where+` ORDER BY path COLLATE "C" LIMIT $7 OFFSET $8`, append(args, q.Limit, q.Offset)...)
	if err != nil {
		return nil, 0, err
	}
	files, err := pgx.CollectRows(rows, pgx.RowToStructByPos[SourceFile])
	return files, total, err
}

type Reference struct {
	Path       string `json:"path"`
	Line       int    `json:"line"`
	Col        int    `json:"col"`
	Kind       string `json:"kind"`
	Text       string `json:"text"`
	SourceKey  string `json:"source_key"`
	Outcome    string `json:"outcome"`
	Confidence string `json:"confidence"`
	TargetKey  string `json:"target_key"`
	Rule       string `json:"rule"`
	Reason     string `json:"reason"`
}

type ReferenceQuery struct {
	Path      string
	Outcome   string
	TargetKey string
	SourceKey string
	Limit     int
	Offset    int
}

func (s *Store) ListReferences(ctx context.Context, orgID, versionID string, q ReferenceQuery) ([]Reference, int64, error) {
	const where = ` FROM symbol_references WHERE organization_id = $1 AND version_id = $2
		AND ($3 = '' OR path = $3) AND ($4 = '' OR outcome = $4) AND ($5 = '' OR target_key = $5) AND ($6 = '' OR source_key = $6)`
	args := []any{orgID, versionID, q.Path, q.Outcome, q.TargetKey, q.SourceKey}
	var total int64
	if err := s.Pool.QueryRow(ctx, `SELECT count(*)`+where, args...).Scan(&total); err != nil {
		return nil, 0, err
	}
	rows, err := s.Pool.Query(ctx, `SELECT path, line, col, kind, text, source_key, outcome, confidence, target_key, rule, reason`+where+
		` ORDER BY path COLLATE "C", line, col, seq LIMIT $7 OFFSET $8`, append(args, q.Limit, q.Offset)...)
	if err != nil {
		return nil, 0, err
	}
	refs, err := pgx.CollectRows(rows, pgx.RowToStructByPos[Reference])
	return refs, total, err
}

type Dependency struct {
	ManifestPath string `json:"manifest_path"`
	Ecosystem    string `json:"ecosystem"`
	Name         string `json:"name"`
	Requirement  string `json:"requirement"`
	Scope        string `json:"scope"`
}

func (s *Store) ListDependencies(ctx context.Context, orgID, versionID string) ([]Dependency, error) {
	rows, err := s.Pool.Query(ctx, `SELECT manifest_path, ecosystem, name, requirement, scope FROM dependencies
		WHERE organization_id = $1 AND version_id = $2 ORDER BY ecosystem, name COLLATE "C", manifest_path COLLATE "C", scope LIMIT 5000`, orgID, versionID)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, pgx.RowToStructByPos[Dependency])
}

type Commit struct {
	SHA         string     `json:"sha"`
	Parents     []string   `json:"parents"`
	AuthorName  string     `json:"author_name"`
	AuthoredAt  *time.Time `json:"authored_at"`
	CommittedAt *time.Time `json:"committed_at"`
	Subject     string     `json:"subject"`
}

// ListCommits returns recent history. Author e-mail addresses are stored but
// not exposed through this listing.
func (s *Store) ListCommits(ctx context.Context, orgID, repoID string, limit int) ([]Commit, error) {
	rows, err := s.Pool.Query(ctx, `SELECT sha, parents, author_name, authored_at, committed_at, subject FROM commits
		WHERE organization_id = $1 AND repository_id = $2 ORDER BY committed_at DESC NULLS LAST, sha LIMIT $3`, orgID, repoID, limit)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, pgx.RowToStructByPos[Commit])
}

type Branch struct {
	Name      string `json:"name"`
	HeadSHA   string `json:"head_sha"`
	IsDefault bool   `json:"is_default"`
}

func (s *Store) ListBranches(ctx context.Context, orgID, repoID string) ([]Branch, error) {
	rows, err := s.Pool.Query(ctx, `SELECT name, head_sha, is_default FROM branches
		WHERE organization_id = $1 AND repository_id = $2 ORDER BY is_default DESC, name COLLATE "C" LIMIT 2000`, orgID, repoID)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, pgx.RowToStructByPos[Branch])
}
