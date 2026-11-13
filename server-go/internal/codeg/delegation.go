package codeg

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"strings"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
)

type Caller struct {
	Token, ConnectionID, RootID string
	Depth                       int
}
type Delegation struct {
	ID                                                              string `json:"task_id"`
	Parent, Root, AgentType, Task, Workdir, Model, Permission, Mode string
	Status                                                          string `json:"status"`
	Result                                                          string `json:"result,omitempty"`
	Error                                                           string `json:"error,omitempty"`
	ConnectionID                                                    string `json:"child_connection_id,omitempty"`
	ConversationID                                                  int    `json:"child_conversation_id,omitempty"`
	ParentConversationID                                            int    `json:"parent_conversation_id,omitempty"`
	NativeSessionID                                                 string `json:"native_session_id,omitempty"`
	NextPrompt                                                      string `json:"next_prompt,omitempty"`
	Executing                                                       bool   `json:"-"`
	Depth                                                           int
	ParentToolUseID                                                 string `json:"parent_tool_use_id"`
	// SplitID marks a child launched by a confirmed task-split card; Title is
	// the card's label for it. Empty on a direct delegate_to_agent.
	SplitID string        `json:"split_id,omitempty"`
	Title   string        `json:"title,omitempty"`
	Updated chan struct{} `json:"-"`
	Inbox   []Row         `json:"inbox,omitempty"`
}

