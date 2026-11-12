package orchestrator

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"runtime/debug"
	"sync"
	"time"
)

// Handler executes one job. report publishes progress. A returned error
// fails the job; wrap it with Permanent when retrying cannot help.
type Handler func(ctx context.Context, job *Job, report func(stage string, progress any)) error

type permanentError struct{ err error }

func (p permanentError) Error() string { return p.err.Error() }
func (p permanentError) Unwrap() error { return p.err }

// Permanent marks an error as not retryable (invalid input, limits exceeded).
func Permanent(err error) error {
	if err == nil {
		return nil
	}
	return permanentError{err}
}

// ErrCancelled is the cause attached to a job context when the user cancels.
var ErrCancelled = errors.New("job cancelled by user")

type Worker struct {
	Queue        *Queue
	Handler      Handler
	Concurrency  int
	Lease        time.Duration
	PollInterval time.Duration
	Version      string
	Capabilities any
	Log          *slog.Logger
	// OnFinish is called after each job (for metrics).
	OnFinish func(job *Job, outcome string, d time.Duration)

	id string
}

// Run registers the worker and processes jobs until ctx is cancelled, then
// waits for in-flight jobs to stop.
func (w *Worker) Run(ctx context.Context) error {
	if w.Concurrency < 1 {
		w.Concurrency = 1
	}
	if w.PollInterval <= 0 {
		w.PollInterval = 2 * time.Second
	}
	host, _ := os.Hostname()
	id, err := w.Queue.RegisterWorker(ctx, host, os.Getpid(), w.Version, w.Capabilities)
	if err != nil {
		return fmt.Errorf("register worker: %w", err)
	}
	w.id = id
	w.Log.Info("worker registered", "worker_id", id, "concurrency", w.Concurrency)

	var active sync.WaitGroup
	slots := make(chan struct{}, w.Concurrency)
	ticker := time.NewTicker(w.PollInterval)
	defer ticker.Stop()
	beat := time.NewTicker(15 * time.Second)
	defer beat.Stop()

	status := func() string {
		if len(slots) > 0 {
			return "busy"
		}
		return "idle"
	}
loop:
	for {
		// Claim as many jobs as there are free slots.
		for len(slots) < cap(slots) && ctx.Err() == nil {
			job, err := w.Queue.Claim(ctx, w.id, w.Lease)
			if err != nil {
				if ctx.Err() == nil {
					w.Log.Error("claim failed", "error", err)
				}
				break
			}
			if job == nil {
				break
			}
			slots <- struct{}{}
			active.Add(1)
			go func() {
				defer func() { <-slots; active.Done() }()
				w.execute(ctx, job)
			}()
		}
		select {
		case <-ctx.Done():
			break loop
		case <-ticker.C:
		case <-beat.C:
			if err := w.Queue.WorkerHeartbeat(ctx, w.id, status()); err != nil && ctx.Err() == nil {
				w.Log.Warn("worker heartbeat failed", "error", err)
			}
		}
	}
	active.Wait()
	stop, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	return w.Queue.WorkerHeartbeat(stop, w.id, "stopped")
}

func (w *Worker) execute(parent context.Context, job *Job) {
	log := w.Log.With("job_id", job.ID, "version_id", job.VersionID, "attempt", job.Attempts)
	started := time.Now()
	ctx, cancel := context.WithCancelCause(parent)
	defer cancel(nil)

	// Heartbeat: keeps the lease, observes cancellation, detects lease loss.
	done := make(chan struct{})
	var hb sync.WaitGroup
	hb.Add(1)
	go func() {
		defer hb.Done()
		t := time.NewTicker(w.Lease / 3)
		defer t.Stop()
		for {
			select {
			case <-done:
				return
			case <-t.C:
				cancelRequested, err := w.Queue.Heartbeat(context.WithoutCancel(ctx), job.ID, w.id, w.Lease)
				switch {
				case errors.Is(err, ErrLeaseLost):
					cancel(ErrLeaseLost)
					return
				case err != nil:
					log.Warn("job heartbeat failed", "error", err)
				case cancelRequested:
					cancel(ErrCancelled)
					return
				}
			}
		}
	}()

	report := func(stage string, progress any) {
		if err := w.Queue.Progress(context.WithoutCancel(ctx), job.ID, w.id, stage, progress); err != nil && !errors.Is(err, ErrLeaseLost) {
			log.Warn("progress update failed", "error", err)
		}
	}
	err := w.safely(ctx, job, report)
	close(done)
	hb.Wait()

	cause := context.Cause(ctx)
	outcome, msg, retry := "succeeded", "", false
	switch {
	case errors.Is(cause, ErrLeaseLost):
		log.Warn("lease lost; abandoning job")
		return
	case errors.Is(cause, ErrCancelled):
		outcome, msg = "cancelled", "cancelled by user"
	case err == nil:
	case parent.Err() != nil:
		// Shutdown: leave the job running; its lease will expire and another
		// worker will pick it up.
		log.Info("worker shutting down; job will be retried")
		return
	default:
		outcome, msg = "failed", err.Error()
		var perm permanentError
		retry = !errors.As(err, &perm)
	}

	finishCtx, cancelFinish := context.WithTimeout(context.WithoutCancel(parent), 15*time.Second)
	defer cancelFinish()
	final, ferr := w.Queue.Finish(finishCtx, job, w.id, outcome, msg, retry)
	if ferr != nil {
		log.Error("recording job outcome failed", "error", ferr, "outcome", outcome)
		return
	}
	if final {
		w.Queue.workerFinished(finishCtx, w.id, outcome == "succeeded")
	}
	log.Info("job finished", "outcome", outcome, "final", final, "duration_ms", time.Since(started).Milliseconds(), "error", msg)
	if w.OnFinish != nil {
		w.OnFinish(job, outcome, time.Since(started))
	}
}

// safely converts a handler panic into a permanent failure so one bad job
// cannot take the worker process down.
func (w *Worker) safely(ctx context.Context, job *Job, report func(string, any)) (err error) {
	defer func() {
		if r := recover(); r != nil {
			w.Log.Error("job handler panicked", "job_id", job.ID, "panic", fmt.Sprint(r), "stack", string(debug.Stack()))
			err = Permanent(fmt.Errorf("internal error while analyzing (panic)"))
		}
	}()
	return w.Handler(ctx, job, report)
}
