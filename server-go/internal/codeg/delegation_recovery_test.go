package codeg

import (
	"context"
	"harnesshub/internal/acp"
	"path/filepath"
	"testing"
	"time"
)

func waitDelegation(t *testing.T, s *Server, tid string) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		s.mu.Lock()
		d := s.tasks[tid]
		done := d != nil && !d.Executing && d.Status != "running"
		status := ""
		message := ""
		if d != nil {
			status = d.Status
			message = d.Error
		}
		s.mu.Unlock()
		if done {
			if status != "completed" {
				t.Fatalf("delegation %s: %s", status, message)
			}
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("delegation stalled")
}
func TestInterruptedDelegationRestartsInOriginalConversation(t *testing.T) {
	s := testServer(t)
	s.Runtime.StateFile = filepath.Join(s.Root, ".data", "codeg-sessions.json")
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	s.mu.Lock()
	parent := s.connections[cid]
	s.mu.Unlock()
	native := text(s.runtimeView(parent.RuntimeID)["nativeSessionId"])
	st := s.store
	st.mu.Lock()
	folder := st.openFolderLocked(s.Root, "regular")
	row := st.createConversationLocked(number(folder["id"]), "codex", "parent", 0)
	parentConv := number(row["id"])
	row["external_id"] = native
	if err := st.saveLocked(); err != nil {
		t.Fatal(err)
	}
	st.mu.Unlock()
	s.bindConversation(parent, parentConv)
	s.mu.Lock()
	caller := *s.callers[cid]
	s.mu.Unlock()
	result, err := s.tool(context.Background(), &caller, "delegate_to_agent", Row{"agent_type": "codex", "task": "original child work"})
	if err != nil {
		t.Fatal(err)
	}
	tid := text(result.(Row)["task_id"])
	waitDelegation(t, s, tid)
	s.mu.Lock()
	child := s.tasks[tid]
	conv := child.ConversationID
	originalNative := child.NativeSessionID
	child.Status = "canceled"
	s.mu.Unlock()
	// Repeated delegate calls after cancellation must reuse the same task, even with a restated instruction.
	result, err = s.tool(context.Background(), &caller, "delegate_to_agent", Row{"agent_type": "codex", "task": "continue original child work"})
	if err != nil {
		t.Fatal(err)
	}
	if text(result.(Row)["task_id"]) != tid {
		t.Fatal("duplicate task created")
	}
	waitDelegation(t, s, tid)
	if child.ConversationID != conv || child.NativeSessionID != originalNative {
		t.Fatal("original child identity lost")
	}
	// Persist a running task to simulate interruption at backend shutdown.
	s.mu.Lock()
	child.Status = "running"
	if err = s.saveDelegationsLocked(); err != nil {
		t.Fatal(err)
	}
	s.mu.Unlock()
	s.Runtime.SaveRecords()
	s.Runtime.Shutdown()
	rt := acp.NewRuntime(s.Hub, s.Runtime.StateFile, 10000)
	rt.LaunchFn = s.Runtime.LaunchFn
	t.Cleanup(rt.Shutdown)
	restored, err := New(s.Root, "", rt, s.Hub, s.Resolve)
	if err != nil {
		t.Fatal(err)
	}
	restoredCID := command(t, restored, "acp_connect", Row{"agentType": "codex", "sessionId": native, "workingDir": s.Root}).(string)
	restored.mu.Lock()
	restoredCaller := *restored.callers[restoredCID]
	restored.mu.Unlock()
	result, err = restored.tool(context.Background(), &restoredCaller, "resume_delegation", Row{"task_id": tid})
	if err != nil {
		t.Fatal(err)
	}
	waitDelegation(t, restored, tid)
	d := restored.tasks[tid]
	if d.ConversationID != conv || d.NativeSessionID != originalNative {
		t.Fatal("restart opened another child session")
	}
	if len(restored.store.Conversations) != 2 || len(restored.tasks) != 1 {
		t.Fatal("restart duplicated conversations or tasks")
	}
}
