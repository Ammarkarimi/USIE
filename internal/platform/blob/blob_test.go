package blob

import (
	"context"
	"errors"
	"strings"
	"testing"
)

func TestFSRoundTrip(t *testing.T) {
	ctx := context.Background()
	s, err := NewFS(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.Get(ctx, "a/b"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("want ErrNotFound, got %v", err)
	}
	if n, err := s.Put(ctx, "a/b", strings.NewReader("hello")); err != nil || n != 5 {
		t.Fatalf("put: %d %v", n, err)
	}
	if ok, _ := s.Exists(ctx, "a/b"); !ok {
		t.Fatal("should exist")
	}
	data, err := GetBytes(ctx, s, "a/b", 10)
	if err != nil || string(data) != "hello" {
		t.Fatalf("get: %q %v", data, err)
	}
	if _, err := GetBytes(ctx, s, "a/b", 3); err == nil {
		t.Fatal("limit not enforced")
	}
	if err := s.Delete(ctx, "a/b"); err != nil {
		t.Fatal(err)
	}
	if err := s.Delete(ctx, "a/b"); err != nil {
		t.Fatal("delete must be idempotent")
	}
}

func TestKeysCannotTraverse(t *testing.T) {
	s, _ := NewFS(t.TempDir())
	for _, key := range []string{"", "/abs", "../x", "a/../b", "a//b", "a/./b", ".hidden", "a\\b", "a/b/", "a b", "a\x00b", "a/..b"} {
		if ValidKey(key) {
			t.Errorf("%q should be invalid", key)
		}
		if _, err := s.Put(context.Background(), key, strings.NewReader("x")); err == nil {
			t.Errorf("put %q should fail", key)
		}
	}
	for _, key := range []string{"a", "org/1f/src/ab/cdef0123"} {
		if !ValidKey(key) {
			t.Errorf("%q should be valid", key)
		}
	}
}
