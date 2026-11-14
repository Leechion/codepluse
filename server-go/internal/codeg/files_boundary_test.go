package codeg

import (
	"os"
	"path/filepath"
	"testing"
)

func TestDirectoryBrowseRespectsWorkspaceBoundary(t *testing.T) {
	s := testServer(t)
	outside := t.TempDir()
	if err := os.WriteFile(filepath.Join(outside, "secret.txt"), []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}

	t.Setenv("HARNESS_HUB_ALLOW_ANY_DIR_BROWSE", "0")
	t.Setenv("HARNESS_HUB_ALLOW_ANY_PATH", "0")

	if _, err := s.files("list_directory_entries", Row{"path": outside}); err == nil {
		t.Fatal("browse outside the workspace was allowed with both flags off")
	}
	if _, err := s.files("list_directory_entries", Row{"path": s.Root}); err != nil {
		t.Fatalf("browse inside the workspace failed: %v", err)
	}

	// Opening a folder is the grant: afterwards it is an authorised root.
	s.store.mu.Lock()
	s.store.Folders = append(s.store.Folders, Row{"id": 99, "path": outside, "kind": "regular"})
	s.store.mu.Unlock()
	if _, err := s.files("list_directory_entries", Row{"path": outside}); err != nil {
		t.Fatalf("browse of an opened folder failed: %v", err)
	}
}

func TestDirectoryBrowseFlagAllowsArbitraryPath(t *testing.T) {
	s := testServer(t)
	outside := t.TempDir()
	t.Setenv("HARNESS_HUB_ALLOW_ANY_PATH", "0")
	t.Setenv("HARNESS_HUB_ALLOW_ANY_DIR_BROWSE", "1")
	if _, err := s.files("list_directory_entries", Row{"path": outside}); err != nil {
		t.Fatalf("browse flag should allow an arbitrary directory: %v", err)
	}
}
