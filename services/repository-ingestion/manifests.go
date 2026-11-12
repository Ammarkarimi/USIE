package ingestion

import (
	"encoding/json"
	"encoding/xml"
	"path"
	"regexp"
	"sort"
	"strings"

	"github.com/BurntSushi/toml"
)

// Dependency is a dependency declared in a manifest.
type Dependency struct {
	ManifestPath string `json:"manifest_path"`
	Ecosystem    string `json:"ecosystem"` // npm | pypi | go | cargo | maven
	Name         string `json:"name"`
	Requirement  string `json:"requirement"`
	Scope        string `json:"scope"` // runtime | dev | optional | peer | build
}

// TsConfig mirrors the resolver's view of a tsconfig/jsconfig file.
type TsConfig struct {
	Dir     string              `json:"dir"`
	BaseURL *string             `json:"base_url,omitempty"`
	Paths   map[string][]string `json:"paths,omitempty"`
}

// WorkspacePackage is an npm package defined inside the repository.
type WorkspacePackage struct {
	Name    string   `json:"name"`
	Dir     string   `json:"dir"`
	Entries []string `json:"entries,omitempty"`
}

// ManifestFacts is everything extracted from manifests in one repository.
type ManifestFacts struct {
	Dependencies []Dependency
	TsConfigs    []TsConfig
	Packages     []WorkspacePackage
	BuildSystems []string
	// Problems lists manifests that could not be parsed. They are reported,
	// not ignored.
	Problems []ManifestProblem
}

type ManifestProblem struct {
	Path    string `json:"path"`
	Message string `json:"message"`
}

// IsManifest reports whether a file should be handed to ManifestCollector.
func IsManifest(p string) bool {
	base := strings.ToLower(path.Base(p))
	switch base {
	case "package.json", "pyproject.toml", "go.mod", "cargo.toml", "pom.xml", "tsconfig.json", "jsconfig.json",
		"pipfile", "setup.py", "setup.cfg", "build.gradle", "build.gradle.kts", "cmakelists.txt", "makefile",
		"dockerfile", "meson.build", "build.bazel", "workspace":
		return true
	}
	if strings.HasPrefix(base, "requirements") && strings.HasSuffix(base, ".txt") {
		return true
	}
	return strings.HasSuffix(base, ".csproj") || strings.HasSuffix(base, ".sln")
}

// ManifestCollector accumulates facts from manifest files.
type ManifestCollector struct {
	facts  ManifestFacts
	builds map[string]bool
	deps   map[Dependency]bool
}

func NewManifestCollector() *ManifestCollector {
	return &ManifestCollector{builds: map[string]bool{}, deps: map[Dependency]bool{}}
}

func (c *ManifestCollector) addDep(d Dependency) {
	d.Name = strings.TrimSpace(d.Name)
	if d.Name == "" || len(d.Name) > 214 || len(d.Requirement) > 300 {
		return
	}
	key := d
	key.Requirement = ""
	if c.deps[key] {
		return
	}
	c.deps[key] = true
	c.facts.Dependencies = append(c.facts.Dependencies, d)
}

func (c *ManifestCollector) problem(p string, err error) {
	c.facts.Problems = append(c.facts.Problems, ManifestProblem{Path: p, Message: truncate(err.Error(), 300)})
}

// Add inspects one manifest. Vendored manifests are skipped by the caller.
func (c *ManifestCollector) Add(p string, content []byte) {
	base := strings.ToLower(path.Base(p))
	dir := path.Dir(p)
	if dir == "." {
		dir = ""
	}
	switch {
	case base == "package.json":
		c.builds["npm"] = true
		c.packageJSON(p, dir, content)
	case base == "tsconfig.json" || base == "jsconfig.json":
		c.tsconfig(p, dir, content)
	case base == "pyproject.toml":
		c.builds["python"] = true
		c.pyproject(p, content)
	case strings.HasPrefix(base, "requirements") && strings.HasSuffix(base, ".txt"):
		c.builds["python"] = true
		scope := "runtime"
		if strings.Contains(base, "dev") || strings.Contains(base, "test") {
			scope = "dev"
		}
		c.requirements(p, content, scope)
	case base == "setup.py" || base == "setup.cfg" || base == "pipfile":
		// Recorded as a build system only: setup.py is code and is not executed.
		c.builds["python"] = true
	case base == "go.mod":
		c.builds["go"] = true
		c.goMod(p, content)
	case base == "cargo.toml":
		c.builds["cargo"] = true
		c.cargo(p, content)
	case base == "pom.xml":
		c.builds["maven"] = true
		c.pom(p, content)
	case base == "build.gradle" || base == "build.gradle.kts":
		c.builds["gradle"] = true
	case base == "cmakelists.txt":
		c.builds["cmake"] = true
	case base == "makefile":
		c.builds["make"] = true
	case base == "dockerfile":
		c.builds["docker"] = true
	case base == "meson.build":
		c.builds["meson"] = true
	case base == "build.bazel" || base == "workspace":
		c.builds["bazel"] = true
	case strings.HasSuffix(base, ".csproj") || strings.HasSuffix(base, ".sln"):
		c.builds["dotnet"] = true
	}
}

