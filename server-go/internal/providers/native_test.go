package providers

import (
	"encoding/json"
	"harnesshub/internal/modelproviders"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestDSHAPIProvidersAndModelsStayBackendOnly(t *testing.T) {
	home := t.TempDir()
	t.Setenv("DSH_HOME", home)
	t.Setenv("CODEX_HOME", t.TempDir())
	t.Setenv("CLAUDE_CONFIG_DIR", t.TempDir())
	t.Setenv("USERPROFILE", t.TempDir())
	dir := filepath.Join(home, "profiles", "desktop")
	_ = os.MkdirAll(dir, 0700)
	source := "- id: llm-pi-ai\n  config:\n    providers:\n      gateway:\n        api: openai-completions\n        baseURL: https://provider.test/v1\n        apiKeyEnv: NATIVE_TEST_API_KEY\n        models:\n          - id: model-small\n            name: Small\n          - id: model-large\n            name: Large\n- id: agent-default-model\n  config:\n    provider: gateway\n    model: model-large\n"
	_ = os.WriteFile(filepath.Join(dir, "cordis.patch.yml"), []byte(source), 0600)
	_ = os.WriteFile(filepath.Join(home, ".credentials.yaml"), []byte("refs:\n  NATIVE_TEST_API_KEY: do-not-expose-this-key\n"), 0600)
	t.Setenv("NATIVE_TEST_API_KEY", "")
	root := t.TempDir()
	public, err := Public(root)
	if err != nil || len(public.Providers) != 1 {
		t.Fatalf("catalogue failed: %v", err)
	}
	p := public.Providers[0]
	if p.Agent != "dsh" || p.Model != "model-large" || len(p.Models) != 2 || !p.HasKey || !p.API {
		t.Fatal("native provider/model metadata missing")
	}
	encoded, _ := json.Marshal(public)
	if strings.Contains(string(encoded), "do-not-expose") || p.Key != "" || p.Native != nil || p.Env != nil {
		t.Fatal("credentials escaped public metadata")
	}
	resolved, err := Find(root, p.ID)
	if err != nil || resolved.Key != "do-not-expose-this-key" || resolved.Native == nil {
		t.Fatal("backend could not resolve native API provider")
	}
	after, _ := os.ReadFile(filepath.Join(dir, "cordis.patch.yml"))
	if string(after) != source {
		t.Fatal("reading native configuration changed it")
	}
}

func TestCodexSubscriptionIsNotAnAPIProvider(t *testing.T) {
	if IsAPI(Provider{URL: "https://api.anthropic.com", Key: "sk-ant-oat-subscription"}) {
		t.Fatal("OAuth subscription treated as API key")
	}
	dir := t.TempDir()
	os.WriteFile(filepath.Join(dir, "config.toml"), []byte("model='gpt-test'\nmodel_provider='gateway'\n[model_providers.gateway]\nbase_url='https://provider.test/v1'\nwire_api='responses'"), 0600)
	os.WriteFile(filepath.Join(dir, "auth.json"), []byte(`{"tokens":{"access_token":"subscription-token"}}`), 0600)
	rows, err := readCodex(dir)
	if err != nil || len(rows) != 1 || rows[0].Key != "" || IsAPI(rows[0]) {
		t.Fatal("subscription account exposed as API provider")
	}
	os.WriteFile(filepath.Join(dir, "auth.json"), []byte(`{"OPENAI_API_KEY":"api-key"}`), 0600)
	rows, err = readCodex(dir)
	if err != nil || len(rows) != 1 || rows[0].Agent != "codex" || rows[0].Model != "gpt-test" || rows[0].Key != "api-key" {
		t.Fatal("Codex API configuration not read")
	}
}

func TestSavedProviderPublicFlagsRemainAPIWithoutExposingKey(t *testing.T) {
	t.Setenv("USERPROFILE", t.TempDir())
	t.Setenv("DSH_HOME", t.TempDir())
	t.Setenv("CODEX_HOME", t.TempDir())
	t.Setenv("CLAUDE_CONFIG_DIR", t.TempDir())
	root := t.TempDir()
	if _, err := SaveConfiguration(root, Provider{Name: "API", Agent: "codex", URL: "https://provider.test/v1", Model: "model-a", Key: "private-key"}); err != nil {
		t.Fatal(err)
	}
	data, err := Public(root)
	if err != nil || len(data.Providers) != 1 {
		t.Fatal("missing saved provider")
	}
	p := data.Providers[0]
	if !p.API || !p.HasKey || p.Key != "" {
		t.Fatal("saved API provider missing metadata or exposed its key")
	}
}

func TestExistingModelProviderStoreCanBackAHarnessProfile(t *testing.T) {
	t.Setenv("USERPROFILE", t.TempDir())
	t.Setenv("DSH_HOME", t.TempDir())
	t.Setenv("CODEX_HOME", t.TempDir())
	t.Setenv("CLAUDE_CONFIG_DIR", t.TempDir())
	root := t.TempDir()
	model := "custom-model"
	if _, err := modelproviders.Create(root, "saved vendor", "https://provider.test/v1", "stored-secret", "claude_code", &model); err != nil {
		t.Fatal(err)
	}
	list, err := Public(root)
	if err != nil || len(list.Providers) != 1 {
		t.Fatal("existing provider store not read")
	}
	p := list.Providers[0]
	if p.Agent != "claude" || p.Model != model || !p.API || p.Key != "" {
		t.Fatal("existing provider metadata invalid")
	}
	private, err := Find(root, p.ID)
	if err != nil || private.Key != "stored-secret" {
		t.Fatal("existing provider key not resolved server-side")
	}
}
