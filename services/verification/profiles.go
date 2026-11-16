package verify

import (
	"encoding/json"
	"fmt"
	"path"
	"regexp"
	"sort"
	"strconv"
	"strings"
)

// Step is one command of a profile. Commands are fixed argv lists: users
// choose a profile, never a command line.
type Step struct {
	Name string            `json:"name"`
	Argv []string          `json:"argv"`
	Env  map[string]string `json:"-"`
	// Tests marks the step whose output is summarized as test results.
	Tests bool `json:"tests"`
}

// Profile is a build-and-test recipe for one ecosystem.
type Profile struct {
	ID          string `json:"id"`
	Label       string `json:"label"`
	Description string `json:"description"`
	// Requires lists the programs that must be available in the sandbox.
	Requires []string `json:"requires"`
	Steps    []Step   `json:"steps"`
	// Detected reports whether the repository looks like this ecosystem.
	Detected bool `json:"detected"`
	// Available is false when a required program is missing.
	Available bool   `json:"available"`
	Reason    string `json:"reason,omitempty"`
	parse     func(string) *TestSummary
}

// TestSummary is what a test runner reported, when its output could be
// read. Counts are as reported by the tool.
type TestSummary struct {
	Passed  int      `json:"passed"`
	Failed  int      `json:"failed"`
	Skipped int      `json:"skipped"`
	Failing []string `json:"failing,omitempty"`
}

// Facts the repository offers for choosing profiles.
type Facts struct {
	Paths        []string
	BuildSystems []string
	// PackageJSONTest reports whether package.json declares a test script.
	PackageJSONTest bool
}

func (f Facts) has(name string) bool {
	for _, p := range f.Paths {
		if p == name || strings.HasSuffix(p, "/"+name) {
			return true
		}
	}
	return false
}

func (f Facts) hasExt(exts ...string) bool {
	for _, p := range f.Paths {
		for _, e := range exts {
			if strings.EqualFold(path.Ext(p), e) {
				return true
			}
		}
	}
	return false
}

func (f Facts) build(name string) bool {
	for _, b := range f.BuildSystems {
		if b == name {
			return true
		}
	}
	return false
}

// offline settings: toolchains must not try to reach the network, so
// failures are immediate and explicit.
var (
	goEnv     = map[string]string{"GOCACHE": "/tmp/go-build", "GOPATH": "/tmp/go", "GOTOOLCHAIN": "local", "GOFLAGS": "-mod=mod -buildvcs=false", "GOPROXY": "off", "GONOSUMDB": "*", "GOSUMDB": "off", "CGO_ENABLED": "0"}
	cargoEnv  = map[string]string{"CARGO_HOME": "/tmp/cargo", "CARGO_TARGET_DIR": "/tmp/target", "CARGO_NET_OFFLINE": "true"}
	pyEnv     = map[string]string{"PYTHONDONTWRITEBYTECODE": "1", "PIP_NO_INDEX": "1"}
	nodeEnv   = map[string]string{"npm_config_offline": "true", "npm_config_cache": "/tmp/npm", "NODE_ENV": "test"}
	jvmEnv    = map[string]string{"GRADLE_USER_HOME": "/tmp/gradle", "MAVEN_OPTS": "-Dmaven.repo.local=/tmp/m2"}
	dotnetEnv = map[string]string{"DOTNET_CLI_HOME": "/tmp/dotnet", "DOTNET_NOLOGO": "1", "DOTNET_CLI_TELEMETRY_OPTOUT": "1", "NUGET_PACKAGES": "/tmp/nuget"}
)