func (s *Server) extraMCP(agent *hub.Agent, o acp.Options, runtimeID string) []map[string]any {
	if d, ok := s.customDef(wireAgent(agent)); ok && !d.SupportsMCP {
		return nil
	}
	s.mu.Lock()
	var c *Connection
	for _, candidate := range s.connections {
		if candidate.Options.SessionKey == o.SessionKey {
			c = candidate
			break
		}
	}
	if c == nil {
		s.mu.Unlock()
		return nil
	}
	c.RuntimeID = runtimeID
	caller := s.callers[c.ID]
	if caller == nil {
		root := c.ID
		if c.RootID != "" {
			root = c.RootID
		}
		caller = &Caller{Token: id() + id(), ConnectionID: c.ID, RootID: root, Depth: c.Depth}
		s.callers[c.ID] = caller
	}
	s.mu.Unlock()
	exe, _ := os.Executable()
	return []map[string]any{{"name": "codeg-mcp", "command": exe, "args": []string{"mcp-codeg", s.Base + "/api/codeg_mcp", caller.Token}, "env": []map[string]string{}}}
}
func (s *Server) companionRequest(w http.ResponseWriter, r *http.Request) {
	if r.Method != "POST" {
		respond(w, 405, Row{"error": "POST required"})
		return
	}
	token := strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
	var caller *Caller
	s.mu.Lock()
	for _, c := range s.callers {
		if token != "" && token == c.Token {
			copy := *c
			caller = &copy
			break
		}
	}
	s.mu.Unlock()
	if caller == nil {
		respond(w, 401, Row{"error": "Invalid companion token"})
		return
	}
	var req struct {
		Name      string `json:"name"`
		Arguments Row    `json:"arguments"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 256<<10)).Decode(&req); err != nil {
		respond(w, 400, Row{"error": err.Error()})
		return
	}
	result, err := s.tool(r.Context(), caller, req.Name, req.Arguments)
	if err != nil {
		respond(w, 400, Row{"error": err.Error()})
		return
	}
	respond(w, 200, result)
}

// delegateScope resolves the launch scope for one child delegation: the
// working dir and permission inherited from the caller's parent connection,
// re-checked against the parent's live native mode (a model can switch modes
// mid-turn, and the child must not outrank it). Returns (cwd, permission,
// level, err). Shared by delegate_to_agent and the task-split fan-out so both
// inherit identical rules.
func (s *Server) delegateScope(caller *Caller, agentType, requestedDir string) (string, string, string, error) {
	s.mu.Lock()
	parent := s.connections[caller.ConnectionID]
	if parent == nil {
		s.mu.Unlock()
		return "", "", "", fmt.Errorf("父会话已断开")
	}
	cwd, permission := parent.Options.Workdir, parent.Options.Permission
	level := acp.ModeLevel(parent.Options.Mode)
	runtimeID := parent.RuntimeID
	if level == "" {
		level = permission
	}
	s.mu.Unlock()
	agent := s.pick(agentType)
	if agent == nil {
		return "", "", "", fmt.Errorf("执行端不可用：%s", agentType)
	}
	if current := text(s.runtimeView(runtimeID)["nativeMode"]); current != "" {
		if actual := acp.ModeLevel(current); actual != "" {
			level = actual
		}
	}
	if level == "full" || level == "workspace" {
		permission = "workspace"
	} else if level == "readonly" {
		permission = "readonly"
	}
	if agent.RealTool == "dsh" && permission != "workspace" {
		return "", "", "", fmt.Errorf("DSH 不支持继承只读权限，请先将负责人切换到工作区模式再委派")
	}
	if requestedDir != "" {
		var err error
		cwd, err = s.Resolve(requestedDir)
		if err != nil {
			return "", "", "", err
		}
	}
	return cwd, permission, level, nil
}

// launchDelegations registers children for a confirmed task split and launches
// them in parallel. The caller has already left s.mu and retired the split
// card; scope resolution (delegateScope locks) runs unlocked, registration is
// one locked pass, and a persistence failure rolls the registrations back.
// Returns the launched task ids; a nil slice means nothing was registered.
func (s *Server) launchDelegations(sp *taskSplit, parentDepth, parentConv int, parentAgentType string) []string {
	type launch struct {
		d       *Delegation
		scopeOK bool
	}
	innerCaller := &Caller{ConnectionID: sp.Parent, RootID: sp.Root, Depth: parentDepth}
	launches := make([]launch, 0, len(sp.Tasks))
	for _, task := range sp.Tasks {
		agentType := text(task["agent_type"])
		if s.pick(agentType) == nil {
			agentType = parentAgentType
		}
		cwd, permission, level, err := s.delegateScope(innerCaller, agentType, text(task["working_dir"]))
		d := &Delegation{ID: id(), Parent: sp.Parent, Root: sp.Root, AgentType: agentType, Task: text(task["prompt"]), Workdir: cwd, Model: text(task["model"]), Permission: permission, Status: "running",
			Depth: parentDepth + 1, Updated: make(chan struct{}), Inbox: []Row{}, ParentConversationID: parentConv, SplitID: sp.ID, Title: text(task["title"])}
		d.Mode = acp.DelegatedMode(s.pick(agentType), level)
		d.Executing = true
		d.ParentToolUseID = d.ID
		if err != nil {
			d.Status, d.Error, d.Executing = "failed", err.Error(), false
		}
		launches = append(launches, launch{d: d, scopeOK: err == nil})
	}
	s.mu.Lock()
	ids := []string{}
	for _, l := range launches {
		s.tasks[l.d.ID] = l.d
		ids = append(ids, l.d.ID)
	}
	errSave := s.saveDelegationsLocked()
	s.mu.Unlock()
	if errSave != nil {
		s.mu.Lock()
		for _, l := range launches {
			delete(s.tasks, l.d.ID)
		}
		_ = s.saveDelegationsLocked()
		s.mu.Unlock()
		return nil
	}
	for _, l := range launches {
		if l.scopeOK {
			go s.runDelegation(l.d)
			continue
		}
		// A subtask the parent's permission or the target's availability
		// refused at dispatch time still needs its row: emit the same event
		// pair so the card shows a failed task rather than an orphaned one.
		s.event(sp.Parent, Row{"type": "delegation_started", "parent_connection_id": sp.Parent, "parent_tool_use_id": l.d.ParentToolUseID, "split_id": sp.ID, "child_connection_id": "", "child_conversation_id": 0, "agent_type": l.d.AgentType, "task_id": l.d.ID, "task_preview": l.d.Task})
		s.event(sp.Parent, Row{"type": "delegation_completed", "parent_connection_id": sp.Parent, "parent_tool_use_id": l.d.ParentToolUseID, "split_id": sp.ID, "child_connection_id": "", "child_conversation_id": 0, "agent_type": l.d.AgentType, "task_id": l.d.ID, "result": Row{"kind": "err", "error_code": "subagent_error"}})
	}
	return ids
}

func (s *Server) tool(ctx context.Context, caller *Caller, name string, a Row) (any, error) {
	switch name {
	case "ask_user_question":
		return s.askUserQuestion(ctx, caller, a)
	case "list_agents":
		return Row{"agents": s.delegationAgents()}, nil
	case "fetch_url":
		return s.fetchURL(a)
	case "propose_task_split":
		return s.proposeTaskSplit(caller, a)
	case "resume_delegation":
		return s.resumeDelegation(caller, text(a["task_id"]), text(a["task"]))
	case "delegate_to_agent":
		if caller.Depth >= 4 {
			return nil, fmt.Errorf("委派深度达到 4 层上限")
		}
		agentType, task := text(a["agent_type"]), strings.TrimSpace(text(a["task"]))
		resolved, profileErr := s.delegationProfile(agentType, text(a["model"]))
		if profileErr != nil {
			return nil, profileErr
		}
		agentType = resolved
		if s.pick(agentType) == nil {
			return nil, fmt.Errorf("执行端不可用：%s", agentType)
		}
		if task == "" || len(task) > 120000 {
			return nil, fmt.Errorf("子任务必须包含完整目标，且不超过 120000 字节")
		}
		cwd, permission, mode, err := s.delegateScope(caller, agentType, text(a["working_dir"]))
		if err != nil {
			return nil, err
		}
		s.mu.Lock()
		parent := s.connections[caller.ConnectionID]
		if parent == nil {
			s.mu.Unlock()
			return nil, fmt.Errorf("父会话已断开")
		}
		parentConv := parent.ConversationID
		s.mu.Unlock()
		if taskID := text(a["task_id"]); taskID != "" {
			return s.resumeDelegation(caller, taskID, task)
		}
		s.mu.Lock()
		candidates := []string{}
		if a["new_task"] != true {
			for _, old := range s.tasks {
				if old.Root != caller.RootID || old.AgentType != agentType || old.Workdir != cwd || (old.ParentConversationID != parentConv && old.Parent != caller.ConnectionID) {
					continue
				}
				if old.Status == "running" && (old.Task == task || old.NextPrompt == task) {
					out := taskReport(old)
					s.mu.Unlock()
					return out, nil
				}
				if old.Status == "interrupted" || old.Status == "failed" || old.Status == "canceled" {
					candidates = append(candidates, old.ID)
				}
			}
		}
		s.mu.Unlock()
		if len(candidates) == 1 {
			return s.resumeDelegation(caller, candidates[0], task)
		}
		if len(candidates) > 1 {
			return nil, fmt.Errorf("存在多个未完成的原子任务 %v，请用 resume_delegation 指定 task_id 继续；确需新任务时传 new_task=true", candidates)
		}
		d := &Delegation{ID: id(), Parent: caller.ConnectionID, Root: caller.RootID, AgentType: agentType, Task: task, Workdir: cwd, Model: text(a["model"]), Permission: permission, Status: "running", Depth: caller.Depth + 1, Updated: make(chan struct{}), Inbox: []Row{}, ParentConversationID: parentConv}
		d.Mode = acp.DelegatedMode(s.pick(agentType), mode)
		d.Executing = true
		s.mu.Lock()
		if a["new_task"] != true {
			for _, old := range s.tasks {
				if old.Root == caller.RootID && old.Parent == caller.ConnectionID && old.AgentType == agentType && old.Workdir == cwd && old.Status == "running" && (old.Task == task || old.NextPrompt == task) {
					out := taskReport(old)
					s.mu.Unlock()
					return out, nil
				}
			}
		}
		d.ParentToolUseID = d.ID
		if parent := s.connections[caller.ConnectionID]; parent != nil {
			if parent.ToolClaims == nil {
				parent.ToolClaims = map[string]bool{}
			}
			for _, tool := range parent.Tools {
				toolID := text(tool["tool_call_id"])
				if parent.ToolClaims[toolID] || internalToolName(text(tool["title"])) != "delegate_to_agent" {
					continue
				}
				var args Row
				if json.Unmarshal([]byte(text(tool["raw_input"])), &args) == nil && args["task"] == task && args["agent_type"] == agentType {
					d.ParentToolUseID = toolID
					parent.ToolClaims[toolID] = true
					break
				}
			}
		}
		s.tasks[d.ID] = d
		if err := s.saveDelegationsLocked(); err != nil {
			delete(s.tasks, d.ID)
			s.mu.Unlock()
			return nil, err
		}
		s.mu.Unlock()
		go s.runDelegation(d)
		return Row{"task_id": d.ID, "status": "running"}, nil
	case "get_delegation_status":
		raw, _ := a["task_ids"].([]any)
		if len(raw) == 0 || len(raw) > 32 {
			return nil, fmt.Errorf("task_ids 需要 1–32 个任务 ID")
		}
		ids := []string{}
		for _, v := range raw {
			ids = append(ids, text(v))
		}
		wait, waiting := a["wait_ms"]
		ms := number(wait)
		if ms < 0 {
			return nil, fmt.Errorf("wait_ms 不能为负数")
		}
		if ms > 60000 {
			ms = 60000
		}
		var timeout <-chan time.Time
		var timer *time.Timer
		if waiting && ms > 0 {
			timer = time.NewTimer(time.Duration(ms) * time.Millisecond)
			defer timer.Stop()
			timeout = timer.C
		}
		ticker := time.NewTicker(100 * time.Millisecond)
		defer ticker.Stop()
		for {
			rows, finished := s.taskStatus(caller, ids)
			if !waiting || finished {
				return Row{"tasks": rows}, nil
			}
			select {
			case <-ctx.Done():
				return nil, ctx.Err()
			case <-timeout:
				return Row{"tasks": rows}, nil
			case <-ticker.C:
			}
		}
	case "cancel_delegation":
		tid := text(a["task_id"])
		s.mu.Lock()
		d := s.tasks[tid]
		if d == nil || d.Root != caller.RootID {
			s.mu.Unlock()
			return nil, fmt.Errorf("任务不存在")
		}
		cid := d.ConnectionID
		if d.Status != "running" {
			out := taskReport(d)
			s.mu.Unlock()
			return out, nil
		}
		d.Status = "canceled"
		_ = s.saveDelegationsLocked()
		s.mu.Unlock()
		if cid != "" {
			_ = s.cancel(cid)
		}
		return Row{"task_id": tid, "status": "canceled"}, nil
	case "check_user_feedback":
		messages := s.consumeFeedback(caller.ConnectionID)
		s.mu.Lock()
		for _, d := range s.tasks {
			if d.ConnectionID == caller.ConnectionID {
				messages = append(messages, d.Inbox...)
				d.Inbox = []Row{}
			}
		}
		s.mu.Unlock()
		rows, _ := s.taskStatus(caller, nil)
		return Row{"messages": messages, "team_tasks": rows}, nil
	case "send_agent_message":
		tid, body := text(a["task_id"]), strings.TrimSpace(text(a["message"]))
		if body == "" || len(body) > 12000 {
			return nil, fmt.Errorf("消息需要 1–12000 字节")
		}
		s.mu.Lock()
		d := s.tasks[tid]
		if d == nil || d.Root != caller.RootID {
			s.mu.Unlock()
			return nil, fmt.Errorf("目标任务不存在或不属于当前协作")
		}
		d.Inbox = append(d.Inbox, Row{"id": id(), "from": caller.ConnectionID, "text": body, "at": timestamp()})
		_ = s.saveDelegationsLocked()
		s.mu.Unlock()
		return Row{"delivered": true, "task_id": tid}, nil
	}
	return nil, fmt.Errorf("未知协作工具：%s", name)
}
func (s *Server) taskStatus(caller *Caller, ids []string) ([]Row, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	rows := []Row{}
	finished := false
	if ids == nil {
		for tid, d := range s.tasks {
			if d.Root == caller.RootID {
				ids = append(ids, tid)
			}
		}
	}
	for _, tid := range ids {
		d := s.tasks[tid]
		if d == nil || d.Root != caller.RootID {
			rows = append(rows, Row{"task_id": tid, "status": "unknown"})
			finished = true
			continue
		}
		row := taskReport(d)
		rows = append(rows, row)
		if d.Status != "running" {
			finished = true
		} else if c := s.connections[d.ConnectionID]; c != nil && c.Pending != nil {
			row["blocked_on"] = clone(c.Pending)
			finished = true
		}
	}
	return rows, finished
}

// Match Codeg's public DelegationTaskReport; internal routing and scope state
// belong to the broker and should not appear in model context or tool cards.
func taskReport(d *Delegation) Row {
	row := Row{"task_id": d.ID, "status": d.Status, "agent_type": d.AgentType, "task_preview": d.Task}
	if d.SplitID != "" {
		// Tells the main bot a task came off a confirmed split card; it can
		// still poll or resume it like any delegation.
		row["split_id"] = d.SplitID
	}
	if d.Status == "interrupted" {
		row["status"] = "failed"
		row["error_code"] = "interrupted"
	}
	if d.ConversationID != 0 {
		row["child_conversation_id"] = d.ConversationID
	}
	if d.Status == "completed" {
		row["text"] = d.Result
	}
	if d.Error != "" {
		row["message"] = d.Error
		if d.Status != "interrupted" {
			row["error_code"] = "subagent_error"
		}
	}
	if d.Status == "canceled" {
		row["error_code"] = "canceled"
	}
	return row
}
func (s *Server) runDelegation(d *Delegation) {
	defer func() { s.mu.Lock(); d.Executing = false; s.mu.Unlock() }()
	mode := d.Mode
	if mode == "" {
		mode = map[bool]string{true: "workspace-write", false: "plan"}[d.Permission == "workspace"]
	}
	args := Row{"agentType": d.AgentType, "workingDir": d.Workdir, "_depth": d.Depth, "_root": d.Root, "preferredModeId": mode, "preferredConfigValues": Row{"model": d.Model}}
	if d.NativeSessionID != "" {
		args["sessionId"] = d.NativeSessionID
	}
	cid, err := s.connect(args)
	if err != nil {
		s.mu.Lock()
		d.Status = "failed"
		d.Error = err.Error()
		_ = s.saveDelegationsLocked()
		s.mu.Unlock()
		return
	}
	s.mu.Lock()
	c := s.connections[cid]
	if s.tasks[d.ID] != d {
		s.mu.Unlock()
		_ = s.disconnect(cid)
		return
	}
	parent := s.connections[d.Parent]
	d.ConnectionID = cid
	c.Depth = d.Depth
	cancelled := d.Status == "canceled"
	s.mu.Unlock()
	if cancelled {
		return
	}
	st := s.store
	st.mu.Lock()
	f := st.openFolderLocked(d.Workdir, "regular")
	parentConv := 0
	if parent != nil {
		parentConv = parent.ConversationID
	}
	if d.ParentConversationID > 0 && st.conversationLocked(d.ParentConversationID) == nil {
		st.mu.Unlock()
		return
	}
	row := st.conversationLocked(d.ConversationID)
	if row == nil && d.ConversationID != 0 {
		st.mu.Unlock()
		s.mu.Lock()
		d.Status = "failed"
		d.Error = "原子对话已删除，不能自动另建"
		_ = s.saveDelegationsLocked()
		s.mu.Unlock()
		return
	}
	if row == nil {
		row = st.createConversationLocked(number(f["id"]), d.AgentType, d.Task, parentConv)
	}
	row["delegation_call_id"] = d.ID
	row["parent_tool_use_id"] = d.ParentToolUseID
	row["status"] = "in_progress"
	input := d.Task
	if d.ConversationID != 0 {
		input = "继续此前未完成的任务，沿用当前会话上下文与已有成果。"
		if d.NextPrompt != "" {
			input += "\n" + d.NextPrompt
		}
	}
	view := s.runtimeView(c.RuntimeID)
	row["external_id"] = text(view["nativeSessionId"])
	st.appendTurnLocked(number(row["id"]), "user", input)
	_ = st.saveLocked()
	s.changed(row)
	if parentRow := st.conversationLocked(parentConv); parentRow != nil {
		s.changed(parentRow)
	}
	conv, folder := number(row["id"]), number(f["id"])
	st.mu.Unlock()
	s.mu.Lock()
	c.ConversationID = conv
	c.FolderID = folder
	c.Busy = true
	c.Status = "prompting"
	c.RunSeq++
	c.RunAccepted = time.Now()
	d.ConversationID = conv
	d.NativeSessionID = text(view["nativeSessionId"])
	d.ParentConversationID = parentConv
	_ = s.saveDelegationsLocked()
	s.mu.Unlock()
	s.bindConversation(c, conv)
	s.event(d.Parent, Row{"type": "delegation_started", "parent_connection_id": d.Parent, "parent_tool_use_id": d.ParentToolUseID, "child_connection_id": cid, "child_conversation_id": conv, "agent_type": d.AgentType, "task_id": d.ID, "task_preview": d.Task, "split_id": d.SplitID})
	s.event(cid, Row{"type": "status_changed", "status": "prompting"})
	res := s.run(c, input)
	s.mu.Lock()
	if d.Status != "canceled" {
		if res.OK {
			d.Status = "completed"
			d.Result = res.Output
		} else {
			d.Status = "failed"
			d.Error = res.Error
		}
	}
	status := d.Status
	_ = s.saveDelegationsLocked()
	s.mu.Unlock()
	summary := Row{"kind": "ok", "duration_ms": res.Meta["durationMs"], "text_preview": res.Output}
	if status != "completed" {
		summary = Row{"kind": "err", "error_code": status}
	}
	s.event(d.Parent, Row{"type": "delegation_completed", "parent_connection_id": d.Parent, "parent_tool_use_id": d.ParentToolUseID, "child_connection_id": cid, "child_conversation_id": conv, "agent_type": d.AgentType, "task_id": d.ID, "result": summary, "split_id": d.SplitID})
}
