package codeg

import (
	"context"
	"encoding/json"
	"os"
	"strings"
	"testing"
	"time"
)

// envelopeEvent pulls the next acp://event envelope off a global subscriber
// channel, skipping frames from other channels.
func envelopeEvent(t *testing.T, ch chan Row) Row {
	t.Helper()
	deadline := time.After(30 * time.Second)
	for {
		select {
		case frame := <-ch:
			if frame["channel"] != "acp://event" {
				continue
			}
			if e, _ := frame["payload"].(Row); e != nil {
				return e
			}
		case <-deadline:
			t.Fatal("timed out waiting for an acp event")
			return nil
		}
	}
}

func TestTaskSplitFanOut(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	events := make(chan Row, 256)
	s.mu.Lock()
	s.subscribers[events] = true
	s.mu.Unlock()
	defer func() {
		s.mu.Lock()
		delete(s.subscribers, events)
		s.mu.Unlock()
	}()

	s.mu.Lock()
	caller := s.callers[cid]
	s.mu.Unlock()
	if caller == nil {
		t.Fatal("no companion caller registered for the main session")
	}

	result, err := s.tool(context.Background(), caller, "propose_task_split", Row{"tasks": []any{
		// A read-only survey: auto-assign should move it off the proposer.
		Row{"title": "盘点依赖", "prompt": "梳理仓库里所有 npm 依赖的版本与用途，输出清单。不要修改任何文件。"},
		// A write task: stays on the proposing agent.
		Row{"title": "补充测试", "prompt": "为 internal/codeg/split.go 补充单元测试并修复发现的问题。"},
	}})
	if err != nil {
		t.Fatal(err)
	}
	proposed, _ := result.(Row)
	if text(proposed["status"]) != "proposed" {
		t.Fatalf("propose result: %v", result)
	}
	splitID := text(proposed["split_id"])
	if splitID == "" {
		t.Fatal("no split_id")
	}

	// The card goes up on the parent's stream.
	var card Row
	for card == nil {
		e := envelopeEvent(t, events)
		switch text(e["type"]) {
		case "task_split_request":
			card = e
		case "turn_complete":
			t.Fatal("turn ended without a split card")
		}
	}
	// clone() round-trips through JSON, which turns Row back into a plain
	// map and []Row into []any; every nested type assertion below matches
	// what actually arrives, not what the server holds. Row is an alias of
	// map[string]any, so .(Row) and .(map[string]any) are the same test.
	split, _ := card["split"].(Row)
	if text(split["split_id"]) != splitID {
		t.Fatalf("card split id: %v", card)
	}
	tasks, _ := split["tasks"].([]any)
	if len(tasks) != 2 {
		t.Fatalf("card tasks: %v", split["tasks"])
	}
	first, _ := tasks[0].(Row)
	second, _ := tasks[1].(Row)
	if first == nil || second == nil {
		t.Fatalf("card task rows: %v", tasks)
	}
	// Read-only task spread away from the proposer; write task stayed.
	if text(first["agent_type"]) == "codex" {
		t.Fatalf("read-only task should spread to another harness: %v", first)
	}
	if first["auto"] != true || text(second["agent_type"]) != "codex" {
		t.Fatalf("auto-assignment: %v %v", first, second)
	}
	if text(first["model"]) != "" {
		t.Fatalf("target without a pinned model should propose no model: %v", first)
	}

	// A second proposal while the first waits is refused: one card per
	// connection.
	if _, err = s.tool(context.Background(), caller, "propose_task_split", Row{"tasks": []any{Row{"prompt": "第二个提案"}}}); err == nil {
		t.Fatal("second split proposed while first pending")
	}

	// The snapshot carries the pending card, so a reloading client sees it.
	snap := s.snapshot(cid)
	if pending, _ := snap["pending_split"].(Row); text(pending["split_id"]) != splitID {
		t.Fatalf("snapshot pending_split: %v", snap["pending_split"])
	}

	// The user edits the plan on the card: drop the second task, retarget the
	// first, and approve.
	// The edited plan must use a model the protocol fixture knows (it rejects
	// unknown ids at session setup), standing in for a real pinned model.
	out := command(t, s, "acp_respond_task_split", Row{"connectionId": cid, "splitId": splitID, "action": "approve", "tasks": []any{
		Row{"title": "盘点依赖", "prompt": "梳理仓库依赖并输出清单。", "agent_type": "claude_code", "model": "fixture-v2"},
	}})
	approved, _ := out.(Row)
	ids, _ := approved["task_ids"].([]any)
	if text(approved["action"]) != "approve" || len(ids) != 1 {
		t.Fatalf("approve response: %v", out)
	}

	// The card resolves on the stream, then the single child runs to
	// completion through the existing delegation events.
	sawResolved, started, done := false, 0, 0
	childConn := ""
	for started < 1 || done < 1 {
		e := envelopeEvent(t, events)
		switch text(e["type"]) {
		case "task_split_resolved":
			if text(e["split_id"]) == splitID && text(e["action"]) == "approve" {
				sawResolved = true
			}
		case "delegation_started":
			if text(e["split_id"]) == splitID {
				started++
				childConn = text(e["child_connection_id"])
				if text(e["agent_type"]) != "claude_code" {
					t.Fatalf("delegated to %v, expected retargeted claude_code", e["agent_type"])
				}
			}
		case "delegation_completed":
			if text(e["split_id"]) == splitID {
				if result, _ := e["result"].(Row); text(result["kind"]) != "ok" {
					t.Fatalf("child failed: %v", e)
				}
				done++
			}
		}
	}
	if !sawResolved {
		t.Fatal("approve never resolved the card on the stream")
	}
	if childConn == "" {
		t.Fatal("split child has no connection")
	}

	// A split child stays inside the parent's depth budget: the card runs at
	// the root, so its worker sits one level below the main bot.
	s.mu.Lock()
	if child := s.connections[childConn]; child == nil || child.Depth != 1 {
		t.Fatalf("split child depth: %#v", child)
	}

	// The child ran the edited prompt, on the edited target, with the edited
	// model, and its delegation record carries the split id.
	d := s.tasks[text(ids[0])]
	sp := s.splits[splitID]
	c := s.connections[cid]
	s.mu.Unlock()
	if sp != nil {
		t.Fatal("split still registered after approve")
	}
	if d == nil || d.SplitID != splitID {
		t.Fatalf("delegation record: %#v", d)
	}
	if !strings.Contains(d.Task, "梳理仓库依赖并输出清单") || d.AgentType != "claude_code" || d.Model != "fixture-v2" {
		t.Fatalf("edited plan not applied: %#v", d)
	}
	if c.PendingSplit != nil {
		t.Fatal("pending slot still set after approve")
	}

	// Answering the same split again is refused, and the snapshot is clean.
	if _, err = s.command("acp_respond_task_split", Row{"connectionId": cid, "splitId": splitID, "action": "reject"}); err == nil {
		t.Fatal("second answer accepted")
	}
	if again := s.snapshot(cid); again["pending_split"] != nil {
		t.Fatalf("snapshot still carries the split: %v", again["pending_split"])
	}
}

