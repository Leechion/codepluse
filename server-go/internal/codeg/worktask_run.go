// Work task execution engine: claims queued tasks, runs them unattended in an
// isolated worktree conversation, settles the diff into review and lands the
// merge. Reuses internal/extra's worktrees (For/Diff/Merge/Discard) and the
// normal connect → prompt dialogue path.
package codeg

import (
	"fmt"
	"os/exec"
	"strings"
	"sync"
	"time"

	"harnesshub/internal/extra"
)

func taskWorktreeName(t *WorkTask) string { return fmt.Sprintf("task-%d", t.ID) }

// taskWorktrees returns the worktree manager rooted at the task's project.
func (s *Server) taskWorktrees(t *WorkTask) *extra.Worktrees {
	folderPath := t.folderPath
	if folderPath == "" {
		folderPath = text(s.folderRow(t.FolderID)["path"])
	}
	return extra.NewWorktrees(folderPath, "")
}

func (s *Server) folderRow(folderID int) Row {
	s.store.mu.Lock()
	defer s.store.mu.Unlock()
	return clone(s.store.folderLocked(folderID))
}

// decorateTask stamps derived read-only fields before returning a task.
func (s *Server) decorateTask(t *WorkTask) {
	if t.worktreePath != "" {
		if _, err := execGit(t.worktreePath, "status"); err != nil {
			t.WorktreeFolderID = nil
		}
	}
}

// gitTask runs a git command in a directory (trivial wrapper for the engine).
func execGit(cwd string, args ...string) (string, error) {
	if cwd == "" {
		return "", fmt.Errorf("no cwd")
	}
	cmd := exec.Command("git", args...)
	cmd.Dir = cwd
	out, err := cmd.Output()
	return string(out), err
}

// claimTask tries to move a queued task into execution, honouring the folder's
// concurrency limit. Safe to call from ticks and start buttons alike.
func (s *Server) claimTask(taskID int) {
	st := s.tasks2
	st.mu.Lock()
	t := st.find(taskID)
	if t == nil || t.Status != "queued" {
		st.mu.Unlock()
		return
	}
	folderID := t.FolderID
	settings := clone(st.settingsFor(folderID))
	running := 0
	for _, other := range st.Tasks {
		if other.FolderID == folderID && (other.Status == "preparing" || other.Status == "running" || other.Status == "awaiting_input" || other.Status == "merging") {
			running++
		}
	}
	max := settings.MaxConcurrent
	if max <= 0 {
		max = 1
	}
	if running >= max {
		st.mu.Unlock() // wait for the tick to retry when a slot frees up
		return
	}
	st.setStatus(t, "preparing", "engine", nil)
	t.folderPath = text(s.folderRow(folderID)["path"])
	st.mu.Unlock()
	st.save()

	if err := s.runTask(t); err != nil {
		st.mu.Lock()
		t.LastError = strPtr(err.Error())
		t.FailureReason = strPtr("setup_error")
		st.setStatus(t, "failed", "engine", Row{"error": err.Error()})
		st.save()
	}
}

