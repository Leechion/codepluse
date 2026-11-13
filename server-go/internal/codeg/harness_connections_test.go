package codeg

import (
	"encoding/json"
	"harnesshub/internal/hub"
	"harnesshub/internal/providers"
	"strings"
	"testing"
)

func TestSubscriptionMainAndSubProfilesPersistIndependently(t *testing.T) {
	s := testServer(t)
	s.Hub.CreateAgent(&hub.Agent{ID: "agt_codex", Name: "Codex", Real: true, RealTool: "codex", Wire: "codex"})
	pid := profileProvider(t, s, "codex", "https://gateway.test")
	api := command(t, s, "update_main_bot_settings", Row{"settings": Row{"agent_type": "codex", "provider_id": pid, "model": "api-model"}}).(map[string]any)
	official := command(t, s, "update_main_bot_settings", Row{"settings": Row{"agent_type": "codex", "auth_mode": "subscription", "model": ""}}).(map[string]any)
	if api["agent_type"] == official["agent_type"] {
		t.Fatal("API and account profiles share identity")
	}
	main := s.pick(text(official["agent_type"]))
	if main.AuthMode != "subscription" || main.ProviderID != "" {
		t.Fatal("official main profile contains API credentials")
	}
	sub := command(t, s, "codepluse_instance_create", Row{"name": "account-worker", "realTool": "codex", "authMode": "subscription"}).(map[string]any)
	subWire := text(sub["agent_type"])
	if _, err := s.delegationProfile(subWire, ""); err != nil {
		t.Fatal(err)
	}
	s.Hub.DeleteAgent(s.pick(subWire).ID)
	s.loadInstances()
	if reloaded := s.pick(subWire); reloaded == nil || reloaded.AuthMode != "subscription" || reloaded.BaseAgentType != "codex" {
		t.Fatal("official sub profile not restored")
	}
	if err := s.validateProfileAuth("codex", "subscription", pid); err == nil {
		t.Fatal("mixed API and account auth accepted")
	}
	if err := s.validateProfileAuth("dsh", "subscription", ""); err == nil {
		t.Fatal("unsupported official authentication accepted")
	}
}

func TestProviderConfigurationPersistsModelsAndNeverReturnsKeys(t *testing.T) {
	s := testServer(t)
	pid := profileProvider(t, s, "codex", "https://gateway.test")
	raw, err := s.saveProviderConfiguration(Row{"id": pid, "name": "copy", "agent": "codex", "url": "https://another.test", "protocol": "openai-responses", "model": "a", "models": []Row{{"id": "a", "enabled": true}, {"id": "b", "enabled": false}}})
	if err != nil {
		t.Fatal(err)
	}
	out := raw.(providers.Provider)
	b, _ := json.Marshal(out)
	if strings.Contains(string(b), "test-key") || !out.HasKey || out.ID != pid {
		t.Fatal("save exposed key or changed source identity")
	}
	p, err := providers.Find(s.Root, out.ID)
	if err != nil || p.Key != "test-key" || len(p.Models) != 2 {
		t.Fatal("saved credential/model list missing")
	}
	if err := validateProfileModel(p, "b"); err == nil {
		t.Fatal("disabled model selectable")
	}
	original, _ := providers.Find(s.Root, pid)
	if original.URL != "https://another.test" {
		t.Fatal("source provider was not updated")
	}
}

func TestCustomAccountDelegationUsesTheSavedHarnessProfile(t *testing.T) {
	s := testServer(t)
	s.Hub.CreateAgent(&hub.Agent{ID: "agt_qoder", Name: "Qoder", Real: true, Wire: "custom:qoder"})
	sub := command(t, s, "codepluse_instance_create", Row{"name": "Qoder account", "realTool": "custom:qoder", "authMode": "subscription", "defaultModel": "qoder-model"}).(map[string]any)
	wire, err := s.delegationProfile("custom:qoder", "qoder-model")
	if err != nil || wire != text(sub["agent_type"]) {
		t.Fatalf("custom account profile bypassed: %s %v", wire, err)
	}
	s.Hub.DeleteAgent(s.pick(wire).ID)
	s.loadInstances()
	if s.pick(wire) == nil || s.pick(wire).BaseAgentType != "custom:qoder" {
		t.Fatal("custom account profile lost its registered harness")
	}
}
