package codeg

import (
	"fmt"
	"testing"
)

func TestDeleteConversationRemovesTreeAndPersistedTabs(t *testing.T) {
	s := testServer(t)
	st := s.store
	st.mu.Lock()
	folder := st.openFolderLocked(s.Root, "regular")
	f := number(folder["id"])
	root := st.createConversationLocked(f, "codex", "root", 0)
	r := number(root["id"])
	child := st.createConversationLocked(f, "codex", "child", r)
	c := number(child["id"])
	grand := st.createConversationLocked(f, "codex", "grandchild", c)
	g := number(grand["id"])
	other := st.createConversationLocked(f, "codex", "unrelated", 0)
	o := number(other["id"])
	for _, id := range []int{r, c, g, o} {
		st.appendTurnLocked(id, "user", "history")
		st.Tabs = append(st.Tabs, Row{"id": id, "conversation_id": id})
	}
	if err := st.saveLocked(); err != nil {
		t.Fatal(err)
	}
	st.mu.Unlock()
	s.mu.Lock()
	s.tasks["child-task"] = &Delegation{ID: "child-task", Status: "interrupted", ParentConversationID: r, ConversationID: c}
	s.connections["child-connection"] = &Connection{ID: "child-connection", ConversationID: c}
	s.callers["child-connection"] = &Caller{ConnectionID: "child-connection"}
	s.mu.Unlock()
	command(t, s, "delete_conversation", Row{"conversationId": r})
	if len(st.Conversations) != 1 || number(st.Conversations[0]["id"]) != o {
		t.Fatal("deleted tree remained or unrelated row removed")
	}
	if len(st.Tabs) != 1 || number(st.Tabs[0]["conversation_id"]) != o {
		t.Fatal("stale persisted tabs")
	}
	if len(st.Turns) != 1 || len(st.Turns[fmt.Sprint(o)]) != 1 {
		t.Fatal("history cleanup failed")
	}
	if len(s.tasks) != 0 || len(s.callers) != 0 || len(s.connections) != 0 {
		t.Fatal("deleted task can still be resumed")
	}
	loaded, err := newStore(st.file)
	if err != nil || len(loaded.Conversations) != 1 {
		t.Fatalf("deletion not persisted: %v", err)
	}
	command(t, s, "delete_conversation", Row{"conversationId": r})
}

func TestDeleteChildUpdatesParentCount(t *testing.T) {
	s := testServer(t)
	st := s.store
	st.mu.Lock()
	f := st.openFolderLocked(s.Root, "regular")
	p := st.createConversationLocked(number(f["id"]), "codex", "parent", 0)
	child := st.createConversationLocked(number(f["id"]), "codex", "child", number(p["id"]))
	id := number(child["id"])
	st.mu.Unlock()
	command(t, s, "delete_conversation", Row{"conversationId": id})
	if len(st.Conversations) != 1 || number(p["child_count"]) != 0 {
		t.Fatal("parent child counter not updated")
	}
}