// Profiles lists every profile with its detection for the repository and
// its availability in the sandbox.
func Profiles(f Facts, sb Sandbox) []Profile {
	all := []Profile{
		{
			ID: "go", Label: "Go", Description: "go build ./... then go test ./... (offline: only the standard library and vendored modules are available)",
			Requires: []string{"go"}, Detected: f.has("go.mod"),
			Steps: []Step{
				{Name: "build", Argv: []string{"go", "build", "./..."}, Env: goEnv},
				{Name: "test", Argv: []string{"go", "test", "-count=1", "./..."}, Env: goEnv, Tests: true},
			},
			parse: parseGoTest,
		},
		{
			ID: "cargo", Label: "Rust (Cargo)", Description: "cargo test --offline (needs dependencies already vendored)",
			Requires: []string{"cargo"}, Detected: f.has("Cargo.toml"),
			Steps: []Step{{Name: "test", Argv: []string{"cargo", "test", "--offline", "--quiet"}, Env: cargoEnv, Tests: true}},
			parse: parseCargoTest,
		},
		{
			ID: "pytest", Label: "Python (pytest)", Description: "compile every module, then python3 -m pytest (pytest must be installed in the sandbox environment)",
			Requires: []string{"python3"}, Detected: f.build("python") || f.hasExt(".py"),
			Steps: []Step{
				{Name: "compile", Argv: []string{"python3", "-m", "compileall", "-q", "."}, Env: pyEnv},
				{Name: "test", Argv: []string{"python3", "-m", "pytest", "-q", "-p", "no:cacheprovider"}, Env: pyEnv, Tests: true},
			},
			parse: parsePytest,
		},
		{
			ID: "unittest", Label: "Python (unittest)", Description: "compile every module, then python3 -m unittest discover",
			Requires: []string{"python3"}, Detected: f.build("python") || f.hasExt(".py"),
			Steps: []Step{
				{Name: "compile", Argv: []string{"python3", "-m", "compileall", "-q", "."}, Env: pyEnv},
				{Name: "test", Argv: []string{"python3", "-m", "unittest", "discover", "-v"}, Env: pyEnv, Tests: true},
			},
			parse: parseUnittest,
		},
		{
			ID: "npm", Label: "Node.js (npm test)", Description: "npm test --offline (node_modules must be part of the repository)",
			Requires: []string{"npm"}, Detected: f.PackageJSONTest,
			Steps: []Step{{Name: "test", Argv: []string{"npm", "test", "--offline", "--no-audit", "--no-fund"}, Env: nodeEnv, Tests: true}},
		},
		{
			ID: "maven", Label: "Java (Maven)", Description: "mvn -o -q test (offline)",
			Requires: []string{"mvn"}, Detected: f.has("pom.xml"),
			Steps: []Step{{Name: "test", Argv: []string{"mvn", "-o", "-q", "-B", "test"}, Env: jvmEnv, Tests: true}},
			parse: parseSurefire,
		},
		{
			ID: "gradle", Label: "JVM (Gradle)", Description: "gradle test --offline (the installed gradle, not the wrapper, which downloads)",
			Requires: []string{"gradle"}, Detected: f.has("build.gradle") || f.has("build.gradle.kts"),
			Steps: []Step{{Name: "test", Argv: []string{"gradle", "test", "--offline", "--no-daemon", "-q"}, Env: jvmEnv, Tests: true}},
		},
		{
			ID: "dotnet", Label: ".NET", Description: "dotnet test (packages must be restorable offline)",
			Requires: []string{"dotnet"}, Detected: f.hasExt(".csproj", ".sln"),
			Steps: []Step{{Name: "test", Argv: []string{"dotnet", "test", "--nologo"}, Env: dotnetEnv, Tests: true}},
			parse: parseDotnet,
		},
		{
			ID: "make", Label: "Make (make test)", Description: "make test",
			Requires: []string{"make"}, Detected: f.has("Makefile"),
			Steps: []Step{{Name: "test", Argv: []string{"make", "test"}, Tests: true}},
		},
	}
	for i := range all {
		p := &all[i]
		p.Available = sb != nil
		if sb == nil {
			p.Reason = "no sandbox is configured"
			continue
		}
		var missing []string
		for _, r := range p.Requires {
			if _, ok := sb.LookPath(r); !ok {
				missing = append(missing, r)
			}
		}
		if len(missing) > 0 {
			p.Available = false
			p.Reason = fmt.Sprintf("%s is not installed in the sandbox environment", strings.Join(missing, ", "))
		}
	}
	sort.SliceStable(all, func(i, j int) bool {
		a, b := all[i], all[j]
		if a.Detected != b.Detected {
			return a.Detected
		}
		return a.Available && !b.Available
	})
	return all
}

// Find returns a profile by id.
func Find(f Facts, sb Sandbox, id string) (Profile, bool) {
	for _, p := range Profiles(f, sb) {
		if p.ID == id {
			return p, true
		}
	}
	return Profile{}, false
}

var (
	goPkgLine   = regexp.MustCompile(`(?m)^(ok|FAIL|---)\s`)
	goFailTest  = regexp.MustCompile(`(?m)^\s*--- FAIL: (\S+)`)
	goPassTest  = regexp.MustCompile(`(?m)^\s*--- PASS: `)
	goSkipTest  = regexp.MustCompile(`(?m)^\s*--- SKIP: `)
	goOKPkg     = regexp.MustCompile(`(?m)^ok\s+\S+`)
	goFailPkg   = regexp.MustCompile(`(?m)^FAIL\s+\S+`)
	cargoResult = regexp.MustCompile(`test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored`)
	cargoFail   = regexp.MustCompile(`(?m)^test (\S+) \.\.\. FAILED`)
	pytestCount = regexp.MustCompile(`(\d+) (passed|failed|error|errors|skipped)`)
	pytestFail  = regexp.MustCompile(`(?m)^FAILED (\S+)`)
	unittestRan = regexp.MustCompile(`(?m)^Ran (\d+) tests?`)
	unittestBad = regexp.MustCompile(`FAILED \((?:failures=(\d+))?(?:, )?(?:errors=(\d+))?(?:, )?(?:skipped=(\d+))?\)`)
	unittestFl  = regexp.MustCompile(`(?m)^(?:FAIL|ERROR): (\S+)`)
	surefire    = regexp.MustCompile(`Tests run: (\d+), Failures: (\d+), Errors: (\d+), Skipped: (\d+)`)
	dotnetLine  = regexp.MustCompile(`Failed:\s+(\d+), Passed:\s+(\d+), Skipped:\s+(\d+)`)
)

