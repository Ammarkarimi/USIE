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
	// AI reasoning. AIProvider is "openai", "anthropic" or "none". It
	// defaults to "openai" when OPENAI_API_KEY is set, otherwise to
	// "anthropic" when ANTHROPIC_API_KEY is set, otherwise to "none".
	AIProvider            string
	OpenAIAPIKey          string
	AnthropicAPIKey       string
	AIModel               string
	AIEffort              string
	AIDailyTokenBudget    int64
	AIUserRequestsPerHour int
	AIMaxConcurrent       int
	// AIChangesPerHour caps AI change proposals per user.
	AIChangesPerHour int

	// PublicURL is the address users open USIE at; links in emails use it.
	PublicURL string
	// SMTP settings for invitation and password-reset emails. Email is
	// enabled when SMTPAddr, SMTPFrom and PublicURL are set.
	SMTPAddr     string
	SMTPUsername string
	SMTPPassword string
	SMTPFrom     string

	// Sandbox is the verification sandbox: "none" (verification disabled)
	// or "bubblewrap". It runs repository builds and tests.
	Sandbox string
	// SandboxNetwork lets sandboxed builds reach the network (off: no
	// dependency downloads, and no way to send data out).
	SandboxNetwork bool
	SandboxTimeout time.Duration
	// SandboxMemoryBytes is the address-space limit of one command.
	SandboxMemoryBytes int64
	// SandboxReadOnlyPaths are extra host paths (toolchains outside /usr)
	// mounted read-only in the sandbox.
	SandboxReadOnlyPaths []string

	// TrustedProxyHops is the number of reverse proxies in front of the API
	// that append to X-Forwarded-For (counting the one that connects). 0
	// ignores forwarding headers and uses the peer address.
	TrustedProxyHops int
}

// EmailEnabled reports whether invitation and reset emails can be sent.
func (c Config) EmailEnabled() bool {
	return c.SMTPAddr != "" && c.SMTPFrom != "" && c.PublicURL != ""
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
	c.OpenAIAPIKey = os.Getenv("OPENAI_API_KEY")
	c.AnthropicAPIKey = os.Getenv("ANTHROPIC_API_KEY")
	c.AIProvider = os.Getenv("USIE_AI_PROVIDER")
	if c.AIProvider == "" {
		switch {
		case c.OpenAIAPIKey != "":
			c.AIProvider = "openai"
		case c.AnthropicAPIKey != "":
			c.AIProvider = "anthropic"
		default:
			c.AIProvider = "none"
		}
	}
	// Empty means the provider's default model.
	c.AIModel = os.Getenv("USIE_AI_MODEL")
	// "off" sends no effort parameter (for models without reasoning).
	c.AIEffort = env("USIE_AI_EFFORT", "medium")
	if c.AIEffort == "off" {
		c.AIEffort = ""
	}
	c.AIDailyTokenBudget = integer("USIE_AI_DAILY_TOKEN_BUDGET", 2_000_000)
	c.AIUserRequestsPerHour = int(integer("USIE_AI_USER_REQUESTS_PER_HOUR", 30))
	c.AIMaxConcurrent = int(integer("USIE_AI_MAX_CONCURRENT", 4))
	c.AIChangesPerHour = int(integer("USIE_AI_CHANGES_PER_HOUR", 10))
	c.PublicURL = strings.TrimRight(os.Getenv("USIE_PUBLIC_URL"), "/")
	if c.PublicURL != "" && !strings.HasPrefix(c.PublicURL, "https://") && !strings.HasPrefix(c.PublicURL, "http://") {
		fail("USIE_PUBLIC_URL", fmt.Errorf("must start with https:// or http://"))
	}
	c.SMTPAddr = os.Getenv("USIE_SMTP_ADDR")
	c.SMTPUsername = os.Getenv("USIE_SMTP_USERNAME")
	c.SMTPPassword = os.Getenv("USIE_SMTP_PASSWORD")
	c.SMTPFrom = os.Getenv("USIE_SMTP_FROM")
	c.Sandbox = env("USIE_SANDBOX", "none")
	c.SandboxNetwork = boolean("USIE_SANDBOX_NETWORK", false)
	c.SandboxTimeout = duration("USIE_SANDBOX_TIMEOUT", 10*time.Minute)
	c.SandboxMemoryBytes = integer("USIE_SANDBOX_MEMORY_BYTES", 8<<30)
	for _, p := range strings.Split(os.Getenv("USIE_SANDBOX_READONLY_PATHS"), ":") {
		if p = strings.TrimSpace(p); p != "" {
			if !filepath.IsAbs(p) {
				fail("USIE_SANDBOX_READONLY_PATHS", fmt.Errorf("%q is not an absolute path", p))
			}
			c.SandboxReadOnlyPaths = append(c.SandboxReadOnlyPaths, filepath.Clean(p))
		}
	}
	if v := os.Getenv("USIE_TRUSTED_PROXY_HOPS"); v != "" {
		n, e := strconv.Atoi(v)
		if e == nil && (n < 0 || n > 10) {
			e = fmt.Errorf("must be between 0 and 10")
		}
		fail("USIE_TRUSTED_PROXY_HOPS", e)
		c.TrustedProxyHops = n
	}
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
	switch c.Sandbox {
	case "none", "bubblewrap":
	default:
		return c, fmt.Errorf("USIE_SANDBOX must be none or bubblewrap")
	}
	switch c.AIProvider {
	case "none":
	case "openai":
		if c.OpenAIAPIKey == "" {
			return c, fmt.Errorf("USIE_AI_PROVIDER=openai requires OPENAI_API_KEY")
		}
	case "anthropic":
		if c.AnthropicAPIKey == "" {
			return c, fmt.Errorf("USIE_AI_PROVIDER=anthropic requires ANTHROPIC_API_KEY")
		}
	default:
		return c, fmt.Errorf("USIE_AI_PROVIDER must be openai, anthropic or none")
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
