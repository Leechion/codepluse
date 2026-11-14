package codeg

import (
	"testing"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
)

func TestForkConversationPreservesOriginalAndContinuesFromSelectedTurn(t *testing.T) {
	s := testServer(t)
	launch := s.Runtime.LaunchFn
	s.Runtime.LaunchFn = func(agent *hub.Agent, permission, cwd string) (*acp.Launch, error) {
		result, err := launch(agent, permission, cwd)
		if err == nil {
			result.Args = append(result.Args, "--fork")
		}
		return result, err
	}
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	s.mu.Lock()
	originalToken := s.callers[cid].Token
	s.mu.Unlock()
	if snap := s.snapshot(cid); snap["fork_supported"] != true {
		t.Fatalf("missing capability: %v", snap)
	}
	send := func(message string, conversation int) {
		t.Helper()
		if err := s.prompt(Row{"connectionId": cid, "conversationId": conversation, "blocks": []any{Row{"type": "text", "text": message}}}); err != nil {
			t.Fatal(err)
		}
		deadline := time.Now().Add(5 * time.Second)
		for {
			s.mu.Lock()
			busy := s.connections[cid].Busy
			s.mu.Unlock()
			if !busy {
				break
			}
			if time.Now().After(deadline) {
				t.Fatal("prompt did not finish")
			}
			time.Sleep(10 * time.Millisecond)
		}
	}
	send("first", 0)
	s.mu.Lock()
	conversation := s.connections[cid].ConversationID
	s.mu.Unlock()
	send("second", conversation)
	st := s.store
	st.mu.Lock()
	sourceTurns := st.turnsLocked(conversation)
	originalNative := text(st.conversationLocked(conversation)["external_id"])
	st.mu.Unlock()
	if len(sourceTurns) != 4 || text(sourceTurns[1]["source_turn_id"]) != "fixture-message-1" {
		t.Fatalf("unexpected source turns: %v", sourceTurns)
	}
	s.mu.Lock()
	s.tasks["running-child"] = &Delegation{Root: s.connections[cid].RootID, Status: "running"}
	s.mu.Unlock()
	if _, err := s.forkConversation(Row{"connectionId": cid, "forkFromTurnId": "fixture-message-1"}); err == nil {
		t.Fatal("fork accepted a running collaboration task")
	}
	s.mu.Lock()
	delete(s.tasks, "running-child")
	s.mu.Unlock()
	// A native agent rejection must not create a visible sibling or change the
	// original conversation, even after the temporary MCP identity was created.
	st.mu.Lock()
	edited := st.turnsLocked(conversation)
	edited[1]["source_turn_id"] = "fixture-message-99"
	_ = st.replaceTurnsLocked(conversation, edited)
	st.mu.Unlock()
	if _, err := s.forkConversation(Row{"connectionId": cid, "forkFromTurnId": "fixture-message-99"}); err == nil {
		t.Fatal("unknown native point was accepted")
	}
	st.mu.Lock()
	if len(st.Conversations) != 1 || text(st.conversationLocked(conversation)["external_id"]) != originalNative {
		t.Fatal("rejected fork changed the conversation")
	}
	_ = st.replaceTurnsLocked(conversation, sourceTurns)
	st.mu.Unlock()
	s.mu.Lock()
	if len(s.callers) != 1 || s.callers[cid].Token != originalToken {
		t.Fatal("rejected fork leaked its temporary caller")
	}
	s.mu.Unlock()
	result, err := s.forkConversation(Row{"connectionId": cid, "conversationId": conversation, "forkFromTurnId": "fixture-message-1"})
	if err != nil {
		t.Fatal(err)
	}
	sibling := number(result["siblingConversationId"])
	forkNative := text(result["forkedSessionId"])
	if sibling == 0 || forkNative == "" || forkNative == originalNative {
		t.Fatalf("bad fork result: %v", result)
	}
	st.mu.Lock()
	forkTurns := st.turnsLocked(conversation)
	siblingTurns := st.turnsLocked(sibling)
	forkRow, siblingRow := clone(st.conversationLocked(conversation)), clone(st.conversationLocked(sibling))
	st.mu.Unlock()
	if len(forkTurns) != 2 || len(siblingTurns) != 4 || text(forkRow["external_id"]) != forkNative || text(siblingRow["external_id"]) != originalNative {
		t.Fatalf("wrong histories: %v %v %v %v", forkTurns, siblingTurns, forkRow, siblingRow)
	}
	restored := reloadStore(t, st.file)
	if len(restored.turnsLocked(conversation)) != 2 || len(restored.turnsLocked(sibling)) != 4 {
		t.Fatalf("fork histories not persisted: %v", restored.turnsLocked(conversation))
	}
	siblingCID := command(t, s, "acp_connect", Row{"agentType": "codex", "sessionId": originalNative}).(string)
	s.mu.Lock()
	forkCaller, siblingCaller := s.callers[cid], s.callers[siblingCID]
	isolated := siblingCID != cid && siblingCaller != nil && forkCaller != nil && siblingCaller.Token == originalToken && forkCaller.Token != originalToken && siblingCaller.RootID != forkCaller.RootID
	s.mu.Unlock()
	if !isolated {
		t.Fatal("fork and original share a collaboration identity")
	}
	if err := s.prompt(Row{"connectionId": siblingCID, "conversationId": sibling, "blocks": []any{Row{"type": "text", "text": "original continues"}}}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		s.mu.Lock()
		busy := s.connections[siblingCID].Busy
		s.mu.Unlock()
		if !busy {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("original prompt did not finish")
		}
		time.Sleep(10 * time.Millisecond)
	}
	send("third", conversation)
	st.mu.Lock()
	after := len(st.turnsLocked(conversation))
	preserved := len(st.turnsLocked(sibling))
	st.mu.Unlock()
	if after != 4 || preserved != 6 {
		t.Fatalf("branch continuation changed original: %d %d", after, preserved)
	}
}
