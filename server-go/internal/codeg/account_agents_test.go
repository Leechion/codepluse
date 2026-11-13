package codeg

import (
	"harnesshub/internal/hub"
	"harnesshub/internal/providers"
	"strings"
	"testing"
)

func TestAccountAgentProfilesRemainIndependentAndBypassCustomRegistry(t *testing.T) {
	s := testServer(t)
	agent := s.Hub.CreateAgent(&hub.Agent{ID: "agt_account_qoder", Name: "Qoder", Real: true, RealTool: "account:qoder", Wire: "account_qoder"})
	if !providers.SupportsSubscription(agent.RealTool) {
		t.Fatal("account subscription not supported")
	}
	if _, handled, err := s.CustomAgentLaunch(agent); handled || err != nil {
		t.Fatalf("builtin account treated as deleted custom agent: %v", err)
	}
	profile := command(t, s, "codepluse_instance_create", Row{"name": "Qoder model B", "realTool": "account:qoder", "authMode": "subscription", "defaultModel": "qoder/b"}).(map[string]any)
	wire, err := s.delegationProfile("account_qoder", "qoder/b")
	if err != nil || wire != text(profile["agent_type"]) {
		t.Fatalf("delegation lost independent profile: %s %v", wire, err)
	}
	delegated := s.pick(wire)
	if delegated == nil || delegated.RealTool != "account:qoder" || delegated.BaseAgentType != "account_qoder" {
		t.Fatal("account profile changed executor")
	}
	if _, handled, err := s.CustomAgentLaunch(delegated); handled || err != nil {
		t.Fatal("account instance routed to custom registry")
	}
	if providers.SupportsSubscription("account:unknown") {
		t.Fatal("unknown account type accepted")
	}
	main := command(t, s, "update_main_bot_settings", Row{"settings": Row{"agent_type": "account_qoder", "auth_mode": "subscription", "model": "qoder/a"}}).(map[string]any)
	mainAgent := s.pick(text(main["agent_type"]))
	if mainAgent == nil || mainAgent.RealTool != "account:qoder" || strings.Contains(mainAgent.Wire, ":") {
		t.Fatal("main account agent lost executor or uses an invalid Windows identifier")
	}
}

func TestIndependentAccountAgentsAreNotFilteredAsHarnessInstances(t *testing.T) {
	s := testServer(t)
	base := s.Hub.CreateAgent(&hub.Agent{ID: "agt_account_qoder", Type: "account-agent", Name: "Qoder", Real: true, RealTool: "account:qoder", Wire: "account_qoder"})
	instance := command(t, s, "codepluse_instance_create", Row{"name": "Qoder model B", "realTool": "account:qoder", "authMode": "subscription", "defaultModel": "qoder/b"}).(map[string]any)

	var baseRow, instanceRow Row
	for _, row := range s.agents() {
		switch row["agent_type"] {
		case wireAgent(base):
			baseRow = row
		case instance["agent_type"]:
			instanceRow = row
		}
	}
	if baseRow == nil || baseRow["instance"] != false {
		t.Fatalf("independent account agent should appear as a base agent: %v", baseRow)
	}
	if instanceRow == nil || instanceRow["instance"] != true {
		t.Fatalf("named account model should remain a harness instance: %v", instanceRow)
	}

	catalog, err := s.connectionCatalog()
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, row := range catalog.([]Row) {
		if row["agent_type"] == "account_qoder" && row["tool"] == "account:qoder" {
			found = true
		}
	}
	if !found {
		t.Fatal("independent Qoder agent is missing from the connection catalog")
	}
}
