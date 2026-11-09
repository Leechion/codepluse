// Automations for CODEG mode: the web contract ports of Codeg's scheduled
// prompts (snake_case wire format, see codeg-ui/src/lib/types.ts Automations).
// A run is "create a conversation in the folder and send the captured prompt",
// so permissions, streaming and audit all ride the normal dialogue path. Cron
// math reuses internal/orch's 5-field parser; schedules are evaluated in the
// server's local timezone (the stored timezone is accepted but informational).
package codeg

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"harnesshub/internal/orch"
)

type AutomationConfig struct {
	Action       *string           `json:"action,omitempty"`
	PromptBlocks []Row             `json:"prompt_blocks"`
	DisplayText  string            `json:"display_text"`
	ModeID       *string           `json:"mode_id"`
	ConfigValues map[string]string `json:"config_values"`
	// LabelSnapshot is a display-only cache (agent/mode labels); stored verbatim.
	LabelSnapshot Row `json:"label_snapshot,omitempty"`
}

type Automation struct {
	ID                    int               `json:"id"`
	Name                  string            `json:"name"`
	Enabled               bool              `json:"enabled"`
	TriggerKind           string            `json:"trigger_kind"`
	Cron                  *string           `json:"cron"`
	Timezone              string            `json:"timezone"`
	NextRunAt             *string           `json:"next_run_at"`
	AgentType             string            `json:"agent_type"`
	RootFolderID          *int              `json:"root_folder_id"`
	Isolation             string            `json:"isolation"`
	Branch                *string           `json:"branch"`
	IsRemoteBranch        bool              `json:"is_remote_branch"`
	Config                *AutomationConfig `json:"config"`
	LastRunAt             *string           `json:"last_run_at"`
	LastRunStatus         *string           `json:"last_run_status"`
	LastRunConversationID *int              `json:"last_run_conversation_id"`
	UnseenFailures        int               `json:"unseen_failures"`
	CreatedAt             string            `json:"created_at"`
	UpdatedAt             string            `json:"updated_at"`

	nextRunMs     int64 `json:"-"`
	lastMinuteKey int64 `json:"-"`
}

type AutomationRun struct {
	ID               int     `json:"id"`
	AutomationID     int     `json:"automation_id"`
	Status           string  `json:"status"`
	Trigger          string  `json:"trigger"`
	ScheduledFor     *string `json:"scheduled_for"`
	StartedAt        *string `json:"started_at"`
	EndedAt          *string `json:"ended_at"`
	ConversationID   *int    `json:"conversation_id"`
	WorktreeFolderID *int    `json:"worktree_folder_id"`
	StopReason       *string `json:"stop_reason"`
	Error            *string `json:"error"`
	Summary          *string `json:"summary"`
	CreatedAt        string  `json:"created_at"`

	connectionID string `json:"-"`
}

const maxRunsPerAutomation = 100
const maxRunDuration = 24 * time.Hour

// autoStore keeps the automations and their run history in two JSON files,
// written atomically (tmp + rename) like every other .data store.
type autoStore struct {
	mu          sync.Mutex
	root        string
	nextID      int
	nextRunID   int
	automations []*Automation
	runs        []*AutomationRun
}

func newAutoStore(root string) *autoStore {
	st := &autoStore{root: root, nextID: 1, nextRunID: 1}
	st.load()
	return st
}

func (st *autoStore) automationsFile() string { return filepath.Join(st.root, ".data", "codeg-automations.json") }
func (st *autoStore) runsFile() string        { return filepath.Join(st.root, ".data", "codeg-automation-runs.json") }

func readJSONFileData(path string) ([]byte, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	return []byte(strings.TrimPrefix(string(data), "\uFEFF")), nil
}

func (st *autoStore) load() {
	if raw, err := readJSONFileData(st.automationsFile()); err == nil {
		var saved []*Automation
		if json.Unmarshal(raw, &saved) == nil {
			st.automations = saved
			for _, a := range saved {
				if a.ID >= st.nextID {
					st.nextID = a.ID + 1
				}
			}
		}
	}
	if raw, err := readJSONFileData(st.runsFile()); err == nil {
		var saved []*AutomationRun
		if json.Unmarshal(raw, &saved) == nil {
			st.runs = saved
			for _, r := range saved {
				if r.ID >= st.nextRunID {
					st.nextRunID = r.ID + 1
				}
			}
		}
	}
}

func atomicWrite(path string, value any) error {
	b, err := json.Marshal(value)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(path+".tmp", b, 0o600); err != nil {
		return err
	}
	return os.Rename(path+".tmp", path)
}