func TestTaskSplitReject(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	s.mu.Lock()
	caller := s.callers[cid]
	s.mu.Unlock()
	result, err := s.tool(context.Background(), caller, "propose_task_split", Row{"tasks": []any{Row{"prompt": "只读扫描任务"}}})
	if err != nil {
		t.Fatal(err)
	}
	splitID := text(result.(Row)["split_id"])

	command(t, s, "acp_respond_task_split", Row{"connectionId": cid, "splitId": splitID, "action": "reject"})
	s.mu.Lock()
	sp, pending := s.splits[splitID], s.connections[cid].PendingSplit
	tasks := len(s.tasks)
	s.mu.Unlock()
	if sp != nil || pending != nil {
		t.Fatalf("reject left state behind: %v %v", sp, pending)
	}
	if tasks != 0 {
		t.Fatalf("reject launched %d delegations", tasks)
	}
}

func TestTaskSplitExpiryOnDisconnect(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	s.mu.Lock()
	caller := s.callers[cid]
	s.mu.Unlock()
	result, err := s.tool(context.Background(), caller, "propose_task_split", Row{"tasks": []any{Row{"prompt": "只读扫描任务"}}})
	if err != nil {
		t.Fatal(err)
	}
	splitID := text(result.(Row)["split_id"])

	if err := s.disconnect(cid); err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	sp := s.splits[splitID]
	s.mu.Unlock()
	if sp != nil {
		t.Fatal("disconnect left the split pending")
	}
	// A card answered after its session is gone is refused, not resurrected.
	if _, err := s.command("acp_respond_task_split", Row{"connectionId": cid, "splitId": splitID, "action": "approve"}); err == nil {
		t.Fatal("answered a split whose parent is gone")
	}
}