// runTask drives one full run: worktree setup, conversation, settle.
func (s *Server) runTask(t *WorkTask) error {
	st := s.tasks2
	folderPath := t.folderPath
	if folderPath == "" {
		return fmt.Errorf("项目文件夹不存在")
	}
	wts := extra.NewWorktrees(folderPath, "")
	name := taskWorktreeName(t)
	checkout := folderPath
	if wts.Available(folderPath) {
		path, err := wts.For(name, folderPath)
		if err != nil {
			return fmt.Errorf("创建隔离工作树失败：%v", err)
		}
		checkout = path
		t.worktreePath = path
		if t.BaseBranch == nil {
			if branch, err := execGit(folderPath, "rev-parse", "--abbrev-ref", "HEAD"); err == nil {
				t.BaseBranch = strPtr(strings.TrimSpace(branch))
			}
		}
		if sha, err := execGit(folderPath, "rev-parse", "HEAD"); err == nil {
			t.BaseSHA = strPtr(strings.TrimSpace(sha))
		}
		t.WorkBranch = strPtr("hh/" + name)
	} else {
		st.addEvent(t, "notice", "engine", Row{"text": "非 git 仓库，回退共享目录执行", "folder_path": folderPath})
	}
	workBranch, _ := execGit(checkout, "rev-parse", "--abbrev-ref", "HEAD")
	if strings.TrimSpace(workBranch) != "" {
		t.WorkBranch = strPtr(strings.TrimSpace(workBranch))
	}

	agentType := s.resolvedAgent(t, st)
	st.mu.Lock()
	st.setStatus(t, "running", "engine", nil)
	st.mu.Unlock()
	st.save()

	cid, err := s.connect(Row{"agentType": agentType, "workingDir": checkout})
	if err != nil {
		return fmt.Errorf("连接执行端失败：%v", err)
	}
	st.mu.Lock()
	t.liveConn = cid
	t.ConnectionID = strPtr(cid)
	st.mu.Unlock()

	title := t.Title
	if len(title) > 80 {
		title = title[:80]
	}
	s.store.mu.Lock()
	row := s.store.createConversationLocked(t.FolderID, agentType, title, 0)
	convID := number(row["id"])
	s.store.mu.Unlock()
	st.mu.Lock()
	t.ConversationID = intPtr(convID)
	st.mu.Unlock()

	blocks := []any{Row{"type": "text", "text": firstTaskText(t.Config)}}
	if err := s.prompt(Row{"connectionId": cid, "conversationId": convID, "folderId": t.FolderID, "blocks": blocks}); err != nil {
		_ = s.disconnect(cid)
		st.mu.Lock()
		t.liveConn = ""
		st.mu.Unlock()
		return fmt.Errorf("启动对话失败：%v", err)
	}

	// Poll until the dialogue settles, surfacing permission-card waits.
	awaitingAnnounced := false
	deadline := time.Now().Add(24 * time.Hour)
	for {
		time.Sleep(2 * time.Second)
		st.mu.Lock()
		t2 := st.find(t.ID)
		canceled := t2 == nil || t2.cancelFlag
		st.mu.Unlock()
		if canceled {
			return nil
		}
		waiting := s.connectionBusy(cid)
		if waiting != awaitingAnnounced {
			awaitingAnnounced = waiting
			st.mu.Lock()
			if waiting {
				st.setStatus(t, "awaiting_input", "engine", nil)
			} else {
				st.setStatus(t, "running", "engine", nil)
			}
			st.mu.Unlock()
			st.save()
		}
		s.store.mu.Lock()
		row := s.store.conversationLocked(convID)
		status := ""
		if row != nil {
			status = text(row["status"])
		}
		s.store.mu.Unlock()
		if status == "pending_review" {
			_ = s.disconnect(cid)
			st.mu.Lock()
			t.liveConn = ""
			st.mu.Unlock()
			s.settleTask(t, convID)
			return nil
		}
		if status == "cancelled" || status == "failed" {
			_ = s.disconnect(cid)
			st.mu.Lock()
			t.liveConn = ""
			t.LastError = strPtr("会话执行失败或被取消")
			t.FailureReason = strPtr("agent_error")
			st.setStatus(t, "failed", "engine", nil)
			st.save()
			return nil
		}
		if time.Now().After(deadline) {
			_ = s.cancel(cid)
			_ = s.disconnect(cid)
			st.mu.Lock()
			t.liveConn = ""
			t.FailureReason = strPtr("agent_error")
			t.LastError = strPtr("运行超过 24 小时上限")
			st.setStatus(t, "failed", "engine", nil)
			st.save()
			return nil
		}
	}
}

// resolvedAgent picks the agent: task override → folder settings → folder default.
func (s *Server) resolvedAgent(t *WorkTask, st *taskStore) string {
	if t.Config != nil && t.Config.AgentType != nil {
		return *t.Config.AgentType
	}
	if settings := st.settingsFor(t.FolderID); settings.DefaultAgentType != nil {
		return *settings.DefaultAgentType
	}
	if row := s.folderRow(t.FolderID); text(row["default_agent_type"]) != "" {
		return text(row["default_agent_type"])
	}
	return "codex"
}

// connectionBusy reports whether the connection is waiting on a permission card.
func (s *Server) connectionBusy(cid string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	c := s.connections[cid]
	return c != nil && c.Busy && c.Pending != nil
}

// settleTask moves a finished dialogue into review and computes diff stats.
func (s *Server) settleTask(t *WorkTask, convID int) {
	st := s.tasks2
	if t.worktreePath != "" {
		if _, err := s.taskWorktrees(t).Diff(taskWorktreeName(t), t.folderPath); err == nil {
			summary := s.lastAssistantText(convID)
			if summary != "" {
				trimmed := summary
				if len(trimmed) > 2000 {
					trimmed = trimmed[:2000]
				}
				t.ResultSummary = strPtr(trimmed)
			}
			files, add, del := diffStats(t.folderPath, taskWorktreeName(t))
			t.FilesChanged = intPtr(files)
			t.Additions = intPtr(add)
			t.Deletions = intPtr(del)
		}
	}
	st.mu.Lock()
	st.setStatus(t, "review", "engine", nil)
	st.mu.Unlock()
	st.save()
	// Optional auto-merge mirrors the settings dialog's promise.
	if settings := st.settingsFor(t.FolderID); settings.AutoMerge {
		go s.processTaskMerge(t, "", false)
	}
}

