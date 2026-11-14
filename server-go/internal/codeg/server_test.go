package codeg

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
)

func testServer(t *testing.T) *Server {
	t.Helper()
	node, err := exec.LookPath("node")
	if err != nil {
		t.Skip("Node required for protocol fixture")
	}
	fixture, err := filepath.Abs("../../../fixtures/acp-agent.cjs")
	if err != nil {
		t.Fatal(err)
	}
	root := t.TempDir()
	h := hub.NewHub()
	h.CreateAgent(&hub.Agent{ID: "codex", Name: "Codex", Real: true, RealTool: "codex"})
	h.CreateAgent(&hub.Agent{ID: "claude", Name: "Claude", Real: true, RealTool: "claude", ACPModes: map[string]string{"readonly": "read-only", "workspace": "workspace-write"}})
	rt := acp.NewRuntime(h, ":memory:", 10000)
	rt.LaunchFn = func(a *hub.Agent, p, c string) (*acp.Launch, error) {
		return &acp.Launch{Command: node, Args: []string{fixture}}, nil
	}
	s, err := New(root, "", rt, h, func(p string) (string, error) {
		if p == "" {
			return root, nil
		}
		return p, nil
	})
	if err != nil {
		t.Fatal(err)
	}
	// Tests exercise fetch_url against httptest servers on 127.0.0.1, which the
	// production dial guard deliberately blocks. Inject a permissive client
	// instead of weakening safeFetchClient.
	s.fetchClient = &http.Client{Timeout: 5 * time.Second}
	t.Cleanup(func() { s.Close() })
	t.Cleanup(rt.Shutdown)
	return s
}
func command(t *testing.T, s *Server, name string, args Row) any {
	t.Helper()
	b, _ := json.Marshal(args)
	req := httptest.NewRequest("POST", "/api/"+name, bytes.NewReader(b))
	rec := httptest.NewRecorder()
	s.ServeHTTP(rec, req)
	if rec.Code != 200 {
		t.Fatalf("%s: %s", name, rec.Body.String())
	}
	var out any
	if err := json.Unmarshal(rec.Body.Bytes(), &out); err != nil {
		t.Fatal(err)
	}
	return out
}
func TestOriginalConversationContractsPersistAcrossRestart(t *testing.T) {
	s := testServer(t)
	if err := os.MkdirAll(filepath.Join(s.Root, ".data"), 0755); err != nil {
		t.Fatal(err)
	}
	created := command(t, s, "create_chat_conversation", Row{"agentType": "codex", "title": "port test"}).(map[string]any)
	n := number(created["conversationId"])
	if n == 0 || number(created["folderId"]) == 0 {
		t.Fatalf("invalid Codeg IDs: %v", created)
	}
	detail := command(t, s, "get_folder_conversation", Row{"conversationId": n}).(map[string]any)
	if detail["summary"].(map[string]any)["title"] != "port test" {
		t.Fatal(detail)
	}
	outcome := command(t, s, "save_opened_tabs", Row{"expectedVersion": 0, "items": []Row{{"id": n, "folder_id": created["folderId"]}}}).(map[string]any)
	if outcome["accepted"] != true {
		t.Fatal(outcome)
	}
	stale := command(t, s, "save_opened_tabs", Row{"expectedVersion": 0, "items": []Row{}}).(map[string]any)
	if stale["accepted"] != false {
		t.Fatal("lost tab CAS")
	}
	loaded := reloadStore(t, s.store.file)
	if len(loaded.Conversations) != 1 || len(loaded.Tabs) != 1 {
		t.Fatalf("persistence: %v", loaded.Conversations)
	}
}
func TestOriginalWebSocketReceivesRealACPStream(t *testing.T) {
	s := testServer(t)
	server := httptest.NewServer(s)
	defer server.Close()
	s.Base = server.URL
	conn, _, err := websocket.DefaultDialer.Dial("ws"+strings.TrimPrefix(server.URL, "http")+"/ws/events", nil)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
	var ready Row
	if err = conn.ReadJSON(&ready); err != nil || ready["channel"] != "__ready__" {
		t.Fatalf("ready: %v %v", ready, err)
	}
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	if err = conn.WriteJSON(Row{"action": "attach", "subscription_id": "test", "connection_id": cid}); err != nil {
		t.Fatal(err)
	}
	command(t, s, "acp_prompt", Row{"connectionId": cid, "blocks": []Row{{"type": "text", "text": "actual ACP payload"}}})
	var content strings.Builder
	var last float64
	seenSnapshot := false
	seenPrompting := false
	for {
		var frame Row
		if err = conn.ReadJSON(&frame); err != nil {
			t.Fatal(err)
		}
		if frame["type"] == "snapshot" {
			seenSnapshot = true
		}
		if frame["type"] != "event" {
			continue
		}
		e := frame["envelope"].(map[string]any)
		seq := e["seq"].(float64)
		if seq <= last {
			t.Fatal("event order regressed")
		}
		last = seq
		if e["type"] == "content_delta" {
			if !seenPrompting {
				t.Fatal("original UI discards content without status_changed prompting")
			}
			content.WriteString(text(e["text"]))
		}
		if e["type"] == "status_changed" && e["status"] == "prompting" {
			seenPrompting = true
		}
		if e["type"] == "turn_complete" {
			break
		}
	}
	if !seenSnapshot || !strings.HasSuffix(content.String(), "actual ACP payload") {
		t.Fatalf("missing stream: %q", content.String())
	}
	rows := command(t, s, "list_all_conversations", Row{}).([]any)
	if len(rows) != 1 || rows[0].(map[string]any)["status"] != "pending_review" {
		t.Fatal(rows)
	}
	detail := command(t, s, "get_folder_conversation", Row{"conversationId": rows[0].(map[string]any)["id"]}).(map[string]any)
	turns := detail["turns"].([]any)
	if got := turns[len(turns)-1].(map[string]any)["source_turn_id"]; got != "fixture-message-1" {
		t.Fatalf("native assistant message ID not persisted: %v", got)
	}
	if _, err := time.Parse(time.RFC3339Nano, text(turns[len(turns)-1].(map[string]any)["timestamp"])); err != nil {
		t.Fatalf("assistant timestamp not persisted: %v", err)
	}
}

