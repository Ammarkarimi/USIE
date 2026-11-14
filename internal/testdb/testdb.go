// Package testdb gives each integration test its own freshly migrated
// PostgreSQL database, so tests in different packages can run in parallel.
package testdb

import (
	"context"
	"fmt"
	"net/url"
	"os"
	"sync/atomic"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/ammarkarimi/usie/database"
)

var counter atomic.Int64

// New returns a pool connected to a new, migrated database that is dropped
// when the test ends. The test is skipped when USIE_TEST_DATABASE_URL is not set.
func New(t testing.TB) *pgxpool.Pool {
	t.Helper()
	base := os.Getenv("USIE_TEST_DATABASE_URL")
	if base == "" {
		t.Skip("USIE_TEST_DATABASE_URL not set (see scripts/test-db.sh)")
	}
	ctx := context.Background()
	admin, err := pgx.Connect(ctx, base)
	if err != nil {
		t.Fatalf("connect to test database server: %v", err)
	}
	defer admin.Close(ctx)
	name := fmt.Sprintf("usie_test_%d_%d_%d", os.Getpid(), time.Now().UnixNano()%1_000_000, counter.Add(1))
	if _, err := admin.Exec(ctx, "CREATE DATABASE "+name); err != nil {
		t.Fatalf("create test database: %v", err)
	}
	u, err := url.Parse(base)
	if err != nil {
		t.Fatal(err)
	}
	u.Path = "/" + name
	pool, err := pgxpool.New(ctx, u.String())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		pool.Close()
		c, err := pgx.Connect(context.Background(), base)
		if err != nil {
			return
		}
		defer c.Close(context.Background())
		c.Exec(context.Background(), "DROP DATABASE IF EXISTS "+name+" WITH (FORCE)") //nolint:errcheck
	})
	if _, err := database.Migrate(ctx, pool); err != nil {
		t.Fatalf("migrate: %v", err)
	}
	return pool
}
