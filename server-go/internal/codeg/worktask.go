// Work tasks: Codeg's unattended to-do board (snake_case wire format, see
// codeg-ui/src/lib/types.ts Work tasks). A task captures a composer snapshot,
// runs unattended in an isolated git worktree (reusing internal/extra's
// worktrees), settles into review with a diff, and lands via merge, PR
// delivery or explicit acceptance.
package codeg

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"sync"
)

type WorkTaskConfig struct {
	PromptBlocks  []Row             `json:"prompt_blocks"`
	DisplayText   string            `json:"display_text"`
	AgentType     *string           `json:"agent_type"`
	ModeID        *string           `json:"mode_id"`
	ConfigValues  map[string]string `json:"config_values"`
	LabelSnapshot Row               `json:"label_snapshot,omitempty"`
	BaseBranch    *string           `json:"base_branch"`
}

type WorkTask struct {
	ID               int            `json:"id"`
	FolderID         int            `json:"folder_id"`
	Title            string         `json:"title"`
	Config           *WorkTaskConfig `json:"config"`
	Status           string         `json:"status"`
	FailureReason    *string        `json:"failure_reason"`
	LastError        *string        `json:"last_error"`
	RunSeq           int            `json:"run_seq"`
	SortOrder        int            `json:"sort_order"`
	WorktreeFolderID *int           `json:"worktree_folder_id"`
	AgentType        *string        `json:"agent_type"`
	ConversationID   *int           `json:"conversation_id"`
	ConnectionID     *string        `json:"connection_id"`
	BaseBranch       *string        `json:"base_branch"`
	BaseSHA          *string        `json:"base_sha"`
	WorkBranch       *string        `json:"work_branch"`
	CleanupState     *string        `json:"cleanup_state"`
	Verdict          *string        `json:"verdict"`
	ResultSummary    *string        `json:"result_summary"`
	FilesChanged     *int           `json:"files_changed"`
	Additions        *int           `json:"additions"`
	Deletions        *int           `json:"deletions"`
	MergeCommit      *string        `json:"merge_commit"`
	CompletionKind   *string        `json:"completion_kind"`
	Preflight        Row            `json:"preflight"`
	MergeQueued      Row            `json:"merge_queued,omitempty"`
	ArchivedAt       *string        `json:"archived_at"`
	ScheduledAt      *string        `json:"scheduled_at"`
	DeferredAt       *string        `json:"deferred_at"`
	SourceKind       *string        `json:"source_kind"`
	SourceKey        *string        `json:"source_key"`
	SourceMeta       Row            `json:"source_meta"`
	Delivery         Row            `json:"delivery"`
	LatestProgress   *string        `json:"latest_progress"`
	CreatedAt        string         `json:"created_at"`
	UpdatedAt        string         `json:"updated_at"`
	StartedAt        *string        `json:"started_at"`
	SettledAt        *string        `json:"settled_at"`
	FinishedAt       *string        `json:"finished_at"`

	// Engine-only state: the worktree checkout path and the live connection.
	worktreePath string `json:"-"`
	folderPath   string `json:"-"`
	liveConn     string `json:"-"`
	cancelFlag   bool   `json:"-"`
}

type WorkTaskEvent struct {
	ID        int    `json:"id"`
	TaskID    int    `json:"task_id"`
	Kind      string `json:"kind"`
	Actor     string `json:"actor"`
	Payload   Row    `json:"payload"`
	CreatedAt string `json:"created_at"`
}

type WorkTaskTemplate struct {
	ID        int             `json:"id"`
	Name      string          `json:"name"`
	Title     string          `json:"title"`
	Config    *WorkTaskConfig `json:"config"`
	CreatedAt string          `json:"created_at"`
	UpdatedAt string          `json:"updated_at"`
}

type WorkTaskFolderSettings struct {
	DefaultAgentType *string           `json:"default_agent_type"`
	ModeID           *string           `json:"mode_id"`
	ConfigValues     map[string]string `json:"config_values"`
	LabelSnapshot    Row               `json:"label_snapshot"`
	AutoProcess      bool              `json:"auto_process"`
	MaxConcurrent    int               `json:"max_concurrent"`
	MergeStrategy    string            `json:"merge_strategy"`
	AutoMerge        bool              `json:"auto_merge"`
}

