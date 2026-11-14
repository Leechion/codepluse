package codeg

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"harnesshub/internal/acp"
)

func newBareServer() *Server {
	return &Server{
		connections: map[string]*Connection{},
		byRuntime:   map[string]*Connection{},
		subscribers: map[chan Row]bool{},
	}
}

func nextACPEvent(t *testing.T, ch chan Row) Row {
	t.Helper()
	select {
	case raw := <-ch:
		if text(raw["channel"]) != "acp://event" {
			t.Fatalf("unexpected channel: %v", raw["channel"])
		}
		payload, _ := raw["payload"].(map[string]any)
		return payload
	default:
		t.Fatal("expected an ACP event, got none")
		return nil
	}
}

func TestReplaySinceReturnsOnlyMissedEvents(t *testing.T) {
	s := newBareServer()
	c := &Connection{ID: "c1", Events: []Row{}}
	s.connections["c1"] = c
	for i := 0; i < 5; i++ {
		s.event("c1", Row{"type": "status_changed", "status": "connected"})
	}

	events, high, ok := s.replaySince("c1", 2)
	if !ok || high != 5 || len(events) != 3 {
		t.Fatalf("replay = %d events, high %d, ok %v; want 3/5/true", len(events), high, ok)
	}
	if eventSeq(events[0]) != 3 {
		t.Fatalf("first replayed seq = %d, want 3", eventSeq(events[0]))
	}

	if events, high, ok = s.replaySince("c1", 5); !ok || high != 5 || len(events) != 0 {
		t.Fatalf("cursor at high water should replay nothing: %d/%d/%v", len(events), high, ok)
	}
	if _, _, ok = s.replaySince("c1", 0); ok {
		t.Fatal("cursor 0 must fall back to a snapshot")
	}
	if _, _, ok = s.replaySince("c1", 99); ok {
		t.Fatal("a cursor ahead of the server must fall back to a snapshot")
	}
	if _, _, ok = s.replaySince("missing", 1); ok {
		t.Fatal("unknown connection must fall back to a snapshot")
	}

	// Rotate the 256-event ring buffer past the cursor: a snapshot is required.
	for i := 0; i < 300; i++ {
		s.event("c1", Row{"type": "status_changed", "status": "connected"})
	}
	if _, _, ok = s.replaySince("c1", 2); ok {
		t.Fatal("rotated buffer must fall back to a snapshot")
	}
	if events, _, ok = s.replaySince("c1", 100); !ok || len(events) == 0 {
		t.Fatal("a cursor still inside the buffer must replay")
	}
}

func TestNativePermissionPushesAndClearsPending(t *testing.T) {
	s := newBareServer()
	ch := make(chan Row, 8)
	s.subscribers[ch] = true
	c := &Connection{ID: "c1", RuntimeID: "r1", Events: []Row{}}
	s.connections["c1"] = c
	s.byRuntime["r1"] = c

	card := &acp.PermissionCard{ID: "p1", Options: []map[string]any{
		{"optionId": "allow_once", "kind": "allow_once"},
	}}
	s.nativePermission("r1", card, false, "")
	ev := nextACPEvent(t, ch)
	if text(ev["type"]) != "permission_request" || text(ev["request_id"]) != "p1" {
		t.Fatalf("unexpected pending event: %v", ev)
	}
	s.mu.Lock()
	pending := c.Pending
	s.mu.Unlock()
	if pending == nil || text(pending["request_id"]) != "p1" {
		t.Fatalf("Pending not set: %v", pending)
	}

	s.nativePermission("r1", card, true, "allow_once")
	ev = nextACPEvent(t, ch)
	if text(ev["type"]) != "permission_resolved" || text(ev["option_id"]) != "allow_once" {
		t.Fatalf("unexpected resolved event: %v", ev)
	}
	s.mu.Lock()
	pending = c.Pending
	s.mu.Unlock()
	if pending != nil {
		t.Fatalf("Pending not cleared: %v", pending)
	}

	// Unknown runtime ids are a no-op (driver died before the event landed).
	s.nativePermission("gone", card, false, "")
}

