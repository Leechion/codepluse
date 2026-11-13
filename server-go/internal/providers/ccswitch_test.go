package providers

import (
	"database/sql"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestCCSwitchReadAndFollowCurrent(t *testing.T) {
	home := t.TempDir()
	t.Setenv("USERPROFILE", home)
	dir := filepath.Join(home, ".cc-switch")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatal(err)
	}
	db, err := sql.Open("sqlite", filepath.Join(dir, "cc-switch.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	_, err = db.Exec(`CREATE TABLE providers (id TEXT,app_type TEXT,name TEXT,settings_config TEXT,is_current BOOLEAN,sort_index INTEGER)`)
	if err != nil {
		t.Fatal(err)
	}
	_, err = db.Exec(`INSERT INTO providers VALUES (?,?,?,?,?,?)`, "id1", "claude", "test", `{"env":{"ANTHROPIC_BASE_URL":"https://example.test","ANTHROPIC_AUTH_TOKEN":"secret-123","ANTHROPIC_MODEL":"example-model","ANTHROPIC_DEFAULT_SONNET_MODEL":"sonnet-x"}}`, true, 1)
	if err != nil {
		t.Fatal(err)
	}
	// Other harnesses must read their own native configuration, even when
	// CC Switch also has entries for them.
	if _, err = db.Exec(`INSERT INTO providers VALUES (?,?,?,?,?,?)`, "codex-id", "codex", "CC Codex", `{"auth":{"OPENAI_API_KEY":"cc-key"},"config":"model='cc-model'"}`, true, 2); err != nil {
		t.Fatal(err)
	}
	public, err := Public(t.TempDir())
	if err != nil || public.CCError != "" {
		t.Fatalf("read failed: %v %s", err, public.CCError)
	}
	if len(public.Providers) != 1 || public.Providers[0].ID != "ccswitch:id1" || !public.Providers[0].CCCurrent || !public.Providers[0].HasKey {
		t.Fatal("missing CC Switch provider")
	}
	b, _ := json.Marshal(public)
	if strings.Contains(string(b), "secret-123") {
		t.Fatal("secret leaked")
	}
	p, ok, err := Active(t.TempDir(), "claude")
	if err != nil || !ok || p.Env["ANTHROPIC_DEFAULT_SONNET_MODEL"] != "sonnet-x" {
		t.Fatalf("current provider not applied: %v", err)
	}
	_, err = db.Exec(`UPDATE providers SET settings_config=? WHERE id='id1'`, `{"env":{"ANTHROPIC_AUTH_TOKEN":"secret-456","ANTHROPIC_MODEL":"model-new"}}`)
	if err != nil {
		t.Fatal(err)
	}
	p, ok, err = Active(t.TempDir(), "claude")
	if err != nil || !ok || p.Model != "model-new" || p.Env["ANTHROPIC_AUTH_TOKEN"] != "secret-456" {
		t.Fatal("CC Switch changes were not reloaded")
	}
}
