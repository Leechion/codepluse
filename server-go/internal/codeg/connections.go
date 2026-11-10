package codeg

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"sync"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
)

// busyStaleGrace is how long a busy connection with no runtime-active turn is
// tolerated before the next prompt takes it over in place. It covers the gap
// between prompt() accepting a turn and Runtime.turn() registering rec.active,
// during which HasActiveTurn is legitimately false. A busy flag older than
// this with no active turn can only be a leak (the completing run always
// clears it under the RunSeq guard), so healing it never kills real work.
const busyStaleGrace = 3 * time.Second

// warmIdleTTL bounds how long an unclaimed pre-built connection is kept.
const warmIdleTTL = 2 * time.Minute

type warmEntry struct {
	ID       string
	PlacedAt time.Time
}

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
	// PendingSplit is the task-split card waiting on the user (see split.go).
	// Like Pending it is published through events and the snapshot, and cleared
	// when the split resolves or the connection goes.
	PendingSplit    Row
	Busy            bool
	SeenPermissions map[string]string
	Depth                    int
	RootID                   string
	Feedback                 []Row
	ToolClaims               map[string]bool
	Warm                     bool
	// RunSeq retires a turn's deferred cleanup once a newer run owns the
	// connection, so healing a leaked busy flag can never let the abandoned
	// run clobber the new turn's Live/Busy/Status state.
	RunSeq       uint64
	RunAccepted  time.Time
	completionMu sync.Mutex
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
	if config, ok := a["preferredConfigValues"].(map[string]any); ok && text(config["mode"]) != "" {
		mode = text(config["mode"])
	}
	if level := acp.ModeLevel(mode); level == "workspace" || level == "full" {
		permission = "workspace"
	}
	key := "codeg:" + id()
	model := ""
	if config, ok := a["preferredConfigValues"].(map[string]any); ok {
		model = text(config["model"])
	}
	// A harness instance pins its own model; the caller's choice (UI selector
	// or a delegation) still wins, so preferredConfigValues stays first above.
	if model == "" {
		model = agent.DefaultModel
	}
	if agent.RealTool == "dsh" {
		mode = ""
	}
	c := &Connection{ID: id(), Agent: agent, AgentType: wireAgent(agent), Status: "connecting", Options: acp.Options{SessionKey: key, Permission: permission, Workdir: cwd, Model: model, Mode: mode, Structured: false, Role: "manager"}, Events: []Row{}, Tools: []Row{}, SeenPermissions: map[string]string{}}
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
				// Keep the record's identity after an in-session mode change.
				permission = text(view["permission"])
				c.Options.Permission = permission
				if savedMode := text(view["nativeMode"]); savedMode != "" {
					c.Options.Mode = savedMode
				}
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
		s.store.mu.Lock()
		conv := 0
		for _, row := range s.store.Conversations {
			if text(row["external_id"]) == previous && text(row["agent_type"]) == c.AgentType {
				conv = number(row["id"])
				break
			}
		}
		s.store.mu.Unlock()
		s.bindConversation(c, conv)
	}
	poolKey := s.warmPoolKey(c.AgentType, cwd, permission, mode, model)
	warm, _ := a["_warm"].(bool)
	if !warm && text(a["sessionId"]) == "" && c.Depth == 0 && c.RootID == "" {
		if cid := s.takeWarm(poolKey); cid != "" {
			s.queueWarm(a, poolKey)
			return cid, nil
		}
	}
	c.Warm = warm
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
	if warm {
		s.putWarm(poolKey, c.ID)
		return c.ID, nil
	}
	s.event(c.ID, Row{"type": "status_changed", "status": "connected"})
	s.publishSession(c.ID)
	s.queueWarm(a, poolKey)
	return c.ID, nil
}

// warmPoolKey identifies a pre-built idle connection. Mode and model are part
// of the key because a claimed connection must already match the caller's
// selectors; queueWarm refills the pool with the exact parameters of the last
// connect, so switching back to the same harness hits immediately.
func (s *Server) warmPoolKey(agentType, cwd, permission, mode, model string) string {
	return strings.Join([]string{agentType, cwd, permission, mode, model}, "|")
}