// TestTaskSplitPersistsAcrossRestart checks the card survives a server bounce
// in the exact state the user left it: un-answered.
func TestTaskSplitPersistsAcrossRestart(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	s.mu.Lock()
	caller := s.callers[cid]
	s.mu.Unlock()
	result, err := s.tool(context.Background(), caller, "propose_task_split", Row{"tasks": []any{
		Row{"title": "审阅改动", "prompt": "逐文件审阅工作区改动并总结风险。"},
	}})
	if err != nil {
		t.Fatal(err)
	}
	splitID := text(result.(Row)["split_id"])

	data, err := os.ReadFile(s.splitFile())
	if err != nil {
		t.Fatal(err)
	}
	var rows []Row
	if err := json.Unmarshal(data, &rows); err != nil || len(rows) != 1 {
		t.Fatalf("persisted splits: %s %v", data, err)
	}
	if text(rows[0]["split_id"]) != splitID {
		t.Fatalf("persisted split id: %v", rows[0])
	}
	// The persisted task carries the server-computed target.
	tasks, _ := rows[0]["tasks"].([]any)
	if len(tasks) != 1 {
		t.Fatalf("persisted tasks: %v", rows[0]["tasks"])
	}

	// A second server over the same registry picks the card back up.
	s2 := testServer(t)
	s2.Root = s.Root
	s2.loadSplits()
	s2.mu.Lock()
	sp := s2.splits[splitID]
	s2.mu.Unlock()
	if sp == nil || len(sp.Tasks) != 1 {
		t.Fatalf("split did not survive reload: %#v", sp)
	}
	if text(sp.Tasks[0]["agent_type"]) == "" {
		t.Fatalf("reloaded task lost its target: %v", sp.Tasks[0])
	}
}

func TestTaskSplitValidation(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	s.mu.Lock()
	caller := s.callers[cid]
	s.mu.Unlock()

	if _, err := s.tool(context.Background(), caller, "propose_task_split", Row{"tasks": []any{}}); err == nil {
		t.Fatal("empty tasks accepted")
	}
	if _, err := s.tool(context.Background(), caller, "propose_task_split", Row{"tasks": []any{Row{"prompt": ""}}}); err == nil {
		t.Fatal("empty prompt accepted")
	}
	if _, err := s.tool(context.Background(), caller, "propose_task_split", Row{"tasks": []any{Row{"prompt": strings.Repeat("x", 120001)}}}); err == nil {
		t.Fatal("oversized prompt accepted")
	}
	// Unknown targets do not fail the proposal — they collapse onto the
	// parent agent, so the card still shows a runnable plan.
	result, err := s.tool(context.Background(), caller, "propose_task_split", Row{"tasks": []any{Row{"prompt": "审阅改动", "agent_type": "inst_nope9"}}})
	if err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	sp := s.splits[text(result.(Row)["split_id"])]
	s.mu.Unlock()
	if text(sp.Tasks[0]["agent_type"]) != "codex" {
		t.Fatalf("unknown target should collapse to the parent: %v", sp.Tasks[0])
	}
	// And a refused target leaves the card pending — the user can still answer
	// the one that was accepted.
	if _, err = s.command("acp_respond_task_split", Row{"connectionId": cid, "splitId": text(result.(Row)["split_id"]), "action": "approve"}); err != nil {
		t.Fatalf("approve of the valid proposal failed: %v", err)
	}
}