func TestNativeMessageIDDoesNotLeakAcrossUnidentifiedChunks(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	s.mu.Lock()
	runtimeID := s.connections[cid].RuntimeID
	s.mu.Unlock()
	s.nativeUpdate(runtimeID, Row{"sessionUpdate": "agent_message_chunk", "messageId": "native-a", "content": Row{"type": "text", "text": "first"}})
	s.mu.Lock()
	createdAt := text(s.connections[cid].Live["created_at"])
	s.mu.Unlock()
	s.nativeUpdate(runtimeID, Row{"sessionUpdate": "agent_message_chunk", "content": Row{"type": "text", "text": "unknown"}})
	s.mu.Lock()
	got := text(s.connections[cid].Live["source_turn_id"])
	stillCreatedAt := text(s.connections[cid].Live["created_at"])
	s.mu.Unlock()
	if createdAt == "" || createdAt != stillCreatedAt {
		t.Fatalf("assistant creation time changed across chunks: %q, %q", createdAt, stillCreatedAt)
	}
	if got != "" {
		t.Fatalf("unidentified final chunk inherited an old message ID: %q", got)
	}
	s.nativeUpdate(runtimeID, Row{"sessionUpdate": "agent_message_chunk", "messageId": "native-b", "content": Row{"type": "text", "text": "last"}})
	s.mu.Lock()
	got = text(s.connections[cid].Live["source_turn_id"])
	s.mu.Unlock()
	if got != "native-b" {
		t.Fatalf("expected final native message ID, got %q", got)
	}
}
func TestDelegationFansOutToNativeSessionsAndScopesResults(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	s.mu.Lock()
	caller := s.callers[cid]
	s.mu.Unlock()
	result, err := s.tool(context.Background(), caller, "delegate_to_agent", Row{"agent_type": "claude_code", "task": "finish the actual child work"})
	if err != nil {
		t.Fatal(err)
	}
	tid := result.(Row)["task_id"].(string)
	status, err := s.tool(context.Background(), caller, "get_delegation_status", Row{"task_ids": []any{tid}, "wait_ms": float64(3000)})
	if err != nil {
		t.Fatal(err)
	}
	b, _ := json.Marshal(status)
	var value struct {
		Tasks []Row `json:"tasks"`
	}
	_ = json.Unmarshal(b, &value)
	if len(value.Tasks) != 1 || value.Tasks[0]["status"] != "completed" || !strings.HasSuffix(text(value.Tasks[0]["text"]), "finish the actual child work") {
		t.Fatalf("delegation failed: %s", b)
	}
	foreign := &Caller{RootID: "another-root"}
	hidden, _ := s.taskStatus(foreign, []string{tid})
	if hidden[0]["status"] != "unknown" {
		t.Fatal("foreign root can read results")
	}
	_, err = s.tool(context.Background(), foreign, "send_agent_message", Row{"task_id": tid, "message": "foreign message"})
	if err == nil {
		t.Fatal("foreign root can message this team")
	}
	_, err = s.tool(context.Background(), caller, "send_agent_message", Row{"task_id": tid, "message": "verify the result"})
	if err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	childCaller := s.callers[s.tasks[tid].ConnectionID]
	s.mu.Unlock()
	mail, err := s.tool(context.Background(), childCaller, "check_user_feedback", Row{})
	if err != nil || len(mail.(Row)["messages"].([]Row)) != 1 {
		t.Fatalf("team inbox: %v %v", mail, err)
	}
	again, _ := s.tool(context.Background(), childCaller, "check_user_feedback", Row{})
	if len(again.(Row)["messages"].([]Row)) != 0 {
		t.Fatal("message delivered twice")
	}
	_, err = s.tool(context.Background(), &Caller{Depth: 4}, "delegate_to_agent", Row{"agent_type": "codex", "task": "too deep"})
	if err == nil {
		t.Fatal("depth limit bypassed")
	}
}

