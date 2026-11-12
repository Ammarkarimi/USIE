// Package orchestrator schedules analysis work: a PostgreSQL-backed job
// queue with leases, heartbeats, bounded retries and cooperative
// cancellation, plus the worker loop that executes jobs.
//
// Delivery is at-least-once: a job whose worker dies is re-queued when its
// lease expires, so handlers must be idempotent. (ADR 0005 records why this
// is a table and not Kafka/Temporal at the current scale.)
package orchestrator

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

type Job struct {
	ID              string          `json:"id"`
	OrgID           string          `json:"-"`
	RepositoryID    string          `json:"repository_id"`
	VersionID       string          `json:"version_id"`
	Kind            string          `json:"kind"`
	Status          string          `json:"status"`
	Priority        int             `json:"priority"`
	Attempts        int             `json:"attempts"`
	MaxAttempts     int             `json:"max_attempts"`
	CancelRequested bool            `json:"cancel_requested"`
	WorkerID        *string         `json:"worker_id"`
	Stage           string          `json:"stage"`
	Progress        json.RawMessage `json:"progress"`
	Error           string          `json:"error"`
	CreatedAt       time.Time       `json:"created_at"`
	StartedAt       *time.Time      `json:"started_at"`
	FinishedAt      *time.Time      `json:"finished_at"`
	UpdatedAt       time.Time       `json:"updated_at"`
	RepositoryName  string          `json:"repository_name"`
}

const jobColumns = `j.id, j.organization_id, j.repository_id, j.version_id, j.kind, j.status, j.priority, j.attempts, j.max_attempts,
	j.cancel_requested, j.worker_id::text, j.stage, j.progress, j.error, j.created_at, j.started_at, j.finished_at, j.updated_at`

func scanJob(row pgx.Row, extra ...any) (Job, error) {
	var j Job
	dest := append([]any{&j.ID, &j.OrgID, &j.RepositoryID, &j.VersionID, &j.Kind, &j.Status, &j.Priority, &j.Attempts, &j.MaxAttempts,
		&j.CancelRequested, &j.WorkerID, &j.Stage, &j.Progress, &j.Error, &j.CreatedAt, &j.StartedAt, &j.FinishedAt, &j.UpdatedAt}, extra...)
	return j, row.Scan(dest...)
}

var ErrNotFound = errors.New("job not found")

// ErrLeaseLost means another worker owns the job now (the lease expired and
// the job was re-queued). The current execution must stop and write nothing.
var ErrLeaseLost = errors.New("job lease lost")

type Queue struct{ pool *pgxpool.Pool }

func NewQueue(pool *pgxpool.Pool) *Queue { return &Queue{pool: pool} }

// Claim leases the next runnable job to a worker, or returns nil if none.
// Jobs whose lease expired are recovered first: re-queued if attempts
// remain, otherwise failed.
func (q *Queue) Claim(ctx context.Context, workerID string, lease time.Duration) (*Job, error) {
	if err := q.recoverExpired(ctx); err != nil {
		return nil, err
	}
	row := q.pool.QueryRow(ctx, `
		UPDATE analysis_jobs j SET status = 'running', worker_id = $1, attempts = attempts + 1,
		       lease_expires_at = now() + $2::interval, started_at = coalesce(started_at, now()), updated_at = now(), error = ''
		 WHERE j.id = (
			SELECT id FROM analysis_jobs
			 WHERE status = 'queued' AND run_after <= now()
			 ORDER BY priority DESC, created_at
			 FOR UPDATE SKIP LOCKED LIMIT 1)
		RETURNING `+jobColumns, workerID, lease.String())
	job, err := scanJob(row)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	return &job, nil
}

func (q *Queue) recoverExpired(ctx context.Context) error {
	return pgx.BeginFunc(ctx, q.pool, func(tx pgx.Tx) error {
		// Out of attempts: fail the job and its version.
		if _, err := tx.Exec(ctx, `
			WITH dead AS (
				UPDATE analysis_jobs SET status = 'failed', finished_at = now(), updated_at = now(), lease_expires_at = NULL,
				       error = 'worker stopped responding and the retry limit was reached'
				 WHERE status = 'running' AND lease_expires_at < now() AND attempts >= max_attempts
				RETURNING version_id)
			UPDATE repository_versions v SET status = 'failed', error = 'analysis worker stopped responding'
			  FROM dead WHERE v.id = dead.version_id AND v.status IN ('pending', 'analyzing')`); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `
			UPDATE analysis_jobs SET status = 'queued', worker_id = NULL, lease_expires_at = NULL, updated_at = now(),
			       run_after = now() + interval '5 seconds', stage = 'requeued after lost lease'
			 WHERE status = 'running' AND lease_expires_at < now() AND attempts < max_attempts`)
		return err
	})
}