// lastAssistantText pulls the final assistant message from a conversation.
func (s *Server) lastAssistantText(convID int) string {
	s.store.mu.Lock()
	defer s.store.mu.Unlock()
	turns := s.store.Turns[fmt.Sprint(convID)]
	for i := len(turns) - 1; i >= 0; i-- {
		if text(turns[i]["role"]) != "assistant" {
			continue
		}
		switch blocks := turns[i]["blocks"].(type) {
		case []Row:
			for _, b := range blocks {
				if text(b["type"]) == "text" {
					return text(b["text"])
				}
			}
		case []any:
			for _, raw := range blocks {
				if b, ok := raw.(Row); ok && text(b["type"]) == "text" {
					return text(b["text"])
				}
			}
		}
	}
	return ""
}

// diffStats summarises numstat for the task branch vs the base workdir.
func diffStats(folderPath, name string) (files, additions, deletions int) {
	out, err := execGit(folderPath, "diff", "--numstat", "HEAD", "hh/"+name)
	if err != nil {
		return 0, 0, 0
	}
	for _, line := range strings.Split(out, "\n") {
		parts := strings.Fields(line)
		if len(parts) >= 3 {
			files++
			if parts[0] != "-" {
				fmt.Sscanf(parts[0], "%d", &additions)
			}
			if parts[1] != "-" {
				fmt.Sscanf(parts[1], "%d", &deletions)
			}
		}
	}
	return files, additions, deletions
}

// ----------------------------- merge -----------------------------

var taskMergeLocks = map[string]*sync.Mutex{}

// dispatchTaskMerge runs or queues a merge; returns true when queued.
func (s *Server) dispatchTaskMerge(t *WorkTask, message string, deleteWorktree bool) bool {
	st := s.tasks2
	key := fmt.Sprint(t.FolderID)
	lock := taskMergeLocks[key]
	if lock == nil {
		lock = &sync.Mutex{}
		taskMergeLocks[key] = lock
	}
	if !lock.TryLock() {
		t.MergeQueued = Row{"message": message, "delete_worktree": deleteWorktree, "queued_at": timestamp()}
		st.save()
		s.emitTaskChanged(t.ID)
		return true
	}
	go func() {
		defer lock.Unlock()
		s.processTaskMerge(t, message, deleteWorktree)
	}()
	return false
}

// processTaskMerge lands the worktree branch onto the base and finishes the task.
func (s *Server) processTaskMerge(t *WorkTask, message string, deleteWorktree bool) {
	st := s.tasks2
	st.mu.Lock()
	st.setStatus(t, "merging", "engine", nil)
	st.mu.Unlock()
	st.save()

	folderPath := t.folderPath
	if folderPath == "" {
		folderPath = text(s.folderRow(t.FolderID)["path"])
	}
	wts := extra.NewWorktrees(folderPath, "")
	name := taskWorktreeName(t)

	// Squash strategy lands as one commit without a merge commit.
	// The dialogue has settled by now; drop the live connection and any pooled
	// driver first so the agent process does not pin the worktree directory.
	if t.liveConn != "" {
		_ = s.disconnect(t.liveConn)
		st.mu.Lock()
		t.liveConn = ""
		st.mu.Unlock()
	}
	if t.worktreePath != "" {
		s.Runtime.KillDriversAt(t.worktreePath)
	}
	if settings := st.settingsFor(t.FolderID); settings.MergeStrategy == "squash" {
		if err := squashMerge(folderPath, "hh/"+name, message); err != nil {
			st.mu.Lock()
			t.LastError = strPtr(err.Error())
			st.setStatus(t, "review", "engine", Row{"error": err.Error()})
			st.mu.Unlock()
			st.save()
			return
		}
	} else {
		if _, err := wts.Merge(name, folderPath, message); err != nil {
			st.mu.Lock()
			t.LastError = strPtr(err.Error())
			st.setStatus(t, "review", "engine", Row{"error": err.Error()})
			st.mu.Unlock()
			st.save()
			return
		}
	}
	if sha, err := execGit(folderPath, "rev-parse", "HEAD"); err == nil {
		t.MergeCommit = strPtr(strings.TrimSpace(sha))
	}
	kind := "merged"
	t.CompletionKind = &kind
	st.mu.Lock()
	st.setStatus(t, "done", "engine", nil)
	if deleteWorktree && t.worktreePath != "" {
		s.discardTaskWorktree(t)
	}
	st.mu.Unlock()
	st.save()
	// Pull the next queued merge of the same project, if any.
	for _, other := range st.Tasks {
		if other.FolderID == t.FolderID && other.MergeQueued != nil {
			msg := text(other.MergeQueued["message"])
			del := other.MergeQueued["delete_worktree"] == true
			other.MergeQueued = nil
			go func(task *WorkTask, m string, d bool) {
				s.processTaskMerge(task, m, d)
			}(other, msg, del)
			break
		}
	}
}

