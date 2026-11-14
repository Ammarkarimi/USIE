// usie-worker executes analysis jobs from the queue.
package main

import (
	"context"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/collectors"
	"github.com/prometheus/client_golang/prometheus/promhttp"

	"github.com/ammarkarimi/usie/internal/pipeline"
	"github.com/ammarkarimi/usie/internal/platform/blob"
	"github.com/ammarkarimi/usie/internal/platform/config"
	"github.com/ammarkarimi/usie/internal/platform/runtime"
	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
	analysis "github.com/ammarkarimi/usie/services/semantic-analysis"
)

func main() {
	cfg, err := config.Load()
	log := runtime.Logger(cfg.LogLevel, "usie-worker")
	if err != nil {
		log.Error("invalid configuration", "error", err.Error())
		os.Exit(2)
	}
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	engine := analysis.Engine{Bin: cfg.EngineBin, ParseTimeout: cfg.ParseTimeout, MaxFileBytes: cfg.MaxFileBytes}
	infoCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	info, err := engine.Info(infoCtx)
	cancel()
	if err != nil {
		// A worker without an engine cannot do anything useful: fail fast.
		log.Error("analysis engine unavailable", "bin", cfg.EngineBin, "error", err.Error())
		os.Exit(1)
	}

	pool, err := runtime.Database(ctx, cfg, log)
	if err != nil {
		log.Error("startup failed", "error", err.Error())
		os.Exit(1)
	}
	defer pool.Close()
	blobs, err := blob.NewFS(cfg.BlobDir())
	if err != nil {
		log.Error("blob store unavailable", "error", err.Error())
		os.Exit(1)
	}
	if err := os.MkdirAll(cfg.WorkDir(), 0o700); err != nil {
		log.Error("work directory unavailable", "error", err.Error())
		os.Exit(1)
	}

	finished := prometheus.NewCounterVec(prometheus.CounterOpts{Name: "usie_worker_jobs_finished_total", Help: "Jobs finished by outcome."}, []string{"outcome"})
	duration := prometheus.NewHistogram(prometheus.HistogramOpts{Name: "usie_worker_job_duration_seconds", Help: "Job execution time.", Buckets: prometheus.ExponentialBuckets(0.5, 2, 12)})
	registry := prometheus.NewRegistry()
	registry.MustRegister(finished, duration, collectors.NewGoCollector(), collectors.NewProcessCollector(collectors.ProcessCollectorOpts{}))
	if addr := os.Getenv("USIE_WORKER_METRICS_ADDR"); addr != "" {
		mux := http.NewServeMux()
		mux.Handle("GET /metrics", promhttp.HandlerFor(registry, promhttp.HandlerOpts{}))
		mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) { w.Write([]byte("ok")) }) //nolint:errcheck
		srv := &http.Server{Addr: addr, Handler: mux, ReadHeaderTimeout: 5 * time.Second}
		go func() {
			if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
				log.Error("metrics listener failed", "error", err.Error())
			}
		}()
		go func() { <-ctx.Done(); srv.Close() }()
	}

	p := &pipeline.Pipeline{Pool: pool, Blobs: blobs, Engine: engine, Info: info, Cfg: cfg, Log: log}
	languages := make([]string, 0, len(info.Languages))
	for _, l := range info.Languages {
		languages = append(languages, l.Language)
	}
	worker := &orchestrator.Worker{
		Queue: orchestrator.NewQueue(pool), Handler: p.Handle, Concurrency: cfg.WorkerConcurrency, Lease: cfg.JobLease,
		Version: runtime.Version, Log: log,
		Capabilities: map[string]any{"job_kinds": []string{"analyze_version"}, "languages": languages, "engine_version": info.Version},
		OnFinish: func(_ *orchestrator.Job, outcome string, d time.Duration) {
			finished.WithLabelValues(outcome).Inc()
			duration.Observe(d.Seconds())
		},
	}
	if err := worker.Run(ctx); err != nil {
		log.Error("worker stopped with error", "error", err.Error())
		os.Exit(1)
	}
	log.Info("stopped")
}