func (c *ManifestCollector) Facts() ManifestFacts {
	f := c.facts
	for b := range c.builds {
		f.BuildSystems = append(f.BuildSystems, b)
	}
	sort.Strings(f.BuildSystems)
	sort.Slice(f.Dependencies, func(i, j int) bool {
		a, b := f.Dependencies[i], f.Dependencies[j]
		if a.ManifestPath != b.ManifestPath {
			return a.ManifestPath < b.ManifestPath
		}
		if a.Ecosystem != b.Ecosystem {
			return a.Ecosystem < b.Ecosystem
		}
		if a.Name != b.Name {
			return a.Name < b.Name
		}
		return a.Scope < b.Scope
	})
	sort.Slice(f.TsConfigs, func(i, j int) bool { return f.TsConfigs[i].Dir < f.TsConfigs[j].Dir })
	sort.Slice(f.Packages, func(i, j int) bool { return f.Packages[i].Dir < f.Packages[j].Dir })
	return f
}

func (c *ManifestCollector) packageJSON(p, dir string, content []byte) {
	var pkg struct {
		Name                 string            `json:"name"`
		Main                 string            `json:"main"`
		Module               string            `json:"module"`
		Types                string            `json:"types"`
		Source               string            `json:"source"`
		Exports              json.RawMessage   `json:"exports"`
		Dependencies         map[string]string `json:"dependencies"`
		DevDependencies      map[string]string `json:"devDependencies"`
		PeerDependencies     map[string]string `json:"peerDependencies"`
		OptionalDependencies map[string]string `json:"optionalDependencies"`
	}
	if err := json.Unmarshal(content, &pkg); err != nil {
		c.problem(p, err)
		return
	}
	for scope, deps := range map[string]map[string]string{
		"runtime": pkg.Dependencies, "dev": pkg.DevDependencies, "peer": pkg.PeerDependencies, "optional": pkg.OptionalDependencies,
	} {
		for name, req := range deps {
			c.addDep(Dependency{ManifestPath: p, Ecosystem: "npm", Name: name, Requirement: req, Scope: scope})
		}
	}
	if pkg.Name != "" {
		var entries []string
		for _, e := range append([]string{pkg.Source, pkg.Types, pkg.Module, pkg.Main}, exportEntries(pkg.Exports)...) {
			e = strings.TrimPrefix(e, "./")
			if e != "" && !strings.Contains(e, "..") && !strings.HasPrefix(e, "/") && !contains(entries, e) {
				entries = append(entries, e)
			}
		}
		c.facts.Packages = append(c.facts.Packages, WorkspacePackage{Name: pkg.Name, Dir: dir, Entries: entries})
	}
}

// exportEntries extracts candidate files for the root export (".").
func exportEntries(raw json.RawMessage) []string {
	if len(raw) == 0 {
		return nil
	}
	var out []string
	var collect func(v any, depth int)
	collect = func(v any, depth int) {
		if depth > 4 {
			return
		}
		switch t := v.(type) {
		case string:
			out = append(out, t)
		case map[string]any:
			// Either a subpath map (keys start with ".") or a condition map.
			if root, ok := t["."]; ok {
				collect(root, depth+1)
				return
			}
			for _, key := range []string{"source", "types", "import", "default", "require"} {
				if inner, ok := t[key]; ok {
					collect(inner, depth+1)
				}
			}
		}
	}
	var v any
	if json.Unmarshal(raw, &v) == nil {
		collect(v, 0)
	}
	return out
}