// Heartbeat extends the lease and reports whether cancellation was requested.
func (q *Queue) Heartbeat(ctx context.Context, jobID, workerID string, lease time.Duration) (cancelRequested bool, err error) {
	err = q.pool.QueryRow(ctx, `UPDATE analysis_jobs SET lease_expires_at = now() + $3::interval, updated_at = now()
		WHERE id = $1 AND worker_id = $2 AND status = 'running' RETURNING cancel_requested`, jobID, workerID, lease.String()).Scan(&cancelRequested)
	if errors.Is(err, pgx.ErrNoRows) {
		return false, ErrLeaseLost
	}
	return cancelRequested, err
}

// Progress records the current stage and counters of a running job.
func (q *Queue) Progress(ctx context.Context, jobID, workerID, stage string, progress any) error {
	data, err := json.Marshal(progress)
	if err != nil {
		return err
	}
	tag, err := q.pool.Exec(ctx, `UPDATE analysis_jobs SET stage = $3, progress = $4, updated_at = now()
		WHERE id = $1 AND worker_id = $2 AND status = 'running'`, jobID, workerID, stage, data)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrLeaseLost
	}
	return err
}

// Finish moves a running job to a terminal state. outcome is "succeeded",
// "failed" or "cancelled". A failed job with attempts remaining is re-queued
// with exponential backoff when retry is true.
func (q *Queue) Finish(ctx context.Context, job *Job, workerID, outcome, errMsg string, retry bool) (final bool, err error) {
	if len(errMsg) > 2000 {
		errMsg = errMsg[:2000]
	}
	return final, pgx.BeginFunc(ctx, q.pool, func(tx pgx.Tx) error {
		if outcome == "failed" && retry && job.Attempts < job.MaxAttempts {
			backoff := time.Duration(1<<uint(job.Attempts)) * 15 * time.Second
			tag, err := tx.Exec(ctx, `UPDATE analysis_jobs SET status = 'queued', worker_id = NULL, lease_expires_at = NULL,
				run_after = now() + $3::interval, error = $4, stage = 'waiting to retry', updated_at = now()
				WHERE id = $1 AND worker_id = $2 AND status = 'running'`, job.ID, workerID, backoff.String(), errMsg)
			if err == nil && tag.RowsAffected() == 0 {
				return ErrLeaseLost
			}
			return err
		}
		final = true
		tag, err := tx.Exec(ctx, `UPDATE analysis_jobs SET status = $3, error = $4, finished_at = now(), lease_expires_at = NULL, updated_at = now()
			WHERE id = $1 AND worker_id = $2 AND status = 'running'`, job.ID, workerID, outcome, errMsg)
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return ErrLeaseLost
		}
		if outcome != "succeeded" {
			// The handler marks the version ready itself, atomically with its results.
			_, err = tx.Exec(ctx, `UPDATE repository_versions SET status = $2, error = $3 WHERE id = $1 AND status IN ('pending', 'analyzing')`,
				job.VersionID, outcome, errMsg)
		}
		return err
	})
}

// RequestCancel cancels a queued job immediately and flags a running job so
// its worker stops at the next heartbeat. Terminal jobs are left unchanged.
func (q *Queue) RequestCancel(ctx context.Context, orgID, jobID string) (Job, error) {
	var job Job
	err := pgx.BeginFunc(ctx, q.pool, func(tx pgx.Tx) error {
		var err error
		job, err = scanJob(tx.QueryRow(ctx, `SELECT `+jobColumns+` FROM analysis_jobs j WHERE j.organization_id = $1 AND j.id = $2 FOR UPDATE`, orgID, jobID))
		if err != nil {
			return err
		}
		switch job.Status {
		case "queued":
			if _, err := tx.Exec(ctx, `UPDATE analysis_jobs SET status = 'cancelled', cancel_requested = true, finished_at = now(), updated_at = now() WHERE id = $1`, jobID); err != nil {
				return err
			}
			if _, err := tx.Exec(ctx, `UPDATE repository_versions SET status = 'cancelled' WHERE id = $1 AND status IN ('pending', 'analyzing')`, job.VersionID); err != nil {
				return err
			}
			job.Status, job.CancelRequested = "cancelled", true
		case "running":
			if _, err := tx.Exec(ctx, `UPDATE analysis_jobs SET cancel_requested = true, updated_at = now() WHERE id = $1`, jobID); err != nil {
				return err
			}
			job.CancelRequested = true
		}
		return nil
	})
	if errors.Is(err, pgx.ErrNoRows) {
		return job, ErrNotFound
	}
	return job, err
}