// squashMerge lands a branch as a single commit on the base workdir.
func squashMerge(folderPath, branch, message string) error {
	statusOut, err := execGit(folderPath, "status", "--porcelain", "--", ".", ":!.worktrees")
	if err != nil {
		return err
	}
	for _, line := range strings.Split(statusOut, "\n") {
		if strings.TrimSpace(line) != "" && !strings.Contains(line, ".worktrees") {
			return fmt.Errorf("主工作区有未提交改动，请先提交或暂存后再合并")
		}
	}
	target := folderPath + string(filepathSep) + ".worktrees" + string(filepathSep) + strings.TrimPrefix(branch, "hh/")
	if _, err := execGit(target, "add", "-A"); err != nil {
		return err
	}
	_, _ = execGit(target, "commit", "-m", "Agent work (auto-committed before review merge)")
	if message == "" {
		message = "Squash merge " + branch
	}
	if _, err := execGit(folderPath, "merge", "--squash", branch); err != nil {
		return fmt.Errorf("squash 合并失败（可能需要先处理冲突）")
	}
	if _, err := execGit(folderPath, "commit", "-m", message); err != nil {
		return fmt.Errorf("提交合并结果失败")
	}
	_, err = execGit(folderPath, "worktree", "remove", "--force", target)
	if err == nil {
		_, _ = execGit(folderPath, "branch", "-D", branch)
	}
	return nil
}

// discardTaskWorktree removes the checkout and its branch (best effort).
func (s *Server) discardTaskWorktree(t *WorkTask) error {
	if t.liveConn != "" {
		_ = s.disconnect(t.liveConn)
		t.liveConn = ""
	}
	if t.worktreePath != "" {
		s.Runtime.KillDriversAt(t.worktreePath)
	}
	folderPath := t.folderPath
	if folderPath == "" {
		folderPath = text(s.folderRow(t.FolderID)["path"])
	}
	_, err := extra.NewWorktrees(folderPath, "").Discard(taskWorktreeName(t), folderPath)
	t.worktreePath = ""
	t.WorktreeFolderID = nil
	t.WorkBranch = nil
	return err
}

// taskChangedFiles lists files touched by the task worktree.
func (s *Server) taskChangedFiles(t *WorkTask) (any, error) {
	if t.worktreePath == "" {
		return []Row{}, nil
	}
	out, err := execGit(t.folderPath, "diff", "--numstat", "HEAD", "hh/"+taskWorktreeName(t))
	if err != nil {
		return []Row{}, nil
	}
	files := []Row{}
	for _, line := range strings.Split(out, "\n") {
		parts := strings.Fields(line)
		if len(parts) >= 3 {
			files = append(files, Row{"file": parts[2], "additions": parts[0], "deletions": parts[1]})
		}
	}
	return files, nil
}

// TickWorkTasks claims scheduled todos and re-tries queued ones; called from
// the server's 30s loop.
func (s *Server) TickWorkTasks() {
	st := s.tasks2
	st.mu.Lock()
	items := append([]*WorkTask{}, st.Tasks...)
	st.mu.Unlock()
	now := time.Now()
	for _, t := range items {
		if t.Status == "todo" && t.ScheduledAt != nil {
			if due, err := time.Parse(time.RFC3339, *t.ScheduledAt); err == nil && due.Before(now) {
				st.mu.Lock()
				if t.Status == "todo" {
					t.ScheduledAt = nil
					st.setStatus(t, "queued", "scheduler", nil)
					st.save()
					st.mu.Unlock()
					s.claimTask(t.ID)
					continue
				}
				st.mu.Unlock()
			}
		}
		if t.Status == "queued" {
			s.claimTask(t.ID)
		}
	}
}
