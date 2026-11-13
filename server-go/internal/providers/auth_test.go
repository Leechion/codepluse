package providers

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestNativeCodexProvidersRemainSeparateFromOfficialLogin(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("CODEX_HOME", dir)
	t.Setenv("TEST_SECOND_PROVIDER_KEY", "second-key")
	os.WriteFile(filepath.Join(dir, "config.toml"), []byte("model='first-model'\nmodel_provider='first'\n[model_providers.first]\nbase_url='https://first.test'\n[model_providers.second]\nbase_url='https://second.test'\nenv_key='TEST_SECOND_PROVIDER_KEY'\nmodel='second-model'\n"), 0600)
	os.WriteFile(filepath.Join(dir, "auth.json"), []byte(`{"OPENAI_API_KEY":"first-key","tokens":{"access_token":"account-secret"}}`), 0600)
	rows, err := readCodex(dir)
	if err != nil || len(rows) != 2 {
		t.Fatalf("native suppliers missing: %v", err)
	}
	if rows[0].ID == rows[1].ID || rows[0].Key != "first-key" || rows[1].Key != "second-key" {
		t.Fatal("native supplier credentials conflated")
	}
	account := Account("codex")
	b, _ := json.Marshal(account)
	if !account.Configured || strings.Contains(string(b), "account-secret") || strings.Contains(string(b), "first-key") {
		t.Fatal("account metadata exposed credentials or missed login")
	}
	os.WriteFile(filepath.Join(dir, "auth.json"), []byte(`{"OPENAI_API_KEY":"api-only"}`), 0600)
	if Account("codex").Configured {
		t.Fatal("API key misidentified as official account")
	}
}