func defaultFolderSettings() *WorkTaskFolderSettings {
	return &WorkTaskFolderSettings{ConfigValues: map[string]string{}, AutoProcess: false, MaxConcurrent: 1, MergeStrategy: "merge", AutoMerge: false}
}

// taskStore persists the board: tasks, per-task events, templates and the
// per-folder settings overrides in one JSON file.
type taskStore struct {
	mu        sync.Mutex
	saveMu    sync.Mutex
	root      string
	onChange  func(taskID int)
	Next      int                                `json:"next"`
	NextEvent int                                `json:"nextEvent"`
	NextTpl   int                                `json:"nextTemplate"`
	Tasks     []*WorkTask                        `json:"tasks"`
	Events    map[string][]*WorkTaskEvent        `json:"events"`
	Templates []*WorkTaskTemplate                `json:"templates"`
	Settings  map[string]*WorkTaskFolderSettings `json:"settings"`
}

func newTaskStore(root string) *taskStore {
	st := &taskStore{root: root, Next: 1, NextEvent: 1, NextTpl: 1,
		Events: map[string][]*WorkTaskEvent{}, Settings: map[string]*WorkTaskFolderSettings{}}
	if raw, err := readJSONFileData(root + string(filepathSep) + ".data" + string(filepathSep) + "codeg-worktasks.json"); err == nil {
		if json.Unmarshal(raw, st) == nil {
			for _, task := range st.Tasks {
				if task.Status == "running" || task.Status == "preparing" || task.Status == "merging" {
					task.Status = "failed"
					reason := "interrupted"
					task.FailureReason = &reason
					task.LastError = strPtr("服务重启，任务中断")
				}
				task.liveConn = ""
				task.cancelFlag = false
			}
		}
	}
	return st
}

func (st *taskStore) save() {
	st.saveMu.Lock()
	defer st.saveMu.Unlock()
	_ = atomicWrite(st.root+string(filepathSep)+".data"+string(filepathSep)+"codeg-worktasks.json", st)
}

func (st *taskStore) find(id int) *WorkTask {
	for _, t := range st.Tasks {
		if t.ID == id {
			return t
		}
	}
	return nil
}

func (st *taskStore) addEvent(task *WorkTask, kind, actor string, payload Row) {
	event := &WorkTaskEvent{ID: st.NextEvent, TaskID: task.ID, Kind: kind, Actor: actor, Payload: payload, CreatedAt: timestamp()}
	st.NextEvent++
	key := fmt.Sprint(task.ID)
	st.Events[key] = append(st.Events[key], event)
	if len(st.Events[key]) > 500 {
		st.Events[key] = st.Events[key][len(st.Events[key])-500:]
	}
}

func (st *taskStore) setStatus(task *WorkTask, status, actor string, payload Row) {
	task.Status = status
	task.UpdatedAt = timestamp()
	now := timestamp()
	switch status {
	case "running", "preparing", "awaiting_input", "merging":
		if task.StartedAt == nil {
			task.StartedAt = &now
		}
		task.FinishedAt = nil
	case "done", "failed", "canceled":
		task.FinishedAt = &now
	}
	st.addEvent(task, "status", actor, payload)
	if st.onChange != nil {
		st.onChange(task.ID)
	}
}

func (st *taskStore) settingsFor(folderID int) *WorkTaskFolderSettings {
	if own, ok := st.Settings[fmt.Sprint(folderID)]; ok && own != nil {
		return own
	}
	return defaultFolderSettings()
}

// ----------------------------- commands -----------------------------

