// Package verify builds and tests repository code, with or without an
// AI-proposed change, inside an unprivileged sandbox. Nothing here runs
// repository code in the worker process itself: every command runs in a
// separate bubblewrap sandbox with no network, a cleared environment, an
// empty home, the host's /usr read-only, the work tree as the only writable
// directory besides /tmp, and resource limits.
package verify

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// Limits bound one command in the sandbox.
type Limits struct {
	// Timeout is the wall-clock limit of one command.
	Timeout time.Duration
	// CPUSeconds is RLIMIT_CPU.
	CPUSeconds int
	// AddressSpaceBytes is RLIMIT_AS (virtual memory).
	AddressSpaceBytes int64
	// FileSizeBytes is RLIMIT_FSIZE (largest file a command may write).
	FileSizeBytes int64
	// Processes is RLIMIT_NPROC (counted per host user; enforced when the
	// worker does not run as root).
	Processes int
	// OutputBytes caps the stdout and stderr kept from one command.
	OutputBytes int
}

func DefaultLimits() Limits {
	return Limits{
		Timeout:           10 * time.Minute,
		CPUSeconds:        900,
		AddressSpaceBytes: 8 << 30,
		FileSizeBytes:     1 << 30,
		Processes:         512,
		OutputBytes:       256 << 10,
	}
}

// Spec is one command to run.
type Spec struct {
	// WorkDir is the host directory mounted read-write at /work.
	WorkDir string
	Argv    []string
	// Env is added to a minimal environment (PATH, HOME=/tmp, LANG).
	Env map[string]string
}

// Result of one command.
type Result struct {
	ExitCode  int           `json:"exit_code"`
	TimedOut  bool          `json:"timed_out"`
	Duration  time.Duration `json:"duration_ns"`
	Stdout    string        `json:"stdout"`
	Stderr    string        `json:"stderr"`
	Truncated bool          `json:"truncated"`
}

// Sandbox runs commands in isolation.
type Sandbox interface {
	Name() string
	// Check reports whether the sandbox can run on this host.
	Check(ctx context.Context) error
	Run(ctx context.Context, spec Spec) (Result, error)
	// LookPath finds a program as the sandbox would.
	LookPath(name string) (string, bool)
}

// Bubblewrap isolates commands with bwrap(1) and prlimit(1).
type Bubblewrap struct {
	Bwrap   string // path to bwrap
	Prlimit string // path to prlimit
	Limits  Limits
	// Network allows the sandbox to reach the network (package downloads).
	// Off by default: with it, repository code can exfiltrate what it reads.
	Network bool
	// ReadOnly are extra host paths mounted read-only at the same place
	// (toolchains outside /usr, such as /opt/go).
	ReadOnly []string
	// Path is the PATH inside the sandbox.
	Path []string
}

func (b *Bubblewrap) Name() string { return "bubblewrap" }

func (b *Bubblewrap) path() []string {
	if len(b.Path) > 0 {
		return b.Path
	}
	return []string{"/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin", "/sbin", "/bin"}
}

func (b *Bubblewrap) LookPath(name string) (string, bool) {
	for _, dir := range b.path() {
		p := filepath.Join(dir, name)
		if !b.visible(p) {
			continue
		}
		if fi, err := os.Stat(p); err == nil && !fi.IsDir() && fi.Mode()&0o111 != 0 {
			return p, true
		}
	}
	return "", false
}

// visible reports whether a host path is inside what the sandbox mounts.
func (b *Bubblewrap) visible(p string) bool {
	for _, root := range append([]string{"/usr", "/bin", "/sbin", "/lib", "/lib64"}, b.ReadOnly...) {
		if p == root || strings.HasPrefix(p, strings.TrimSuffix(root, "/")+"/") {
			return true
		}
	}
	return false
}