func (c *ManifestCollector) tsconfig(p, dir string, content []byte) {
	var cfg struct {
		CompilerOptions struct {
			BaseURL *string             `json:"baseUrl"`
			Paths   map[string][]string `json:"paths"`
		} `json:"compilerOptions"`
	}
	if err := json.Unmarshal(stripJSONC(content), &cfg); err != nil {
		c.problem(p, err)
		return
	}
	if cfg.CompilerOptions.BaseURL == nil && len(cfg.CompilerOptions.Paths) == 0 {
		return
	}
	// tsconfig.json takes precedence over jsconfig.json in the same directory.
	for i, existing := range c.facts.TsConfigs {
		if existing.Dir == dir {
			if strings.HasSuffix(strings.ToLower(p), "tsconfig.json") {
				c.facts.TsConfigs[i] = TsConfig{Dir: dir, BaseURL: cfg.CompilerOptions.BaseURL, Paths: cfg.CompilerOptions.Paths}
			}
			return
		}
	}
	c.facts.TsConfigs = append(c.facts.TsConfigs, TsConfig{Dir: dir, BaseURL: cfg.CompilerOptions.BaseURL, Paths: cfg.CompilerOptions.Paths})
}

// stripJSONC removes comments and trailing commas, which tsconfig files allow.
func stripJSONC(in []byte) []byte {
	out := make([]byte, 0, len(in))
	inString, escaped := false, false
	for i := 0; i < len(in); i++ {
		ch := in[i]
		if inString {
			out = append(out, ch)
			switch {
			case escaped:
				escaped = false
			case ch == '\\':
				escaped = true
			case ch == '"':
				inString = false
			}
			continue
		}
		switch {
		case ch == '"':
			inString = true
			out = append(out, ch)
		case ch == '/' && i+1 < len(in) && in[i+1] == '/':
			for i < len(in) && in[i] != '\n' {
				i++
			}
			out = append(out, '\n')
		case ch == '/' && i+1 < len(in) && in[i+1] == '*':
			i += 2
			for i+1 < len(in) && !(in[i] == '*' && in[i+1] == '/') {
				i++
			}
			i++
		case ch == ',':
			// Drop the comma when only whitespace/comments precede a closer.
			j := i + 1
			for j < len(in) {
				if in[j] == ' ' || in[j] == '\t' || in[j] == '\n' || in[j] == '\r' {
					j++
				} else if in[j] == '/' && j+1 < len(in) && in[j+1] == '/' {
					for j < len(in) && in[j] != '\n' {
						j++
					}
				} else if in[j] == '/' && j+1 < len(in) && in[j+1] == '*' {
					j += 2
					for j+1 < len(in) && !(in[j] == '*' && in[j+1] == '/') {
						j++
					}
					j += 2
				} else {
					break
				}
			}
			if j < len(in) && (in[j] == '}' || in[j] == ']') {
				continue
			}
			out = append(out, ch)
		default:
			out = append(out, ch)
		}
	}
	return out
}

var pep508Name = regexp.MustCompile(`^\s*([A-Za-z0-9][A-Za-z0-9._-]*)\s*(\[[^\]]*\])?\s*(.*)$`)

func splitRequirement(spec string) (name, req string, ok bool) {
	if i := strings.Index(spec, ";"); i >= 0 {
		spec = spec[:i]
	}
	m := pep508Name.FindStringSubmatch(spec)
	if m == nil {
		return "", "", false
	}
	return m[1], strings.TrimSpace(m[3]), true
}

func (c *ManifestCollector) requirements(p string, content []byte, scope string) {
	for _, line := range strings.Split(string(content), "\n") {
		if i := strings.Index(line, " #"); i >= 0 {
			line = line[:i]
		}
		line = strings.TrimSpace(line)
		// Options (-r, -e, --index-url) and direct URLs are not named requirements.
		if line == "" || strings.HasPrefix(line, "#") || strings.HasPrefix(line, "-") || strings.Contains(line, "://") {
			continue
		}
		if name, req, ok := splitRequirement(line); ok {
			c.addDep(Dependency{ManifestPath: p, Ecosystem: "pypi", Name: name, Requirement: req, Scope: scope})
		}
	}
}

