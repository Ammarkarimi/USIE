package ingestion

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"time"
)

// Commit is one entry of a repository's history.
type Commit struct {
	SHA         string
	Parents     []string
	AuthorName  string
	AuthorEmail string
	AuthoredAt  time.Time
	CommittedAt time.Time
	Subject     string
}

type Branch struct {
	Name    string
	HeadSHA string
	Default bool
}

// GitMetadata describes a checked-out revision.
type GitMetadata struct {
	HeadSHA  string
	Branch   string
	Commits  []Commit
	Branches []Branch
}

type GitOptions struct {
	AllowPrivateHosts bool
	// Depth bounds the fetched history.
	Depth int
	// Resolver is replaceable for tests.
	LookupIP func(ctx context.Context, host string) ([]net.IP, error)
}

var (
	ErrInvalidGitURL = errors.New("invalid git URL")
	refPattern       = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._/+-]{0,199}$`)
	shaPattern       = regexp.MustCompile(`^[0-9a-f]{40}([0-9a-f]{24})?$`)
)

// ValidateGitURL accepts only credential-free HTTPS URLs. Other transports
// (ssh, git, file, ext::) can reach local files or run helper programs.
func ValidateGitURL(raw string) (*url.URL, error) {
	if len(raw) > 2000 || strings.ContainsAny(raw, " \t\r\n\x00") {
		return nil, fmt.Errorf("%w: malformed", ErrInvalidGitURL)
	}
	u, err := url.Parse(raw)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrInvalidGitURL, err)
	}
	switch {
	case u.Scheme != "https":
		return nil, fmt.Errorf("%w: only https:// URLs are supported", ErrInvalidGitURL)
	case u.User != nil:
		return nil, fmt.Errorf("%w: credentials must not be embedded in the URL", ErrInvalidGitURL)
	case u.Hostname() == "":
		return nil, fmt.Errorf("%w: missing host", ErrInvalidGitURL)
	case u.RawQuery != "" || u.Fragment != "":
		return nil, fmt.Errorf("%w: query strings and fragments are not allowed", ErrInvalidGitURL)
	case strings.HasPrefix(u.Path, "/-") || strings.Contains(u.Path, ".."):
		return nil, fmt.Errorf("%w: suspicious path", ErrInvalidGitURL)
	}
	return u, nil
}

// ValidRef reports whether s is acceptable as a branch, tag or commit name.
// The leading-character rule prevents option injection (`--upload-pack=…`).
func ValidRef(s string) bool {
	return refPattern.MatchString(s) && !strings.Contains(s, "..") && !strings.HasSuffix(s, ".lock") && !strings.Contains(s, "//")
}

// checkHost rejects hosts that resolve to non-public addresses, so the
// service cannot be used to probe internal networks (SSRF). A hostile DNS
// server could still answer differently for git's own lookup; deployments
// that need a hard guarantee must also restrict worker egress.
func checkHost(ctx context.Context, host string, opt GitOptions) error {
	if opt.AllowPrivateHosts {
		return nil
	}
	lookup := opt.LookupIP
	if lookup == nil {
		lookup = func(ctx context.Context, host string) ([]net.IP, error) {
			return net.DefaultResolver.LookupIP(ctx, "ip", host)
		}
	}
	var ips []net.IP
	if ip := net.ParseIP(host); ip != nil {
		ips = []net.IP{ip}
	} else {
		var err error
		if ips, err = lookup(ctx, host); err != nil {
			return fmt.Errorf("resolve %s: %w", host, err)
		}
	}
	if len(ips) == 0 {
		return fmt.Errorf("resolve %s: no addresses", host)
	}
	for _, ip := range ips {
		if ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast() || ip.IsLinkLocalMulticast() || ip.IsUnspecified() || ip.IsMulticast() {
			return fmt.Errorf("%w: %s resolves to a non-public address", ErrInvalidGitURL, host)
		}
	}
	return nil
}

// gitEnv is the complete environment for git: nothing is inherited except
// what is needed to find the binary and reach the network.
func gitEnv(home string) []string {
	env := []string{
		"HOME=" + home,
		"GIT_CONFIG_NOSYSTEM=1",
		"GIT_CONFIG_GLOBAL=" + os.DevNull,
		"GIT_TERMINAL_PROMPT=0",
		"GIT_ASKPASS=",
		"SSH_ASKPASS=",
		"GIT_LFS_SKIP_SMUDGE=1",
		"GIT_ALLOW_PROTOCOL=https",
		"LC_ALL=C",
	}
	for _, name := range []string{"PATH", "HTTPS_PROXY", "https_proxy", "NO_PROXY", "no_proxy", "SSL_CERT_FILE", "SSL_CERT_DIR", "GIT_SSL_CAINFO", "TMPDIR"} {
		if v, ok := os.LookupEnv(name); ok {
			env = append(env, name+"="+v)
		}
	}
	return env
}

// Settings applied to every git invocation. Hooks, submodules, LFS filters,
// symlink creation and non-HTTPS transports are all disabled.
var gitHardening = []string{
	"-c", "core.hooksPath=" + os.DevNull,
	"-c", "core.symlinks=false",
	"-c", "core.fsmonitor=false",
	"-c", "protocol.allow=never",
	"-c", "protocol.https.allow=always",
	"-c", "http.followRedirects=false",
	"-c", "credential.helper=",
	"-c", "filter.lfs.smudge=",
	"-c", "filter.lfs.process=",
	"-c", "filter.lfs.required=false",
	"-c", "submodule.recurse=false",
	"-c", "fetch.recurseSubmodules=false",
	"-c", "advice.detachedHead=false",
	"-c", "log.showSignature=false",
	"-c", "gc.auto=0",
}

func runGit(ctx context.Context, dir, home string, args ...string) ([]byte, error) {
	cmd := exec.CommandContext(ctx, "git", append(append([]string{}, gitHardening...), args...)...)
	cmd.Dir = dir
	cmd.Env = gitEnv(home)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	cmd.WaitDelay = 5 * time.Second
	if err := cmd.Run(); err != nil {
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
		return nil, fmt.Errorf("git %s: %s", args[0], truncate(strings.TrimSpace(stderr.String()), 500))
	}
	return stdout.Bytes(), nil
}

// Clone fetches a repository over HTTPS into dest (which must not exist)
// and checks out ref (empty for the remote's default branch).
func Clone(ctx context.Context, rawURL, ref, dest string, opt GitOptions) (*GitMetadata, error) {
	u, err := ValidateGitURL(rawURL)
	if err != nil {
		return nil, err
	}
	if ref != "" && !ValidRef(ref) {
		return nil, fmt.Errorf("invalid ref %q", ref)
	}
	if err := checkHost(ctx, u.Hostname(), opt); err != nil {
		return nil, err
	}
	depth := opt.Depth
	if depth <= 0 {
		depth = 500
	}
	home, err := os.MkdirTemp(filepath.Dir(dest), "githome-")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(home)

	args := []string{"clone", "--quiet", "--no-tags", "--single-branch", "--no-recurse-submodules", fmt.Sprintf("--depth=%d", depth)}
	isSHA := shaPattern.MatchString(ref)
	if ref != "" && !isSHA {
		args = append(args, "--branch", ref)
	}
	args = append(args, "--", u.String(), dest)
	if _, err := runGit(ctx, filepath.Dir(dest), home, args...); err != nil {
		return nil, err
	}
	if isSHA {
		if _, err := runGit(ctx, dest, home, "fetch", "--quiet", "--depth", fmt.Sprint(depth), "origin", ref); err != nil {
			return nil, err
		}
		if _, err := runGit(ctx, dest, home, "checkout", "--quiet", "--detach", ref); err != nil {
			return nil, err
		}
	}
	meta, err := cloneMetadata(ctx, dest, home, depth)
	if err != nil {
		return nil, err
	}
	// Remote branch list; failure here is not fatal to the import.
	if out, err := runGit(ctx, dest, home, "ls-remote", "--heads", "origin"); err == nil {
		for _, line := range strings.Split(string(out), "\n") {
			fields := strings.Fields(line)
			if len(fields) == 2 && strings.HasPrefix(fields[1], "refs/heads/") && len(meta.Branches) < 2000 {
				name := strings.TrimPrefix(fields[1], "refs/heads/")
				meta.Branches = append(meta.Branches, Branch{Name: name, HeadSHA: fields[0]})
			}
		}
	}
	if out, err := runGit(ctx, dest, home, "symbolic-ref", "--short", "refs/remotes/origin/HEAD"); err == nil {
		def := strings.TrimPrefix(strings.TrimSpace(string(out)), "origin/")
		for i := range meta.Branches {
			meta.Branches[i].Default = meta.Branches[i].Name == def
		}
	}
	return meta, nil
}

// cloneMetadata reads history from a clone this process created. It must
// not be pointed at a pre-existing repository: a hostile `.git/config`
// there could make git run programs.
func cloneMetadata(ctx context.Context, dir, home string, limit int) (*GitMetadata, error) {
	meta := &GitMetadata{}
	out, err := runGit(ctx, dir, home, "rev-parse", "HEAD")
	if err != nil {
		return nil, err
	}
	meta.HeadSHA = strings.TrimSpace(string(out))
	if out, err := runGit(ctx, dir, home, "rev-parse", "--abbrev-ref", "HEAD"); err == nil {
		if b := strings.TrimSpace(string(out)); b != "HEAD" {
			meta.Branch = b
		}
	}
	const sep = "\x1f"
	out, err = runGit(ctx, dir, home, "log", fmt.Sprintf("--max-count=%d", limit), "--format=%H"+sep+"%P"+sep+"%an"+sep+"%ae"+sep+"%aI"+sep+"%cI"+sep+"%s%x1e")
	if err != nil {
		return nil, err
	}
	for _, record := range strings.Split(string(out), "\x1e") {
		fields := strings.Split(strings.TrimSpace(record), sep)
		if len(fields) != 7 || !shaPattern.MatchString(fields[0]) {
			continue
		}
		c := Commit{SHA: fields[0], AuthorName: truncate(fields[2], 200), AuthorEmail: truncate(fields[3], 320), Subject: truncate(fields[6], 500)}
		if fields[1] != "" {
			c.Parents = strings.Fields(fields[1])
		}
		c.AuthoredAt, _ = time.Parse(time.RFC3339, fields[4])
		c.CommittedAt, _ = time.Parse(time.RFC3339, fields[5])
		meta.Commits = append(meta.Commits, c)
	}
	return meta, nil
}

// ReadLocalHead extracts the checked-out commit and branch of a local
// repository by reading `.git/HEAD` directly. Git itself is deliberately not
// run inside directories this process did not create.
func ReadLocalHead(root string) (sha, branch string) {
	gitDir := filepath.Join(root, ".git")
	if info, err := os.Lstat(gitDir); err != nil || !info.IsDir() {
		return "", ""
	}
	readSmall := func(p string) string {
		info, err := os.Lstat(p)
		if err != nil || !info.Mode().IsRegular() || info.Size() > 4096 {
			return ""
		}
		data, _ := os.ReadFile(p)
		return strings.TrimSpace(string(data))
	}
	head := readSmall(filepath.Join(gitDir, "HEAD"))
	if shaPattern.MatchString(head) {
		return head, ""
	}
	ref, ok := strings.CutPrefix(head, "ref: ")
	if !ok || !strings.HasPrefix(ref, "refs/heads/") || !ValidRef(ref) {
		return "", ""
	}
	branch = strings.TrimPrefix(ref, "refs/heads/")
	if s := readSmall(filepath.Join(gitDir, filepath.FromSlash(ref))); shaPattern.MatchString(s) {
		return s, branch
	}
	// The ref may be packed.
	if info, err := os.Lstat(filepath.Join(gitDir, "packed-refs")); err == nil && info.Mode().IsRegular() && info.Size() < 16<<20 {
		data, _ := os.ReadFile(filepath.Join(gitDir, "packed-refs"))
		for _, line := range strings.Split(string(data), "\n") {
			fields := strings.Fields(line)
			if len(fields) == 2 && fields[1] == ref && shaPattern.MatchString(fields[0]) {
				return fields[0], branch
			}
		}
	}
	return "", branch
}
