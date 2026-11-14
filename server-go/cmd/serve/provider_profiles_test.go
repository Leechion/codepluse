package main

import (
	"github.com/pelletier/go-toml/v2"
	"harnesshub/internal/hub"
	"harnesshub/internal/providers"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func pinnedTestProvider(t *testing.T, tool string) string {
	t.Helper()
	if _, err := providers.SaveConfiguration(ROOT, providers.Provider{Name: "test", Agent: tool, URL: "https://api.test/v1", Key: "pinned-key", Model: "default-model"}); err != nil {
		t.Fatal(err)
	}
	list, err := providers.Public(ROOT)
	if err != nil {
		t.Fatal(err)
	}
	for _, p := range list.Providers {
		if p.Source == "" && p.Agent == tool {
			return p.ID
		}
	}
	t.Fatal("missing provider")
	return ""
}

func TestDSHProfilesHaveIndependentModelsAndDoNotWriteNativeConfig(t *testing.T) {
	old := ROOT
	ROOT = t.TempDir()
	t.Cleanup(func() { ROOT = old })
	native := t.TempDir()
	t.Setenv("DSH_HOME", native)
	t.Setenv("USERPROFILE", t.TempDir())
	pid := pinnedTestProvider(t, "dsh")
	a := &hub.Agent{RealTool: "dsh", Wire: "inst_dsh2", ProviderID: pid, DefaultModel: "model-a"}
	b := *a
	b.Wire = "inst_dsh3"
	b.DefaultModel = "model-b"
	first, env, err := dshProviderPatch(a, nil)
	if err != nil {
		t.Fatal(err)
	}
	second, _, err := dshProviderPatch(&b, nil)
	if err != nil || first == second {
		t.Fatal("parallel models share a mutable patch")
	}
	one, _ := os.ReadFile(first)
	two, _ := os.ReadFile(second)
	if !strings.Contains(string(one), "model-a") || !strings.Contains(string(two), "model-b") || strings.Contains(string(one), "pinned-key") {
		t.Fatal("DSH model or credential isolation failed")
	}
	if len(env) != 1 || env[0] != "CODEPLUSE_DSH_API_KEY=pinned-key" {
		t.Fatal("DSH credential not injected into its own process")
	}
	if _, err = os.Stat(filepath.Join(native, "profiles", "desktop", "cordis.patch.yml")); !os.IsNotExist(err) {
		t.Fatal("native DSH configuration modified")
	}
	if providerVersion(a) == providerVersion(&b) {
		t.Fatal("different model profiles reuse a driver fingerprint")
	}
}

func TestCodexProviderOnlyChangesIsolatedHome(t *testing.T) {
	old := ROOT
	ROOT = t.TempDir()
	t.Cleanup(func() { ROOT = old })
	home := t.TempDir()
	t.Setenv("USERPROFILE", home)
	native := filepath.Join(home, ".codex")
	_ = os.MkdirAll(native, 0700)
	source := "model='original'\n"
	_ = os.WriteFile(filepath.Join(native, "config.toml"), []byte(source), 0600)
	pid := pinnedTestProvider(t, "codex")
	agent := &hub.Agent{RealTool: "codex", Wire: "inst_codex2", ProviderID: pid, DefaultModel: "selected-model"}
	env, err := applyCodexProvider(agent, []string{"CODEX_HOME=" + native})
	if err != nil {
		t.Fatal(err)
	}
	isolated := ""
	for _, e := range env {
		if strings.HasPrefix(e, "CODEX_HOME=") {
			isolated = strings.TrimPrefix(e, "CODEX_HOME=")
		}
	}
	if isolated == native || isolated == "" {
		t.Fatal("pinned provider used the native configuration home")
	}
	data, _ := os.ReadFile(filepath.Join(isolated, "config.toml"))
	var cfg map[string]any
	if toml.Unmarshal(data, &cfg) != nil || cfg["model"] != "selected-model" || cfg["model_provider"] != "codepluse" {
		t.Fatal("Codex provider/model binding missing")
	}
	original, _ := os.ReadFile(filepath.Join(native, "config.toml"))
	if string(original) != source {
		t.Fatal("native Codex configuration modified")
	}
	agent.ProviderID = "missing"
	if _, _, err = resolveProvider(agent); err == nil {
		t.Fatal("invalid pin silently fell back to another provider")
	}
}
