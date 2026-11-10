package main

import (
	"os"
	"path/filepath"
	"testing"
)

// HARNESS_HUB_ALLOW_ANY_PATH is the security boundary for external workspace
// folders: off (default) keeps a web deployment exposing only the service
// directory, on lets the picker add any folder. Directory browsing must go
// through resolveWorkdir — if the listing path ever stops calling it, these
// assertions are what break first.
func TestResolveWorkdirAllowAnyPathBoundary(t *testing.T) {
	allowed := t.TempDir()
	outside := t.TempDir()
	// Keep both roots comparable after OS-level temp path normalization.
	realAllowed, err := filepath.EvalSymlinks(allowed)
	if err != nil {
		t.Fatal(err)
	}
	realOutside, err := filepath.EvalSymlinks(outside)
	if err != nil {
		t.Fatal(err)
	}
	previous := cfgString("allowedRoot")
	cfgSet("allowedRoot", realAllowed)
	t.Cleanup(func() { cfgSet("allowedRoot", previous) })

	inside := filepath.Join(realAllowed, "project")
	if err := os.MkdirAll(inside, 0o755); err != nil {
		t.Fatal(err)
	}
	external := filepath.Join(realOutside, "notes")
	if err := os.MkdirAll(external, 0o755); err != nil {
		t.Fatal(err)
	}

	t.Setenv("HARNESS_HUB_ALLOW_ANY_PATH", "0")
	if got, err := resolveWorkdir(inside); err != nil || got != inside {
		t.Fatalf("path inside allowed root rejected: %q, %v", got, err)
	}
	if _, err := resolveWorkdir(external); err == nil {
		t.Fatal("external path accepted with HARNESS_HUB_ALLOW_ANY_PATH=0")
	}
	// Traversal must fail even when the lexical prefix looks like the root.
	if _, err := resolveWorkdir(filepath.Join(realAllowed, "..", filepath.Base(outside))); err == nil {
		t.Fatal("traversal outside allowed root accepted with flag off")
	}

	t.Setenv("HARNESS_HUB_ALLOW_ANY_PATH", "1")
	if got, err := resolveWorkdir(external); err != nil || got != external {
		t.Fatalf("external path rejected with flag on: %q, %v", got, err)
	}
}

// A symlink pointing out of the root must not be treated as inside it, even
// with the flag off — EvalSymlinks is what the check relies on.
func TestResolveWorkdirSymlinkEscape(t *testing.T) {
	if os.Getenv("HARNESS_HUB_TEST_SYMLINKS") == "" {
		t.Skip("creating symlinks needs Developer Mode on Windows; set HARNESS_HUB_TEST_SYMLINKS=1 to run")
	}
	allowed := t.TempDir()
	outside := t.TempDir()
	if err := os.WriteFile(filepath.Join(outside, "secret.txt"), []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(allowed, "escape")
	if err := os.Symlink(outside, link); err != nil {
		t.Fatal(err)
	}
	previous := cfgString("allowedRoot")
	cfgSet("allowedRoot", allowed)
	t.Cleanup(func() { cfgSet("allowedRoot", previous) })
	t.Setenv("HARNESS_HUB_ALLOW_ANY_PATH", "0")
	if _, err := resolveWorkdir(filepath.Join(link, "secret.txt")); err == nil {
		t.Fatal("symlink escape resolved inside the allowed root")
	}
}