func (b *Bubblewrap) Check(ctx context.Context) error {
	if b.Bwrap == "" || b.Prlimit == "" {
		return errors.New("bwrap and prlimit are required")
	}
	dir, err := os.MkdirTemp("", "usie-sandbox-check-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(dir)
	res, err := b.Run(ctx, Spec{WorkDir: dir, Argv: []string{"true"}})
	if err != nil {
		return err
	}
	if res.ExitCode != 0 {
		return fmt.Errorf("sandbox self-test failed (exit %d): %s", res.ExitCode, strings.TrimSpace(res.Stderr))
	}
	return nil
}

func (b *Bubblewrap) args(spec Spec) []string {
	l := b.Limits
	args := []string{
		"--cpu=" + strconv.Itoa(l.CPUSeconds),
		"--as=" + strconv.FormatInt(l.AddressSpaceBytes, 10),
		"--fsize=" + strconv.FormatInt(l.FileSizeBytes, 10),
		"--nproc=" + strconv.Itoa(l.Processes),
		"--core=0",
		b.Bwrap,
		"--die-with-parent",
		"--new-session",
		"--cap-drop", "ALL",
		"--unshare-user", "--unshare-ipc", "--unshare-pid", "--unshare-uts", "--unshare-cgroup-try",
		"--uid", "65534", "--gid", "65534",
		"--hostname", "sandbox",
		"--ro-bind", "/usr", "/usr",
		"--symlink", "usr/bin", "/bin",
		"--symlink", "usr/sbin", "/sbin",
		"--symlink", "usr/lib", "/lib",
	}
	if _, err := os.Stat("/usr/lib64"); err == nil {
		args = append(args, "--symlink", "usr/lib64", "/lib64")
	}
	if !b.Network {
		args = append(args, "--unshare-net")
	} else {
		// Name resolution and certificates for package downloads.
		for _, p := range []string{"/etc/resolv.conf", "/etc/hosts", "/etc/ssl", "/etc/ca-certificates", "/etc/pki"} {
			if _, err := os.Stat(p); err == nil {
				args = append(args, "--ro-bind", p, p)
			}
		}
	}
	for _, p := range b.ReadOnly {
		args = append(args, "--ro-bind", p, p)
	}
	// Minimal /etc so tools that look up users and time zones work.
	for _, p := range []string{"/etc/passwd", "/etc/group", "/etc/nsswitch.conf", "/etc/localtime", "/etc/alternatives"} {
		if _, err := os.Stat(p); err == nil {
			args = append(args, "--ro-bind", p, p)
		}
	}
	args = append(args,
		"--proc", "/proc",
		"--dev", "/dev",
		"--tmpfs", "/tmp",
		"--bind", spec.WorkDir, "/work",
		"--chdir", "/work",
		"--clearenv",
	)
	env := map[string]string{
		"PATH":   strings.Join(b.path(), ":"),
		"HOME":   "/tmp/home",
		"LANG":   "C.UTF-8",
		"TMPDIR": "/tmp",
		"CI":     "true",
	}
	for k, v := range spec.Env {
		env[k] = v
	}
	keys := make([]string, 0, len(env))
	for k := range env {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		args = append(args, "--setenv", k, env[k])
	}
	args = append(args, "--")
	return append(args, spec.Argv...)
}

func (b *Bubblewrap) Run(ctx context.Context, spec Spec) (Result, error) {
	if len(spec.Argv) == 0 {
		return Result{}, errors.New("verify: empty command")
	}
	if fi, err := os.Stat(spec.WorkDir); err != nil || !fi.IsDir() {
		return Result{}, fmt.Errorf("verify: work directory %q is not a directory", spec.WorkDir)
	}
	l := b.Limits
	ctx, cancel := context.WithTimeout(ctx, l.Timeout)
	defer cancel()
	cmd := exec.Command(b.Prlimit, b.args(spec)...)
	cmd.Env = []string{"PATH=/usr/bin:/bin"}
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	stdout := &capped{max: l.OutputBytes}
	stderr := &capped{max: l.OutputBytes}
	cmd.Stdout, cmd.Stderr = stdout, stderr
	started := time.Now()
	if err := cmd.Start(); err != nil {
		return Result{}, fmt.Errorf("verify: starting the sandbox: %w", err)
	}
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	var waitErr error
	timedOut := false
	select {
	case waitErr = <-done:
	case <-ctx.Done():
		// Kill the whole process group; --die-with-parent takes the
		// sandboxed tree down with bwrap.
		_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
		waitErr = <-done
		timedOut = errors.Is(ctx.Err(), context.DeadlineExceeded)
	}
	res := Result{
		Duration:  time.Since(started),
		Stdout:    stdout.String(),
		Stderr:    stderr.String(),
		Truncated: stdout.dropped > 0 || stderr.dropped > 0,
		TimedOut:  timedOut,
	}
	var exit *exec.ExitError
	switch {
	case waitErr == nil:
	case errors.As(waitErr, &exit):
		res.ExitCode = exit.ExitCode()
		if res.ExitCode < 0 {
			res.ExitCode = 128 + int(exit.Sys().(syscall.WaitStatus).Signal())
		}
	default:
		return res, fmt.Errorf("verify: sandbox: %w", waitErr)
	}
	if timedOut && res.ExitCode == 0 {
		res.ExitCode = 124
	}
	return res, nil
}

// capped keeps the first and last part of a stream.
type capped struct {
	max     int
	head    bytes.Buffer
	tail    []byte
	dropped int64
}

func (c *capped) Write(p []byte) (int, error) {
	n := len(p)
	half := c.max / 2
	if c.head.Len() < half {
		take := min(half-c.head.Len(), len(p))
		c.head.Write(p[:take])
		p = p[take:]
	}
	if len(p) > 0 {
		c.tail = append(c.tail, p...)
		if over := len(c.tail) - half; over > 0 {
			c.dropped += int64(over)
			c.tail = c.tail[over:]
		}
	}
	return n, nil
}

func (c *capped) String() string {
	if c.dropped == 0 {
		return c.head.String() + string(c.tail)
	}
	return fmt.Sprintf("%s\n… %d bytes omitted …\n%s", c.head.String(), c.dropped, string(c.tail))
}
