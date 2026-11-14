package codeg

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// Stable conversation identities survive reconnects and backend restarts.
func (s *Store) treeRootLocked(conv int) (int, int) {
	depth := 0
	seen := map[int]bool{}
	for !seen[conv] {
		seen[conv] = true
		row := s.conversationLocked(conv)
		if row == nil {
			return conv, depth
		}
		parent := number(row["parent_id"])
		if parent == 0 {
			return conv, depth
		}
		conv = parent
		depth++
	}
	return conv, depth
}
func (s *Server) bindConversation(c *Connection, conv int) {
	if conv == 0 {
		return
	}
	st := s.store
	st.mu.Lock()
	row := st.conversationLocked(conv)
	root, depth := st.treeRootLocked(conv)
	folder := 0
	if row != nil {
		folder = number(row["folder_id"])
	}
	st.mu.Unlock()
	s.mu.Lock()
	defer s.mu.Unlock()
	oldRoot := c.RootID
	if oldRoot == "" {
		oldRoot = c.ID
	}
	c.ConversationID = conv
	c.FolderID = folder
	if c.Depth == 0 || c.RootID == "" {
		c.RootID = fmt.Sprintf("conversation:%d", root)
		c.Depth = depth
	}
	if caller := s.callers[c.ID]; caller != nil {
		caller.RootID = c.RootID
		caller.Depth = depth
	}
	for _, d := range s.tasks {
		if d.Root == oldRoot {
			d.Root = c.RootID
		}
		if d.Parent == c.ID {
			d.ParentConversationID = conv
		}
	}
}
func (s *Server) saveDelegationsLocked() error {
	dir := filepath.Join(s.Root, ".data")
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}
	b, err := json.Marshal(s.tasks)
	if err != nil {
		return err
	}
	file := filepath.Join(dir, "codeg-delegations.json")
	if err = os.WriteFile(file+".tmp", b, 0600); err != nil {
		return err
	}
	return os.Rename(file+".tmp", file)
}
func (s *Server) loadDelegations() error {
	b, err := os.ReadFile(filepath.Join(s.Root, ".data", "codeg-delegations.json"))
	if err == nil {
		if err = json.Unmarshal(b, &s.tasks); err != nil {
			return err
		}
	} else if !os.IsNotExist(err) {
		return err
	}
	if s.tasks == nil {
		s.tasks = map[string]*Delegation{}
	}
	for taskID, d := range s.tasks {
		if (d.ParentConversationID > 0 && s.store.conversationLocked(d.ParentConversationID) == nil) || (d.ConversationID > 0 && s.store.conversationLocked(d.ConversationID) == nil) {
			delete(s.tasks, taskID)
			continue
		}
		d.Updated = make(chan struct{})
		d.ConnectionID = ""
		if d.Status == "running" {
			d.Status = "interrupted"
			d.Error = "后台重启，等待在原会话继续"
		}
	}
	// Upgrade existing child history without opening new conversations.
	st := s.store
	for _, row := range st.Conversations {
		taskID := text(row["delegation_call_id"])
		parent := number(row["parent_id"])
		if taskID == "" || parent == 0 || s.tasks[taskID] != nil {
			continue
		}
		root, depth := st.treeRootLocked(number(row["id"]))
		folder := st.folderLocked(number(row["folder_id"]))
		cwd := ""
		if folder != nil {
			cwd = text(folder["path"])
		}
		status := "interrupted"
		result := ""
		if text(row["status"]) == "pending_review" {
			status = "completed"
			for _, turn := range st.turnsLocked(number(row["id"])) {
				if turn["role"] == "assistant" {
					blocks, _ := turn["blocks"].([]any)
					for _, raw := range blocks {
						block, _ := raw.(map[string]any)
						if block["type"] == "text" {
							result = text(block["text"])
						}
					}
				}
			}
		}
		s.tasks[taskID] = &Delegation{ID: taskID, Root: fmt.Sprintf("conversation:%d", root), AgentType: text(row["agent_type"]), Task: text(row["title"]), Workdir: cwd, ParentConversationID: parent, ConversationID: number(row["id"]), NativeSessionID: text(row["external_id"]), Depth: depth, Status: status, Result: result, ParentToolUseID: text(row["parent_tool_use_id"]), Updated: make(chan struct{})}
	}
	return nil
}
func (s *Server) delegationContext(c *Connection) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	var b strings.Builder
	for _, d := range s.tasks {
		if d.ParentConversationID != 0 && d.ParentConversationID == c.ConversationID {
			fmt.Fprintf(&b, "任务 %s；执行端 %s；状态 %s；原子对话 %d；目标：%s\n", d.ID, d.AgentType, d.Status, d.ConversationID, d.Task)
		}
	}
	if b.Len() == 0 {
		return ""
	}
	return "已有子任务（请先查询状态；中断、失败或取消的任务用 resume_delegation 在原对话继续，不要重新建同一子任务）：\n" + b.String() + "\n"
}
func (s *Server) resumeDelegation(caller *Caller, taskID, instruction string) (any, error) {
	s.mu.Lock()
	d := s.tasks[taskID]
	parent := s.connections[caller.ConnectionID]
	if d == nil || parent == nil || d.Root != caller.RootID || (d.ParentConversationID != 0 && d.ParentConversationID != parent.ConversationID) {
		s.mu.Unlock()
		return nil, fmt.Errorf("任务不存在或不属于当前父对话")
	}
	if d.Status == "running" {
		out := taskReport(d)
		s.mu.Unlock()
		return out, nil
	}
	if d.Executing {
		s.mu.Unlock()
		return nil, fmt.Errorf("原子任务仍在停止，请稍后继续")
	}
	if c := s.connections[d.ConnectionID]; c != nil && c.Busy {
		s.mu.Unlock()
		return nil, fmt.Errorf("原子会话正在执行，请等待当前调用结束")
	}
	if d.ConversationID > 0 && d.NativeSessionID == "" {
		s.mu.Unlock()
		return nil, fmt.Errorf("原子对话没有保存原生会话 ID，无法安全恢复；请打开原子对话检查")
	}
	previous := *d
	d.Parent = caller.ConnectionID
	d.Status = "running"
	d.Error = ""
	d.NextPrompt = instruction
	d.Executing = true
	if err := s.saveDelegationsLocked(); err != nil {
		*d = previous
		s.mu.Unlock()
		return nil, err
	}
	out := taskReport(d)
	s.mu.Unlock()
	go s.runDelegation(d)
	return out, nil
}
