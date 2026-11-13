package main

import (
	"encoding/json"
	"github.com/pelletier/go-toml/v2"
	"harnesshub/internal/hub"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestCodexSubscriptionAndAPIUseSeparateCredentials(t *testing.T) {
	old := ROOT
	ROOT = t.TempDir()
	t.Cleanup(func() { ROOT = old })
	native := t.TempDir()
	t.Setenv("CODEX_HOME", native)
	t.Setenv("USERPROFILE", t.TempDir())
	auth := `{"auth_mode":"chatgpt","OPENAI_API_KEY":"native-api","tokens":{"access_token":"account-token"}}`
	config := "model='gateway-model'\nmodel_provider='gateway'\nforced_login_method='api'\n[model_providers.gateway]\nbase_url='https://gateway.test'\n[model_providers.openai]\nbase_url='https://override.test'\n"
	os.WriteFile(filepath.Join(native, "auth.json"), []byte(auth), 0600)
	os.WriteFile(filepath.Join(native, "config.toml"), []byte(config), 0600)
	account := &hub.Agent{RealTool: "codex", Wire: "inst_official", AuthMode: "subscription"}
	env, err := applyOfficialAccount(account, []string{"OPENAI_API_KEY=ambient", "OPENAI_BASE_URL=https://ambient.test", "KEEP=yes"})
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(ROOT, ".data", "codex-homes", account.Wire)
	data, _ := os.ReadFile(filepath.Join(dir, "auth.json"))
	var credentials map[string]any
	json.Unmarshal(data, &credentials)
	if credentials["OPENAI_API_KEY"] != nil || credentials["tokens"] == nil || credentials["auth_mode"] != "chatgpt" {
		t.Fatal("official profile retained API credentials")
	}
	data, _ = os.ReadFile(filepath.Join(dir, "config.toml"))
	var cfg map[string]any
	toml.Unmarshal(data, &cfg)
	if cfg["model_provider"] != "openai" || cfg["model"] != nil || cfg["forced_login_method"] != nil {
		t.Fatal("official profile inherited gateway routing")
	}
	if strings.Contains(strings.Join(env, "\n"), "OPENAI_API_KEY=") || !strings.Contains(strings.Join(env, "\n"), "KEEP=yes") {
		t.Fatal("account environment was not isolated")
	}
	if p, configured, e := resolveProvider(account); e != nil || configured || p.Key != "" {
		t.Fatal("official mode resolved an API supplier")
	}
	api := &hub.Agent{RealTool: "codex", Wire: "inst_api", ProviderID: pinnedTestProvider(t, "codex")}
	if _, err = applyCodexProvider(api, nil); err != nil {
		t.Fatal(err)
	}
	data, _ = os.ReadFile(filepath.Join(ROOT, ".data", "codex-homes", api.Wire, "auth.json"))
	credentials = nil
	json.Unmarshal(data, &credentials)
	if credentials["tokens"] != nil || credentials["OPENAI_API_KEY"] != "pinned-key" {
		t.Fatal("API profile inherited subscription tokens")
	}
	data, _ = os.ReadFile(filepath.Join(native, "auth.json"))
	if string(data) != auth {
		t.Fatal("native login modified")
	}
	data, _ = os.ReadFile(filepath.Join(native, "config.toml"))
	if string(data) != config {
		t.Fatal("native config modified")
	}
	os.WriteFile(filepath.Join(native, "auth.json"), []byte(`{"OPENAI_API_KEY":"key-only"}`), 0600)
	if _, err = applyOfficialAccount(account, nil); err == nil {
		t.Fatal("API key accepted as an official account")
	}
}

func TestClaudeSubscriptionPreservesNonAPISettings(t *testing.T) {
	old := ROOT
	ROOT = t.TempDir()
	t.Cleanup(func() { ROOT = old })
	native := t.TempDir()
	t.Setenv("CLAUDE_CONFIG_DIR", native)
	t.Setenv("CLAUDE_CODE_OAUTH_TOKEN", "")
	os.WriteFile(filepath.Join(native, ".credentials.json"), []byte(`{"claudeAiOauth":{"accessToken":"account-token"}}`), 0600)
	original := `{"env":{"ANTHROPIC_AUTH_TOKEN":"api-token","CLAUDE_CODE_SUBAGENT_MODEL":"gateway","KEEP":"yes"},"model":"gateway-model","apiKeyHelper":"get-api-key","permissions":{"defaultMode":"default"}}`
	os.WriteFile(filepath.Join(native, "settings.json"), []byte(original), 0600)
	agent := &hub.Agent{RealTool: "claude", Wire: "inst_account", AuthMode: "subscription"}
	env, err := applyOfficialAccount(agent, []string{"ANTHROPIC_API_KEY=key", "CLAUDE_CODE_SUBAGENT_MODEL=gateway", "KEEP=yes"})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(strings.Join(env, "\n"), "ANTHROPIC_") || strings.Contains(strings.Join(env, "\n"), "CLAUDE_CODE_SUBAGENT_MODEL=") {
		t.Fatal("API overrides retained")
	}
	data, _ := os.ReadFile(filepath.Join(ROOT, ".data", "claude-homes", agent.Wire, "settings.json"))
	var cfg map[string]any
	json.Unmarshal(data, &cfg)
	e := cfg["env"].(map[string]any)
	if e["KEEP"] != "yes" || len(e) != 1 || cfg["apiKeyHelper"] != nil || cfg["model"] != nil || cfg["permissions"] == nil {
		t.Fatal("incorrect settings isolation")
	}
	data, _ = os.ReadFile(filepath.Join(native, "settings.json"))
	if string(data) != original {
		t.Fatal("native settings modified")
	}
}