func TestNativeUpdateUsesRuntimeIndex(t *testing.T) {
	s := newBareServer()
	ch := make(chan Row, 8)
	s.subscribers[ch] = true
	c1 := &Connection{ID: "c1", RuntimeID: "r1", Events: []Row{}}
	c2 := &Connection{ID: "c2", RuntimeID: "r2", Events: []Row{}}
	s.connections["c1"], s.connections["c2"] = c1, c2
	s.byRuntime["r1"], s.byRuntime["r2"] = c1, c2

	s.nativeUpdate("r2", map[string]any{
		"sessionUpdate": "notice",
		"severity":      "info",
		"title":         "t",
		"description":   "d",
	})
	ev := nextACPEvent(t, ch)
	if text(ev["connection_id"]) != "c2" {
		t.Fatalf("event routed to %v, want c2", ev["connection_id"])
	}
}

func TestWebSocketAttachReplaysMissedEvents(t *testing.T) {
	s := newBareServer()
	c := &Connection{ID: "c1", Events: []Row{}}
	s.connections["c1"] = c
	for i := 0; i < 3; i++ {
		s.event("c1", Row{"type": "status_changed", "status": "connected"})
	}

	srv := httptest.NewServer(http.HandlerFunc(s.websocket))
	defer srv.Close()
	conn, _, err := websocket.DefaultDialer.Dial("ws"+strings.TrimPrefix(srv.URL, "http"), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))

	var ready map[string]any
	if err := conn.ReadJSON(&ready); err != nil {
		t.Fatal(err)
	}
	if err := conn.WriteJSON(map[string]any{
		"action": "attach", "subscription_id": "sub1", "connection_id": "c1", "since_seq": 1,
	}); err != nil {
		t.Fatal(err)
	}
	var frame map[string]any
	if err := conn.ReadJSON(&frame); err != nil {
		t.Fatal(err)
	}
	if frame["type"] != "replay" {
		t.Fatalf("frame type = %v, want replay (snapshot would lose the cursor)", frame["type"])
	}
	if number(frame["high_water_seq"]) != 3 {
		t.Fatalf("high_water_seq = %v, want 3", frame["high_water_seq"])
	}
	events, _ := frame["events"].([]any)
	if len(events) != 2 {
		t.Fatalf("replayed %d events, want 2", len(events))
	}
}

func TestWebSocketDoesNotForwardQueuedEventsTwice(t *testing.T) {
	s := newBareServer()
	c := &Connection{ID: "c1", Events: []Row{}}
	s.connections["c1"] = c

	srv := httptest.NewServer(http.HandlerFunc(s.websocket))
	defer srv.Close()
	conn, _, err := websocket.DefaultDialer.Dial("ws"+strings.TrimPrefix(srv.URL, "http"), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	var ready map[string]any
	if err := conn.ReadJSON(&ready); err != nil {
		t.Fatal(err)
	}

	// Emitted while the socket is attached but before the attach command is
	// handled: these land on the broadcast channel and used to be forwarded
	// again after the replay, duplicating text deltas on the client.
	for i := 0; i < 3; i++ {
		s.event("c1", Row{"type": "status_changed", "status": "connected"})
	}
	if err := conn.WriteJSON(map[string]any{
		"action": "attach", "subscription_id": "sub1", "connection_id": "c1", "since_seq": 1,
	}); err != nil {
		t.Fatal(err)
	}

	_ = conn.SetReadDeadline(time.Now().Add(400 * time.Millisecond))
	wrapped := 0
	for {
		var frame map[string]any
		if err := conn.ReadJSON(&frame); err != nil {
			break
		}
		if frame["type"] == "event" {
			wrapped++
		}
	}
	if wrapped != 0 {
		t.Fatalf("forwarded %d already-replayed events a second time", wrapped)
	}
}
