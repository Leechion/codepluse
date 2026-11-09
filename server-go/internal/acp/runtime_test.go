package acp

import (
	"encoding/json"
	"fmt"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"harnesshub/internal/hub"
)

func TestNativeSteeringInjectsOnlyIntoRunningTurn(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	launch := r.LaunchFn
	r.LaunchFn = func(a *hub.Agent, permission, cwd string) (*Launch, error) {
		result, err := launch(a, permission, cwd)
		if err == nil {
			result.Args = append(result.Args, "--fork", "--steer")
		}
		return result, err
	}
	sid, err := r.Connect(agent, options)
	if err != nil {
		t.Fatal(err)
	}
	if !r.SupportsNativeSteering(sid) {
		t.Fatal("steering capability not detected")
	}
	blocks := []map[string]any{{"type": "text", "text": "follow up"}}
	if err := r.Steer(sid, blocks); err == nil || !strings.Contains(err.Error(), "no active turn") {
		t.Fatalf("idle steer: %v", err)
	}
	done := make(chan Result, 1)
	go func() { done <- r.Prompt(agent, "wait", options) }()
	deadline := time.Now().Add(3 * time.Second)
	for {
		err = r.Steer(sid, blocks)
		if err == nil {
			break
		}
		if !strings.Contains(err.Error(), "no active turn") || time.Now().After(deadline) {
			t.Fatalf("steering failed: %v", err)
		}
		time.Sleep(10 * time.Millisecond)
	}
	select {
	case result := <-done:
		if !result.OK || result.Output != "steered:follow up" {
			t.Fatalf("wrong result: %#v", result)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("steered turn did not complete")
	}
	if err := r.Steer(sid, blocks); err == nil || !strings.Contains(err.Error(), "no active turn") {
		t.Fatalf("idle steer started a detached turn: %v", err)
	}
}

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

func TestForkCapabilityRequiresAnExplicitLiveAgentDeclaration(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	id, err := r.Connect(agent, options)
	if err != nil {
		t.Fatal(err)
	}
	if r.SupportsFork(id) || r.SupportsFork("missing") {
		t.Fatal("unadvertised fork was reported as supported")
	}
	r.mu.Lock()
	d := r.bindings[id]
	r.mu.Unlock()
	d.mu.Lock()
	caps := d.Info["agentCapabilities"].(map[string]any)
	caps["sessionCapabilities"] = map[string]any{"fork": map[string]any{}}
	d.mu.Unlock()
	if !r.SupportsFork(id) {
		t.Fatal("advertised fork capability was not detected")
	}
	if r.SupportsPointFork(id, agent) {
		t.Fatal("an unverified adapter must not expose message-point forks")
	}
	d.mu.Lock()
	d.Info["agentInfo"] = map[string]any{"name": "@agentclientprotocol/codex-acp", "version": "2.5.0"}
	d.mu.Unlock()
	if !r.SupportsPointFork(id, agent) {
		t.Fatal("verified adapter did not expose message-point forks")
	}
	d.mu.Lock()
	caps["sessionCapabilities"] = map[string]any{"fork": nil}
	d.mu.Unlock()
	if r.SupportsFork(id) {
		t.Fatal("null fork capability was reported as supported")
	}
}

func TestForkUsesNativeMessagePointAndKeepsSessionsIndependent(t *testing.T) {
	r, agent, source := fixtureRuntime(t)
	launch := r.LaunchFn
	r.LaunchFn = func(a *hub.Agent, permission, cwd string) (*Launch, error) {
		result, err := launch(a, permission, cwd)
		if err == nil {
			result.Args = append(result.Args, "--fork")
		}
		return result, err
	}
	if got := r.Prompt(agent, "remember original", source); !got.OK {
		t.Fatal(got.Error)
	}
	if got := r.Prompt(agent, "remember later", source); !got.OK {
		t.Fatal(got.Error)
	}
	sourceID := r.Records("")[0].ID
	forkID, err := r.Fork(agent, sourceID, source, "new-branch", "fixture-message-1", nil)
	if err != nil || forkID == sourceID {
		t.Fatalf("fork: %s %v", forkID, err)
	}
	forkOptions := source
	forkOptions.SessionKey = "new-branch"
	if got := r.Prompt(agent, "recall", forkOptions); !got.OK || got.Output != "original" {
		t.Fatalf("fork has wrong history: %+v", got)
	}
	if got := r.Prompt(agent, "recall", source); !got.OK || got.Output != "later" {
		t.Fatalf("source history changed: %+v", got)
	}
	if _, err = r.Fork(agent, sourceID, source, "bad-branch", "missing-native-id", nil); err == nil {
		t.Fatal("unknown fork point should fail rather than silently fork the tail")
	}
}

func TestStreamingRenewsInactivityTimeout(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	options.TimeoutMs = 1000
	res := r.Prompt(agent, "stream-slow", options)
	if !res.OK || res.Output != "....." {
		t.Fatalf("active stream was cancelled: %#v", res)
	}
}

func TestIdlePromptStillTimesOut(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	options.TimeoutMs = 700
	res := r.Prompt(agent, "wait", options)
	if res.OK || res.Error != "ACP 会话超时" {
		t.Fatalf("idle prompt did not time out: %#v", res)
	}
}

func TestFullAccessModeSurvivesDriverRestart(t *testing.T) {
	r, agent, options := fixtureRuntime(t)
	options.Permission = "workspace"
	options.Mode = "agent-full-access"
	id, err := r.Connect(agent, options)
	if err != nil {
		t.Fatal(err)
	}
	if r.Records("")[0].Modes.CurrentModeID != "agent-full-access" {
		t.Fatal("full access was downgraded")
	}
	if err = r.Configure(id, "mode", "read-only"); err != nil {
		t.Fatal(err)
	}
	r.Shutdown()
	if _, err = r.Connect(agent, options); err != nil {
		t.Fatal(err)
	}
	rec := r.Records("")[0]
	if rec.NativeMode != "read-only" || rec.Modes.CurrentModeID != "read-only" {
		t.Fatal("latest user mode was not restored")
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
