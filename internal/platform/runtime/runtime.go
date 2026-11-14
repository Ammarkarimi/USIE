// Package runtime holds process bootstrap shared by the service binaries.
package runtime

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/ammarkarimi/usie/database"
	"github.com/ammarkarimi/usie/internal/platform/config"
)

// Version is the build version, set with -ldflags "-X …/runtime.Version=…".
var Version = "dev"

func Logger(level, service string) *slog.Logger {
	var lvl slog.Level
	if err := lvl.UnmarshalText([]byte(level)); err != nil {
		lvl = slog.LevelInfo
	}
	return slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{Level: lvl})).With("service", service, "version", Version)
}

// Database connects with bounded retries (the database may still be
// starting) and applies pending migrations.
func Database(ctx context.Context, cfg config.Config, log *slog.Logger) (*pgxpool.Pool, error) {
	pool, err := pgxpool.New(ctx, cfg.DatabaseURL)
	if err != nil {
		return nil, fmt.Errorf("database configuration: %w", err)
	}
	var pingErr error
	for attempt := 0; attempt < 30; attempt++ {
		pingCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
		pingErr = pool.Ping(pingCtx)
		cancel()
		if pingErr == nil {
			break
		}
		select {
		case <-ctx.Done():
			pool.Close()
			return nil, ctx.Err()
		case <-time.After(time.Second):
		}
	}
	if pingErr != nil {
		pool.Close()
		return nil, fmt.Errorf("database unreachable: %w", pingErr)
	}
	applied, err := database.Migrate(ctx, pool)
	if err != nil {
		pool.Close()
		return nil, fmt.Errorf("migrations: %w", err)
	}
	if len(applied) > 0 {
		log.Info("migrations applied", "versions", applied)
	}
	return pool, nil
}
