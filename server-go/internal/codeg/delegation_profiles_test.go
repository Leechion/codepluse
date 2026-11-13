package codeg

// Adding a harness in Settings → Agents has to reach the collaboration side:
// the provider/model profiles below only describe the three tools whose spawn
// env the app rewrites, so a custom ACP agent would otherwise wait forever for
// a configuration that can never be created, and stay invisible to
// list_agents / delegate_to_agent.
import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

func customWireRows(rows []Row) []string {
	out := []string{}
	for _, row := range rows {
		out = append(out, text(row["agent_type"]))
	}
	return out
}

// Registers a custom ACP harness whose install cache is already warm, which is
// what makes it report as available without touching the network.
func registerAvailableCustom(t *testing.T, s *Server, id string) {
	t.Helper()
	node, err := exec.LookPath("node")
	if err != nil {
		t.Skip("node required")
	}
	d := customAgentDef{RegistryID: id, Name: "Fixture " + id, Kind: "npx",
		Spec:        customAgentSpec{Npx: &customPackageSpec{Package: "example-agent@1"}},
		Version:     "1",
		SupportsMCP: true}
	data, _ := json.Marshal(d)
	params := Row{}
	if err = json.Unmarshal(data, &params); err != nil {
		t.Fatal(err)
	}
	if err = s.saveCustomAgent(params); err != nil {
		t.Fatal(err)
	}
	dir := s.customCacheDir(d)
	if err = os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	entry := filepath.Join(dir, "entry.cjs")
	if err = os.WriteFile(entry, []byte("// fixture"), 0o600); err != nil {
		t.Fatal(err)
	}
	cache, _ := json.Marshal(customLaunch{Command: node, Args: []string{entry}, Version: "1.1"})
	if err = os.WriteFile(filepath.Join(dir, "launch.json"), cache, 0o600); err != nil {
		t.Fatal(err)
	}
	for _, row := range s.agents() {
		if text(row["agent_type"]) == "custom:"+id && row["available"] == true {
			return
		}
	}
	t.Fatalf("custom harness %s not reported available", id)
}

func TestDelegationTargetsFollowTheHarnessRegistry(t *testing.T) {
	s := testServer(t)
	wire := "custom:delegation-fixture"

	// No profiles yet: the whole registry is offered, the new harness included.
	registerAvailableCustom(t, s, "delegation-fixture")
	if got, err := s.delegationProfile(wire, ""); err != nil || got != wire {
		t.Fatalf("plain registry does not offer the new harness: %v %v", got, err)
	}

	// Once provider profiles exist, the pinnable harnesses drop out of the
	// list (the profiles carry the precise provider/model) — but a harness
	// that can never have a profile must stay.
	pid := profileProvider(t, s, "claude", "https://provider.test")
	sub := command(t, s, "codepluse_instance_create", Row{"name": "worker", "realTool": "claude", "providerId": pid, "defaultModel": "sub-model"}).(map[string]any)
	rows := s.delegationAgents()
	types := customWireRows(rows)
	hasProfile, hasCustom, hasBareHarness := false, false, false
	for _, row := range rows {
		switch text(row["agent_type"]) {
		case text(sub["agent_type"]):
			hasProfile = true
		case wire:
			hasCustom = true
		case "claude_code", "codex":
			hasBareHarness = true
		}
	}
	if !hasProfile || !hasCustom {
		t.Fatalf("profile set %v lost the un-pinnable harness %s", types, wire)
	}
	if hasBareHarness {
		t.Fatalf("subscription harnesses offered beside the profiles: %v", types)
	}
	if got, err := s.delegationProfile(wire, ""); err != nil || got != wire {
		t.Fatalf("profile mode refuses the new harness: %v %v", got, err)
	}
}

func TestDelegationTargetsKeepCustomHarnessesInStrictMainAPIProfile(t *testing.T) {
	s := testServer(t)
	registerAvailableCustom(t, s, "strict-fixture")
	wire := "custom:strict-fixture"
	// The main bot pinned to an API provider withholds the subscription
	// harnesses — it always did — but a custom harness is not a subscription,
	// and there is no profile the user could create to unlock it.
	command(t, s, "update_main_bot_settings", Row{"settings": Row{"agent_type": "claude_code", "provider_id": profileProvider(t, s, "claude", "https://provider.test"), "model": "main-a"}})
	types := customWireRows(s.delegationAgents())
	if len(types) != 1 || types[0] != wire {
		t.Fatalf("strict main profile hid the harness the user just added: %v", types)
	}
	if got, err := s.delegationProfile(wire, ""); err != nil || got != wire {
		t.Fatalf("delegate_to_agent rejects the new harness: %v %v", got, err)
	}
}
