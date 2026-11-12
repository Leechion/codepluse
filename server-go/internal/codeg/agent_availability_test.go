package codeg

import (
	"testing"

	"harnesshub/internal/hub"
)

// With no probe injected, "available" keeps the prior behavior and follows the
// agent's enabled flag, so deployments that never wire a detector are unchanged.
func TestAgentAvailableDefaultsToEnabled(t *testing.T) {
	s := testServer(t)
	rows := s.agents()
	if len(rows) != 2 {
		t.Fatalf("want 2 real agents, got %d", len(rows))
	}
	for _, r := range rows {
		if r["available"] != true || r["enabled"] != true {
			t.Fatalf("default available should follow enabled: %v", r)
		}
	}
}

// When the local-environment probe reports a harness missing, "available" is
// false even though "enabled" (the user/config switch) stays true — proving the
// new-conversation page can hide it rather than show a greyed-out pill.
func TestAgentAvailableProbeOverridesFlag(t *testing.T) {
	s := testServer(t)
	s.AgentAvailable = func(a *hub.Agent) bool { return false }
	for _, r := range s.agents() {
		if r["available"] != false {
			t.Fatalf("probed-missing agent must report available=false: %v", r)
		}
		if r["enabled"] != true {
			t.Fatalf("enabled stays the user switch independent of the probe: %v", r)
		}
	}
}

// The probe is evaluated per agent, so a machine with only one harness installed
// exposes that one as available and hides the other.
func TestAgentAvailableProbePerTool(t *testing.T) {
	s := testServer(t)
	s.AgentAvailable = func(a *hub.Agent) bool { return a.RealTool == "codex" }
	got := map[string]bool{}
	for _, r := range s.agents() {
		got[r["agent_type"].(string)] = r["available"].(bool)
	}
	if got["codex"] != true || got["claude_code"] != false {
		t.Fatalf("per-tool probe mismatch: %v", got)
	}
}
