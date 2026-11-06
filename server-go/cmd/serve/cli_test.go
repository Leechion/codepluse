package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"harnesshub/internal/hub"
)

func TestConfiguredWorkBuddyLaunch(t *testing.T) {
	previous := ROOT
	ROOT = t.TempDir()
	t.Cleanup(func() { ROOT = previous })
	entry := filepath.Join(ROOT, "cli", "bin", "codebuddy")
	if err := os.MkdirAll(filepath.Dir(entry), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(entry, nil, 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(ROOT, "cli", "package.json"), []byte(`{"version":"2.165.0"}`), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(ROOT, ".data"), 0700); err != nil {
		t.Fatal(err)
	}
	data, _ := json.Marshal(map[string]string{"workbuddy": entry})
	if err := os.WriteFile(dataFile("harness-paths.json"), data, 0600); err != nil {
		t.Fatal(err)
	}
	if acpInstalled("workbuddy") != entry || adapterVersion(entry) != "2.165.0" {
		t.Fatal("configured engine not discovered")
	}
	s := &server{}
	launch, err := s.launchFn(&hub.Agent{RealTool: "workbuddy"}, "readonly", ROOT)
	if err != nil {
		t.Fatal(err)
	}
	if len(launch.Args) != 5 || launch.Args[1] != "--serve" || launch.Args[2] != "--acp" {
		t.Fatalf("invalid launch: %v", launch.Args)
	}
	found := false
	for _, env := range launch.Env {
		if env == "CODEBUDDY_FORCE_LITE_WB_BUNDLE=1" {
			found = true
		}
	}
	if !found {
		t.Fatal("desktop bundle routing missing")
	}
}
