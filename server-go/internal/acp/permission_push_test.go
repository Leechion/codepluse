package acp

import (
	"testing"

	"harnesshub/internal/hub"
)

func TestRespondPermissionNotifiesHostBeforeReply(t *testing.T) {
	r := NewRuntime(hub.NewHub(), ":memory:", 0)
	rec := &SessionRecord{ID: "session_x", State: "requires_action"}
	card := &PermissionCard{ID: "p1", State: "pending", Options: []map[string]any{
		{"optionId": "allow_once", "kind": "allow_once"},
		{"optionId": "reject_once", "kind": "reject_once"},
	}}
	reply := make(chan map[string]any, 1)
	r.approvals["p1"] = &approval{card: card, rec: rec, reply: reply}
	gotReplyBeforeNotify := false
	r.OnPermission = func(_ string, c *PermissionCard, resolved bool, option string) {
		select {
		case <-reply:
			gotReplyBeforeNotify = true
		default:
		}
		if !resolved || option != "allow_once" || c.State != "answered" {
			t.Fatalf("unexpected notification: resolved=%v option=%q state=%q", resolved, option, c.State)
		}
	}

	choice := "allow_once"
	if err := r.RespondPermission("p1", &choice); err != nil {
		t.Fatal(err)
	}
	if gotReplyBeforeNotify {
		t.Fatal("agent was released before the host learned the card resolved")
	}
	select {
	case outcome := <-reply:
		if outcome["outcome"].(map[string]any)["optionId"] != "allow_once" {
			t.Fatalf("unexpected outcome: %v", outcome)
		}
	default:
		t.Fatal("agent reply was not delivered")
	}
	if err := r.RespondPermission("p1", &choice); err == nil {
		t.Fatal("a resolved card must not be answerable twice")
	}
}

func TestCancelPermissionsNotifiesEachCard(t *testing.T) {
	r := NewRuntime(hub.NewHub(), ":memory:", 0)
	rec := &SessionRecord{ID: "session_y", State: "requires_action"}
	for _, id := range []string{"p1", "p2"} {
		card := &PermissionCard{ID: id, State: "pending", Options: []map[string]any{{"optionId": "allow_once"}}}
		r.approvals[id] = &approval{card: card, rec: rec, reply: make(chan map[string]any, 1)}
	}
	resolved := 0
	r.OnPermission = func(_ string, c *PermissionCard, isResolved bool, option string) {
		if !isResolved || option != "" || c.State != "cancelled" {
			t.Fatalf("unexpected cancellation notification: %+v", c)
		}
		resolved++
	}
	r.cancelPermissions(rec.ID)
	if resolved != 2 {
		t.Fatalf("resolved %d cards, want 2", resolved)
	}
}