func TestDelegationInheritsSelectedFullAccessMode(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	command(t, s, "acp_set_config_option", Row{"connectionId": cid, "configId": "mode", "value": "agent-full-access"})
	s.mu.Lock()
	caller := s.callers[cid]
	s.mu.Unlock()
	result, err := s.tool(context.Background(), caller, "delegate_to_agent", Row{"agent_type": "codex", "task": "child full access test"})
	if err != nil {
		t.Fatal(err)
	}
	tid := result.(Row)["task_id"].(string)
	if _, err = s.tool(context.Background(), caller, "get_delegation_status", Row{"task_ids": []any{tid}, "wait_ms": float64(3000)}); err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	childID := s.tasks[tid].ConnectionID
	mode := s.tasks[tid].Mode
	s.mu.Unlock()
	if mode != "agent-full-access" || childID == "" {
		t.Fatal("full access was not forwarded")
	}
	snapshot := command(t, s, "acp_get_session_snapshot", Row{"connectionId": childID}).(map[string]any)
	if snapshot["modes"].(map[string]any)["current_mode_id"] != "agent-full-access" {
		t.Fatal("child mode was downgraded")
	}
}

func TestReconnectUsesOneNativeStreamOwner(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	snap := command(t, s, "acp_get_session_snapshot", Row{"connectionId": cid}).(map[string]any)
	resumed := command(t, s, "acp_connect", Row{"agentType": "codex", "sessionId": snap["external_id"]}).(string)
	if resumed != cid {
		t.Fatal("two wrappers would split one native event stream")
	}
}

func TestHistoryRestoresDelegationCardsFromPersistedChild(t *testing.T) {
	st, err := newStore(filepath.Join(t.TempDir(), "workspace.json"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = st.Close() })
	f := st.openFolderLocked(t.TempDir(), "regular")
	p := st.createConversationLocked(number(f["id"]), "claude_code", "parent", 0)
	parentID := number(p["id"])
	if err := st.insertTurnLocked(parentID, Row{
		"id": id(), "role": "assistant", "timestamp": timestamp(),
		"blocks": []Row{{"type": "tool_use", "tool_use_id": "native-call-1", "tool_name": "mcp__codeg-mcp__delegate_to_agent"}},
	}); err != nil {
		t.Fatal(err)
	}
	child := st.createConversationLocked(number(f["id"]), "codex", "calculate", parentID)
	child["parent_tool_use_id"], child["delegation_call_id"], child["status"] = "native-call-1", "task-1", "pending_review"
	if err := st.saveLocked(); err != nil {
		t.Fatal(err)
	}
	restored := reloadStore(t, st.file)
	turns := restored.turnsWithDelegationsLocked(parentID)
	block := turns[0]["blocks"].([]any)[0].(map[string]any)
	meta := block["meta"].(map[string]any)["codeg.delegation"].(map[string]any)
	if meta["status"] != "completed" || number(meta["child_conversation_id"]) != number(child["id"]) || meta["task_id"] != "task-1" {
		t.Fatal(meta)
	}
}
func TestStaticExportResolvesPageBesideRSCFolder(t *testing.T) {
	s := testServer(t)
	dir := filepath.Join(s.Root, "codeg-ui", "out")
	_ = os.MkdirAll(filepath.Join(dir, "workspace"), 0755)
	_ = os.WriteFile(filepath.Join(dir, "workspace.html"), []byte("original UI"), 0644)
	_ = os.WriteFile(filepath.Join(dir, "workspace.txt"), []byte("RSC"), 0644)
	for _, rsc := range []bool{false, true} {
		req := httptest.NewRequest(http.MethodGet, "/workspace", nil)
		if rsc {
			req.Header.Set("Rsc", "1")
		}
		w := httptest.NewRecorder()
		s.ServeHTTP(w, req)
		if w.Code != 200 {
			t.Fatalf("export routing: %d %s", w.Code, w.Body.String())
		}
	}
}