func (s *Server) workTaskCommand(command string, a Row) (any, error) {
	st := s.tasks2
	st.mu.Lock()
	defer st.mu.Unlock()
	switch command {
	case "work_task_list":
		folderID := number(a["folderId"])
		out := []*WorkTask{}
		for _, t := range st.Tasks {
			if folderID != 0 && t.FolderID != folderID {
				continue
			}
			s.decorateTask(t)
			out = append(out, t)
		}
		sort.SliceStable(out, func(i, j int) bool { return out[i].SortOrder < out[j].SortOrder })
		return out, nil
	case "work_task_get":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		s.decorateTask(t)
		return t, nil
	case "work_task_events":
		t := st.find(number(a["taskId"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		limit := number(a["limit"])
		if limit <= 0 || limit > 500 {
			limit = 500
		}
		events := st.Events[fmt.Sprint(t.ID)]
		if len(events) > limit {
			events = events[len(events)-limit:]
		}
		out := []*WorkTaskEvent{}
		for _, e := range events {
			out = append(out, e)
		}
		return out, nil
	case "work_task_create":
		draft, _ := a["draft"].(Row)
		folderID := number(draft["folder_id"])
		st.mu.Unlock()
		s.store.mu.Lock()
		folder := s.store.folderLocked(folderID)
		s.store.mu.Unlock()
		st.mu.Lock()
		if folder == nil {
			return nil, fmt.Errorf("项目文件夹不存在")
		}
		config, err := parseTaskConfig(draft["config"])
		if err != nil {
			return nil, err
		}
		title := strings.TrimSpace(text(draft["title"]))
		if title == "" {
			title = firstTaskText(config)
		}
		if title == "" {
			return nil, fmt.Errorf("请填写任务标题或提示词")
		}
		task := &WorkTask{ID: st.Next, FolderID: folderID, Title: title, Config: config, Status: "todo",
			SortOrder: len(st.Tasks) + 1, CreatedAt: timestamp(), UpdatedAt: timestamp()}
		st.Next++
		st.Tasks = append(st.Tasks, task)
		st.addEvent(task, "created", "user", Row{"title": title})
		st.save()
		s.emitTaskChanged(task.ID)
		return task, nil
	case "work_task_update":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if t.Status != "todo" {
			return nil, fmt.Errorf("仅待办状态的任务可以编辑")
		}
		draft, _ := a["draft"].(Row)
		if title := strings.TrimSpace(text(draft["title"])); title != "" {
			t.Title = title
		}
		if raw, ok := draft["config"]; ok {
			config, err := parseTaskConfig(raw)
			if err != nil {
				return nil, err
			}
			t.Config = config
		}
		t.UpdatedAt = timestamp()
		st.addEvent(t, "updated", "user", nil)
		st.save()
		s.emitTaskChanged(t.ID)
		return t, nil
	case "work_task_reorder":
		folderID := number(a["folderId"])
		ids, _ := a["orderedIds"].([]any)
		order := map[int]int{}
		for i, v := range ids {
			order[number(v)] = i + 1
		}
		for _, t := range st.Tasks {
			if t.FolderID == folderID {
				if pos, ok := order[t.ID]; ok {
					t.SortOrder = pos
				}
			}
		}
		st.save()
		return nil, nil
	case "work_task_delete":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if deleteWorktree, _ := a["deleteWorktree"].(bool); deleteWorktree && t.worktreePath != "" {
			s.discardTaskWorktree(t)
		}
		filtered := st.Tasks[:0]
		for _, item := range st.Tasks {
			if item.ID != t.ID {
				filtered = append(filtered, item)
			}
		}
		st.Tasks = filtered
		delete(st.Events, fmt.Sprint(t.ID))
		st.save()
		s.emitTaskChanged(t.ID)
		return nil, nil
	case "work_task_start":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if t.Status != "todo" {
			return nil, fmt.Errorf("仅待办任务可以开始（当前 %s）", t.Status)
		}
		st.setStatus(t, "queued", "user", nil)
		st.save()
		go s.claimTask(t.ID)
		return nil, nil
	case "work_task_retry", "work_task_requeue":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if command == "work_task_retry" && t.Status != "failed" {
			return nil, fmt.Errorf("仅失败任务可以重试（当前 %s）", t.Status)
		}
		if command == "work_task_requeue" && t.Status != "canceled" {
			return nil, fmt.Errorf("仅已取消任务可以重新排队（当前 %s）", t.Status)
		}
		if note := text(a["note"]); note != "" {
			st.addEvent(t, "note", "user", Row{"text": note})
			t.Config = appendTaskNote(t.Config, note)
		}
		target := "todo"
		if command == "work_task_retry" {
			target = "queued"
		}
		t.LastError = nil
		t.FailureReason = nil
		st.setStatus(t, target, "user", nil)
		st.save()
		if target == "queued" {
			go s.claimTask(t.ID)
		}
		return nil, nil
	case "work_task_schedule":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if text(a["scheduledAt"]) == "" {
			t.ScheduledAt = nil
		} else {
			t.ScheduledAt = strPtr(text(a["scheduledAt"]))
		}
		t.UpdatedAt = timestamp()
		st.save()
		s.emitTaskChanged(t.ID)
		return nil, nil
	case "work_task_return":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if t.Status != "review" && t.Status != "failed" {
			return nil, fmt.Errorf("仅评审或失败状态可以追加反馈（当前 %s）", t.Status)
		}
		feedback := strings.TrimSpace(text(a["feedback"]))
		if feedback == "" {
			return nil, fmt.Errorf("请填写反馈内容")
		}
		t.Config = appendTaskNote(t.Config, feedback)
		t.LastError = nil
		t.FailureReason = nil
		st.setStatus(t, "queued", "user", Row{"feedback": feedback})
		st.save()
		go s.claimTask(t.ID)
		return nil, nil
	case "work_task_cancel":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if t.Status == "done" || t.Status == "canceled" {
			return nil, fmt.Errorf("任务已结束（当前 %s）", t.Status)
		}
		if t.liveConn != "" {
			_ = s.cancel(t.liveConn)
		}
		t.cancelFlag = true
		if deleteWorktree, _ := a["deleteWorktree"].(bool); deleteWorktree && t.worktreePath != "" {
			s.discardTaskWorktree(t)
		}
		reason := text(a["reason"])
		st.setStatus(t, "canceled", "user", Row{"reason": reason})
		st.save()
		return nil, nil
	case "work_task_merge":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if t.Status != "review" {
			return nil, fmt.Errorf("仅评审状态可以合并（当前 %s）", t.Status)
		}
		queued := s.dispatchTaskMerge(t, text(a["message"]), a["deleteWorktree"] == true)
		return queued, nil
	case "work_task_merge_unqueue":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		t.MergeQueued = nil
		st.save()
		s.emitTaskChanged(t.ID)
		return nil, nil
	case "work_task_deliver_pr":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		return s.deliverTaskPr(t, text(a["prTitle"]), a["draft"] == true, a["deleteWorktree"] == true, a["markDone"] == true)
	case "work_task_complete":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if t.Status != "review" {
			return nil, fmt.Errorf("仅评审状态可以完成（当前 %s）", t.Status)
		}
		if t.worktreePath != "" {
			if diff, err := s.taskWorktrees(t).Diff(taskWorktreeName(t), t.folderPath); err == nil && diff["hasDiff"] == true {
				return nil, fmt.Errorf("工作树仍有未合并改动，请合并或丢弃后再完成")
			}
		}
		kind := "accepted_without_merge"
		t.CompletionKind = &kind
		st.setStatus(t, "done", "user", nil)
		if deleteWorktree, _ := a["deleteWorktree"].(bool); deleteWorktree && t.worktreePath != "" {
			s.discardTaskWorktree(t)
		}
		st.save()
		return nil, nil
	case "work_task_archive":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if a["archived"] == true {
			t.ArchivedAt = strPtr(timestamp())
		} else {
			t.ArchivedAt = nil
		}
		t.UpdatedAt = timestamp()
		st.save()
		s.emitTaskChanged(t.ID)
		return nil, nil
	case "work_task_defer":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if a["deferred"] == true {
			t.DeferredAt = strPtr(timestamp())
		} else {
			t.DeferredAt = nil
		}
		t.UpdatedAt = timestamp()
		st.save()
		s.emitTaskChanged(t.ID)
		return nil, nil
	case "work_task_cleanup":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if t.worktreePath == "" {
			return nil, fmt.Errorf("该任务没有隔离工作树")
		}
		if err := s.discardTaskWorktree(t); err != nil {
			t.CleanupState = strPtr("failed")
			st.save()
			return nil, err
		}
		t.CleanupState = nil
		st.save()
		s.emitTaskChanged(t.ID)
		return nil, nil
	case "work_task_diff":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		if t.worktreePath == "" {
			return "", nil
		}
		diff, err := s.taskWorktrees(t).Diff(taskWorktreeName(t), t.folderPath)
		if err != nil {
			return nil, err
		}
		if file := text(a["file"]); file != "" {
			return filterTaskDiff(diff["text"].(string), file), nil
		}
		return diff["text"], nil
	case "work_task_changed_files":
		t := st.find(number(a["id"]))
		if t == nil {
			return nil, fmt.Errorf("任务不存在")
		}
		return s.taskChangedFiles(t)
	case "work_task_settings_get", "work_task_settings_effective":
		return st.settingsFor(number(a["folderId"])), nil
	case "work_task_settings_get_own":
		if own, ok := st.Settings[fmt.Sprint(number(a["folderId"]))]; ok && own != nil {
			return own, nil
		}
		return nil, nil
	case "work_task_settings_set":
		folderID := number(a["folderId"])
		raw, _ := a["settings"].(Row)
		settings := parseFolderSettings(raw)
		st.Settings[fmt.Sprint(folderID)] = settings
		st.save()
		return nil, nil
	case "work_task_settings_delete":
		delete(st.Settings, fmt.Sprint(number(a["folderId"])))
		st.save()
		return nil, nil
	case "work_task_template_list":
		return st.Templates, nil
	case "work_task_template_save":
		draft, _ := a["draft"].(Row)
		name := strings.TrimSpace(text(draft["name"]))
		if name == "" {
			return nil, fmt.Errorf("请填写模板名称")
		}
		config, err := parseTaskConfig(draft["config"])
		if err != nil {
			return nil, err
		}
		var tpl *WorkTaskTemplate
		for _, existing := range st.Templates {
			if existing.Name == name {
				tpl = existing
			}
		}
		if tpl == nil {
			tpl = &WorkTaskTemplate{ID: st.NextTpl, CreatedAt: timestamp()}
			st.NextTpl++
			st.Templates = append(st.Templates, tpl)
		}
		tpl.Name = name
		tpl.Title = strings.TrimSpace(text(draft["title"]))
		tpl.Config = config
		tpl.UpdatedAt = timestamp()
		st.save()
		return tpl, nil
	case "work_task_template_delete":
		id := number(a["id"])
		kept := st.Templates[:0]
		for _, tpl := range st.Templates {
			if tpl.ID != id {
				kept = append(kept, tpl)
			}
		}
		st.Templates = kept
		st.save()
		return nil, nil
	case "work_task_lookup_by_source":
		keys, _ := a["sourceKeys"].([]any)
		want := map[string]bool{}
		for _, k := range keys {
			want[text(k)] = true
		}
		out := []Row{}
		for _, t := range st.Tasks {
			if t.SourceKey != nil && want[*t.SourceKey] {
				out = append(out, Row{"source_key": *t.SourceKey, "task_id": t.ID, "status": t.Status})
			}
		}
		return out, nil
	case "work_task_create_from_forge":
		draft, _ := a["draft"].(Row)
		return s.createTaskFromForge(draft)
	}
	return nil, fmt.Errorf("未知的任务命令：%s", command)
}

// emitTaskChanged notifies the board over WebSocket.
func (s *Server) emitTaskChanged(taskID int) {
	s.emit("task://changed", Row{"id": taskID})
}

// deliverTaskPr pushes the work branch and opens a GitHub pull request.
// Wired to forge after Phase 3; until then it reports clearly.
func (s *Server) deliverTaskPr(t *WorkTask, title string, draft, deleteWorktree, markDone bool) (any, error) {
	return nil, fmt.Errorf("PR 交付尚未接通（Forge 模块完成后可用）")
}

// createTaskFromForge mints a task from a forge issue/PR snapshot.
func (s *Server) createTaskFromForge(draft Row) (any, error) {
	folderID := number(draft["folder_id"])
	source, _ := draft["source"].(Row)
	if source == nil {
		return nil, fmt.Errorf("缺少来源信息")
	}
	snapshot, _ := draft["snapshot"].(Row)
	title := text(snapshot["title"])
	prompt := buildForgePrompt(source, snapshot)
	config := &WorkTaskConfig{PromptBlocks: []Row{{"type": "text", "text": prompt}}, DisplayText: prompt, ConfigValues: map[string]string{}}
	st := s.tasks2
	if s.store.folderLocked(folderID) == nil {
		return nil, fmt.Errorf("项目文件夹不存在")
	}
	task := &WorkTask{ID: st.Next, FolderID: folderID, Title: title, Config: config, Status: "todo",
		SortOrder: len(st.Tasks) + 1, CreatedAt: timestamp(), UpdatedAt: timestamp()}
	st.Next++
	kind := text(source["kind"])
	task.SourceKind = strPtr("forge_" + kind)
	task.SourceKey = strPtr(fmt.Sprintf("github:%s:%s:%s:%d", text(source["server_host"]), text(source["owner_repo"]), kind, number(source["number"])))
	task.SourceMeta = Row{"provider": text(source["provider"]), "server_host": text(source["server_host"]), "owner_repo": text(source["owner_repo"]), "number": number(source["number"]), "title": title, "url": text(snapshot["url"])}
	st.Tasks = append(st.Tasks, task)
	st.addEvent(task, "created", "forge", Row{"title": title})
	st.save()
	s.emitTaskChanged(task.ID)
	return task, nil
}

// buildForgePrompt renders a forge issue/PR into the task prompt.
func buildForgePrompt(source, snapshot Row) string {
	var out strings.Builder
	kind := "议题"
	if text(source["kind"]) == "pr" {
		kind = "拉取请求"
	}
	out.WriteString("处理 " + text(source["owner_repo"]) + " 的" + kind + " #" + fmt.Sprint(number(source["number"])) + "：" + text(snapshot["title"]) + "\n\n")
	if body := text(snapshot["body"]); body != "" {
		out.WriteString(body + "\n")
	}
	return out.String()
}

// parseTaskConfig normalizes the composer snapshot.
func parseTaskConfig(raw any) (*WorkTaskConfig, error) {
	config := &WorkTaskConfig{PromptBlocks: []Row{}, ConfigValues: map[string]string{}}
	m, ok := raw.(Row)
	if !ok {
		return config, nil
	}
	if blocks, ok := m["prompt_blocks"].([]any); ok {
		for _, b := range blocks {
			if row, ok := b.(Row); ok {
				config.PromptBlocks = append(config.PromptBlocks, row)
			}
		}
	}
	config.DisplayText = text(m["display_text"])
	if v, ok := m["agent_type"].(string); ok && v != "" {
		config.AgentType = strPtr(v)
	}
	if v, ok := m["mode_id"].(string); ok && v != "" {
		config.ModeID = strPtr(v)
	}
	if values, ok := m["config_values"].(map[string]any); ok {
		for k, v := range values {
			config.ConfigValues[k] = fmt.Sprint(v)
		}
	}
	if label, ok := m["label_snapshot"].(Row); ok {
		config.LabelSnapshot = label
	}
	if v, ok := m["base_branch"].(string); ok && v != "" {
		config.BaseBranch = strPtr(v)
	}
	return config, nil
}

func firstTaskText(config *WorkTaskConfig) string {
	if config == nil {
		return ""
	}
	if config.DisplayText != "" {
		return config.DisplayText
	}
	var out strings.Builder
	for _, block := range config.PromptBlocks {
		out.WriteString(text(block["text"]))
	}
	return strings.TrimSpace(out.String())
}

// appendTaskNote attaches a retry/cancel note to the task prompt.
func appendTaskNote(config *WorkTaskConfig, note string) *WorkTaskConfig {
	if config == nil {
		config = &WorkTaskConfig{PromptBlocks: []Row{}, ConfigValues: map[string]string{}}
	}
	if config.DisplayText != "" {
		config.DisplayText = config.DisplayText + "\n\n" + note
	} else {
		config.PromptBlocks = append(config.PromptBlocks, Row{"type": "text", "text": note})
	}
	return config
}

func parseFolderSettings(raw Row) *WorkTaskFolderSettings {
	settings := defaultFolderSettings()
	if raw == nil {
		return settings
	}
	if v, ok := raw["default_agent_type"].(string); ok && v != "" {
		settings.DefaultAgentType = strPtr(v)
	}
	if v, ok := raw["mode_id"].(string); ok && v != "" {
		settings.ModeID = strPtr(v)
	}
	if values, ok := raw["config_values"].(map[string]any); ok {
		for k, v := range values {
			settings.ConfigValues[k] = fmt.Sprint(v)
		}
	}
	if label, ok := raw["label_snapshot"].(Row); ok {
		settings.LabelSnapshot = label
	}
	if v, ok := raw["auto_process"].(bool); ok {
		settings.AutoProcess = v
	}
	if v, ok := raw["max_concurrent"].(float64); ok {
		settings.MaxConcurrent = int(v)
	}
	if v, ok := raw["merge_strategy"].(string); ok && (v == "squash" || v == "merge") {
		settings.MergeStrategy = v
	}
	if v, ok := raw["auto_merge"].(bool); ok {
		settings.AutoMerge = v
	}
	return settings
}

// filterTaskDiff keeps one file's section out of a unified diff.
func filterTaskDiff(diff, file string) string {
	if file == "" {
		return diff
	}
	sections := strings.Split(diff, "\ndiff --git ")
	for _, section := range sections {
		if strings.Contains(section, "a/"+file) || strings.Contains(section, "b/"+file) {
			return strings.TrimPrefix(section, "diff --git ")
		}
	}
	return ""
}
