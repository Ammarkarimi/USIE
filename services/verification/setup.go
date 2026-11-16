package verify

import (
	"context"
	"errors"
	"fmt"
	"os/exec"
	"sort"
	"time"
)

// Config selects and limits the sandbox.
type Config struct {
	Backend       string // "none" or "bubblewrap"
	Network       bool
	Timeout       time.Duration
	MemoryBytes   int64
	ReadOnlyPaths []string
}

// New returns the configured sandbox after a self-test, or nil when
// verification is disabled.
func New(ctx context.Context, cfg Config) (Sandbox, error) {
	switch cfg.Backend {
	case "", "none":
		return nil, nil
	case "bubblewrap":
	default:
		return nil, fmt.Errorf("verify: unknown sandbox %q", cfg.Backend)
	}
	bwrap, err := exec.LookPath("bwrap")
	if err != nil {
		return nil, errors.New("verify: bwrap (bubblewrap) is not installed")
	}
	prlimit, err := exec.LookPath("prlimit")
	if err != nil {
		return nil, errors.New("verify: prlimit (util-linux) is not installed")
	}
	l := DefaultLimits()
	if cfg.Timeout > 0 {
		l.Timeout = cfg.Timeout
	}
	if cfg.MemoryBytes > 0 {
		l.AddressSpaceBytes = cfg.MemoryBytes
	}
	sb := &Bubblewrap{Bwrap: bwrap, Prlimit: prlimit, Limits: l, Network: cfg.Network, ReadOnly: cfg.ReadOnlyPaths}
	for _, p := range cfg.ReadOnlyPaths {
		sb.Path = append(sb.Path, p+"/bin")
	}
	if len(sb.Path) > 0 {
		sb.Path = append(sb.Path, (&Bubblewrap{}).path()...)
	}
	checkCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	if err := sb.Check(checkCtx); err != nil {
		return nil, fmt.Errorf("verify: the sandbox does not work on this host (unprivileged user namespaces may be disabled): %w", err)
	}
	return sb, nil
}

// Capability is what a worker reports about its sandbox, so the API can
// say which profiles can run without running anything itself.
type Capability struct {
	Sandbox  string   `json:"sandbox"`
	Network  bool     `json:"network"`
	Programs []string `json:"programs"`
}

// Describe reports the sandbox and the profile programs it can run.
func Describe(sb Sandbox) Capability {
	if sb == nil {
		return Capability{Sandbox: "none", Programs: []string{}}
	}
	c := Capability{Sandbox: sb.Name(), Programs: []string{}}
	if b, ok := sb.(*Bubblewrap); ok {
		c.Network = b.Network
	}
	seen := map[string]bool{}
	for _, p := range Profiles(Facts{}, sb) {
		for _, r := range p.Requires {
			if _, ok := sb.LookPath(r); ok && !seen[r] {
				seen[r] = true
				c.Programs = append(c.Programs, r)
			}
		}
	}
	sort.Strings(c.Programs)
	return c
}

// Remote answers profile availability from a worker's reported capability.
// It cannot run anything.
type Remote struct{ Cap Capability }

func (r Remote) Name() string { return r.Cap.Sandbox }
func (r Remote) Check(context.Context) error {
	return errors.New("verify: commands run on workers")
}
func (r Remote) Run(context.Context, Spec) (Result, error) {
	return Result{}, errors.New("verify: commands run on workers")
}
func (r Remote) LookPath(name string) (string, bool) {
	for _, p := range r.Cap.Programs {
		if p == name {
			return name, true
		}
	}
	return "", false
}
