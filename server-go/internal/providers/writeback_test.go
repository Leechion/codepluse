package providers

import (
	"database/sql"
	"encoding/json"
	"github.com/pelletier/go-toml/v2"
	"gopkg.in/yaml.v3"
	"os"
	"path/filepath"
	"testing"
)

func writebackHomes(t *testing.T) string {
	t.Helper()
	t.Setenv("USERPROFILE", t.TempDir())
	t.Setenv("CODEX_HOME", t.TempDir())
	t.Setenv("DSH_HOME", t.TempDir())
	t.Setenv("CLAUDE_CONFIG_DIR", t.TempDir())
	return t.TempDir()
}
func TestCCSwitchSaveUpdatesOriginalRowWithoutCreatingProvider(t *testing.T) {
	root := writebackHomes(t)
	home, _ := os.UserHomeDir()
	dir := filepath.Join(home, ".cc-switch")
	os.MkdirAll(dir, 0700)
	db, err := sql.Open("sqlite", filepath.Join(dir, "cc-switch.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	_, err = db.Exec(`CREATE TABLE providers(id TEXT,app_type TEXT,name TEXT,settings_config TEXT,is_current BOOLEAN,sort_index INTEGER); INSERT INTO providers VALUES('same','claude','old','{"env":{"ANTHROPIC_BASE_URL":"https://old.test","ANTHROPIC_AUTH_TOKEN":"kept-key","ANTHROPIC_MODEL":"a","KEEP":"unchanged"},"unrelated":{"keep":true}}',1,2); INSERT INTO providers VALUES('other','codex','untouched','{}',0,3)`)
	if err != nil {
		t.Fatal(err)
	}
	off := false
	saved, err := SaveConfiguration(root, Provider{ID: "ccswitch:same", Agent: "claude", Name: "new", URL: "https://new.test", Model: "b", Protocol: "anthropic", Models: []Model{{ID: "b"}, {ID: "a", Enabled: &off}}})
	if err != nil {
		t.Fatal(err)
	}
	if saved.ID != "ccswitch:same" {
		t.Fatal("source identity changed")
	}
	var raw, name string
	var current bool
	var count int
	if err = db.QueryRow("SELECT name,settings_config,is_current FROM providers WHERE id='same' AND app_type='claude'").Scan(&name, &raw, &current); err != nil {
		t.Fatal(err)
	}
	cfg := map[string]any{}
	json.Unmarshal([]byte(raw), &cfg)
	env := object(cfg["env"])
	if name != "new" || !current || env["ANTHROPIC_BASE_URL"] != "https://new.test" || env["ANTHROPIC_AUTH_TOKEN"] != "kept-key" || env["KEEP"] != "unchanged" || cfg["unrelated"] == nil {
		t.Fatal("save did not preserve source settings")
	}
	db.QueryRow("SELECT count(*) FROM providers").Scan(&count)
	if count != 2 {
		t.Fatal("duplicate source row created")
	}
	loaded, err := Find(root, saved.ID)
	if err != nil || len(loaded.Models) != 2 || loaded.Models[1].Enabled == nil || *loaded.Models[1].Enabled {
		t.Fatal("model changes not restored")
	}
	if _, err = os.Stat(path(root)); !os.IsNotExist(err) {
		t.Fatal("independent supplier file created")
	}
}

func TestCodexSaveUpdatesNativeProviderAndRetainsLoginAndOtherSettings(t *testing.T) {
	root := writebackHomes(t)
	dir := NativeHome("codex")
	file := filepath.Join(dir, "config.toml")
	os.WriteFile(file, []byte("model='a'\nmodel_provider='gateway'\n[model_providers.gateway]\nname='Old'\nbase_url='https://old.test'\n[mcp_servers.keep]\ncommand='keep-command'\n"), 0600)
	auth := `{"OPENAI_API_KEY":"kept-key","tokens":{"access_token":"keep-account-token"}}`
	os.WriteFile(filepath.Join(dir, "auth.json"), []byte(auth), 0600)
	p := Provider{ID: "native:codex:provider:gateway", Agent: "codex", Name: "New", URL: "https://new.test", Model: "b", Protocol: "openai-responses", Models: []Model{{ID: "b"}, {ID: "c"}}}
	if _, err := SaveConfiguration(root, p); err != nil {
		t.Fatal(err)
	}
	b, _ := os.ReadFile(file)
	cfg := map[string]any{}
	toml.Unmarshal(b, &cfg)
	if cfg["model"] != "b" || cfg["model_provider"] != "gateway" || object(object(cfg["mcp_servers"])["keep"])["command"] != "keep-command" {
		t.Fatal("native settings lost")
	}
	loaded, err := Find(root, p.ID)
	if err != nil || loaded.URL != p.URL || loaded.Key != "kept-key" || len(loaded.Models) != 2 {
		t.Fatal("native save did not persist")
	}
	b, _ = os.ReadFile(filepath.Join(dir, "auth.json"))
	if string(b) != auth {
		t.Fatal("native account auth changed")
	}
	p.Key = "new-api-key"
	if _, err = SaveConfiguration(root, p); err != nil {
		t.Fatal(err)
	}
	loaded, err = Find(root, p.ID)
	if err != nil || loaded.Key != "new-api-key" {
		t.Fatal("edited native API key not applied")
	}
	if _, err = os.Stat(path(root)); !os.IsNotExist(err) {
		t.Fatal("independent supplier file created")
	}
}

func TestDSHSaveUpdatesOnlySelectedNativeProfile(t *testing.T) {
	root := writebackHomes(t)
	dir := NativeHome("dsh")
	original := "- id: llm-pi-ai\n  config:\n    providers:\n      gateway:\n        baseURL: https://old.test\n        apiKey: kept-key\n        models:\n          - id: a\n            contextWindow: 12345\n      other:\n        baseURL: https://other.test\n- id: agent-default-model\n  config:\n    provider: gateway\n    model: a\n- id: extra-plugin\n  config:\n    keep: true\n"
	for _, profile := range []string{"desktop", "acp"} {
		d := filepath.Join(dir, "profiles", profile)
		os.MkdirAll(d, 0700)
		os.WriteFile(filepath.Join(d, "cordis.patch.yml"), []byte(original), 0600)
	}
	p := Provider{ID: "native:dsh:desktop:gateway", Agent: "dsh", Name: "Gateway edited", URL: "https://new.test", Model: "a", Protocol: "openai-responses", Models: []Model{{ID: "a", Name: "A"}, {ID: "b"}}}
	if _, err := SaveConfiguration(root, p); err != nil {
		t.Fatal(err)
	}
	loaded, err := Find(root, p.ID)
	if err != nil || loaded.URL != p.URL || loaded.Key != "kept-key" || loaded.Name != p.Name || loaded.Protocol != p.Protocol {
		t.Fatal("DSH source not updated")
	}
	b, _ := os.ReadFile(filepath.Join(dir, "profiles", "desktop", "cordis.patch.yml"))
	var plugins []map[string]any
	yaml.Unmarshal(b, &plugins)
	entry := object(object(object(plugins[0]["config"])["providers"])["gateway"])
	models := entry["models"].([]any)
	if object(models[0])["contextWindow"] != 12345 || object(object(plugins[0]["config"])["providers"])["other"] == nil || object(plugins[2]["config"])["keep"] != true {
		t.Fatal("unrelated DSH options lost")
	}
	b, _ = os.ReadFile(filepath.Join(dir, "profiles", "acp", "cordis.patch.yml"))
	if string(b) != original {
		t.Fatal("other profile changed")
	}
}
