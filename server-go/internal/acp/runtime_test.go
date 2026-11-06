package acp

import (
	"encoding/json"
	"fmt"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	"harnesshub/internal/hub"
)

func fixtureRuntime(t *testing.T) (*Runtime, *hub.Agent, Options) {
	t.Helper()
	node, err := exec.LookPath("node")
	if err != nil {
		t.Skip("Node is needed for the ACP protocol fixture")
	}
	root, err := filepath.Abs("../../..")
	if err != nil {
		t.Fatal(err)
	}
	r := NewRuntime(hub.NewHub(), ":memory:", 10000)
	r.LaunchFn = func(a *hub.Agent, permission, cwd string) (*Launch, error) {
		return &Launch{Command: node, Args: []string{filepath.Join(root, "fixtures", "acp-agent.cjs")}}, nil
	}
	t.Cleanup(r.Shutdown)
	return r, &hub.Agent{ID: "agent", RealTool: "codex"}, Options{Workdir: root, Permission: "readonly", SessionKey: "test"}
}

func TestPromptPreservesWireOrderAcrossTurns(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	for i := 0; i < 40; i++ {
		text := fmt.Sprintf("turn-%02d:abcdefghijklmnopqrstuv", i)
		res := r.Prompt(agent, text, options)
		if !res.OK || res.Output != text {
			t.Fatalf("turn %d: got %#v", i, res)
		}
	}
	res := r.Prompt(agent, "submit", options)
	if !res.OK || res.Structured["output"] != "tool says done" {
		t.Fatalf("lost final tool submission: %#v", res)
	}
	tools := r.Records("")[0].Turns[len(r.Records("")[0].Turns)-1].Tools
	if len(tools) != 1 || tools[0]["status"] != "completed" {
		t.Fatalf("tool updates out of order: %v", tools)
	}
}

func TestResumeOnlyHarnessWithoutModes(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	base := r.LaunchFn
	r.LaunchFn = func(a *hub.Agent, permission, cwd string) (*Launch, error) {
		launch, err := base(a, permission, cwd)
		if err == nil {
			launch.Args = append(launch.Args, "--resume-only")
		}
		return launch, err
	}
	agent.RealTool = "dsh"
	options.Permission = "workspace"
	if result := r.Prompt(agent, "first", options); !result.OK {
		t.Fatal(result)
	}
	r.Shutdown()
	if result := r.Prompt(agent, "resumed", options); !result.OK || result.Output != "resumed" {
		t.Fatal(result)
	}
}

func TestRefusalReportsNativeAuthenticationError(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	result := r.Prompt(agent, "refusal", options)
	if result.OK || result.Error != "Please sign in to WorkBuddy" {
		t.Fatal(result)
	}
}

func readSessions(t *testing.T, r *Runtime) []map[string]any {
	t.Helper()
	var records []map[string]any
	if err := json.Unmarshal(r.ListJSON(""), &records); err != nil {
		t.Fatal(err)
	}
	return records
}

func TestSessionViewNormalizesArraysAndShowsActiveTurn(t *testing.T) {
	r := NewRuntime(hub.NewHub(), ":memory:", 10000)
	if string(r.ListJSON("")) != "[]" {
		t.Fatalf("empty session list: %s", r.ListJSON(""))
	}
	r.records = []*SessionRecord{{ID: "first", State: "running", active: &Turn{Output: "stream", ReceivedChars: 6}}}
	view := readSessions(t, r)[0]
	for _, key := range []string{"turns", "permissions"} {
		if values, ok := view[key].([]any); !ok || len(values) != 0 {
			t.Fatalf("%s must be []: %v", key, view)
		}
	}
	active, ok := view["activeTurn"].(map[string]any)
	if !ok || active["output"] != "stream" || active["receivedChars"] != float64(6) {
		t.Fatalf("missing active turn: %v", view)
	}
	if _, ok := active["tools"].([]any); !ok {
		t.Fatalf("tools must be []: %v", active)
	}
	r.records[0].active = nil
	r.records[0].Turns = []*Turn{{Status: "completed"}}
	view = readSessions(t, r)[0]
	if view["activeTurn"] != nil {
		t.Fatal("finished turn still active")
	}
	if _, ok := view["turns"].([]any)[0].(map[string]any)["tools"].([]any); !ok {
		t.Fatal("finished tools must be []")
	}
}

func TestActiveTurnCanBeObservedAndCancelled(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	done := make(chan Result, 1)
	go func() { done <- r.Prompt(agent, "wait", options) }()
	deadline := time.After(3 * time.Second)
	for {
		select {
		case <-deadline:
			t.Fatal("active session did not appear")
		case <-time.After(5 * time.Millisecond):
			views := readSessions(t, r)
			if len(views) == 0 || views[0]["activeTurn"] == nil {
				continue
			}
			id := views[0]["id"].(string)
			if err := r.Cancel(id); err != nil {
				t.Fatal(err)
			}
			select {
			case <-done:
			case <-time.After(3 * time.Second):
				t.Fatal("cancel stalled")
			}
			if readSessions(t, r)[0]["activeTurn"] != nil {
				t.Fatal("cancelled turn still active")
			}
			return
		}
	}
}

func TestPersistenceRestoresLatestHundredSessions(t *testing.T) {
	file := filepath.Join(t.TempDir(), "nested", "sessions.json")
	r := NewRuntime(hub.NewHub(), file, 10000)
	for i := 0; i < 105; i++ {
		r.records = append(r.records, &SessionRecord{ID: fmt.Sprintf("session-%03d", i), NativeSessionID: fmt.Sprintf("native-%03d", i)})
	}
	r.save()
	restored := NewRuntime(hub.NewHub(), file, 10000).Records("")
	if len(restored) != 100 || restored[0].ID != "session-005" || restored[99].NativeSessionID != "native-104" {
		t.Fatalf("latest sessions were not retained: %v", restored)
	}
}

func TestPermissionRequestDoesNotBlockStreamReader(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	options.Permission = "workspace"
	done := make(chan Result, 1)
	go func() { done <- r.Prompt(agent, "permission", options) }()
	deadline := time.After(3 * time.Second)
	for {
		select {
		case <-deadline:
			t.Fatal("permission request stalled")
		case <-time.After(5 * time.Millisecond):
			views := readSessions(t, r)
			if len(views) == 0 {
				continue
			}
			cards := views[0]["permissions"].([]any)
			if len(cards) == 0 {
				continue
			}
			card := cards[0].(map[string]any)
			choice := "yes"
			if err := r.RespondPermission(card["id"].(string), &choice); err != nil {
				t.Fatal(err)
			}
			select {
			case res := <-done:
				if !res.OK || res.Output != choice {
					t.Fatalf("permission reply lost: %#v", res)
				}
			case <-time.After(3 * time.Second):
				t.Fatal("permission response stalled")
			}
			return
		}
	}
}

func TestOnlyOwnedInternalToolsMayBeAllowedInReadOnlySessions(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	r.AllowInternalTool = func(id string, tool map[string]any) bool { return tool["title"] == "mcp__codeg-mcp__list_agents" }
	allowed := r.Prompt(agent, "permission-internal", options)
	if !allowed.OK || allowed.Output != "yes" {
		t.Fatalf("internal tool blocked: %#v", allowed)
	}
	denied := r.Prompt(agent, "permission", options)
	if !denied.OK || denied.Output != "cancelled" {
		t.Fatalf("native edit bypassed read-only: %#v", denied)
	}
}