// parseGoTest counts test results; with -v absent, packages are counted
// when individual tests are not printed.
func parseGoTest(out string) *TestSummary {
	if !goPkgLine.MatchString(out) {
		return nil
	}
	s := &TestSummary{Passed: len(goPassTest.FindAllString(out, -1)), Skipped: len(goSkipTest.FindAllString(out, -1))}
	for _, m := range goFailTest.FindAllStringSubmatch(out, -1) {
		s.Failing = appendLimited(s.Failing, m[1])
	}
	s.Failed = len(goFailTest.FindAllString(out, -1))
	if s.Passed == 0 && s.Failed == 0 {
		s.Passed = len(goOKPkg.FindAllString(out, -1))
		s.Failed = len(goFailPkg.FindAllString(out, -1))
	}
	return s
}

func parseCargoTest(out string) *TestSummary {
	ms := cargoResult.FindAllStringSubmatch(out, -1)
	if ms == nil {
		return nil
	}
	s := &TestSummary{}
	for _, m := range ms {
		s.Passed += atoi(m[1])
		s.Failed += atoi(m[2])
		s.Skipped += atoi(m[3])
	}
	for _, m := range cargoFail.FindAllStringSubmatch(out, -1) {
		s.Failing = appendLimited(s.Failing, m[1])
	}
	return s
}

func parsePytest(out string) *TestSummary {
	lines := strings.Split(strings.TrimSpace(out), "\n")
	last := ""
	for i := len(lines) - 1; i >= 0; i-- {
		if pytestCount.MatchString(lines[i]) {
			last = lines[i]
			break
		}
	}
	if last == "" {
		return nil
	}
	s := &TestSummary{}
	for _, m := range pytestCount.FindAllStringSubmatch(last, -1) {
		switch m[2] {
		case "passed":
			s.Passed = atoi(m[1])
		case "failed", "error", "errors":
			s.Failed += atoi(m[1])
		case "skipped":
			s.Skipped = atoi(m[1])
		}
	}
	for _, m := range pytestFail.FindAllStringSubmatch(out, -1) {
		s.Failing = appendLimited(s.Failing, m[1])
	}
	return s
}

func parseUnittest(out string) *TestSummary {
	m := unittestRan.FindStringSubmatch(out)
	if m == nil {
		return nil
	}
	s := &TestSummary{}
	total := atoi(m[1])
	if b := unittestBad.FindStringSubmatch(out); b != nil {
		s.Failed = atoi(b[1]) + atoi(b[2])
		s.Skipped = atoi(b[3])
	}
	s.Passed = total - s.Failed - s.Skipped
	for _, f := range unittestFl.FindAllStringSubmatch(out, -1) {
		s.Failing = appendLimited(s.Failing, f[1])
	}
	return s
}

func parseSurefire(out string) *TestSummary {
	ms := surefire.FindAllStringSubmatch(out, -1)
	if ms == nil {
		return nil
	}
	// The last line is the module total.
	m := ms[len(ms)-1]
	run, fail, errs, skip := atoi(m[1]), atoi(m[2]), atoi(m[3]), atoi(m[4])
	return &TestSummary{Passed: run - fail - errs - skip, Failed: fail + errs, Skipped: skip}
}

func parseDotnet(out string) *TestSummary {
	ms := dotnetLine.FindAllStringSubmatch(out, -1)
	if ms == nil {
		return nil
	}
	s := &TestSummary{}
	for _, m := range ms {
		s.Failed += atoi(m[1])
		s.Passed += atoi(m[2])
		s.Skipped += atoi(m[3])
	}
	return s
}

func atoi(s string) int {
	n, _ := strconv.Atoi(s)
	return n
}

func appendLimited(list []string, v string) []string {
	if len(list) >= 50 {
		return list
	}
	return append(list, v)
}

// PackageJSONHasTest reports whether package.json declares scripts.test
// (other than npm's placeholder).
func PackageJSONHasTest(content []byte) bool {
	var doc struct {
		Scripts map[string]string `json:"scripts"`
	}
	if json.Unmarshal(content, &doc) != nil {
		return false
	}
	t := doc.Scripts["test"]
	return t != "" && !strings.Contains(t, "no test specified")
}