func (st *autoStore) save() {
	_ = atomicWrite(st.automationsFile(), st.automations)
	_ = atomicWrite(st.runsFile(), st.runs)
}

func (st *autoStore) find(id int) *Automation {
	for _, a := range st.automations {
		if a.ID == id {
			return a
		}
	}
	return nil
}

func (st *autoStore) runsFor(id int, limit int) []*AutomationRun {
	out := []*AutomationRun{}
	for i := len(st.runs) - 1; i >= 0 && len(out) < limit; i-- {
		if st.runs[i].AutomationID == id {
			out = append(out, st.runs[i])
		}
	}
	return out
}

func (st *autoStore) activeRun(automationID int) *AutomationRun {
	for i := len(st.runs) - 1; i >= 0; i-- {
		if st.runs[i].AutomationID == automationID && st.runs[i].Status == "running" {
			return st.runs[i]
		}
	}
	return nil
}

func (st *autoStore) appendRun(run *AutomationRun) {
	st.runs = append(st.runs, run)
	// Keep the per-automation history bounded.
	kept := []*AutomationRun{}
	counts := map[int]int{}
	for i := len(st.runs) - 1; i >= 0; i-- {
		r := st.runs[i]
		counts[r.AutomationID]++
		if counts[r.AutomationID] <= maxRunsPerAutomation {
			kept = append([]*AutomationRun{r}, kept...)
		}
	}
	st.runs = kept
}

func iso(ms int64) *string {
	if ms == 0 {
		return nil
	}
	s := time.UnixMilli(ms).Format(time.RFC3339)
	return &s
}

func strPtr(v string) *string { return &v }
func intPtr(v int) *int       { return &v }

// wire refreshes the derived display fields before returning an automation.
func (a *Automation) wire() {
	a.NextRunAt = iso(a.nextRunMs)
}

// parseDraft applies the frontend draft onto an automation.
func parseDraft(a *Automation, draft Row, creating bool) error {
	name := strings.TrimSpace(text(draft["name"]))
	if name == "" {
		return fmt.Errorf("请填写定时任务名称")
	}
	a.Name = name
	enabled := true
	if v, ok := draft["enabled"].(bool); ok {
		enabled = v
	}
	a.Enabled = enabled
	trigger := text(draft["trigger_kind"])
	if trigger == "" {
		trigger = "schedule"
	}
	if trigger != "schedule" && trigger != "manual" {
		return fmt.Errorf("未知的触发类型：%s", trigger)
	}
	a.TriggerKind = trigger
	a.Timezone = text(draft["timezone"])
	agentType := text(draft["agent_type"])
	if agentType == "" {
		return fmt.Errorf("请选择执行端")
	}
	a.AgentType = agentType
	isolation := text(draft["isolation"])
	if isolation == "" {
		isolation = "shared_in_root"
	}
	if isolation != "shared_in_root" && isolation != "worktree_per_run" {
		return fmt.Errorf("未知的隔离方式：%s", isolation)
	}
	a.Isolation = isolation
	if isolation == "worktree_per_run" {
		return fmt.Errorf("worktree 隔离暂未接通，请使用 shared_in_root")
	}
	if v, ok := draft["root_folder_id"]; ok {
		if v == nil {
			a.RootFolderID = nil
		} else {
			n := number(v)
			a.RootFolderID = intPtr(n)
		}
	} else if creating {
		a.RootFolderID = nil
	}
	if raw, ok := draft["branch"].(string); ok && raw != "" {
		a.Branch = strPtr(raw)
		a.IsRemoteBranch, _ = draft["is_remote_branch"].(bool)
	} else if creating {
		a.Branch = nil
		a.IsRemoteBranch = false
	}
	if raw, ok := draft["cron"]; ok {
		if raw == nil || text(raw) == "" {
			a.Cron = nil
			a.nextRunMs = 0
		} else {
			cron := strings.TrimSpace(text(raw))
			parsed, err := orch.ParseCron(cron)
			if err != nil {
				return err
			}
			next, err := orch.NextRun(parsed, time.Now())
			if err != nil {
				return err
			}
			a.Cron = strPtr(cron)
			a.nextRunMs = next.UnixMilli()
		}
	}
	if trigger == "schedule" && a.Cron == nil {
		return fmt.Errorf("计划触发的定时任务需要填写 cron")
	}
	if raw, ok := draft["config"].(map[string]any); ok {
		config := &AutomationConfig{PromptBlocks: []Row{}, ConfigValues: map[string]string{}}
		if blocks, ok := raw["prompt_blocks"].([]any); ok {
			for _, b := range blocks {
				if m, ok := b.(map[string]any); ok {
					config.PromptBlocks = append(config.PromptBlocks, m)
				}
			}
		}
		config.DisplayText = text(raw["display_text"])
		if v, ok := raw["mode_id"].(string); ok && v != "" {
			config.ModeID = strPtr(v)
		}
		if values, ok := raw["config_values"].(map[string]any); ok {
			for k, v := range values {
				config.ConfigValues[k] = fmt.Sprint(v)
			}
		}
		if label, ok := raw["label_snapshot"].(map[string]any); ok {
			config.LabelSnapshot = label
		}
		if action, ok := raw["action"].(string); ok && action != "" {
			config.Action = strPtr(action)
		}
		a.Config = config
	}
	if creating && a.Config == nil {
		return fmt.Errorf("请填写任务提示词")
	}
	return nil
}