func (c *ManifestCollector) pyproject(p string, content []byte) {
	var doc struct {
		Project struct {
			Dependencies         []string            `toml:"dependencies"`
			OptionalDependencies map[string][]string `toml:"optional-dependencies"`
		} `toml:"project"`
		DependencyGroups map[string][]any `toml:"dependency-groups"`
		Tool             struct {
			Poetry struct {
				Dependencies    map[string]any `toml:"dependencies"`
				DevDependencies map[string]any `toml:"dev-dependencies"`
				Group           map[string]struct {
					Dependencies map[string]any `toml:"dependencies"`
				} `toml:"group"`
			} `toml:"poetry"`
		} `toml:"tool"`
	}
	if err := toml.Unmarshal(content, &doc); err != nil {
		c.problem(p, err)
		return
	}
	add := func(specs []string, scope string) {
		for _, spec := range specs {
			if name, req, ok := splitRequirement(spec); ok {
				c.addDep(Dependency{ManifestPath: p, Ecosystem: "pypi", Name: name, Requirement: req, Scope: scope})
			}
		}
	}
	add(doc.Project.Dependencies, "runtime")
	for _, specs := range doc.Project.OptionalDependencies {
		add(specs, "optional")
	}
	for _, group := range doc.DependencyGroups {
		for _, item := range group {
			if spec, ok := item.(string); ok {
				add([]string{spec}, "dev")
			}
		}
	}
	poetry := func(deps map[string]any, scope string) {
		for name, v := range deps {
			if strings.EqualFold(name, "python") {
				continue
			}
			req, _ := v.(string)
			if table, ok := v.(map[string]any); ok {
				req, _ = table["version"].(string)
			}
			c.addDep(Dependency{ManifestPath: p, Ecosystem: "pypi", Name: name, Requirement: req, Scope: scope})
		}
	}
	poetry(doc.Tool.Poetry.Dependencies, "runtime")
	poetry(doc.Tool.Poetry.DevDependencies, "dev")
	for _, g := range doc.Tool.Poetry.Group {
		poetry(g.Dependencies, "dev")
	}
}

func (c *ManifestCollector) goMod(p string, content []byte) {
	inBlock := false
	for _, line := range strings.Split(string(content), "\n") {
		if i := strings.Index(line, "//"); i >= 0 {
			line = line[:i]
		}
		fields := strings.Fields(line)
		switch {
		case len(fields) == 0:
		case inBlock && fields[0] == ")":
			inBlock = false
		case inBlock && len(fields) >= 2:
			c.addDep(Dependency{ManifestPath: p, Ecosystem: "go", Name: fields[0], Requirement: fields[1], Scope: "runtime"})
		case fields[0] == "require" && len(fields) == 2 && fields[1] == "(":
			inBlock = true
		case fields[0] == "require" && len(fields) >= 3:
			c.addDep(Dependency{ManifestPath: p, Ecosystem: "go", Name: fields[1], Requirement: fields[2], Scope: "runtime"})
		}
	}
}

func (c *ManifestCollector) cargo(p string, content []byte) {
	var doc struct {
		Dependencies      map[string]any `toml:"dependencies"`
		DevDependencies   map[string]any `toml:"dev-dependencies"`
		BuildDependencies map[string]any `toml:"build-dependencies"`
		Workspace         struct {
			Dependencies map[string]any `toml:"dependencies"`
		} `toml:"workspace"`
	}
	if err := toml.Unmarshal(content, &doc); err != nil {
		c.problem(p, err)
		return
	}
	add := func(deps map[string]any, scope string) {
		for name, v := range deps {
			req, _ := v.(string)
			if table, ok := v.(map[string]any); ok {
				req, _ = table["version"].(string)
				// `path` dependencies are workspace members, not external crates.
				if _, local := table["path"]; local {
					continue
				}
			}
			c.addDep(Dependency{ManifestPath: p, Ecosystem: "cargo", Name: name, Requirement: req, Scope: scope})
		}
	}
	add(doc.Dependencies, "runtime")
	add(doc.Workspace.Dependencies, "runtime")
	add(doc.DevDependencies, "dev")
	add(doc.BuildDependencies, "build")
}

func (c *ManifestCollector) pom(p string, content []byte) {
	var doc struct {
		Dependencies []struct {
			GroupID    string `xml:"groupId"`
			ArtifactID string `xml:"artifactId"`
			Version    string `xml:"version"`
			Scope      string `xml:"scope"`
		} `xml:"dependencies>dependency"`
	}
	// encoding/xml does not resolve external entities, so XXE is not possible.
	if err := xml.Unmarshal(content, &doc); err != nil {
		c.problem(p, err)
		return
	}
	for _, d := range doc.Dependencies {
		scope := "runtime"
		if d.Scope == "test" {
			scope = "dev"
		}
		c.addDep(Dependency{ManifestPath: p, Ecosystem: "maven", Name: strings.TrimSpace(d.GroupID) + ":" + strings.TrimSpace(d.ArtifactID), Requirement: strings.TrimSpace(d.Version), Scope: scope})
	}
}

func contains(list []string, s string) bool {
	for _, x := range list {
		if x == s {
			return true
		}
	}
	return false
}

func truncate(s string, max int) string {
	if len(s) <= max {
		return s
	}
	return s[:max] + "…"
}
