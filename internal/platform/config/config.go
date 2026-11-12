// Package config loads service configuration from environment variables.
// See docs/operations/configuration.md for the reference.
package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

type Config struct {
	DatabaseURL string
	HTTPAddr    string
	// DataDir holds the blob store and worker scratch space.
	DataDir   string
	EngineBin string
	LogLevel  string

	// SignupMode: "first-user" (default) lets exactly one account bootstrap
	// the installation; "open" allows anyone to register; "closed" nobody.
	SignupMode    string
	CookieSecure  bool
	SessionTTL    time.Duration
	AllowedOrigin string

	// AllowedLocalRoots are the only directories 'local' repositories may
	// live under. Empty disables local import.
	AllowedLocalRoots []string
	// GitAllowPrivateHosts permits cloning from loopback/private addresses.
	GitAllowPrivateHosts bool
	GitTimeout           time.Duration

	MaxUploadBytes int64
	MaxRepoFiles   int
	MaxRepoBytes   int64
	// MaxFileBytes bounds what is stored and parsed per file.
	MaxFileBytes int64

	WorkerConcurrency int
	JobLease          time.Duration
	ParseTimeout      time.Duration
	EngineTimeout     time.Duration

	RateLimitPerMinute     int
	AuthRateLimitPerMinute int
}

func Load() (Config, error) {
	c := Config{
		DatabaseURL:   os.Getenv("USIE_DATABASE_URL"),
		HTTPAddr:      env("USIE_HTTP_ADDR", ":8080"),
		DataDir:       env("USIE_DATA_DIR", ".data"),
		EngineBin:     env("USIE_ENGINE_BIN", "usie-engine"),
		LogLevel:      env("USIE_LOG_LEVEL", "info"),
		SignupMode:    env("USIE_SIGNUP_MODE", "first-user"),
		AllowedOrigin: os.Getenv("USIE_ALLOWED_ORIGIN"),
	}
	var err error
	fail := func(name string, e error) {
		if e != nil && err == nil {
			err = fmt.Errorf("%s: %w", name, e)
		}
	}
	boolean := func(name string, def bool) bool {
		v, ok := os.LookupEnv(name)
		if !ok || v == "" {
			return def
		}
		b, e := strconv.ParseBool(v)
		fail(name, e)
		return b
	}
	integer := func(name string, def int64) int64 {
		v, ok := os.LookupEnv(name)
		if !ok || v == "" {
			return def
		}
		n, e := strconv.ParseInt(v, 10, 64)
		if e == nil && n <= 0 {
			e = fmt.Errorf("must be positive")
		}
		fail(name, e)
		return n
	}
	duration := func(name string, def time.Duration) time.Duration {
		v, ok := os.LookupEnv(name)
		if !ok || v == "" {
			return def
		}
		d, e := time.ParseDuration(v)
		if e == nil && d <= 0 {
			e = fmt.Errorf("must be positive")
		}
		fail(name, e)
		return d
	}

	c.CookieSecure = boolean("USIE_COOKIE_SECURE", true)
	c.SessionTTL = duration("USIE_SESSION_TTL", 12*time.Hour)
	c.GitAllowPrivateHosts = boolean("USIE_GIT_ALLOW_PRIVATE_HOSTS", false)
	c.GitTimeout = duration("USIE_GIT_TIMEOUT", 10*time.Minute)
	c.MaxUploadBytes = integer("USIE_MAX_UPLOAD_BYTES", 200<<20)
	c.MaxRepoFiles = int(integer("USIE_MAX_REPO_FILES", 200_000))
	c.MaxRepoBytes = integer("USIE_MAX_REPO_BYTES", 2<<30)
	c.MaxFileBytes = integer("USIE_MAX_FILE_BYTES", 2<<20)
	c.WorkerConcurrency = int(integer("USIE_WORKER_CONCURRENCY", 1))
	c.JobLease = duration("USIE_JOB_LEASE", 2*time.Minute)
	c.ParseTimeout = duration("USIE_PARSE_TIMEOUT", 10*time.Second)
	c.EngineTimeout = duration("USIE_ENGINE_TIMEOUT", 30*time.Minute)
	c.RateLimitPerMinute = int(integer("USIE_RATE_LIMIT_PER_MINUTE", 600))
	c.AuthRateLimitPerMinute = int(integer("USIE_AUTH_RATE_LIMIT_PER_MINUTE", 10))
	if err != nil {
		return c, err
	}

	if c.DatabaseURL == "" {
		return c, fmt.Errorf("USIE_DATABASE_URL is required")
	}
	switch c.SignupMode {
	case "first-user", "open", "closed":
	default:
		return c, fmt.Errorf("USIE_SIGNUP_MODE must be first-user, open or closed")
	}
	if c.DataDir, err = filepath.Abs(c.DataDir); err != nil {
		return c, err
	}
	for _, root := range strings.Split(os.Getenv("USIE_ALLOWED_LOCAL_ROOTS"), string(os.PathListSeparator)) {
		if root = strings.TrimSpace(root); root == "" {
			continue
		}
		abs, e := filepath.Abs(root)
		if e != nil {
			return c, fmt.Errorf("USIE_ALLOWED_LOCAL_ROOTS: %w", e)
		}
		// Resolve symlinks once so later containment checks compare real paths.
		if real, e := filepath.EvalSymlinks(abs); e == nil {
			abs = real
		}
		c.AllowedLocalRoots = append(c.AllowedLocalRoots, abs)
	}
	return c, nil
}

func (c Config) BlobDir() string { return filepath.Join(c.DataDir, "blobs") }
func (c Config) WorkDir() string { return filepath.Join(c.DataDir, "work") }

func env(name, def string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return def
}
