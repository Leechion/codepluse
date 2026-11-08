package codeg

import (
	"fmt"
	"os"
	"reflect"
	"strings"
)

// forkConversation keeps the original as a sibling and switches the current
// row to the native fork, as expected by the existing conversation UI.
func (s *Server) forkConversation(a Row) (Row, error) {
	cid := text(a["connectionId"])
	s.mu.Lock()
	c := s.connections[cid]
	if c == nil {
		s.mu.Unlock()
		return nil, fmt.Errorf("连接不存在")
	}
	if c.Busy {
		s.mu.Unlock()
		return nil, fmt.Errorf("turn_in_progress")
	}
	if c.Depth != 0 {
		s.mu.Unlock()
		return nil, fmt.Errorf("子任务会话暂不支持分支")
	}
	for _, task := range s.tasks {
		if task.Root == c.RootID && task.Status == "running" {
			s.mu.Unlock()
			return nil, fmt.Errorf("协作任务运行中，暂不支持分支")
		}
	}
	c.Busy = true
	sourceRuntime, sourceOptions, agent := c.RuntimeID, c.Options, c.Agent
	conversationID := c.ConversationID
	s.mu.Unlock()
	defer func() {
		s.mu.Lock()
		if s.connections[cid] == c {
			c.Busy = false
		}
		s.mu.Unlock()
	}()
	if conversationID == 0 {
		conversationID = number(a["conversationId"])
	}
	if conversationID == 0 {
		return nil, fmt.Errorf("会话尚未保存")
	}
	if !s.Runtime.SupportsPointFork(sourceRuntime, agent) {
		return nil, fmt.Errorf("当前执行端不支持会话分支")
	}
	originalNative := text(s.runtimeView(sourceRuntime)["nativeSessionId"])
	requested := strings.TrimSpace(text(a["forkFromTurnId"]))
	if requested == "" {
		return nil, fmt.Errorf("请选择分支位置")
	}

	st := s.store
	st.mu.Lock()
	row := st.conversationLocked(conversationID)
	if row == nil || text(row["external_id"]) != originalNative || originalNative == "" {
		st.mu.Unlock()
		return nil, fmt.Errorf("会话与原生记录不匹配")
	}
	if number(row["parent_id"]) != 0 || number(row["child_count"]) != 0 {
		st.mu.Unlock()
		return nil, fmt.Errorf("含子任务的会话暂不支持分支")
	}
	for _, child := range st.Conversations {
		if number(child["parent_id"]) == conversationID {
			st.mu.Unlock()
			return nil, fmt.Errorf("含子任务的会话暂不支持分支")
		}
	}
	turns := clone(st.Turns[fmt.Sprint(conversationID)])
	point, nativeMessage := -1, ""
	for i, turn := range turns {
		if text(turn["role"]) != "assistant" {
			continue
		}
		native := text(turn["source_turn_id"])
		if requested == text(turn["id"]) || (native != "" && requested == native) {
			if point >= 0 {
				st.mu.Unlock()
				return nil, fmt.Errorf("分支位置不唯一")
			}
			point, nativeMessage = i, native
		}
	}
	st.mu.Unlock()
	if point < 0 || nativeMessage == "" {
		return nil, fmt.Errorf("该消息缺少可靠的原生分支位置")
	}
	exe, err := os.Executable()
	if err != nil {
		return nil, err
	}
	tempCallerKey, forkToken := "fork:"+id(), id()+id()
	s.mu.Lock()
	oldCaller := s.callers[cid]
	if oldCaller == nil || s.connections[cid] != c {
		s.mu.Unlock()
		return nil, fmt.Errorf("原会话协作连接不存在")
	}
	forkCaller := &Caller{Token: forkToken, ConnectionID: cid, RootID: fmt.Sprintf("conversation:%d", conversationID), Depth: 0}
	s.callers[tempCallerKey] = forkCaller
	s.mu.Unlock()
	defer func() { s.mu.Lock(); delete(s.callers, tempCallerKey); s.mu.Unlock() }()
	extraMCP := []map[string]any{{"name": "codeg-mcp", "command": exe, "args": []string{"mcp-codeg", s.Base + "/api/codeg_mcp", forkToken}, "env": []map[string]string{}}}
	newKey := "codeg:" + id()
	forkRuntime, err := s.Runtime.Fork(agent, sourceRuntime, sourceOptions, newKey, nativeMessage, extraMCP)
	if err != nil {
		return nil, err
	}
	committed := false
	defer func() {
		if !committed {
			s.Runtime.ForgetFork(forkRuntime)
		}
	}()
	forkNative := text(s.runtimeView(forkRuntime)["nativeSessionId"])
	if forkNative == "" {
		return nil, fmt.Errorf("分支会话标识缺失")
	}

	st.mu.Lock()
	s.mu.Lock()
	row = st.conversationLocked(conversationID)
	currentTurns := st.Turns[fmt.Sprint(conversationID)]
	if s.connections[cid] != c || s.callers[cid] != oldCaller || s.callers[tempCallerKey] != forkCaller || c.RuntimeID != sourceRuntime || row == nil || text(row["external_id"]) != originalNative || !reflect.DeepEqual(clone(currentTurns), turns) || number(row["child_count"]) != 0 {
		s.mu.Unlock()
		st.mu.Unlock()
		return nil, fmt.Errorf("会话在分支过程中发生变化，请重试")
	}
	for _, task := range s.tasks {
		if task.Root == c.RootID && task.Status == "running" {
			s.mu.Unlock()
			st.mu.Unlock()
			return nil, fmt.Errorf("分支过程中启动了协作任务，请重试")
		}
	}
	for _, child := range st.Conversations {
		if number(child["parent_id"]) == conversationID {
			s.mu.Unlock()
			st.mu.Unlock()
			return nil, fmt.Errorf("会话在分支过程中新增了子任务")
		}
	}
	oldRows, oldTurns, oldNext := clone(st.Conversations), clone(st.Turns), st.Next
	sibling := clone(row)
	siblingID := st.allocate()
	sibling["id"], sibling["created_at"], sibling["updated_at"] = siblingID, timestamp(), timestamp()
	sibling["external_id"], sibling["pinned_at"] = originalNative, nil
	st.Conversations = append(st.Conversations, sibling)
	st.Turns[fmt.Sprint(siblingID)] = clone(turns)
	st.Turns[fmt.Sprint(conversationID)] = clone(turns[:point+1])
	row["external_id"], row["message_count"], row["updated_at"] = forkNative, point+1, timestamp()
	if err = st.saveLocked(); err != nil {
		st.Conversations, st.Turns, st.Next = oldRows, oldTurns, oldNext
		s.mu.Unlock()
		st.mu.Unlock()
		return nil, err
	}
	// The source and the fork must retain different MCP caller tokens. Reuse a
	// disconnected wrapper for the source so reconnecting it cannot inherit
	// the fork's collaboration identity.
	siblingCID := id()
	siblingConn := &Connection{ID: siblingCID, Agent: agent, AgentType: c.AgentType, Status: "disconnected", Options: sourceOptions, RuntimeID: sourceRuntime, ConversationID: siblingID, FolderID: number(row["folder_id"]), Events: []Row{}, Tools: []Row{}, SeenPermissions: map[string]string{}, RootID: fmt.Sprintf("conversation:%d", siblingID), Feedback: []Row{}, ToolClaims: map[string]bool{}}
	s.connections[siblingCID] = siblingConn
	oldCaller.ConnectionID, oldCaller.RootID, oldCaller.Depth = siblingCID, siblingConn.RootID, 0
	s.callers[siblingCID], s.callers[cid] = oldCaller, forkCaller
	delete(s.callers, tempCallerKey)
	c.RuntimeID, c.Options.SessionKey = forkRuntime, newKey
	c.ConversationID, c.FolderID = conversationID, number(row["folder_id"])
	changedRow, changedSibling := clone(row), clone(sibling)
	s.mu.Unlock()
	st.mu.Unlock()
	committed = true
	s.changed(changedRow)
	s.changed(changedSibling)
	s.publishSession(cid)
	return Row{"forkedSessionId": forkNative, "originalSessionId": originalNative, "siblingConversationId": siblingID}, nil
}
