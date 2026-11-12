package ingestion

import (
	"archive/zip"
	"context"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path"
	"path/filepath"
	"strings"
)

// ExtractZip unpacks an archive into dest (an existing empty directory).
//
// Defences: entry names are validated (no absolute paths, no `..`), symlinks
// and special files are skipped, and the entry count and the *actual*
// decompressed byte count are bounded, which defeats zip bombs regardless of
// what the archive headers claim.
func ExtractZip(ctx context.Context, archive io.ReaderAt, size int64, dest string, limits ScanLimits) (files int, err error) {
	zr, err := zip.NewReader(archive, size)
	if err != nil {
		return 0, fmt.Errorf("not a valid zip archive: %w", err)
	}
	if len(zr.File) > limits.MaxFiles*2 {
		return 0, fmt.Errorf("%w: archive has %d entries", ErrLimit, len(zr.File))
	}
	// Archives produced by code hosts wrap everything in one directory.
	strip := commonTopDir(zr.File)
	dest, err = filepath.EvalSymlinks(dest)
	if err != nil {
		return 0, err
	}
	var total int64
	for _, entry := range zr.File {
		if err := ctx.Err(); err != nil {
			return files, err
		}
		name := strings.TrimPrefix(strings.ReplaceAll(entry.Name, "\\", "/"), strip)
		mode := entry.Mode()
		if name == "" || strings.HasSuffix(name, "/") || mode.IsDir() {
			continue
		}
		if mode&fs.ModeSymlink != 0 || !mode.IsRegular() {
			continue
		}
		clean := path.Clean(name)
		if clean != name || path.IsAbs(name) || name == ".." || strings.HasPrefix(name, "../") || !validRepoPath(name) || strings.Contains(name, ":") {
			return files, fmt.Errorf("archive entry %q has an unsafe path", truncate(entry.Name, 200))
		}
		if files >= limits.MaxFiles {
			return files, fmt.Errorf("%w: more than %d files", ErrLimit, limits.MaxFiles)
		}
		target := filepath.Join(dest, filepath.FromSlash(name))
		if !strings.HasPrefix(target, dest+string(os.PathSeparator)) {
			return files, fmt.Errorf("archive entry %q escapes the destination", truncate(entry.Name, 200))
		}
		if err := os.MkdirAll(filepath.Dir(target), 0o700); err != nil {
			return files, err
		}
		n, err := extractEntry(entry, target, limits.MaxBytes-total)
		total += n
		if err != nil {
			return files, err
		}
		files++
	}
	return files, nil
}

func extractEntry(entry *zip.File, target string, remaining int64) (int64, error) {
	src, err := entry.Open()
	if err != nil {
		return 0, fmt.Errorf("archive entry %q: %w", truncate(entry.Name, 200), err)
	}
	defer src.Close()
	// O_EXCL: duplicate entry names must not overwrite earlier files.
	dst, err := os.OpenFile(target, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return 0, fmt.Errorf("archive entry %q: %w", truncate(entry.Name, 200), err)
	}
	n, err := io.Copy(dst, io.LimitReader(src, remaining+1))
	if cerr := dst.Close(); err == nil {
		err = cerr
	}
	if err != nil {
		return n, fmt.Errorf("archive entry %q: %w", truncate(entry.Name, 200), err)
	}
	if n > remaining {
		return n, fmt.Errorf("%w: archive expands beyond the size limit", ErrLimit)
	}
	return n, nil
}

func commonTopDir(entries []*zip.File) string {
	top := ""
	for _, e := range entries {
		name := strings.ReplaceAll(e.Name, "\\", "/")
		i := strings.Index(name, "/")
		if i <= 0 {
			if e.Mode().IsDir() || strings.HasSuffix(name, "/") {
				continue
			}
			return "" // a file at the top level
		}
		if top == "" {
			top = name[:i+1]
		} else if name[:i+1] != top {
			return ""
		}
	}
	if top == "../" || top == "./" {
		return ""
	}
	return top
}

// ResolveLocalPath validates a 'local' repository location: it must be an
// existing directory that, after resolving symlinks, lies inside one of the
// configured allowed roots. With no roots configured, local import is off.
func ResolveLocalPath(location string, allowedRoots []string) (string, error) {
	if len(allowedRoots) == 0 {
		return "", fmt.Errorf("local repository import is disabled (no allowed roots configured)")
	}
	if !filepath.IsAbs(location) {
		return "", fmt.Errorf("local repository path must be absolute")
	}
	real, err := filepath.EvalSymlinks(filepath.Clean(location))
	if err != nil {
		return "", fmt.Errorf("local repository path is not accessible")
	}
	info, err := os.Stat(real)
	if err != nil || !info.IsDir() {
		return "", fmt.Errorf("local repository path is not a directory")
	}
	for _, root := range allowedRoots {
		if real == root || strings.HasPrefix(real, root+string(os.PathSeparator)) {
			return real, nil
		}
	}
	return "", fmt.Errorf("local repository path is outside the allowed roots")
}
