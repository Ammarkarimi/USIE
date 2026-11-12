// Package ingestion turns a repository (git remote, local directory or
// uploaded archive) into a classified, hashed file inventory plus manifest
// facts. It reads repository content as data and never executes it.
package ingestion

import (
	"path"
	"strings"
)

// Language identifiers. The ten programming languages the platform targets
// are detected; only those with a parser adapter are analyzed further.
var extLanguages = map[string]string{
	".py": "python", ".pyi": "python",
	".js": "javascript", ".jsx": "javascript", ".mjs": "javascript", ".cjs": "javascript",
	".ts": "typescript", ".tsx": "typescript", ".mts": "typescript", ".cts": "typescript",
	".java": "java",
	".c":    "c", ".h": "c",
	".cc": "cpp", ".cpp": "cpp", ".cxx": "cpp", ".hpp": "cpp", ".hh": "cpp", ".hxx": "cpp",
	".rs": "rust",
	".go": "go",
	".cs": "csharp",
	".kt": "kotlin", ".kts": "kotlin",
	// Non-programming formats, reported for inventory purposes.
	".json": "json", ".yaml": "yaml", ".yml": "yaml", ".toml": "toml", ".xml": "xml",
	".md": "markdown", ".rst": "restructuredtext", ".html": "html", ".htm": "html",
	".css": "css", ".scss": "scss", ".less": "less", ".sql": "sql",
	".sh": "shell", ".bash": "shell", ".zsh": "shell", ".ps1": "powershell",
	".proto": "protobuf", ".graphql": "graphql", ".gql": "graphql", ".tf": "terraform",
	".rb": "ruby", ".php": "php", ".swift": "swift", ".scala": "scala", ".lua": "lua", ".r": "r", ".dart": "dart",
	".vue": "vue", ".svelte": "svelte",
}

var nameLanguages = map[string]string{
	"dockerfile": "dockerfile", "makefile": "makefile", "gnumakefile": "makefile",
	"cmakelists.txt": "cmake", "jenkinsfile": "groovy", "go.mod": "gomod", "go.sum": "gosum",
}

// ProgrammingLanguages are the languages the platform targets (spec module A).
var ProgrammingLanguages = []string{"python", "javascript", "typescript", "java", "c", "cpp", "rust", "go", "csharp", "kotlin"}

// DetectLanguage identifies a file's language from its name. Empty means unknown.
func DetectLanguage(p string) string {
	base := strings.ToLower(path.Base(p))
	if lang, ok := nameLanguages[base]; ok {
		return lang
	}
	if strings.HasPrefix(base, "dockerfile.") {
		return "dockerfile"
	}
	return extLanguages[path.Ext(base)]
}

// ParserLanguage returns the analysis-engine language for a file, or "" when
// no parser adapter exists for it. `.d.ts` declaration files are parsed.
func ParserLanguage(p string) string {
	switch strings.ToLower(path.Ext(p)) {
	case ".py", ".pyi":
		return "python"
	case ".js", ".jsx", ".mjs", ".cjs":
		return "javascript"
	case ".ts", ".mts", ".cts":
		return "typescript"
	case ".tsx":
		return "tsx"
	}
	return ""
}
