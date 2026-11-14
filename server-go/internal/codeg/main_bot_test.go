package codeg

import (
	"harnesshub/internal/providers"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"harnesshub/internal/hub"
)

func profileProvider(t *testing.T, s *Server, agent, url string) string {
	t.Helper()
	t.Setenv("DSH_HOME", t.TempDir())
	t.Setenv("CODEX_HOME", t.TempDir())
	t.Setenv("CLAUDE_CONFIG_DIR", t.TempDir())
	t.Setenv("USERPROFILE", t.TempDir())
	if _, err := providers.SaveConfiguration(s.Root, providers.Provider{Name: "gateway", Agent: agent, URL: url, Key: "test-key", Model: "model-a"}); err != nil {
		t.Fatal(err)
	}
	list, err := providers.Public(s.Root)
	if err != nil {
		t.Fatal(err)
	}
	return list.Providers[0].ID
}

func TestProfilesSaveAndReloadWithRegistryIDs(t *testing.T) {
	for _, tool := range []string{"claude", "codex", "dsh"} {
		t.Run(tool, func(t *testing.T) {
			s := testServer(t)
			// Production registry IDs are not the tool names. In particular,
			// Claude and DSH answer under claude_code and deepseek on the wire.
			for _, agent := range s.Hub.Agents() {
				s.Hub.DeleteAgent(agent.ID)
			}
			s.Hub.CreateAgent(&hub.Agent{ID: "agt_local_" + tool, Name: tool, Real: true, RealTool: tool})
			pid := profileProvider(t, s, tool, "https://provider.test")
			bot := command(t, s, "update_main_bot_settings", Row{"settings": Row{"agent_type": baseWire(tool), "provider_id": pid, "model": "main-model"}}).(map[string]any)
			mainWire := text(bot["agent_type"])
			sub := command(t, s, "codepluse_instance_create", Row{"name": "worker", "realTool": tool, "providerId": pid, "defaultModel": "sub-model"}).(map[string]any)
			subWire := text(sub["agent_type"])
			for _, wire := range []string{mainWire, subWire} {
				agent := s.pick(wire)
				if agent == nil || agent.RealTool != tool || agent.ProviderID != pid {
					t.Fatalf("profile %s did not bind the correct harness and provider", wire)
				}
				s.Hub.DeleteAgent(agent.ID)
			}
			s.loadInstances()
			for wire, model := range map[string]string{mainWire: "main-model", subWire: "sub-model"} {
				agent := s.pick(wire)
				if agent == nil || agent.ProviderID != pid || agent.DefaultModel != model {
					t.Fatalf("profile %s was not restored with its provider/model", wire)
				}
			}
		})
	}
}

func TestMainAgentIsIsolatedAndMultipleModelsAreDelegationTargets(t *testing.T) {
	s := testServer(t)
	for _, row := range s.agents() {
		if row["host_tools_supports_delegation"] != true {
			t.Fatal("native tool mode incorrectly withholds collaboration tools")
		}
	}
	pid := profileProvider(t, s, "claude", "https://provider.test")
	bot := command(t, s, "update_main_bot_settings", Row{"settings": Row{"agent_type": "claude_code", "provider_id": pid, "model": "main-a"}}).(map[string]any)
	first := text(bot["agent_type"])
	if !strings.HasPrefix(first, "inst_main_claude_") {
		t.Fatal("main agent not isolated")
	}
	if s.pick(first).ProviderID != pid || s.pick(first).DefaultModel != "main-a" {
		t.Fatal("main profile did not pin provider/model")
	}
	if len(s.delegationAgents()) != 0 {
		t.Fatal("main API mode offered unconfigured subscription harnesses")
	}
	bot = command(t, s, "update_main_bot_settings", Row{"settings": Row{"agent_type": "claude_code", "provider_id": pid, "model": "main-b"}}).(map[string]any)
	if first == text(bot["agent_type"]) || s.pick(first).DefaultModel != "main-a" {
		t.Fatal("new main model changed the old conversation's profile")
	}
	a := command(t, s, "codepluse_instance_create", Row{"name": "fast", "realTool": "claude", "providerId": pid, "defaultModel": "model-a"}).(map[string]any)
	b := command(t, s, "codepluse_instance_create", Row{"name": "deep", "realTool": "claude", "providerId": pid, "defaultModel": "model-b"}).(map[string]any)
	rows := s.delegationAgents()
	if len(rows) != 2 {
		t.Fatal("configured sub-agent targets not isolated from the main agent")
	}
	wire, err := s.delegationProfile("claude_code", "model-b")
	if err != nil || wire != text(b["agent_type"]) {
		t.Fatal("same-harness model selection chose the wrong profile")
	}
	if _, err = s.delegationProfile(text(a["agent_type"]), "model-b"); err == nil {
		t.Fatal("profile accepted a different model")
	}
	if _, err = s.delegationProfile("claude_code", ""); err == nil {
		t.Fatal("ambiguous provider/model silently selected")
	}
	target, _, ok := resolveSplitModelTarget("claude_code", "model-b", rows, first)
	if !ok || target != text(b["agent_type"]) {
		t.Fatal("split selected the wrong model profile")
	}
	list := command(t, s, "codepluse_instance_list", Row{}).([]any)
	if len(list) != 2 {
		t.Fatal("internal main profiles leaked into sub-agent editor")
	}
}

func TestProviderModelsResolveTheStoredCredentialAndURL(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/models" || r.Header.Get("x-api-key") != "test-key" {
			t.Error("provider URL or credentials not selected correctly")
			w.WriteHeader(401)
			return
		}
		_, _ = w.Write([]byte(`{"data":[{"id":"model-b","display_name":"B"}]}`))
	}))
	defer server.Close()
	s := testServer(t)
	pid := profileProvider(t, s, "claude", server.URL)
	// Do not route an in-process fixture through the user's system proxy.
	t.Setenv("HTTPS_PROXY", "")
	t.Setenv("HTTP_PROXY", "")
	raw, err := s.harnessProviderModels(pid)
	if err != nil {
		t.Fatal(err)
	}
	row := raw.(map[string]any)
	models := row["models"].([]listedModel)
	found := false
	for _, model := range models {
		if model.ID == "model-b" {
			found = true
		}
	}
	if !found {
		t.Fatal("remote provider models not merged with configured models")
	}
}
