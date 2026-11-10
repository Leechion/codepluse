package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"harnesshub/internal/hub"
	"harnesshub/internal/providers"
)

func TestClaudeProviderIsAppliedToLaunch(t *testing.T) {
	previous := ROOT
	ROOT = t.TempDir()
	t.Cleanup(func() { ROOT = previous })
	entry := filepath.Join(ROOT, "node_modules", "@agentclientprotocol", "claude-agent-acp", "dist", "index.js")
	if err := os.MkdirAll(filepath.Dir(entry), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(entry, nil, 0600); err != nil {
		t.Fatal(err)
	}
	// acpPackages is computed at process startup; point the test entry at its temporary root.
	old := acpPackages["claude"]
	acpPackages["claude"] = []string{entry}
	t.Cleanup(func() { acpPackages["claude"] = old })
	if err := providers.Save(ROOT, providers.Provider{Name: "test", Agent: "claude", URL: "https://example.test", Model: "test-model", Key: "test-key"}); err != nil {
		t.Fatal(err)
	}
	d, err := providers.Public(ROOT)
	if err != nil {
		t.Fatal(err)
	}
	if err := providers.Activate(ROOT, "claude", d.Providers[0].ID); err != nil {
		t.Fatal(err)
	}
	launch, err := (&server{}).launchFn(&hub.Agent{RealTool: "claude"}, "workspace", ROOT)
	if err != nil {
		t.Fatal(err)
	}
	for _, value := range []string{"ANTHROPIC_BASE_URL=https://example.test", "ANTHROPIC_AUTH_TOKEN=test-key", "ANTHROPIC_MODEL=test-model"} {
		if !slices.Contains(launch.Env, value) {
			t.Fatalf("missing %s", value)
		}
	}
}

func TestDeepSeekLocalHome(t *testing.T) {
	previous := ROOT
	ROOT = t.TempDir()
	t.Cleanup(func() { ROOT = previous })
	t.Setenv("DSH_HOME", "")
	dir := filepath.Join(ROOT, ".data", "dsh-home")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatal(err)
	}
	env, err := buildEnv(&hub.Agent{RealTool: "dsh"})
	if err != nil || !slices.Contains(env, "DSH_HOME="+dir) {
		t.Fatalf("local DSH home missing: %v", err)
	}
}

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
