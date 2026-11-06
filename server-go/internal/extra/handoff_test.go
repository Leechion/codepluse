package extra

import (
	"fmt"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
	"harnesshub/internal/orch"
)

func TestFailedHandoffKeepsSourceUsableAndCanRetry(t *testing.T) {
	node, err := exec.LookPath("node")
	if err != nil {
		t.Skip("Node is needed for the ACP protocol fixture")
	}
	root, err := filepath.Abs("../../..")
	if err != nil {
		t.Fatal(err)
	}
	h := hub.NewHub()
	from := h.CreateAgent(&hub.Agent{ID: "source", Real: true, RealTool: "codex"})
	to := h.CreateAgent(&hub.Agent{ID: "target", Real: true, RealTool: "codex"})
	rt := acp.NewRuntime(h, ":memory:", 10000)
	t.Cleanup(rt.Shutdown)
	failTarget := true
	rt.LaunchFn = func(agent *hub.Agent, permission, cwd string) (*acp.Launch, error) {
		if agent.ID == to.ID && failTarget {
			return nil, fmt.Errorf("adapter unavailable")
		}
		return &acp.Launch{Command: node, Args: []string{filepath.Join(root, "fixtures", "acp-agent.cjs")}}, nil
	}
	o := &orch.Orchestrator{Hub: h, Invoke: func(a *hub.Agent, p string, opts acp.Options) acp.Result {
		return acp.Result{OK: true, Output: `{"summary":"token42"}`}
	}}
	options := acp.Options{Workdir: root, SessionKey: "source", Permission: "readonly"}
	if res := rt.Prompt(from, "remember token42", options); !res.OK {
		t.Fatal(res)
	}
	source := rt.Records("")[0]
	if _, err := HandoffSession(rt, o, h, nil, source.ID, to.ID); err == nil || !strings.Contains(err.Error(), "adapter unavailable") {
		t.Fatalf("expected target error, got %v", err)
	}
	if source.HandedOff || source.HandedOffTo != "" {
		t.Fatal("failed target locked the source")
	}
	if res := rt.Prompt(from, "recall", options); !res.OK || res.Output != "token42" {
		t.Fatalf("source unusable: %#v", res)
	}
	failTarget = false
	if _, err := HandoffSession(rt, o, h, nil, source.ID, to.ID); err != nil {
		t.Fatal(err)
	}
	if !source.HandedOff || source.HandedOffTo != to.ID {
		t.Fatal("successful retry was not recorded")
	}
}
