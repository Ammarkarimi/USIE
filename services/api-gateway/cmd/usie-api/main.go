// usie-api serves the HTTP API and, optionally, the web application.
package main

import (
	"context"
	"errors"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/ammarkarimi/usie/internal/platform/blob"
	"github.com/ammarkarimi/usie/internal/platform/config"
	"github.com/ammarkarimi/usie/internal/platform/runtime"
	"github.com/ammarkarimi/usie/internal/store"
	api "github.com/ammarkarimi/usie/services/api-gateway"
	orchestrator "github.com/ammarkarimi/usie/services/execution-orchestrator"
	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
	analysis "github.com/ammarkarimi/usie/services/semantic-analysis"
)

func main() {
	cfg, err := config.Load()
	log := runtime.Logger(cfg.LogLevel, "usie-api")
	if err != nil {
		log.Error("invalid configuration", "error", err.Error())
		os.Exit(2)
	}
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

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

	st := store.New(pool)
	server := &api.Server{
		Cfg: cfg, Store: st, Queue: orchestrator.NewQueue(pool), Graph: graph.NewPostgres(pool), Blobs: blobs,
		Log: log, Version: runtime.Version, WebDir: os.Getenv("USIE_WEB_DIR"),
	}
	// The engine is optional here: it only feeds the system-information endpoint.
	infoCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	if info, err := (analysis.Engine{Bin: cfg.EngineBin}).Info(infoCtx); err == nil {
		server.Engine = &info
	} else {
		log.Warn("analysis engine not available to the API process", "error", err.Error())
	}
	cancel()

	go func() {
		t := time.NewTicker(15 * time.Minute)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-t.C:
				if err := st.DeleteExpiredSessions(ctx); err != nil && ctx.Err() == nil {
					log.Warn("session cleanup failed", "error", err.Error())
				}
			}
		}
	}()

	httpServer := &http.Server{
		Addr:              cfg.HTTPAddr,
		Handler:           server.Handler(),
		ReadHeaderTimeout: 10 * time.Second,
		// Uploads and event streams are long-lived; bodies are bounded by size instead.
		ReadTimeout:    0,
		WriteTimeout:   0,
		IdleTimeout:    120 * time.Second,
		MaxHeaderBytes: 64 << 10,
	}
	go func() {
		<-ctx.Done()
		shutdown, cancel := context.WithTimeout(context.Background(), 15*time.Second)
		defer cancel()
		httpServer.Shutdown(shutdown) //nolint:errcheck
	}()
	log.Info("listening", "addr", cfg.HTTPAddr, "signup_mode", cfg.SignupMode, "web_dir", server.WebDir)
	if err := httpServer.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
		log.Error("server failed", "error", err.Error())
		os.Exit(1)
	}
	log.Info("stopped")
}