// takeWarm claims an idle connection and replays the events a fresh connect
// would have sent. Stale or expired entries are released, not handed out.
func (s *Server) takeWarm(key string) string {
	for {
		s.warmMu.Lock()
		entries := s.warm[key]
		if len(entries) == 0 {
			s.warmMu.Unlock()
			return ""
		}
		e := entries[0]
		s.warm[key] = entries[1:]
		s.warmMu.Unlock()
		if time.Since(e.PlacedAt) > warmIdleTTL {
			// Nobody claimed it in time; free it instead of pinning a harness
			// process with a session no conversation will ever use.
			s.releaseWarm(e.ID)
			continue
		}
		s.mu.Lock()
		c := s.connections[e.ID]
		warm := c != nil && c.Warm
		valid := warm && c.Status == "connected" && !c.Busy && c.ConversationID == 0
		if valid {
			c.Warm = false
		}
		s.mu.Unlock()
		if !valid {
			// Retire only entries still marked warm (idle ones that went
			// stale/busy). A connection whose warm flag is gone was claimed by
			// a real conversation — releasing it would kill a live session.
			if warm {
				s.releaseWarm(e.ID)
			}
			continue
		}
		s.event(e.ID, Row{"type": "status_changed", "status": "connected"})
		s.publishSession(e.ID)
		return e.ID
	}
}

// putWarm parks an idle connection. Overflow duplicates are released
// immediately instead of lingering unclaimed in the connection map.
func (s *Server) putWarm(key, cid string) {
	s.warmMu.Lock()
	if len(s.warm[key]) > 0 {
		s.warmMu.Unlock()
		s.releaseWarm(cid)
		return
	}
	s.warm[key] = append(s.warm[key], warmEntry{ID: cid, PlacedAt: time.Now()})
	s.warmMu.Unlock()
}

// releaseWarm retires an idle pre-built connection for good: the runtime
// forgets its session record and the connection map entry goes away. The
// shared driver process stays alive per the runtime's pooling rules.
func (s *Server) releaseWarm(cid string) {
	s.warmMu.Lock()
	for k, list := range s.warm {
		for i, e := range list {
			if e.ID == cid {
				s.warm[k] = append(list[:i], list[i+1:]...)
				break
			}
		}
	}
	s.warmMu.Unlock()
	s.mu.Lock()
	c := s.connections[cid]
	runtimeID := ""
	if c != nil {
		c.Warm = false
		runtimeID = c.RuntimeID
		delete(s.connections, cid)
	}
	s.mu.Unlock()
	if c != nil && runtimeID != "" {
		s.Runtime.ForgetSession(runtimeID)
	}
}

// warmSweeper periodically releases pre-built connections nobody claimed, so
// startup prewarming and last-parameters refills cannot accumulate idle
// harness sessions for parameters the user moved on from.
func (s *Server) warmSweeper() {
	for range time.NewTicker(30 * time.Second).C {
		now := time.Now()
		s.warmMu.Lock()
		var expired []string
		for k, list := range s.warm {
			kept := list[:0]
			for _, e := range list {
				if now.Sub(e.PlacedAt) > warmIdleTTL {
					expired = append(expired, e.ID)
				} else {
					kept = append(kept, e)
				}
			}
			s.warm[k] = kept
		}
		s.warmMu.Unlock()
		for _, cid := range expired {
			s.releaseWarm(cid)
		}
	}
}

// queueWarm pre-builds one idle connection with the exact parameters of a
// successful connect. It never blocks the caller and is a no-op when a fill
// for the same pool key (agent+cwd+permission+mode+model) is already running —
// the dedup key must match warmPoolKey granularity, or switching only the
// model would be swallowed as "already warming".
func (s *Server) queueWarm(a Row, poolKey string) {
	s.warmMu.Lock()
	started := s.started
	s.warmMu.Unlock()
	if !started || os.Getenv("HARNESS_HUB_WARM") == "0" {
		return
	}
	next := clone(a)
	delete(next, "sessionId")
	delete(next, "_depth")
	delete(next, "_root")
	next["_warm"] = true
	dedup := poolKey
	s.warmMu.Lock()
	if s.warming[dedup] {
		s.warmMu.Unlock()
		return
	}
	s.warming[dedup] = true
	s.warmMu.Unlock()
	go func() {
		defer func() {
			_ = recover()
			s.warmMu.Lock()
			delete(s.warming, dedup)
			s.warmMu.Unlock()
		}()
		if _, err := s.connect(next); err != nil {
			fmt.Fprintln(os.Stderr, "[warm] 预建连接失败：", err)
		}
	}()
}