// ----------------------------- commands -----------------------------

func (s *Server) automationCommand(command string, a Row) (any, error) {
	st := s.auto
	st.mu.Lock()
	defer st.mu.Unlock()
	switch command {
	case "automation_list":
		out := []*Automation{}
		for _, item := range st.automations {
			item.wire()
			out = append(out, item)
		}
		return out, nil
	case "automation_get":
		item := st.find(number(a["id"]))
		if item == nil {
			return nil, fmt.Errorf("定时任务不存在")
		}
		item.wire()
		return item, nil
	case "automation_runs":
		limit := number(a["limit"])
		if limit <= 0 || limit > 500 {
			limit = 100
		}
		return st.runsFor(number(a["automationId"]), limit), nil
	case "automation_create":
		item := &Automation{ID: st.nextID, CreatedAt: timestamp(), UpdatedAt: timestamp(), Config: nil}
		if err := parseDraft(item, a["draft"].(Row), true); err != nil {
			return nil, err
		}
		st.nextID++
		st.automations = append(st.automations, item)
		st.save()
		s.emit("automation://changed", Row{"kind": "created", "id": item.ID})
		item.wire()
		return item, nil
	case "automation_update":
		item := st.find(number(a["id"]))
		if item == nil {
			return nil, fmt.Errorf("定时任务不存在")
		}
		draft, _ := a["draft"].(Row)
		if err := parseDraft(item, draft, false); err != nil {
			return nil, err
		}
		item.UpdatedAt = timestamp()
		st.save()
		s.emit("automation://changed", Row{"kind": "updated", "id": item.ID})
		item.wire()
		return item, nil
	case "automation_set_enabled":
		item := st.find(number(a["id"]))
		if item == nil {
			return nil, fmt.Errorf("定时任务不存在")
		}
		enabled, _ := a["enabled"].(bool)
		item.Enabled = enabled
		if enabled && item.TriggerKind == "schedule" && item.Cron != nil {
			if parsed, err := orch.ParseCron(*item.Cron); err == nil {
				if next, err := orch.NextRun(parsed, time.Now()); err == nil {
					item.nextRunMs = next.UnixMilli()
				}
			}
		}
		item.UpdatedAt = timestamp()
		st.save()
		s.emit("automation://changed", Row{"kind": "updated", "id": item.ID})
		item.wire()
		return item, nil
	case "automation_delete":
		id := number(a["id"])
		found := false
		filtered := st.automations[:0]
		for _, item := range st.automations {
			if item.ID == id {
				found = true
				continue
			}
			filtered = append(filtered, item)
		}
		st.automations = filtered
		kept := st.runs[:0]
		for _, run := range st.runs {
			if run.AutomationID != id {
				kept = append(kept, run)
			}
		}
		st.runs = kept
		if !found {
			return nil, fmt.Errorf("定时任务不存在")
		}
		st.save()
		s.emit("automation://changed", Row{"kind": "deleted", "id": id})
		return nil, nil
	case "automation_mark_seen":
		for _, item := range st.automations {
			item.UnseenFailures = 0
		}
		st.save()
		return nil, nil
	case "automation_compute_next_run":
		parsed, err := orch.ParseCron(strings.TrimSpace(text(a["cron"])))
		if err != nil {
			return nil, err
		}
		next, err := orch.NextRun(parsed, time.Now())
		if err != nil {
			return nil, nil
		}
		return next.Format(time.RFC3339), nil
	case "automation_run_now":
		item := st.find(number(a["automationId"]))
		if item == nil {
			return nil, fmt.Errorf("定时任务不存在")
		}
		if st.activeRun(item.ID) != nil {
			return nil, fmt.Errorf("上一次运行尚未结束")
		}
		run := s.startRun(item, "manual")
		st.save()
		item.wire()
		s.emit("automation://changed", Row{"kind": "run_started", "id": item.ID, "runId": run.ID})
		if run.Status == "skipped" {
			return run.ID, nil
		}
		go s.finishAutomationRun(item, run)
		return run.ID, nil
	case "automation_cancel_run":
		runID := number(a["runId"])
		var target *AutomationRun
		for _, run := range st.runs {
			if run.ID == runID {
				target = run
			}
		}
		if target == nil {
			return nil, fmt.Errorf("运行记录不存在")
		}
		if target.Status != "running" {
			return nil, nil
		}
		if target.connectionID != "" {
			_ = s.cancel(target.connectionID)
		}
		target.Status = "cancelled"
		now := timestamp()
		target.EndedAt = &now
		target.StopReason = strPtr("cancelled by user")
		st.save()
		s.emit("automation://changed", Row{"kind": "run_updated", "automationId": target.AutomationID, "runId": target.ID})
		return nil, nil
	}
	return nil, fmt.Errorf("未知的定时任务命令：%s", command)
}

