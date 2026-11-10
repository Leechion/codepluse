package codeg

import (
	"encoding/json"
	"os"
	"slices"
	"strings"
	"testing"

	"harnesshub/internal/providers"
)

// The instance CRUD surface must round-trip through the wire names the
// frontend addresses agents by, and must never let a mutation touch the
// built-in it was cloned from.
func TestHarnessInstanceLifecycle(t *testing.T) {
	s := testServer(t)

	created := command(t, s, "codepluse_instance_create", Row{"name": "Claude 2号", "realTool": "claude", "defaultModel": "claude-sonnet-5"}).(map[string]any)
	wire, _ := created["agent_type"].(string)
	if !strings.HasPrefix(wire, "inst_claude") {
		t.Fatalf("instance wire should be inst_claudeN, got %q", created["agent_type"])
	}
	if created["instance"] != true {
		t.Fatalf("instance flag missing: %v", created)
	}

	list := command(t, s, "codepluse_instance_list", Row{}).([]any)
	if len(list) != 1 {
		t.Fatalf("expected one instance, got %v", list)
	}

	// Wire names must be unique per instance of the same tool.
	second := command(t, s, "codepluse_instance_create", Row{"realTool": "claude"}).(map[string]any)
	if second["agent_type"] == wire {
		t.Fatalf("two instances share wire %q", wire)
	}

	// A pinned provider is validated at create time, before anything is stored.
	if _, err := s.command("codepluse_instance_create", Row{"realTool": "claude", "providerId": "missing"}); err == nil {
		t.Fatal("unknown provider id accepted")
	}
	if err := providers.Save(s.Root, providers.Provider{Name: "pinned", Agent: "claude", URL: "https://example.test", Model: "m", Key: "k"}); err != nil {
		t.Fatal(err)
	}
	d, err := providers.Public(s.Root)
	if err != nil {
		t.Fatal(err)
	}
	var pid string
	for _, p := range d.Providers {
		if p.Name == "pinned" {
			pid = p.ID
		}
	}
	if pid == "" {
		t.Fatal("provider not visible")
	}
	if _, err := s.command("codepluse_instance_create", Row{"realTool": "codex", "providerId": pid}); err == nil {
		t.Fatal("codex instance accepted a pinned provider")
	}

	updated := command(t, s, "codepluse_instance_update", Row{"agentType": wire, "providerId": pid, "name": "Claude 2号·改名"}).(map[string]any)
	if updated["provider_id"] != pid || updated["name"] != "Claude 2号·改名" {
		t.Fatalf("update lost fields: %v", updated)
	}

	command(t, s, "codepluse_instance_delete", Row{"agentType": second["agent_type"]})
	if s.pick(wire) == nil {
		t.Fatal("deleting one instance removed the other")
	}
	if s.pick("claude_code") == nil || s.pick("claude_code").ProviderID != "" {
		t.Fatal("built-in must follow the global selection, never an instance pin")
	}

	// The persisted registry is what survives restart: same wires, same pins.
	raw, err := os.ReadFile(s.instancesFile())
	if err != nil {
		t.Fatal(err)
	}
	var saved []instanceSpec
	if err := json.Unmarshal(raw, &saved); err != nil {
		t.Fatal(err)
	}
	if len(saved) != 1 || saved[0].Wire != wire || saved[0].ProviderID != pid || saved[0].DefaultModel != "claude-sonnet-5" {
		t.Fatalf("bad persisted registry: %s", raw)
	}
}

// The instance's pinned model fills the session default only when the caller
// passed nothing — an explicit selector or delegation still wins.
func TestInstanceDefaultModelOnConnect(t *testing.T) {
	s := testServer(t)
	created := command(t, s, "codepluse_instance_create", Row{"realTool": "claude", "defaultModel": "pinned-model"}).(map[string]any)
	wire := created["agent_type"].(string)

	cid, err := s.connect(Row{"agentType": wire, "workingDir": s.Root})
	if err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	got := s.connections[cid].Options.Model
	s.mu.Unlock()
	if got != "pinned-model" {
		t.Fatalf("default model not applied: %q", got)
	}

	cid2, err := s.connect(Row{"agentType": wire, "workingDir": s.Root, "preferredConfigValues": Row{"model": "explicit"}})
	if err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	got2 := s.connections[cid2].Options.Model
	s.mu.Unlock()
	if got2 != "explicit" {
		t.Fatalf("explicit model overridden by instance default: %q", got2)
	}
}

// Instances load back with their wire names and never shadow a built-in.
func TestInstancesReloadAfterRestart(t *testing.T) {
	s := testServer(t)
	created := command(t, s, "codepluse_instance_create", Row{"name": "Codex 2号", "realTool": "codex", "defaultModel": "gpt-x"}).(map[string]any)
	wire := created["agent_type"].(string)
	root := s.Root

	h := s.Hub
	rt := s.Runtime
	restarted, err := New(root, "", rt, h, func(p string) (string, error) { return p, nil })
	if err != nil {
		t.Fatal(err)
	}
	// loadInstances runs inside New; the wire must resolve and carry its pin.
	agent := restarted.pick(wire)
	if agent == nil {
		t.Fatalf("instance %q lost across restart", wire)
	}
	if agent.DefaultModel != "gpt-x" || agent.RealTool != "codex" {
		t.Fatalf("reload lost fields: %+v", agent)
	}
	if agent.ID == "codex" {
		t.Fatal("instance reloaded over the built-in id")
	}

	// A registry entry for a tool the server does not have is skipped, not
	// registered as a phantom the frontend would render as a dead chip.
	file := restarted.instancesFile()
	var saved []instanceSpec
	b, _ := os.ReadFile(file)
	_ = json.Unmarshal(b, &saved)
	saved = append(saved, instanceSpec{ID: "dead", Wire: "inst_gemini9", Name: "Ghost", RealTool: "gemini"})
	out, _ := json.Marshal(saved)
	if err := os.WriteFile(file, out, 0o600); err != nil {
		t.Fatal(err)
	}
	restarted2, err := New(root, "", rt, h, func(p string) (string, error) { return p, nil })
	if err != nil {
		t.Fatal(err)
	}
	if restarted2.pick("inst_gemini9") != nil {
		t.Fatal("uninstalled tool registered as instance")
	}
	names := []string{}
	for _, a := range restarted2.agents() {
		names = append(names, a["agent_type"].(string))
	}
	if !slices.Contains(names, wire) || !slices.Contains(names, "codex") {
		t.Fatalf("agent list after reload: %v", names)
	}
}
