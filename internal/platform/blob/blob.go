// Package blob is the object storage abstraction. The filesystem
// implementation is the only one today; an S3-compatible implementation can
// satisfy the same interface (see ADR 0004).
package blob

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"strings"
)

var ErrNotFound = errors.New("blob: not found")

type Store interface {
	Put(ctx context.Context, key string, r io.Reader) (int64, error)
	Get(ctx context.Context, key string) (io.ReadCloser, error)
	Exists(ctx context.Context, key string) (bool, error)
	Delete(ctx context.Context, key string) error
}

// Keys are restricted to a conservative alphabet so that no key can ever be
// interpreted as a path traversal by any backend.
var keyPattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._+-]*(/[A-Za-z0-9][A-Za-z0-9._+-]*)*$`)

func ValidKey(key string) bool {
	return len(key) <= 512 && keyPattern.MatchString(key) && !strings.Contains(key, "..")
}

// FS stores blobs as files under a root directory.
type FS struct{ root string }

func NewFS(root string) (*FS, error) {
	if err := os.MkdirAll(root, 0o700); err != nil {
		return nil, err
	}
	real, err := filepath.EvalSymlinks(root)
	if err != nil {
		return nil, err
	}
	return &FS{root: real}, nil
}

// Root is the directory blobs live in. Used to hand a read-only view of
// stored sources to the analysis engine.
func (s *FS) Root() string { return s.root }

func (s *FS) path(key string) (string, error) {
	if !ValidKey(key) {
		return "", fmt.Errorf("blob: invalid key %q", key)
	}
	return filepath.Join(s.root, filepath.FromSlash(key)), nil
}

func (s *FS) Put(ctx context.Context, key string, r io.Reader) (int64, error) {
	path, err := s.path(key)
	if err != nil {
		return 0, err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return 0, err
	}
	// Write to a temporary file and rename: readers never see partial blobs.
	tmp, err := os.CreateTemp(filepath.Dir(path), ".tmp-*")
	if err != nil {
		return 0, err
	}
	defer os.Remove(tmp.Name())
	n, err := io.Copy(tmp, &ctxReader{ctx: ctx, r: r})
	if cerr := tmp.Close(); err == nil {
		err = cerr
	}
	if err != nil {
		return n, err
	}
	if err := os.Chmod(tmp.Name(), 0o600); err != nil {
		return n, err
	}
	return n, os.Rename(tmp.Name(), path)
}

func (s *FS) Get(_ context.Context, key string) (io.ReadCloser, error) {
	path, err := s.path(key)
	if err != nil {
		return nil, err
	}
	f, err := os.Open(path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, ErrNotFound
	}
	return f, err
}

func (s *FS) Exists(_ context.Context, key string) (bool, error) {
	path, err := s.path(key)
	if err != nil {
		return false, err
	}
	_, err = os.Stat(path)
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	return err == nil, err
}

func (s *FS) Delete(_ context.Context, key string) error {
	path, err := s.path(key)
	if err != nil {
		return err
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	return nil
}

// PutBytes and GetBytes are conveniences for small blobs.
func PutBytes(ctx context.Context, s Store, key string, data []byte) error {
	_, err := s.Put(ctx, key, bytes.NewReader(data))
	return err
}

func GetBytes(ctx context.Context, s Store, key string, limit int64) ([]byte, error) {
	r, err := s.Get(ctx, key)
	if err != nil {
		return nil, err
	}
	defer r.Close()
	data, err := io.ReadAll(io.LimitReader(r, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, fmt.Errorf("blob: %s exceeds %d bytes", key, limit)
	}
	return data, nil
}

type ctxReader struct {
	ctx context.Context
	r   io.Reader
}

func (c *ctxReader) Read(p []byte) (int, error) {
	if err := c.ctx.Err(); err != nil {
		return 0, err
	}
	return c.r.Read(p)
}
