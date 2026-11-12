package ingestion

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// File is one inventoried repository file.
type File struct {
	// Path is repository-relative with forward slashes.
	Path        string
	Size        int64
	ContentHash string // lowercase hex SHA-256
	Language    string
	Category    string
	Encoding    string
	Generated   bool
	Vendored    bool
	Lines       int
	// Oversize files are hashed and listed but their content is not retained.
	Oversize bool
}

type ScanLimits struct {
	MaxFiles     int
	MaxBytes     int64
	MaxFileBytes int64
}

type ScanResult struct {
	Files []File
	// TreeHash identifies the whole tree: SHA-256 over sorted path/hash pairs.
	TreeHash        string
	TotalBytes      int64
	SkippedSymlinks int
	SkippedIgnored  int
	SkippedSpecial  int
}

// ErrLimit is returned when a repository exceeds a configured bound. The
// scan stops; nothing is silently truncated.
var ErrLimit = errors.New("repository exceeds configured limits")

// Directories never descended into regardless of ignore files: VCS metadata
// and dependency/caches that are both huge and not part of the project.
var alwaysSkipDirs = map[string]bool{
	".git": true, ".hg": true, ".svn": true, "node_modules": true, "__pycache__": true, ".venv": true, "venv": true,
	".tox": true, ".mypy_cache": true, ".pytest_cache": true, ".ruff_cache": true, ".gradle": true, ".idea": true,
}

// Visitor receives each file. content is nil for oversize files.
type Visitor func(f File, content []byte) error

// Scan walks a directory tree. Symbolic links are never followed (they are
// counted and skipped), so a repository cannot make the scanner read files
// outside its own tree.
func Scan(ctx context.Context, root string, limits ScanLimits, visit Visitor) (*ScanResult, error) {
	root, err := filepath.EvalSymlinks(root)
	if err != nil {
		return nil, err
	}
	res := &ScanResult{}
	ignores := &IgnoreSet{}

	var walk func(dir, rel string) error
	walk = func(dir, rel string) error {
		if err := ctx.Err(); err != nil {
			return err
		}
		entries, err := os.ReadDir(dir)
		if err != nil {
			return fmt.Errorf("read %s: %w", displayPath(rel), err)
		}
		// Ignore files apply to everything in and below their directory, so
		// load them before looking at siblings.
		for _, name := range []string{".gitignore", ".usieignore"} {
			info, err := os.Lstat(filepath.Join(dir, name))
			if err != nil || !info.Mode().IsRegular() || info.Size() > 1<<20 {
				continue
			}
			if data, err := os.ReadFile(filepath.Join(dir, name)); err == nil {
				ignores.Add(rel, data)
			}
		}
		for _, entry := range entries {
			name := entry.Name()
			childRel := name
			if rel != "" {
				childRel = rel + "/" + name
			}
			mode := entry.Type()
			switch {
			case mode&fs.ModeSymlink != 0:
				res.SkippedSymlinks++
			case mode.IsDir():
				if alwaysSkipDirs[name] || ignores.Match(childRel, true) {
					res.SkippedIgnored++
					continue
				}
				if err := walk(filepath.Join(dir, name), childRel); err != nil {
					return err
				}
			case mode.IsRegular():
				if ignores.Match(childRel, false) {
					res.SkippedIgnored++
					continue
				}
				if !validRepoPath(childRel) {
					res.SkippedSpecial++
					continue
				}
				if len(res.Files) >= limits.MaxFiles {
					return fmt.Errorf("%w: more than %d files", ErrLimit, limits.MaxFiles)
				}
				f, content, err := inspect(filepath.Join(dir, name), childRel, limits.MaxFileBytes)
				if err != nil {
					return fmt.Errorf("read %s: %w", childRel, err)
				}
				res.TotalBytes += f.Size
				if res.TotalBytes > limits.MaxBytes {
					return fmt.Errorf("%w: more than %d bytes", ErrLimit, limits.MaxBytes)
				}
				res.Files = append(res.Files, f)
				if visit != nil {
					if err := visit(f, content); err != nil {
						return err
					}
				}
			default:
				res.SkippedSpecial++ // devices, sockets, FIFOs
			}
		}
		return nil
	}
	if err := walk(root, ""); err != nil {
		return nil, err
	}
	sort.Slice(res.Files, func(i, j int) bool { return res.Files[i].Path < res.Files[j].Path })
	res.TreeHash = TreeHash(res.Files)
	return res, nil
}

func inspect(abs, rel string, maxFileBytes int64) (File, []byte, error) {
	f := File{Path: rel, Language: DetectLanguage(rel), Vendored: IsVendored(rel)}
	// O_NOFOLLOW closes the race between listing and opening.
	file, err := os.OpenFile(abs, os.O_RDONLY|oNoFollow, 0)
	if err != nil {
		return f, nil, err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return f, nil, err
	}
	if !info.Mode().IsRegular() {
		return f, nil, fmt.Errorf("not a regular file")
	}
	hash := sha256.New()
	var content []byte
	if info.Size() <= maxFileBytes {
		content, err = io.ReadAll(io.LimitReader(file, maxFileBytes+1))
		if err != nil {
			return f, nil, err
		}
	}
	if content != nil && int64(len(content)) <= maxFileBytes {
		hash.Write(content)
		f.Size = int64(len(content))
	} else {
		// Too large to retain: stream the rest through the hash.
		f.Oversize = true
		hash.Write(content)
		n, err := io.Copy(hash, file)
		if err != nil {
			return f, nil, err
		}
		f.Size = int64(len(content)) + n
		content = nil
	}
	f.ContentHash = hex.EncodeToString(hash.Sum(nil))
	if content != nil {
		f.Encoding = DetectEncoding(content)
		f.Generated = IsGenerated(rel, content)
		if f.Encoding != "binary" && len(content) > 0 {
			f.Lines = bytes.Count(content, []byte{'\n'})
			if content[len(content)-1] != '\n' {
				f.Lines++
			}
		}
	} else {
		f.Encoding = "unknown"
		f.Generated = IsGenerated(rel, nil)
	}
	f.Category = Categorize(rel, f.Language)
	if f.Encoding == "binary" {
		f.Category = CategoryBinary
		if assetExts[strings.ToLower(filepath.Ext(rel))] {
			f.Category = CategoryAsset
		}
	}
	return f, content, nil
}

// validRepoPath rejects names that cannot be represented safely downstream
// (control characters, backslashes, invalid UTF-8, absurd length).
func validRepoPath(p string) bool {
	if len(p) == 0 || len(p) > 1024 {
		return false
	}
	for _, r := range p {
		if r < 0x20 || r == 0x7f || r == '\\' || r == 0xFFFD {
			return false
		}
	}
	return true
}

// TreeHash is a deterministic identity for a set of files.
func TreeHash(files []File) string {
	sorted := make([]File, len(files))
	copy(sorted, files)
	sort.Slice(sorted, func(i, j int) bool { return sorted[i].Path < sorted[j].Path })
	h := sha256.New()
	for _, f := range sorted {
		fmt.Fprintf(h, "%s\x00%s\n", f.Path, f.ContentHash)
	}
	return hex.EncodeToString(h.Sum(nil))
}

func displayPath(rel string) string {
	if rel == "" {
		return "."
	}
	return rel
}