// startRun creates the run record and (for schedule fires) may skip when the
// previous run is still in flight or the feature is disabled.
func (s *Server) startRun(item *Automation, trigger string) *AutomationRun {
	st := s.auto
	run := &AutomationRun{ID: st.nextRunID, AutomationID: item.ID, Status: "running", Trigger: trigger, CreatedAt: timestamp()}
	st.nextRunID++
	if !s.automationsEnabled() {
		run.Status = "skipped"
		reason := "automations disabled"
		run.StopReason = &reason
		run.EndedAt = strPtr(timestamp())
	} else if prev := st.activeRun(item.ID); prev != nil {
		run.Status = "skipped"
		reason := "previous run still active"
		run.StopReason = &reason
		run.EndedAt = strPtr(timestamp())
	} else {
		run.StartedAt = strPtr(timestamp())
	}
	st.appendRun(run)
	now := timestamp()
	item.LastRunAt = &now
	item.LastRunStatus = strPtr(run.Status)
	return run
}

func (s *Server) automationsEnabled() bool {
	st := s.store
	st.mu.Lock()
	defer st.mu.Unlock()
	if v, ok := st.Settings["automations_enabled"]; ok {
		enabled, _ := v.(bool)
		return enabled
	}
	return true
}

// finishAutomationRun drives one run end to end on its own goroutine:
// connect → create the conversation → prompt → poll until the dialogue ends.
func (s *Server) finishAutomationRun(item *Automation, run *AutomationRun) {
	defer s.autoTouch(item.ID)
	st := s.store
	folderID := 0
	folderPath := s.Root
	if item.RootFolderID != nil {
		st.mu.Lock()
		if folder := st.folderLocked(*item.RootFolderID); folder != nil {
			folderID = *item.RootFolderID
			folderPath = text(folder["path"])
		}
		st.mu.Unlock()
		if folderID == 0 {
			s.completeAutomationRun(item.ID, run.ID, "failed", "项目文件夹不存在")
			return
		}
	}
	connectArgs := Row{"agentType": item.AgentType, "workingDir": folderPath}
	if item.Config != nil {
		if item.Config.ModeID != nil {
			connectArgs["preferredModeId"] = *item.Config.ModeID
		}
		values := Row{}
		for k, v := range item.Config.ConfigValues {
			values[k] = v
		}
		connectArgs["preferredConfigValues"] = values
	}
	cid, err := s.connect(connectArgs)
	if err != nil {
		s.completeAutomationRun(item.ID, run.ID, "failed", err.Error())
		return
	}
	s.auto.mu.Lock()
	run.connectionID = cid
	s.auto.mu.Unlock()

	title := item.Name
	if item.Config != nil && item.Config.DisplayText != "" {
		title = item.Config.DisplayText
	}
	if len(title) > 80 {
		title = title[:80]
	}
	st.mu.Lock()
	row := st.createConversationLocked(folderID, item.AgentType, title, 0)
	convID := number(row["id"])
	st.mu.Unlock()
	s.auto.mu.Lock()
	run.ConversationID = intPtr(convID)
	item.LastRunConversationID = intPtr(convID)
	s.auto.mu.Unlock()
	s.auto.save()

	blocks := []any{Row{"type": "text", "text": runPrompt(item)}}
	if err := s.prompt(Row{"connectionId": cid, "conversationId": convID, "folderId": folderID, "blocks": blocks}); err != nil {
		_ = s.disconnect(cid)
		s.completeAutomationRun(item.ID, run.ID, "failed", err.Error())
		return
	}
	// The dialogue itself runs detached; poll the conversation status.
	deadline := time.Now().Add(maxRunDuration)
	for {
		time.Sleep(2 * time.Second)
		st.mu.Lock()
		row := st.conversationLocked(convID)
		status := ""
		if row != nil {
			status = text(row["status"])
		}
		st.mu.Unlock()
		if status == "pending_review" {
			_ = s.disconnect(cid)
			s.completeAutomationRun(item.ID, run.ID, "succeeded", "")
			return
		}
		if status == "cancelled" || status == "failed" {
			_ = s.disconnect(cid)
			s.completeAutomationRun(item.ID, run.ID, "failed", "会话执行失败或被取消")
			return
		}
		if time.Now().After(deadline) {
			_ = s.cancel(cid)
			_ = s.disconnect(cid)
			s.completeAutomationRun(item.ID, run.ID, "failed", "运行超过 24 小时上限")
			return
		}
	}
}

