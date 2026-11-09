package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestDSHReadsDesktopModel(t *testing.T) {
	previous := ROOT
	ROOT = t.TempDir()
	t.Cleanup(func() { ROOT = previous })
	dsh := t.TempDir()
	t.Setenv("DSH_HOME", dsh)
	dir := filepath.Join(dsh, "profiles", "desktop")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatal(err)
	}
	source := "- id: llm-pi-ai\n  config:\n    providers:\n      example:\n        api: openai-completions\n        baseURL: https://example.test/v1\n- id: agent-default-model\n  config:\n    provider: example\n    model: selected-model\n"
	if err := os.WriteFile(filepath.Join(dir, "cordis.patch.yml"), []byte(source), 0600); err != nil {
		t.Fatal(err)
	}
	before := dshModelVersion()
	patch, err := dshDesktopPatch()
	if err != nil {
		t.Fatal(err)
	}
	b, err := os.ReadFile(patch)
	if err != nil {
		t.Fatal(err)
	}
	text := string(b)
	for _, want := range []string{"llm-pi-ai", "agent-default-model", "id: acp", "selected-model"} {
		if !strings.Contains(text, want) {
			t.Fatalf("missing %s", want)
		}
	}
	if err = os.WriteFile(filepath.Join(dir, "cordis.patch.yml"), []byte(strings.ReplaceAll(source, "selected-model", "other-model")), 0600); err != nil {
		t.Fatal(err)
	}
	if dshModelVersion() == before {
		t.Fatal("model change must create a new driver version")
	}
}
