package verify

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// sandbox returns a bubblewrap sandbox, or skips when the host cannot run
// one (no bwrap, or no unprivileged user namespaces).
func sandbox(t *testing.T) *Bubblewrap {
	t.Helper()
	bwrap, err1 := exec.LookPath("bwrap")
	prlimit, err2 := exec.LookPath("prlimit")
	if err1 != nil || err2 != nil {
		t.Skip("bwrap or prlimit not installed")
	}
	l := DefaultLimits()
	l.Timeout = 20 * time.Second
	sb := &Bubblewrap{Bwrap: bwrap, Prlimit: prlimit, Limits: l}
	if err := sb.Check(context.Background()); err != nil {
		t.Skipf("sandbox unavailable here: %v", err)
	}
	return sb
}

func run(t *testing.T, sb *Bubblewrap, dir, script string) Result {
	t.Helper()
	res, err := sb.Run(context.Background(), Spec{WorkDir: dir, Argv: []string{"sh", "-c", script}})
	if err != nil {
		t.Fatal(err)
	}
	return res
}

func TestSandboxIsolation(t *testing.T) {
	sb := sandbox(t)
	dir := t.TempDir()
	os.WriteFile(filepath.Join(dir, "input.txt"), []byte("data"), 0o644)
	t.Setenv("USIE_SECRET_FOR_TEST", "s3cr3t")

	// The work tree is readable and writable.
	if res := run(t, sb, dir, "cat input.txt && echo out > output.txt"); res.ExitCode != 0 || res.Stdout != "data" {
		t.Fatalf("work tree: %+v", res)
	}
	if b, _ := os.ReadFile(filepath.Join(dir, "output.txt")); string(b) != "out\n" {
		t.Errorf("output not written: %q", b)
	}
	// The worker's environment does not leak in.
	if res := run(t, sb, dir, "env"); strings.Contains(res.Stdout, "s3cr3t") || strings.Contains(res.Stdout, "DATABASE") {
		t.Errorf("environment leaked: %s", res.Stdout)
	}
	// Host files outside /usr and the work tree are not visible.
	for _, p := range []string{"/root", "/home", "/etc/shadow", "/var"} {
		res := run(t, sb, dir, "ls "+p+" 2>/dev/null | head -1")
		if strings.TrimSpace(res.Stdout) != "" {
			t.Errorf("%s is visible: %q", p, res.Stdout)
		}
	}
	if res := run(t, sb, dir, "ls "+filepath.Dir(dir)); res.ExitCode == 0 && strings.Contains(res.Stdout, filepath.Base(dir)) {
		t.Errorf("the host directory around the work tree is visible: %s", res.Stdout)
	}
	// Nothing outside /work and /tmp is writable.
	if res := run(t, sb, dir, "echo x > /usr/evil 2>/dev/null || echo denied"); !strings.Contains(res.Stdout, "denied") {
		t.Errorf("/usr writable: %+v", res)
	}
	// Commands run as an unprivileged user without capabilities.
	if res := run(t, sb, dir, "id -u; grep CapEff /proc/self/status"); !strings.HasPrefix(res.Stdout, "65534") || !strings.Contains(res.Stdout, "0000000000000000") {
		t.Errorf("identity: %s", res.Stdout)
	}
	// No network: only a loopback interface without a route out.
	if _, ok := sb.LookPath("python3"); ok {
		res := run(t, sb, dir, `python3 -c "import socket; socket.create_connection(('1.1.1.1', 443), timeout=3)" 2>&1 | tail -1`)
		if !strings.Contains(res.Stdout, "Error") && !strings.Contains(res.Stdout, "unreachable") {
			t.Errorf("network reachable: %s", res.Stdout)
		}
	}
	if res := run(t, sb, dir, "cat /sys/class/net/*/address 2>/dev/null | wc -l"); strings.TrimSpace(res.Stdout) != "1" && strings.TrimSpace(res.Stdout) != "0" {
		t.Errorf("network interfaces: %s", res.Stdout)
	}
}

func TestSandboxLimits(t *testing.T) {
	sb := sandbox(t)
	sb.Limits.Timeout = 2 * time.Second
	sb.Limits.OutputBytes = 1000
	dir := t.TempDir()
	start := time.Now()
	res := run(t, sb, dir, "sleep 30")
	if !res.TimedOut || res.ExitCode == 0 || time.Since(start) > 10*time.Second {
		t.Errorf("timeout: %+v after %v", res, time.Since(start))
	}
	// Background children die with the sandbox.
	res = run(t, sb, dir, "(sleep 30 &) ; echo started")
	if res.ExitCode != 0 {
		t.Errorf("background: %+v", res)
	}
	res = run(t, sb, dir, "yes | head -c 100000")
	if !res.Truncated || len(res.Stdout) > 1200 {
		t.Errorf("output cap: truncated=%v len=%d", res.Truncated, len(res.Stdout))
	}
	res = run(t, sb, dir, "exit 3")
	if res.ExitCode != 3 {
		t.Errorf("exit code: %+v", res)
	}
}