func (q *Queue) Get(ctx context.Context, orgID, jobID string) (Job, error) {
	var name string
	job, err := scanJob(q.pool.QueryRow(ctx, `SELECT `+jobColumns+`, r.name FROM analysis_jobs j JOIN repositories r ON r.id = j.repository_id
		WHERE j.organization_id = $1 AND j.id = $2`, orgID, jobID), &name)
	if errors.Is(err, pgx.ErrNoRows) {
		return job, ErrNotFound
	}
	job.RepositoryName = name
	return job, err
}

func (q *Queue) List(ctx context.Context, orgID, status string, limit int) ([]Job, error) {
	rows, err := q.pool.Query(ctx, `SELECT `+jobColumns+`, r.name FROM analysis_jobs j JOIN repositories r ON r.id = j.repository_id
		WHERE j.organization_id = $1 AND ($2 = '' OR j.status = $2) ORDER BY j.created_at DESC LIMIT $3`, orgID, status, limit)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, func(row pgx.CollectableRow) (Job, error) {
		var name string
		job, err := scanJob(row, &name)
		job.RepositoryName = name
		return job, err
	})
}

// QueueDepth reports the number of queued and running jobs (all tenants);
// exported as service metrics.
func (q *Queue) QueueDepth(ctx context.Context) (queued, running int64, err error) {
	err = q.pool.QueryRow(ctx, `SELECT count(*) FILTER (WHERE status = 'queued'), count(*) FILTER (WHERE status = 'running') FROM analysis_jobs
		WHERE status IN ('queued', 'running')`).Scan(&queued, &running)
	return
}

// ---- workers --------------------------------------------------------------

type WorkerInfo struct {
	ID              string          `json:"id"`
	Hostname        string          `json:"hostname"`
	PID             int             `json:"pid"`
	Version         string          `json:"version"`
	Capabilities    json.RawMessage `json:"capabilities"`
	Status          string          `json:"status"`
	StartedAt       time.Time       `json:"started_at"`
	LastHeartbeatAt time.Time       `json:"last_heartbeat_at"`
	JobsCompleted   int             `json:"jobs_completed"`
	JobsFailed      int             `json:"jobs_failed"`
	// Online is derived: a heartbeat was seen recently and the worker has not stopped.
	Online bool `json:"online"`
}

func (q *Queue) RegisterWorker(ctx context.Context, hostname string, pid int, version string, capabilities any) (string, error) {
	caps, err := json.Marshal(capabilities)
	if err != nil {
		return "", err
	}
	var id string
	err = q.pool.QueryRow(ctx, `INSERT INTO workers (hostname, pid, version, capabilities) VALUES ($1, $2, $3, $4) RETURNING id`, hostname, pid, version, caps).Scan(&id)
	return id, err
}

func (q *Queue) WorkerHeartbeat(ctx context.Context, workerID, status string) error {
	_, err := q.pool.Exec(ctx, `UPDATE workers SET last_heartbeat_at = now(), status = $2 WHERE id = $1`, workerID, status)
	return err
}

func (q *Queue) workerFinished(ctx context.Context, workerID string, succeeded bool) {
	col := "jobs_failed"
	if succeeded {
		col = "jobs_completed"
	}
	q.pool.Exec(ctx, fmt.Sprintf(`UPDATE workers SET %s = %s + 1 WHERE id = $1`, col, col), workerID) //nolint:errcheck
}

// ListWorkers lists workers seen in the last day. Workers are shared
// infrastructure, so the listing is not tenant-scoped and carries no tenant data.
func (q *Queue) ListWorkers(ctx context.Context) ([]WorkerInfo, error) {
	rows, err := q.pool.Query(ctx, `SELECT id, hostname, pid, version, capabilities, status, started_at, last_heartbeat_at, jobs_completed, jobs_failed,
		status <> 'stopped' AND last_heartbeat_at > now() - interval '45 seconds'
		FROM workers WHERE last_heartbeat_at > now() - interval '1 day' ORDER BY started_at DESC LIMIT 200`)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, pgx.RowToStructByPos[WorkerInfo])
}
