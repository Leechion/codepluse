package codeg

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
)

type Connection struct {
	ID, AgentType, Status    string
	Agent                    *hub.Agent
	Options                  acp.Options
	RuntimeID                string
	ConversationID, FolderID int
	Seq                      uint64
	Events                   []Row
	Live                     Row
	Tools                    []Row
	Pending                  Row
	Busy                     bool
	SeenPermissions          map[string]string
	Depth                    int
	RootID                   string
	Feedback                 []Row
	ToolClaims               map[string]bool
}

func (s *Server) connect(a Row) (string, error) {
	agentType := text(a["agentType"])
	agent := s.pick(agentType)
	if agent == nil {
		return "", fmt.Errorf("执行端 %s 未连接", agentType)
	}
	cwd, err := s.Resolve(text(a["workingDir"]))
	if err != nil {
		return "", err
	}
	permission := "readonly"
	// DSH advertises model configuration, but no ACP permission modes. Its
	// native policy owns tools; do not label that session as enforced read-only.
	if agent.RealTool == "dsh" {
		permission = "workspace"
	}
	mode := text(a["preferredModeId"])
	if mode == "workspace-write" || mode == "default" {
		permission = "workspace"
	}
	key := "codeg:" + id()
	model := ""
	if config, ok := a["preferredConfigValues"].(map[string]any); ok {
		model = text(config["model"])
	}
	c := &Connection{ID: id(), Agent: agent, AgentType: wireAgent(agent), Status: "connecting", Options: acp.Options{SessionKey: key, Permission: permission, Workdir: cwd, Model: model, Structured: false, Role: "manager"}, Events: []Row{}, Tools: []Row{}, SeenPermissions: map[string]string{}}
	c.Depth = number(a["_depth"])
	c.RootID = text(a["_root"])
	c.Feedback = []Row{}
	c.ToolClaims = map[string]bool{}
	if c.Depth > 0 {
		c.Options.Role = "worker"
	}
	if previous := text(a["sessionId"]); previous != "" {
		// Reuse only a session already owned by this Go runtime; native history
		// import has its own adapter-specific path and must not silently start cold.
		var views []Row
		_ = json.Unmarshal(s.Runtime.ListJSON(""), &views)
		found := false
		for _, view := range views {
			if text(view["nativeSessionId"]) == previous && text(view["agentId"]) == agent.ID {
				// One native stream has one owner. Reconnecting another window
				// must not create two wrappers that receive updates at random.
				s.mu.Lock()
				for _, existing := range s.connections {
					if existing.RuntimeID == text(view["id"]) && existing.Options.Workdir == cwd && existing.Options.Permission == permission {
						cid := existing.ID
						if existing.Status == "disconnected" {
							existing.Status = "connected"
						}
						s.mu.Unlock()
						return cid, nil
					}
				}
				s.mu.Unlock()
				key = text(view["key"])
				idx := strings.LastIndex(key, "|"+agent.ID+"|")
				if idx > 0 {
					key = key[:idx]
				}
				suffix := "|" + cwd
				key = strings.TrimSuffix(key, suffix)
				c.Options.SessionKey = key
				found = true
				break
			}
		}
		if !found {
			return "", fmt.Errorf("未导入此原生会话，不能无上下文恢复")
		}
	}
	s.mu.Lock()
	s.connections[c.ID] = c
	s.mu.Unlock()
	native, err := s.Runtime.Connect(agent, c.Options)
	if err != nil {
		s.mu.Lock()
		delete(s.connections, c.ID)
		s.mu.Unlock()
		return "", err
	}
	s.mu.Lock()
	c.RuntimeID = native
	c.Status = "connected"
	s.mu.Unlock()
	s.event(c.ID, Row{"type": "status_changed", "status": "connected"})
	s.publishSession(c.ID)
	return c.ID, nil
}
func (s *Server) event(connectionID string, e Row) {
	s.mu.Lock()
	defer s.mu.Unlock()
	c := s.connections[connectionID]
	if c == nil {
		return
	}
	e = clone(e)
	ensureLive := func() {
		if c.Live == nil {
			c.Live = Row{"id": id(), "role": "assistant", "content": []Row{}, "started_at": timestamp()}
		}
	}
	switch text(e["type"]) {
	case "content_delta":
		ensureLive()
		blocks, _ := c.Live["content"].([]Row)
		if len(blocks) == 0 || blocks[len(blocks)-1]["kind"] != "text" {
			blocks = append(blocks, Row{"kind": "text", "text": ""})
		}
		blocks[len(blocks)-1]["text"] = text(blocks[len(blocks)-1]["text"]) + text(e["text"])
		c.Live["content"] = blocks
	case "tool_call", "tool_call_update":
		found := false
		for _, tool := range c.Tools {
			if tool["tool_call_id"] == e["tool_call_id"] {
				for k, v := range e {
					if v != nil {
						tool[k] = v
					}
				}
				found = true
				break
			}
		}
		if !found {
			c.Tools = append(c.Tools, clone(e))
			ensureLive()
			blocks, _ := c.Live["content"].([]Row)
			c.Live["content"] = append(blocks, Row{"kind": "tool_call_ref", "tool_call_id": e["tool_call_id"]})
		}
	}
	c.Seq++
	e["seq"] = c.Seq
	e["connection_id"] = c.ID
	c.Events = append(c.Events, e)
	if len(c.Events) > 256 {
		c.Events = c.Events[len(c.Events)-256:]
	}
	event := Row{"channel": "acp://event", "payload": e}
	for ch := range s.subscribers {
		select {
		case ch <- event:
		default:
		}
	}
}
func (s *Server) nativeUpdate(runtimeID string, u map[string]any) {
	s.mu.Lock()
	cid := ""
	for _, c := range s.connections {
		if c.RuntimeID == runtimeID {
			cid = c.ID
			break
		}
	}
	s.mu.Unlock()
	if cid == "" {
		return
	}
	switch text(u["sessionUpdate"]) {
	case "agent_message_chunk":
		content, _ := u["content"].(map[string]any)
		s.event(cid, Row{"type": "content_delta", "text": text(content["text"])})
	case "tool_call", "tool_call_update":
		event := Row{"type": text(u["sessionUpdate"]), "tool_call_id": u["toolCallId"], "title": u["title"], "kind": u["kind"], "status": u["status"], "locations": u["locations"], "raw_input": nil, "raw_output": nil, "content": nil, "meta": u["_meta"]}
		for _, key := range []string{"rawInput", "rawOutput", "content"} {
			if value, exists := u[key]; exists {
				wire := map[string]string{"rawInput": "raw_input", "rawOutput": "raw_output", "content": "content"}[key]
				if str, ok := value.(string); ok {
					event[wire] = str
				} else {
					b, _ := json.Marshal(value)
					event[wire] = string(b)
				}
			}
		}
		s.event(cid, event)
	case "plan":
		s.event(cid, Row{"type": "plan_update", "entries": u["entries"]})
	case "notice":
		s.event(cid, Row{"type": "session_notice", "notice": Row{"severity": u["severity"], "title": u["title"], "description": u["description"]}})
	case "config_option_update", "current_mode_update":
		s.publishSession(cid)
	}
}
func (s *Server) prompt(a Row) error {
	cid := text(a["connectionId"])
	s.mu.Lock()
	c := s.connections[cid]
	if c == nil {
		s.mu.Unlock()
		return fmt.Errorf("连接不存在")
	}
	if c.Busy {
		s.mu.Unlock()
		return fmt.Errorf("turn_in_progress")
	}
	c.Busy = true
	c.Status = "prompting"
	c.Live = nil
	c.Tools = []Row{}
	c.Feedback = []Row{}
	c.ToolClaims = map[string]bool{}
	s.mu.Unlock()
	accepted := false
	defer func() {
		if !accepted {
			s.mu.Lock()
			c.Busy = false
			c.Status = "connected"
			s.mu.Unlock()
		}
	}()
	blocks, _ := a["blocks"].([]any)
	var prompt strings.Builder
	for _, raw := range blocks {
		b, _ := raw.(map[string]any)
		if text(b["type"]) != "text" {
			s.mu.Lock()
			c.Busy = false
			c.Status = "connected"
			s.mu.Unlock()
			return fmt.Errorf("Go 移植尚未接通图片或资源输入，请使用文本")
		}
		prompt.WriteString(text(b["text"]))
	}
	if strings.TrimSpace(prompt.String()) == "" {
		s.mu.Lock()
		c.Busy = false
		c.Status = "connected"
		s.mu.Unlock()
		return fmt.Errorf("消息不能为空")
	}
	st := s.store
	st.mu.Lock()
	convID := number(a["conversationId"])
	folderID := number(a["folderId"])
	if convID == 0 {
		f := st.openFolderLocked(c.Options.Workdir, "regular")
		folderID = number(f["id"])
		row := st.createConversationLocked(folderID, c.AgentType, prompt.String(), 0)
		convID = number(row["id"])
	}
	row := st.conversationLocked(convID)
	if row == nil {
		st.mu.Unlock()
		s.mu.Lock()
		c.Busy = false
		c.Status = "connected"
		s.mu.Unlock()
		return fmt.Errorf("会话不存在")
	}
	st.appendTurnLocked(convID, "user", prompt.String())
	row["status"] = "in_progress"
	s.changed(row)
	err := st.saveLocked()
	st.mu.Unlock()
	if err != nil {
		return err
	}
	s.mu.Lock()
	c.ConversationID = convID
	c.FolderID = folderID
	s.mu.Unlock()
	s.event(cid, Row{"type": "conversation_linked", "conversation_id": convID, "folder_id": folderID})
	s.event(cid, Row{"type": "conversation_status_changed", "conversation_id": convID, "status": "in_progress"})
	s.event(cid, Row{"type": "status_changed", "status": "prompting"})
	accepted = true
	go s.run(c, prompt.String())
	return nil
}
func (s *Server) run(c *Connection, prompt string) acp.Result {
	opts := c.Options
	// This is a native dialogue. The model chooses tools and sub-agents itself,
	// rather than being required to output an externally planned task JSON.
	prompt = "本工作台支持 codeg-mcp 协作工具。需要其他执行端时使用 delegate_to_agent 自主分工，并用 get_delegation_status 收集真实结果后汇报；用户点名执行端时遵从其选择。子任务必须自包含并继承当前权限边界。\n\n" + prompt
	result := s.Runtime.Prompt(c.Agent, prompt, opts)
	s.mu.Lock()
	blocks := persistedBlocks(c.Live, c.Tools, result.Output)
	s.mu.Unlock()
	st := s.store
	st.mu.Lock()
	row := st.conversationLocked(c.ConversationID)
	if row != nil {
		if result.OK {
			st.appendTurnLocked(c.ConversationID, "assistant", result.Output)
			turns := st.Turns[fmt.Sprint(c.ConversationID)]
			turns[len(turns)-1]["blocks"] = blocks
			row["status"] = "pending_review"
		} else {
			row["status"] = "cancelled"
		}
		if sid := text(result.Meta["sessionId"]); sid != "" {
			row["external_id"] = sid
		}
		s.changed(row)
		_ = st.saveLocked()
	}
	st.mu.Unlock()
	s.mu.Lock()
	c.Busy = false
	c.Status = "connected"
	c.Live = nil
	c.Pending = nil
	s.mu.Unlock()
	if !result.OK {
		s.event(c.ID, Row{"type": "error", "message": result.Error})
	}
	reason := text(result.Meta["stopReason"])
	if reason == "" {
		reason = "end_turn"
	}
	s.event(c.ID, Row{"type": "turn_complete", "session_id": result.Meta["sessionId"], "stop_reason": reason, "agent_type": c.AgentType})
	return result
}
func (s *Server) cancel(cid string) error {
	s.mu.Lock()
	c := s.connections[cid]
	s.mu.Unlock()
	if c == nil {
		return fmt.Errorf("连接不存在")
	}
	return s.Runtime.Cancel(c.RuntimeID)
}
func (s *Server) disconnect(cid string) error {
	s.mu.Lock()
	c := s.connections[cid]
	s.mu.Unlock()
	if c == nil {
		return nil
	}
	if c.Busy {
		_ = s.Runtime.Cancel(c.RuntimeID)
	}
	s.mu.Lock()
	c.Status = "disconnected"
	s.mu.Unlock()
	s.event(cid, Row{"type": "status_changed", "status": "disconnected"})
	return nil
}
func (s *Server) connectionList() []Row {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := []Row{}
	for _, c := range s.connections {
		out = append(out, Row{"id": c.ID, "agent_type": c.AgentType, "status": c.Status})
	}
	return out
}
func (s *Server) runtimeView(runtimeID string) Row {
	var views []Row
	_ = json.Unmarshal(s.Runtime.ListJSON(""), &views)
	for _, v := range views {
		if text(v["id"]) == runtimeID {
			return v
		}
	}
	return Row{}
}
func (s *Server) publishSession(cid string) {
	s.mu.Lock()
	c := s.connections[cid]
	s.mu.Unlock()
	v := s.runtimeView(c.RuntimeID)
	s.event(cid, Row{"type": "session_started", "session_id": v["nativeSessionId"]})
	if modes := v["modes"]; modes != nil {
		s.event(cid, Row{"type": "session_modes", "modes": modeWire(modes)})
	}
	s.event(cid, Row{"type": "session_config_options", "config_options": configWire(v)})
}
func (s *Server) snapshot(cid string) Row {
	s.mu.Lock()
	c := s.connections[cid]
	if c == nil {
		s.mu.Unlock()
		return nil
	}
	out := Row{"connection_id": cid, "conversation_id": c.ConversationID, "folder_id": c.FolderID, "status": c.Status, "event_seq": c.Seq, "live_message": clone(c.Live), "active_tool_calls": toolStateWire(c.Tools), "pending_permission": clone(c.Pending), "active_delegations": []Row{}, "feedback": clone(c.Feedback), "feedback_tool_available": true, "fork_supported": false, "available_commands": []Row{}, "selectors_ready": true, "config_options": []Row{}, "prompt_capabilities": Row{"image": false, "audio": false, "embedded_context": false}}
	active := []Row{}
	for _, d := range s.tasks {
		if d.Parent == cid && d.Status == "running" && d.ConnectionID != "" {
			active = append(active, Row{"parent_tool_use_id": d.ParentToolUseID, "child_connection_id": d.ConnectionID, "child_conversation_id": d.ConversationID, "agent_type": d.AgentType, "task_id": d.ID, "task_preview": d.Task})
		}
	}
	out["active_delegations"] = active
	rid := c.RuntimeID
	s.mu.Unlock()
	v := s.runtimeView(rid)
	out["external_id"] = v["nativeSessionId"]
	out["modes"] = modeWire(v["modes"])
	out["config_options"] = configWire(v)
	if m, ok := v["modes"].(map[string]any); ok {
		out["current_mode"] = m["currentModeId"]
	}
	return out
}
func (s *Server) permissionsLoop() {
	for range time.NewTicker(200 * time.Millisecond).C {
		s.mu.Lock()
		connections := make([]*Connection, 0, len(s.connections))
		for _, c := range s.connections {
			connections = append(connections, c)
		}
		s.mu.Unlock()
		for _, c := range connections {
			v := s.runtimeView(c.RuntimeID)
			cards, _ := v["permissions"].([]any)
			for _, raw := range cards {
				p, _ := raw.(map[string]any)
				pid, state := text(p["id"]), text(p["state"])
				s.mu.Lock()
				previous := c.SeenPermissions[pid]
				c.SeenPermissions[pid] = state
				s.mu.Unlock()
				if previous == state {
					continue
				}
				if state == "pending" {
					card := Row{"request_id": pid, "tool_call": p["toolCall"], "options": permissionOptions(p["options"]), "created_at": timestamp(), "queued": 0}
					s.mu.Lock()
					c.Pending = card
					s.mu.Unlock()
					s.event(c.ID, Row{"type": "permission_request", "request_id": pid, "tool_call": p["toolCall"], "options": permissionOptions(p["options"]), "queued": 0})
				} else if previous == "pending" {
					s.mu.Lock()
					c.Pending = nil
					s.mu.Unlock()
					s.event(c.ID, Row{"type": "permission_resolved", "request_id": pid, "option_id": p["selected"]})
				}
			}
		}
	}
}