// warmAll pre-connects one idle session per installed harness at startup.
// Serial on purpose: each session/new is expensive, and starting all of them
// at once would starve the first real user request.
func (s *Server) warmAll() {
	time.Sleep(1500 * time.Millisecond)
	for _, a := range s.Hub.Agents() {
		if !a.Enabled || !a.Real {
			continue
		}
		if os.Getenv("HARNESS_HUB_WARM") == "0" {
			return
		}
		if _, err := s.connect(Row{"agentType": wireAgent(a), "workingDir": "", "_warm": true}); err != nil {
			fmt.Fprintln(os.Stderr, "[warm] 启动预热失败：", wireAgent(a), err)
		}
	}
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
		parent := text(e["parent_tool_use_id"])
		// ACP message IDs are optional. Only retain the last non-empty text
		// chunk's ID; an unidentifiable later chunk must not inherit an older ID.
		if text(e["text"]) != "" && parent == "" {
			if text(c.Live["created_at"]) == "" {
				c.Live["created_at"] = timestamp()
			}
			if nativeID := text(e["source_message_id"]); nativeID != "" {
				c.Live["source_turn_id"] = nativeID
			} else {
				delete(c.Live, "source_turn_id")
			}
		}
		delete(e, "source_message_id")
		blocks, _ := c.Live["content"].([]Row)
		if len(blocks) == 0 || blocks[len(blocks)-1]["kind"] != "text" || text(blocks[len(blocks)-1]["parent_tool_use_id"]) != parent {
			blocks = append(blocks, Row{"kind": "text", "text": "", "parent_tool_use_id": parent})
		}
		blocks[len(blocks)-1]["text"] = text(blocks[len(blocks)-1]["text"]) + text(e["text"])
		c.Live["content"] = blocks
	case "thinking":
		ensureLive()
		parent := text(e["parent_tool_use_id"])
		blocks, _ := c.Live["content"].([]Row)
		if len(blocks) == 0 || blocks[len(blocks)-1]["kind"] != "thinking" || text(blocks[len(blocks)-1]["parent_tool_use_id"]) != parent {
			blocks = append(blocks, Row{"kind": "thinking", "text": "", "parent_tool_use_id": parent})
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
	meta, _ := u["_meta"].(map[string]any)
	claude, _ := meta["claudeCode"].(map[string]any)
	parent := text(claude["parentToolUseId"])
	switch text(u["sessionUpdate"]) {
	case "agent_message_chunk":
		content, _ := u["content"].(map[string]any)
		s.event(cid, Row{"type": "content_delta", "text": text(content["text"]), "source_message_id": u["messageId"], "parent_tool_use_id": parent})
	case "agent_thought_chunk":
		content, _ := u["content"].(map[string]any)
		s.event(cid, Row{"type": "thinking", "text": text(content["text"]), "parent_tool_use_id": parent})
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
		rid, since := c.RuntimeID, c.RunAccepted
		s.mu.Unlock()
		// The runtime is the source of truth: an active turn means real work
		// is running and must never be killed — reject as busy. With no
		// active turn past the grace window, Busy=true can only be a leaked
		// flag from an abandoned run, so take the connection over in place
		// (the RunSeq bump below retires that run's late cleanup, should it
		// ever surface) and let this prompt proceed.
		stale := !s.Runtime.HasActiveTurn(rid) && !since.IsZero() && time.Since(since) >= busyStaleGrace
		s.mu.Lock()
		// Re-check under the lock: between the unlock and here another prompt
		// may have already taken the stale run over (RunAccepted moves).
		if c.Busy && (!stale || !c.RunAccepted.Equal(since)) {
			s.mu.Unlock()
			return fmt.Errorf("turn_in_progress")
		}
	}
	c.Busy = true
	c.Status = "prompting"
	c.RunSeq++
	mySeq := c.RunSeq
	c.RunAccepted = time.Now()
	c.Live = nil
	c.Tools = []Row{}
	c.Feedback = []Row{}
	c.ToolClaims = map[string]bool{}
	s.mu.Unlock()
	accepted := false
	defer func() {
		if !accepted {
			s.releaseRun(c, mySeq)
		}
	}()
	blocks, _ := a["blocks"].([]any)
	var prompt strings.Builder
	var wireContent []map[string]any
	attachmentCount := 0
	var userBlocks []Row
	for _, raw := range blocks {
		b, _ := raw.(map[string]any)
		switch text(b["type"]) {
		case "text":
			prompt.WriteString(text(b["text"]))
			wireContent = append(wireContent, map[string]any{"type": "text", "text": text(b["text"])})
			userBlocks = append(userBlocks, Row{"type": "text", "text": text(b["text"])})
		case "image":
			if !s.Runtime.PromptCapabilities(c.RuntimeID)["image"] {
				return fmt.Errorf("当前执行端不支持图片输入")
			}
			mime := text(b["mime_type"])
			if !strings.HasPrefix(mime, "image/") {
				return fmt.Errorf("图片类型无效")
			}
			data, err := s.imageData(b)
			if err != nil {
				return err
			}
			wireContent = append(wireContent, map[string]any{"type": "image", "data": data, "mimeType": mime})
			attachmentCount++
			userBlocks = append(userBlocks, Row{"type": "image", "data": data, "mime_type": mime, "uri": b["uri"]})
		case "resource_link":
			wire, display, err := resourceLink(b)
			if err != nil {
				return err
			}
			wireContent = append(wireContent, wire)
			userBlocks = append(userBlocks, display)
			attachmentCount++
		case "resource":
			if !s.Runtime.PromptCapabilities(c.RuntimeID)["embedded_context"] {
				return fmt.Errorf("当前执行端不支持嵌入资源")
			}
			wire, display, err := s.embeddedResource(b)
			if err != nil {
				return err
			}
			wireContent = append(wireContent, wire)
			userBlocks = append(userBlocks, display)
			attachmentCount++
		default:
			s.releaseRun(c, mySeq)
			return fmt.Errorf("不支持的消息内容类型")
		}
	}
	if strings.TrimSpace(prompt.String()) == "" && attachmentCount == 0 {
		s.releaseRun(c, mySeq)
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
		s.releaseRun(c, mySeq)
		return fmt.Errorf("会话不存在")
	}
	st.appendTurnLocked(convID, "user", prompt.String())
	turns := st.Turns[fmt.Sprint(convID)]
	turns[len(turns)-1]["blocks"] = userBlocks
	row["status"] = "in_progress"
	row["external_id"] = text(s.runtimeView(c.RuntimeID)["nativeSessionId"])
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
	s.bindConversation(c, convID)
	s.event(cid, Row{"type": "conversation_linked", "conversation_id": convID, "folder_id": folderID})
	s.event(cid, Row{"type": "conversation_status_changed", "conversation_id": convID, "status": "in_progress"})
	s.event(cid, Row{"type": "status_changed", "status": "prompting"})
	accepted = true
	go s.runWithContent(c, prompt.String(), wireContent)
	return nil
}

// releaseRun clears the connection's busy state for the run that started at
// seq, but only if no newer prompt/delegation has claimed the connection in
// the meantime. A healed run (client reset the flag and sent a new prompt)
// finishes late and must not clobber the live turn's state.
func (s *Server) releaseRun(c *Connection, seq uint64) {
	s.mu.Lock()
	if c.RunSeq == seq {
		c.Busy = false
		c.Status = "connected"
		c.RunAccepted = time.Time{}
	}
	s.mu.Unlock()
}
func (s *Server) run(c *Connection, prompt string) acp.Result {
	return s.runWithContent(c, prompt, nil)
}
func (s *Server) runWithContent(c *Connection, prompt string, content []map[string]any) acp.Result {
	s.mu.Lock()
	mySeq := c.RunSeq
	s.mu.Unlock()
	opts := c.Options
	// This is a native dialogue. The model chooses tools and sub-agents itself,
	// rather than being required to output an externally planned task JSON.
	prefix := "本工作台支持 codeg-mcp 协作工具。需要其他执行端时使用 delegate_to_agent 自主分工，并用 get_delegation_status 收集真实结果后汇报；用户点名执行端时遵从其选择。任务可拆成多个各自独立的子任务时，优先用 propose_task_split 一次性提案（界面卡片让用户确认或改选执行端），确认后系统自动派发，你无需也不得重复派发；无需用户挑选的单独立项仍用 delegate_to_agent。子任务必须自包含并继承当前权限边界。\n\n" + s.delegationContext(c) + s.splitContext(c.ID)
	wireBlocks := content
	if len(wireBlocks) == 0 {
		wireBlocks = []map[string]any{{"type": "text", "text": prefix + prompt}}
	} else if text(wireBlocks[0]["type"]) == "text" {
		wireBlocks[0]["text"] = prefix + text(wireBlocks[0]["text"])
	} else {
		wireBlocks = append([]map[string]any{{"type": "text", "text": prefix}}, wireBlocks...)
	}
	result := s.Runtime.PromptBlocks(c.Agent, wireBlocks, opts)
	c.completionMu.Lock()
	defer c.completionMu.Unlock()
	// A self-healed run (busy flag reset + new prompt claimed the connection)
	// must not clobber the live turn's state or events when it finally returns.
	s.mu.Lock()
	stale := c.RunSeq != mySeq
	s.mu.Unlock()
	if stale {
		return result
	}
	s.mu.Lock()
	blocks := persistedBlocks(c.Live, c.Tools, result.Output)
	sourceTurnID := text(c.Live["source_turn_id"])
	createdAt := text(c.Live["created_at"])
	s.mu.Unlock()
	st := s.store
	st.mu.Lock()
	row := st.conversationLocked(c.ConversationID)
	if row != nil {
		if result.OK {
			st.appendTurnLocked(c.ConversationID, "assistant", result.Output)
			turns := st.Turns[fmt.Sprint(c.ConversationID)]
			turns[len(turns)-1]["blocks"] = blocks
			if createdAt != "" {
				turns[len(turns)-1]["timestamp"] = createdAt
			}
			if sourceTurnID != "" {
				turns[len(turns)-1]["source_turn_id"] = sourceTurnID
			}
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
	// The card can no longer be answered once the session is gone.
	s.dropConnectionSplits(cid)
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
		if c.Warm {
			continue
		}
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
	if c == nil {
		s.mu.Unlock()
		return
	}
	rid, agent := c.RuntimeID, c.Agent
	s.mu.Unlock()
	v := s.runtimeView(rid)
	s.mu.Lock()
	agentType := c.AgentType
	s.mu.Unlock()
	s.event(cid, Row{"type": "session_started", "session_id": v["nativeSessionId"]})
	s.event(cid, Row{"type": "fork_supported", "supported": s.forkSupported(agent, rid)})
	s.event(cid, Row{"type": "prompt_capabilities", "prompt_capabilities": s.Runtime.PromptCapabilities(rid)})
	if modes := v["modes"]; modes != nil {
		s.event(cid, Row{"type": "session_modes", "modes": modeWire(modes)})
	}
	// agent_type lets UI surfaces that watch the global acp://event firehose
	// (e.g. the ZCode model picker) filter by harness without owning a
	// connection→session map.
	s.event(cid, Row{"type": "session_config_options", "config_options": configWire(v), "agent_type": agentType})
}
func (s *Server) forkSupported(agent *hub.Agent, runtimeID string) bool {
	return s.Runtime.SupportsPointFork(runtimeID, agent)
}
func (s *Server) snapshot(cid string) Row {
	s.mu.Lock()
	c := s.connections[cid]
	if c == nil {
		s.mu.Unlock()
		return nil
	}
	out := Row{"connection_id": cid, "conversation_id": c.ConversationID, "folder_id": c.FolderID, "status": c.Status, "event_seq": c.Seq, "live_message": clone(c.Live), "active_tool_calls": toolStateWire(c.Tools), "pending_permission": nilIfEmpty(clone(c.Pending)), "pending_split": nilIfEmpty(clone(c.PendingSplit)), "active_delegations": []Row{}, "feedback": clone(c.Feedback), "feedback_tool_available": true, "fork_supported": false, "available_commands": []Row{}, "selectors_ready": true, "config_options": []Row{}, "prompt_capabilities": Row{"image": false, "audio": false, "embedded_context": false}}
	active := []Row{}
	for _, d := range s.tasks {
		if d.Parent == cid && d.Status == "running" && d.ConnectionID != "" {
			active = append(active, Row{"parent_tool_use_id": d.ParentToolUseID, "child_connection_id": d.ConnectionID, "child_conversation_id": d.ConversationID, "agent_type": d.AgentType, "task_id": d.ID, "task_preview": d.Task, "split_id": d.SplitID})
		}
	}
	out["active_delegations"] = active
	rid, agent := c.RuntimeID, c.Agent
	s.mu.Unlock()
	v := s.runtimeView(rid)
	out["fork_supported"] = s.forkSupported(agent, rid)
	out["prompt_capabilities"] = s.Runtime.PromptCapabilities(rid)
	out["native_steering_available"] = s.Runtime.SupportsNativeSteering(rid)
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