// runPrompt renders the captured composer snapshot into the prompt text.
func runPrompt(item *Automation) string {
	if item.Config == nil {
		return ""
	}
	if item.Config.DisplayText != "" {
		return item.Config.DisplayText
	}
	var out strings.Builder
	for _, block := range item.Config.PromptBlocks {
		out.WriteString(text(block["text"]))
	}
	return out.String()
}

// completeAutomationRun records the terminal state of a run.
func (s *Server) completeAutomationRun(automationID, runID int, status, errMsg string) {
	st := s.auto
	st.mu.Lock()
	var item *Automation
	var run *AutomationRun
	for _, a := range st.automations {
		if a.ID == automationID {
			item = a
		}
	}
	for _, r := range st.runs {
		if r.ID == runID {
			run = r
		}
	}
	if run != nil {
		run.Status = status
		now := timestamp()
		run.EndedAt = &now
		if errMsg != "" {
			run.Error = strPtr(errMsg)
		}
		if item != nil {
			item.LastRunStatus = strPtr(status)
			if status == "failed" {
				item.UnseenFailures++
			}
		}
		st.save()
	}
	st.mu.Unlock()
	if item != nil && run != nil {
		s.emit("automation://changed", Row{"kind": "run_finished", "id": item.ID, "runId": run.ID, "status": status})
	}
}

// autoTouch refreshes the derived fields after a background mutation.
func (s *Server) autoTouch(id int) {
	s.auto.mu.Lock()
	item := s.auto.find(id)
	if item != nil {
		item.wire()
	}
	s.auto.mu.Unlock()
}

// TickAutomations fires due schedules; called from the server's 30s loop.
func (s *Server) TickAutomations() {
	st := s.auto
	st.mu.Lock()
	items := append([]*Automation{}, st.automations...)
	st.mu.Unlock()
	now := time.Now()
	minuteKey := now.Unix() / 60
	for _, item := range items {
		if !item.Enabled || item.TriggerKind != "schedule" || item.Cron == nil {
			continue
		}
		func() {
			st.mu.Lock()
			defer st.mu.Unlock()
			parsed, err := orch.ParseCron(*item.Cron)
			if err != nil {
				item.LastRunStatus = strPtr("error: " + err.Error())
				return
			}
			if item.nextRunMs == 0 {
				next, err := orch.NextRun(parsed, now)
				if err != nil {
					item.LastRunStatus = strPtr("error: " + err.Error())
					return
				}
				item.nextRunMs = next.UnixMilli()
			}
			if now.UnixMilli() < item.nextRunMs {
				return
			}
			next, err := orch.NextRun(parsed, now)
			if err != nil {
				item.LastRunStatus = strPtr("error: " + err.Error())
				return
			}
			item.nextRunMs = next.UnixMilli()
			if item.lastMinuteKey == minuteKey {
				return
			}
			item.lastMinuteKey = minuteKey
			if st.activeRun(item.ID) != nil {
				return
			}
			run := s.startRun(item, "schedule")
			st.save()
			item.wire()
			if run.Status == "running" {
				go s.finishAutomationRun(item, run)
			}
			s.emit("automation://changed", Row{"kind": "run_started", "id": item.ID, "runId": run.ID})
		}()
	}
}
