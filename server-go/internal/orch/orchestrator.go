// Package orch is the 宰相 orchestrator: mission planning, dependency waves,
// peer messaging, steering, fork and manager verification — the Go port of
// core/orchestrator.js.
package orch

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"regexp"
	"strings"
	"sync"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
	"harnesshub/internal/textutil"
)

// --- JSON extraction (prose JSON fallback) ---

var fenceRe = regexp.MustCompile(`(?is)` + "```(?:json)?\\s*(.*?)```")

// extractJSON mirrors the Node json(): direct parse, then a single fenced
// block, then the outermost brace slice.
func extractJSON(text string) (map[string]any, error) {
	trimmed := strings.TrimSpace(text)
	var out map[string]any
	if err := json.Unmarshal([]byte(trimmed), &out); err == nil {
		return out, nil
	}
	candidate := ""
	blocks := fenceRe.FindAllStringSubmatch(trimmed, -1)
	if len(blocks) == 1 {
		candidate = blocks[0][1]
	} else if len(blocks) == 0 {
		i := strings.Index(trimmed, "{")
		j := strings.LastIndex(trimmed, "}")
		if i >= 0 && j > i {
			candidate = trimmed[i : j+1]
		}
	}
	if json.Unmarshal([]byte(candidate), &out) != nil {
		return nil, errors.New("Harness 未返回约定的 JSON，任务未被视为完成")
	}
	return out, nil
}

// parseResult prefers the captured submit_result arguments; prose JSON stays
// the fallback.
func parseResult(res acp.Result) (map[string]any, error) {
	if res.Structured != nil {
		return res.Structured, nil
	}
	return extractJSON(res.Output)
}

// OUTPUT_SCHEMAS are the per-stage submit_result schemas (codeg's
// --output-schema idea): a refused submission carries what to fix back.
var OUTPUT_SCHEMAS = map[string]map[string]any{
	"plan": map[string]any{"type": "object", "required": []string{"tasks"},
		"properties": map[string]any{"message": map[string]any{"type": "string"}, "requiresPeerCommunication": map[string]any{"type": "boolean"},
			"tasks": map[string]any{"type": "array", "items": map[string]any{"type": "object", "required": []string{"id", "agentId", "title", "instructions"},
				"properties": map[string]any{"id": map[string]any{"type": "string"}, "agentId": map[string]any{"type": "string"}, "title": map[string]any{"type": "string"}, "instructions": map[string]any{"type": "string"}, "dependsOn": map[string]any{"type": "array"}}}}}},
	"worker": map[string]any{"type": "object", "required": []string{"output", "complete"},
		"properties": map[string]any{"output": map[string]any{"type": "string"}, "complete": map[string]any{"type": "boolean"}, "blocked": map[string]any{"type": "boolean"},
			"messages": map[string]any{"type": "array"}, "reads": map[string]any{"type": "array"}}},
	"report": map[string]any{"type": "object", "required": []string{"complete", "report", "taskVerdicts"},
		"properties": map[string]any{"complete": map[string]any{"type": "boolean"}, "reply": map[string]any{"type": "string"}, "report": map[string]any{"type": "string"},
			"taskVerdicts": map[string]any{"type": "array", "items": map[string]any{"type": "object", "required": []string{"id", "accepted", "reason"},
				"properties": map[string]any{"id": map[string]any{"type": "string"}, "accepted": map[string]any{"type": "boolean"}, "reason": map[string]any{"type": "string"}}}}}},
	"decision": map[string]any{"type": "object", "required": []string{"reply", "action"},
		"properties": map[string]any{"reply": map[string]any{"type": "string"}, "action": map[string]any{"type": "string", "enum": []string{"reply", "execute", "steer"}},
			"objective": map[string]any{"type": "string"}, "models": map[string]any{"type": "object"}, "assignments": map[string]any{"type": "array"}}},
	"adjust": map[string]any{"type": "object", "required": []string{"updates", "newTasks"},
		"properties": map[string]any{"reply": map[string]any{"type": "string"}, "updates": map[string]any{"type": "array"}, "newTasks": map[string]any{"type": "array"}}},
	"briefing": map[string]any{"type": "object", "required": []string{"summary"}, "properties": map[string]any{"summary": map[string]any{"type": "string"}}},
}

func schemaKeyFor(stage string) string {
	switch stage {
	case "宰相拆解任务", "宰相修正计划":
		return "plan"
	case "宰相汇总汇报":
		return "report"
	case "宰相对话":
		return "decision"
	case "宰相调整分工":
		return "adjust"
	case "会话移交简报":
		return "briefing"
	}
	return "worker"
}

// --- mission model ---

// Steering is one queued user intervention.
type Steering struct {
	Text      string           `json:"text"`
	Overrides []map[string]any `json:"overrides"`
	At        int64            `json:"at"`
}

// InboxMsg is one delivered peer message.
type InboxMsg struct {
	From string `json:"from"`
	Text string `json:"text"`
}

// MissionEvent is a bus-visible event inside a mission.
type MissionEvent struct {
	ID       string `json:"id"`
	From     string `json:"from"`
	To       string `json:"to"`
	Text     string `json:"text"`
	Kind     string `json:"kind"`
	At       int64  `json:"at"`
	TaskFrom string `json:"taskFrom,omitempty"`
	TaskTo   string `json:"taskTo,omitempty"`
}

// MissionTask is one planned unit of work.
type MissionTask struct {
	ID           string           `json:"id"`
	AgentID      string           `json:"agentId"`
	Title        string           `json:"title"`
	Instructions string           `json:"instructions"`
	DependsOn    []string         `json:"dependsOn"`
	Model        string           `json:"model"`
	Status       string           `json:"status"`
	Output       string           `json:"output"`
	Inbox        []InboxMsg       `json:"inbox"`
	Files        []map[string]any `json:"files"`
	Turns        int              `json:"turns"`
	Error        string           `json:"error"`
}

// Mission is one orchestrated run.
type Mission struct {
	mu sync.Mutex `json:"-"`

	ID                        string                    `json:"id"`
	Objective                 string                    `json:"objective"`
	ManagerID                 string                    `json:"managerId"`
	WorkerIDs                 []string                  `json:"workerIds"`
	Sensitivity               int                       `json:"sensitivity"`
	Permission                string                    `json:"permission"`
	Workdir                   string                    `json:"workdir"`
	Isolate                   string                    `json:"isolate"`
	Status                    string                    `json:"status"`
	Tasks                     []*MissionTask            `json:"tasks"`
	Events                    []*MissionEvent           `json:"events"`
	Report                    string                    `json:"report"`
	Reply                     string                    `json:"reply,omitempty"`
	Error                     string                    `json:"error"`
	Round                     int                       `json:"round"`
	CreatedAt                 int64                     `json:"createdAt"`
	FinishedAt                int64                     `json:"finishedAt,omitempty"`
	ManagerModel              string                    `json:"managerModel,omitempty"`
	Models                    map[string]string         `json:"models,omitempty"`
	ChatID                    string                    `json:"chatId,omitempty"`
	SkipPlanning              bool                      `json:"skipPlanning,omitempty"`
	PlanMessage               string                    `json:"planMessage,omitempty"`
	RequiresPeerCommunication bool                      `json:"requiresPeerCommunication,omitempty"`
	CommunicationRequested    bool                      `json:"communicationRequested,omitempty"`
	PendingInstructions       []*Steering               `json:"pendingInstructions"`
	PinnedTasks               map[string]map[string]any `json:"pinnedTasks"`
	DirectiveHistory          []*Steering               `json:"directiveHistory,omitempty"`
	Adjustments               []map[string]any          `json:"adjustments,omitempty"`
	Verdicts                  []map[string]any          `json:"verdicts,omitempty"`
	ForkedFrom                string                    `json:"forkedFrom,omitempty"`
	BaseWorkdir               string                    `json:"baseWorkdir,omitempty"`
	Worktree                  string                    `json:"worktree,omitempty"`
	IsolationError            string                    `json:"isolationError,omitempty"`
	UpdatedAt                 int64                     `json:"updatedAt"`
}

func (m *Mission) lock()   { m.mu.Lock() }
func (m *Mission) unlock() { m.mu.Unlock() }

// Spec is the creation request (chat config + objective).
type Spec struct {
	Objective    string            `json:"objective"`
	ManagerID    string            `json:"managerId"`
	WorkerIDs    []string          `json:"workerIds"`
	Sensitivity  *int              `json:"sensitivity"`
	Permission   string            `json:"permission"`
	Workdir      string            `json:"workdir"`
	Isolate      string            `json:"isolate"`
	ManagerModel string            `json:"managerModel"`
	Models       map[string]string `json:"models"`
	ChatID       string            `json:"chatId"`
	SkipPlanning bool              `json:"skipPlanning"`
}

// WorktreeManager creates an isolated git worktree for one mission.
type WorktreeManager interface {
	For(missionID, base string) (string, error)
}

// SkillsProvider renders the agent's skill-pack prompt section.
type SkillsProvider interface {
	PromptFor(agent *hub.Agent) string
}

// Orchestrator owns all missions.
type Orchestrator struct {
	Hub       *hub.Hub
	Invoke    func(agent *hub.Agent, prompt string, o acp.Options) acp.Result
	ReadFile  func(m *Mission, read map[string]any) (map[string]any, error)
	Worktrees WorktreeManager
	Skills    SkillsProvider

	mu       sync.Mutex
	missions []*Mission
}

func nowMs() int64 { return time.Now().UnixMilli() }

func newOrchID() string {
	b := make([]byte, 12)
	_, _ = rand.Read(b)
	return hex.EncodeToString(b)
}

func (o *Orchestrator) emit(m *Mission) {
	m.lock()
	m.UpdatedAt = nowMs()
	b, err := json.Marshal(m)
	m.unlock()
	if err == nil {
		o.Hub.Emit("mission", json.RawMessage(b))
	}
}

func (o *Orchestrator) fail(m *Mission, msg string) {
	m.lock()
	m.Status = "failed"
	m.Error = msg
	m.unlock()
	o.emit(m)
}

// Eligible validates that id may execute at the given sensitivity.
func (o *Orchestrator) Eligible(id string, sensitivity int) (*hub.Agent, error) {
	a := o.Hub.Agent(id)
	if a == nil || !a.Enabled || !(a.Real || a.Communication == "http") || a.Clearance < sensitivity {
		return nil, errors.New("执行端未连接、已禁用或密级不足：" + id)
	}
	return a, nil
}

// Create validates the spec, records the mission and starts its goroutine.
// The HTTP request returns before any model call starts.
func (o *Orchestrator) Create(spec Spec) (*Mission, error) {
	objective := strings.TrimSpace(spec.Objective)
	if objective == "" || len(objective) > 12000 {
		return nil, errors.New("请填写 1–12000 字符的任务目标")
	}
	sensitivity := 1
	if spec.Sensitivity != nil {
		sensitivity = *spec.Sensitivity
	}
	if sensitivity < 0 || sensitivity > 3 {
		return nil, errors.New("无效密级")
	}
	manager, err := o.Eligible(spec.ManagerID, sensitivity)
	if err != nil {
		return nil, err
	}
	seen := map[string]bool{}
	var workers []string
	for _, id := range spec.WorkerIDs {
		if !seen[id] {
			seen[id] = true
			workers = append(workers, id)
		}
	}
	if len(workers) == 0 || len(workers) > 8 {
		return nil, errors.New("请选择 1–8 个执行 Harness")
	}
	for _, id := range workers {
		if _, err := o.Eligible(id, sensitivity); err != nil {
			return nil, err
		}
	}
	permission := spec.Permission
	if permission == "" {
		permission = "readonly"
	}
	if permission != "readonly" && permission != "workspace" && permission != "shell" {
		return nil, errors.New("未知权限模式")
	}
	isolate := "shared"
	if spec.Isolate != "" {
		if spec.Isolate != "shared" && spec.Isolate != "worktree" {
			return nil, errors.New("隔离设置无效，仅支持 shared 或 worktree")
		}
		isolate = spec.Isolate
	}
	m := &Mission{
		ID: "mission_" + newOrchID(), Objective: objective, ManagerID: manager.ID,
		WorkerIDs: workers, Sensitivity: sensitivity, Permission: permission,
		Workdir: spec.Workdir, Isolate: isolate, Status: "planning",
		Tasks: []*MissionTask{}, Events: []*MissionEvent{}, CreatedAt: nowMs(),
		ManagerModel: spec.ManagerModel, Models: spec.Models, ChatID: spec.ChatID,
		SkipPlanning: spec.SkipPlanning, PendingInstructions: []*Steering{},
		PinnedTasks: map[string]map[string]any{},
	}
	if m.Models == nil {
		m.Models = map[string]string{}
	}
	o.mu.Lock()
	o.missions = append([]*Mission{m}, o.missions...)
	o.mu.Unlock()
	o.emit(m)
	o.launch(m)
	return m, nil
}

func (o *Orchestrator) launch(m *Mission) {
	go func() {
		defer func() {
			if r := recover(); r != nil {
				o.fail(m, fmt.Sprintf("编排器异常：%v", r))
			}
		}()
		if err := o.run(m); err != nil {
			o.fail(m, err.Error())
		}
	}()
}

func (o *Orchestrator) event(m *Mission, from, to, text, kind string) error {
	if kind == "" {
		kind = "collab.ask"
	}
	msg := o.Hub.SendMessage(from, to, text, kind, m.Sensitivity, m.ID)
	if msg.Status == "rejected" {
		return errors.New(msg.Auth.Reason)
	}
	m.lock()
	m.Events = append(m.Events, &MissionEvent{ID: msg.ID, From: from, To: to, Text: text, Kind: kind, At: nowMs()})
	m.unlock()
	o.emit(m)
	return nil
}

// CallResult carries the raw output text plus the parsed JSON object.
type CallResult struct {
	Output string
	Parsed map[string]any
}

func (o *Orchestrator) call(m *Mission, agentID, stage, prompt, modelOverride string, overrideSet bool, taskID string) (*CallResult, error) {
	agent, err := o.Eligible(agentID, m.Sensitivity)
	if err != nil {
		return nil, err
	}
	schemaKey := schemaKeyFor(stage)
	task := o.Hub.SubmitTask(&hub.Task{Title: stage, Prompt: prompt, Real: true, Sensitivity: m.Sensitivity,
		AssignedTo: agentID, MissionID: m.ID, Execution: hub.Execution{Permission: m.Permission, Workdir: m.Workdir},
		RequestedBy: m.ManagerID})
	o.Hub.StartTask(task.ID, agentID, hub.Execution{Tool: agent.Type, Permission: m.Permission})
	managerStage := false
	switch stage {
	case "宰相对话", "宰相拆解任务", "宰相修正计划", "宰相调整分工", "宰相汇总汇报":
		managerStage = true
	}
	model := ""
	if overrideSet {
		model = modelOverride
	} else if managerStage {
		model = m.ManagerModel
	} else {
		model = m.Models[agentID]
	}
	o.Hub.SetTaskMeta(task.ID, model, "")
	skillText := ""
	if o.Skills != nil {
		skillText = o.Skills.PromptFor(agent)
	}
	fullPrompt := prompt
	if skillText != "" {
		fullPrompt = prompt + skillText
	}
	sessionKey := m.ChatID
	if sessionKey == "" {
		sessionKey = m.ID
	}
	if managerStage {
		sessionKey += ":manager:" + schemaKey
	} else if taskID != "" {
		sessionKey += ":" + taskID
	} else {
		sessionKey += ":" + stage
	}
	result := o.Invoke(agent, fullPrompt, acp.Options{
		Permission: m.Permission, Model: model, Workdir: m.Workdir, MissionID: m.ID,
		ChatID: m.ChatID, TaskID: taskID, Role: map[bool]string{true: "manager", false: "worker"}[managerStage],
		Structured: true, SessionKey: sessionKey, Sensitivity: m.Sensitivity, Stage: stage,
		OutputSchema: OUTPUT_SCHEMAS[schemaKey],
	})
	if !result.OK {
		o.Hub.CompleteTask(task.ID, "", result.Error)
	} else {
		o.Hub.CompleteTask(task.ID, hub.TrimResult(result.Output, 20000), "")
	}
	metaSession := ""
	if result.Meta != nil {
		metaSession, _ = result.Meta["runtimeSessionId"].(string)
	}
	o.Hub.SetTaskMeta(task.ID, model, metaSession)
	if t := o.Hub.Task(task.ID); t != nil && t.Status == "cancelled" {
		return nil, errors.New("此调用已取消，忽略迟到结果")
	}
	if !result.OK || strings.TrimSpace(result.Output) == "" {
		if result.Error != "" {
			return nil, errors.New(result.Error)
		}
		return nil, errors.New("Harness 未返回结果")
	}
	parsed, err := parseResult(result)
	if err != nil {
		return nil, err
	}
	return &CallResult{Output: result.Output, Parsed: parsed}, nil
}

var taskIDRe = regexp.MustCompile(`^[a-zA-Z0-9_-]{1,40}$`)

// tasksFromPlan validates a raw plan object and materialises the board.
func tasksFromPlan(m *Mission, plan map[string]any) ([]*MissionTask, error) {
	raw, ok := plan["tasks"].([]any)
	if !ok || len(raw) == 0 || len(raw) > 8 {
		return nil, errors.New("宰相须拆分为 1–8 个子任务")
	}
	ids := map[string]bool{}
	var out []*MissionTask
	for _, item := range raw {
		t, ok := item.(map[string]any)
		if !ok {
			return nil, errors.New("子任务格式无效")
		}
		id, _ := t["id"].(string)
		agentID, _ := t["agentId"].(string)
		title, _ := t["title"].(string)
		instructions, _ := t["instructions"].(string)
		if !taskIDRe.MatchString(id) || ids[id] {
			return nil, errors.New("子任务 ID 无效或重复")
		}
		inWorkers := false
		for _, w := range m.WorkerIDs {
			if w == agentID {
				inWorkers = true
			}
		}
		if !inWorkers || strings.TrimSpace(title) == "" || strings.TrimSpace(instructions) == "" {
			return nil, errors.New("子任务执行端或指令无效")
		}
		ids[id] = true
		dependsOn := []string{}
		if rawDep, ok := t["dependsOn"].([]any); ok {
			for _, d := range rawDep {
				s, ok := d.(string)
				if !ok {
					return nil, errors.New("依赖列表无效")
				}
				dependsOn = append(dependsOn, s)
			}
		} else if t["dependsOn"] != nil {
			return nil, errors.New("依赖列表无效")
		}
		out = append(out, &MissionTask{ID: id, AgentID: agentID, Title: title, Instructions: instructions,
			DependsOn: dependsOn, Model: m.Models[agentID], Status: "queued", Inbox: []InboxMsg{}, Files: []map[string]any{}})
	}
	resolved := map[string]bool{}
	for i := 0; i < len(out); i++ {
		for _, t := range out {
			ok := true
			for _, dep := range t.DependsOn {
				if !resolved[dep] {
					ok = false
				}
			}
			if ok {
				resolved[t.ID] = true
			}
		}
	}
	if len(resolved) != len(out) {
		return nil, errors.New("计划包含循环依赖或不存在的依赖")
	}
	return out, nil
}

// validateDefs re-checks an existing board after adjust merges new definitions.
func validateDefs(m *Mission, defs []*MissionTask) error {
	if len(defs) == 0 || len(defs) > 8 {
		return errors.New("宰相须拆分为 1–8 个子任务")
	}
	ids := map[string]bool{}
	for _, t := range defs {
		if !taskIDRe.MatchString(t.ID) || ids[t.ID] {
			return errors.New("子任务 ID 无效或重复")
		}
		inWorkers := false
		for _, w := range m.WorkerIDs {
			if w == t.AgentID {
				inWorkers = true
			}
		}
		if !inWorkers || strings.TrimSpace(t.Title) == "" || strings.TrimSpace(t.Instructions) == "" {
			return errors.New("子任务执行端或指令无效")
		}
		ids[t.ID] = true
	}
	resolved := map[string]bool{}
	for i := 0; i < len(defs); i++ {
		for _, t := range defs {
			ok := true
			for _, dep := range t.DependsOn {
				if !resolved[dep] {
					ok = false
				}
			}
			if ok {
				resolved[t.ID] = true
			}
		}
	}
	if len(resolved) != len(defs) {
		return errors.New("计划包含循环依赖或不存在的依赖")
	}
	return nil
}

// Steer queues a user intervention for the running mission.
func (o *Orchestrator) Steer(id, text string, overrides []map[string]any) error {
	o.mu.Lock()
	m := o.missionLocked(id)
	o.mu.Unlock()
	if m == nil {
		return errors.New("任务不存在")
	}
	m.lock()
	valid := m.Status == "planning" || m.Status == "running" || m.Status == "reporting"
	m.unlock()
	if !valid {
		return errors.New("此轮已结束，请通过对话发起后续工作")
	}
	modelRe := regexp.MustCompile(`^[a-zA-Z0-9][\w.:/@+\-]{0,119}$`)
	for _, change := range overrides {
		taskID, _ := change["taskId"].(string)
		found := false
		m.lock()
		for _, t := range m.Tasks {
			if t.ID == taskID {
				found = true
			}
		}
		m.unlock()
		if !found {
			return errors.New("子任务不存在")
		}
		if agentID, ok := change["agentId"].(string); ok && agentID != "" {
			inWorkers := false
			m.lock()
			for _, w := range m.WorkerIDs {
				if w == agentID {
					inWorkers = true
				}
			}
			m.unlock()
			if !inWorkers {
				return errors.New("请选择此轮允许使用的 Harness")
			}
		}
		if model, ok := change["model"].(string); ok && model != "" && !modelRe.MatchString(model) {
			return errors.New("模型名称无效")
		}
	}
	m.lock()
	m.PendingInstructions = append(m.PendingInstructions, &Steering{Text: text, Overrides: overrides, At: nowMs()})
	m.unlock()
	o.emit(m)
	return nil
}

func mapToTask(m *Mission, t map[string]any) *MissionTask {
	id, _ := t["id"].(string)
	agentID, _ := t["agentId"].(string)
	title, _ := t["title"].(string)
	instructions, _ := t["instructions"].(string)
	model, _ := t["model"].(string)
	status, _ := t["status"].(string)
	output, _ := t["output"].(string)
	turns := 0
	if v, ok := t["turns"].(float64); ok {
		turns = int(v)
	}
	dependsOn := []string{}
	if raw, ok := t["dependsOn"].([]any); ok {
		for _, d := range raw {
			if s, ok := d.(string); ok {
				dependsOn = append(dependsOn, s)
			}
		}
	}
	files := []map[string]any{}
	if raw, ok := t["files"].([]any); ok {
		for _, f := range raw {
			if fm, ok := f.(map[string]any); ok {
				files = append(files, fm)
			}
		}
	}
	return &MissionTask{ID: id, AgentID: agentID, Title: title, Instructions: instructions,
		DependsOn: dependsOn, Model: model, Status: status, Output: output, Inbox: []InboxMsg{},
		Files: files, Turns: turns}
}

// adjust applies queued user interventions through the manager.
func (o *Orchestrator) adjust(m *Mission) error {
	m.lock()
	pending := m.PendingInstructions
	m.PendingInstructions = []*Steering{}
	m.unlock()
	if len(pending) == 0 {
		return nil
	}
	pendingJSON, _ := json.Marshal(pending)
	m.lock()
	tasksJSON, _ := json.Marshal(m.Tasks)
	workersJSON, _ := json.Marshal(m.WorkerIDs)
	m.unlock()
	objective := m.Objective
	raw, err := o.call(m, m.ManagerID, "宰相调整分工",
		"用户正在干预当前任务。请基于已有成果调整后续分工，只输出 JSON：{\"reply\":\"向用户解释调整\",\"updates\":[{\"taskId\":\"已有任务ID\",\"agentId\":\"允许的Harness ID\",\"instructions\":\"新指令\"}],\"newTasks\":[]}。"+
			"newTasks 如需使用，格式与原 tasks 相同；已有执行结果不能删除。人工指定的执行端与模型必须遵守。\n允许执行端："+string(workersJSON)+
			"\n原始目标："+objective+"\n用户要求："+string(pendingJSON)+"\n当前任务："+string(tasksJSON), "", false, "")
	if err != nil {
		return err
	}
	rawUpdates, _ := raw.Parsed["updates"].([]any)
	rawNew, _ := raw.Parsed["newTasks"].([]any)
	if raw.Parsed["updates"] != nil && rawUpdates == nil || raw.Parsed["newTasks"] != nil && rawNew == nil {
		return errors.New("分工调整格式无效")
	}
	m.lock()
	defs := make([]*MissionTask, 0, len(m.Tasks))
	for _, t := range m.Tasks {
		cp := *t
		cp.Inbox = append([]InboxMsg{}, t.Inbox...)
		cp.Files = append([]map[string]any{}, t.Files...)
		defs = append(defs, &cp)
	}
	m.unlock()
	for _, u := range rawUpdates {
		um, ok := u.(map[string]any)
		if !ok {
			return errors.New("分工调整格式无效")
		}
		taskID, _ := um["taskId"].(string)
		var target *MissionTask
		for _, t := range defs {
			if t.ID == taskID {
				target = t
			}
		}
		if target == nil {
			return errors.New("调整引用了不存在的子任务")
		}
		if agentID, ok := um["agentId"].(string); ok && agentID != "" {
			target.AgentID = agentID
		}
		if instructions, ok := um["instructions"].(string); ok && instructions != "" {
			target.Instructions = instructions
		}
	}
	m.lock()
	pins := map[string]map[string]any{}
	for k, v := range m.PinnedTasks {
		pins[k] = v
	}
	m.unlock()
	var manual []map[string]any
	for _, p := range pending {
		manual = append(manual, p.Overrides...)
	}
	for _, change := range manual {
		taskID, _ := change["taskId"].(string)
		base := pins[taskID]
		merged := map[string]any{}
		for k, v := range base {
			merged[k] = v
		}
		for k, v := range change {
			merged[k] = v
		}
		pins[taskID] = merged
	}
	for _, t := range defs {
		if pin := pins[t.ID]; pin != nil {
			if agentID, ok := pin["agentId"].(string); ok && agentID != "" {
				t.AgentID = agentID
			}
			if instructions, ok := pin["instructions"].(string); ok && instructions != "" {
				t.Instructions = instructions
			}
		}
	}
	for _, nt := range rawNew {
		if nm, ok := nt.(map[string]any); ok {
			defs = append(defs, mapToTask(m, nm))
		}
	}
	if err := validateDefs(m, defs); err != nil {
		return err
	}
	m.lock()
	old := map[string]*MissionTask{}
	for _, t := range m.Tasks {
		old[t.ID] = t
	}
	changedIDs := map[string]bool{}
	for _, c := range manual {
		if taskID, _ := c["taskId"].(string); taskID != "" {
			changedIDs[taskID] = true
		}
	}
	rebuilt := make([]*MissionTask, 0, len(defs))
	for _, t := range defs {
		prev := old[t.ID]
		changed := prev == nil || t.AgentID != prev.AgentID || t.Instructions != prev.Instructions || changedIDs[t.ID]
		t.Status = map[bool]string{true: "queued", false: t.Status}[changed]
		if changed {
			t.Error = ""
		}
		if pinModel, ok := pins[t.ID]["model"].(string); ok && pinModel != "" {
			t.Model = pinModel
		} else if changed {
			t.Model = m.Models[t.AgentID]
		}
		rebuilt = append(rebuilt, t)
	}
	m.Tasks = rebuilt
	m.PinnedTasks = pins
	m.DirectiveHistory = append(m.DirectiveHistory, pending...)
	reply := "已更新后续分工。"
	if r, ok := raw.Parsed["reply"].(string); ok && r != "" {
		reply = r
	}
	m.Adjustments = append(m.Adjustments, map[string]any{"reply": reply, "at": nowMs()})
	m.Report = ""
	m.Status = "running"
	m.unlock()
	o.emit(m)
	return nil
}

func (o *Orchestrator) run(m *Mission) error {
	if m.Isolate == "worktree" && m.Workdir != "" {
		m.lock()
		base := m.Workdir
		m.BaseWorkdir = base
		m.unlock()
		if o.Worktrees == nil {
			m.lock()
			m.IsolationError = "无法创建隔离工作树，已回退共享目录：服务未启用工作树隔离"
			m.unlock()
		} else {
			wt, err := o.Worktrees.For(m.ID, base)
			if err != nil {
				m.lock()
				m.IsolationError = "无法创建隔离工作树，已回退共享目录：" + err.Error()
				m.unlock()
			} else {
				m.lock()
				m.Worktree = wt
				m.Workdir = wt
				m.unlock()
			}
		}
		o.emit(m)
	}
	if err := o.event(m, "hub", m.ManagerID, m.Objective, "task.assign"); err != nil {
		return err
	}
	if m.SkipPlanning {
		// Direct @-delegation: no manager planning call, the delegate executes
		// the objective verbatim; the manager still verifies and reports.
		agentID := m.WorkerIDs[0]
		plan := map[string]any{"tasks": []any{map[string]any{"id": "t1", "agentId": agentID, "title": "直接委派", "instructions": m.Objective}}}
		tasks, err := tasksFromPlan(m, plan)
		if err != nil {
			return err
		}
		m.lock()
		m.Tasks = tasks
		name := agentID
		if a := o.Hub.Agent(agentID); a != nil {
			name = a.Name
		}
		m.PlanMessage = "已直接委派给 " + name + "（不经拆解），完成后由宰相验收汇报。"
		m.Status = "running"
		m.unlock()
		o.emit(m)
		m.lock()
		assignments := make([]*MissionTask, len(m.Tasks))
		copy(assignments, m.Tasks)
		m.unlock()
		for _, t := range assignments {
			payload, _ := json.Marshal(map[string]any{"taskId": t.ID, "title": t.Title, "instructions": t.Instructions})
			if err := o.event(m, m.ManagerID, t.AgentID, string(payload), "task.assign"); err != nil {
				return err
			}
		}
		return o.work(m)
	}
	type rosterEntry struct {
		ID           string   `json:"id"`
		Name         string   `json:"name"`
		Model        string   `json:"model"`
		Capabilities []string `json:"capabilities"`
	}
	m.lock()
	roster := make([]rosterEntry, 0, len(m.WorkerIDs))
	for _, id := range m.WorkerIDs {
		a := o.Hub.Agent(id)
		name, caps := id, []string(nil)
		if a != nil {
			name, caps = a.Name, a.Capabilities
		}
		roster = append(roster, rosterEntry{ID: id, Name: name, Model: m.Models[id], Capabilities: caps})
	}
	m.unlock()
	rosterJSON, _ := json.Marshal(roster)
	objective := m.Objective
	rawPlan, err := o.call(m, m.ManagerID, "宰相拆解任务",
		"你是宰相，负责分解用户目标并交给真实 Harness 执行。优先通过 submit_result 工具提交 JSON（字段作为工具参数传入），工具不可用时才在正文中只输出 JSON，不要执行子任务。\n"+
			"格式：{\"message\":\"向用户解释你决定如何分工\",\"requiresPeerCommunication\":false,\"tasks\":[{\"id\":\"t1\",\"agentId\":\"名单中的ID\",\"title\":\"标题\",\"instructions\":\"可执行指令与验收要求\",\"dependsOn\":[]}]}。"+
			"用户要求执行端相互发送消息或双向通信时 requiresPeerCommunication=true，并在指令中明确要求实际 messages 请求。任务板输出不能替代消息。你负责最终汇报，不必额外拆一个汇报子任务。"+
			"拆成 1–8 项，尽可能让不同执行端分工，明确依赖；禁止编造执行端或已完成结果。\n"+
			"只读不等于禁止读取命令：Codex 使用只读命令读取文件。若用户未禁止，不要禁止文件读取所需的命令。需要互相询问的任务应避免把回答者设置成提问者的前置依赖。\n"+
			"用户目标："+objective+"\n执行端（agentId 必须逐字符复制 id，禁止省略数字）："+string(rosterJSON), "", false, "")
	if err != nil {
		return err
	}
	var planErr error
	for attempt := 0; attempt < 2; attempt++ {
		if attempt > 0 {
			rawPlan, err = o.call(m, m.ManagerID, "宰相修正计划",
				"上一次计划验证失败："+planErr.Error()+"。只修正无效字段，返回同一 JSON tasks 格式（优先用 submit_result 工具提交），agentId 必须逐字符复制名单中的 id。\n"+
					"执行端名单："+string(rosterJSON)+"\n用户目标："+objective+"\n原计划："+rawPlan.Output, "", false, "")
			if err != nil {
				return err
			}
		}
		tasks, verr := tasksFromPlan(m, rawPlan.Parsed)
		if verr != nil {
			planErr = verr
			continue
		}
		m.lock()
		m.Tasks = tasks
		msg, _ := rawPlan.Parsed["message"].(string)
		m.PlanMessage = msg
		peerWanted, _ := rawPlan.Parsed["requiresPeerCommunication"].(bool)
		m.unlock()
		peerRe := regexp.MustCompile(`双向|相互.{0,12}(消息|通信|发送)|互相.{0,12}(消息|通信|发送)|互发`)
		if peerWanted || peerRe.MatchString(objective) {
			m.lock()
			m.RequiresPeerCommunication = true
			m.unlock()
		}
		planErr = nil
		break
	}
	if planErr != nil {
		return planErr
	}
	m.lock()
	m.Status = "running"
	assign := make([]*MissionTask, len(m.Tasks))
	copy(assign, m.Tasks)
	m.unlock()
	o.emit(m)
	for _, t := range assign {
		payload, _ := json.Marshal(map[string]any{"taskId": t.ID, "title": t.Title, "instructions": t.Instructions})
		if err := o.event(m, m.ManagerID, t.AgentID, string(payload), "task.assign"); err != nil {
			return err
		}
	}
	return o.work(m)
}

type waveOutcome struct {
	task     *MissionTask
	messages []map[string]any
}

func (o *Orchestrator) work(m *Mission) error {
	for round := 1; round <= 6; round++ {
		if err := o.adjust(m); err != nil {
			return err
		}
		m.lock()
		m.Round = round
		var runnable []*MissionTask
		for _, t := range m.Tasks {
			ready := t.Status == "queued" || len(t.Inbox) > 0
			for _, dep := range t.DependsOn {
				for _, x := range m.Tasks {
					if x.ID == dep && x.Status != "completed" {
						ready = false
					}
				}
			}
			if ready {
				runnable = append(runnable, t)
			}
		}
		if len(runnable) == 0 {
			m.unlock()
			break
		}
		// Each wave receives the same board; messages deliver after the wave.
		board := make([]map[string]any, 0, len(m.Tasks))
		for _, t := range m.Tasks {
			board = append(board, map[string]any{"id": t.ID, "agentId": t.AgentID, "title": t.Title,
				"dependsOn": t.DependsOn, "status": t.Status, "output": t.Output})
		}
		boardJSON, _ := json.Marshal(board)
		objective := m.Objective
		perm := m.Permission
		directiveJSON, _ := json.Marshal(m.DirectiveHistory)
		m.unlock()

		outcomes := make(chan waveOutcome, len(runnable))
		execute := func(t *MissionTask) {
			defer func() {
				if r := recover(); r != nil {
					m.lock()
					t.Status = "failed"
					t.Error = fmt.Sprintf("执行异常：%v", r)
					m.unlock()
					o.emit(m)
				}
			}()
			m.lock()
			inbox := t.Inbox
			t.Inbox = []InboxMsg{}
			t.Status = "running"
			t.Turns++
			m.unlock()
			o.emit(m)
			tJSON, _ := json.Marshal(map[string]any{"id": t.ID, "agentId": t.AgentID, "title": t.Title,
				"instructions": t.Instructions, "dependsOn": t.DependsOn, "model": t.Model, "status": "running",
				"output": t.Output, "inbox": inbox, "files": t.Files, "turns": t.Turns})
			modeNote := "可使用 Harness 原生工具完成授权范围内的实际修改或命令执行，需授权时等待用户选择；工具不可用时如实返回 blocked=true。\n"
			if perm == "readonly" {
				modeNote = "当前只读：可以读取和检索文件，不能修改文件或执行写入命令；权限请求会受当前只读模式限制。\n"
			}
			response, err := o.call(m, t.AgentID, t.Title+" · 第 "+fmt.Sprint(t.Turns)+" 轮",
				"你是执行 Harness。实际完成指派工作，可读取共享分工和同僚消息。不要声称未执行的工作已完成。\n"+
					"优先通过 submit_result 工具提交结果，把 JSON 字段作为工具参数传入；工具不可用时才在正文中只输出 JSON：{\"output\":\"实际成果、文件路径和验证依据，或明确阻塞原因\",\"complete\":true,\"blocked\":false,\"reads\":[{\"path\":\"相对工作目录的文件路径\",\"startLine\":1,\"lineCount\":200}],\"messages\":[{\"to\":\"子任务ID\",\"text\":\"给同僚的消息或问题\"}]}。"+
					"你运行在真实的 Harness 会话中，可以使用自己的已授权原生工具。读取文件工具不可用时，可用 reads 数组请求服务端读取，在下一轮 files 中返回内容，此时 complete=false、blocked=false。已有 files 内容可直接使用，不要重复请求。\n"+
					modeNote+
					"需要同僚回答时 complete=false。messages 是实际投递请求，只能发给当前任务板中的其他子任务；不要重复发送相同消息。\n"+
					"如果任务要求相互通信或互相核对，必须真实返回 messages，不能只看任务板后宣称已经交流。\n"+
					"complete 表示工作实际达成，不是本次回复结束。受阻、无法读取文件、未验证目标时必须 complete=false；环境或权限导致无法继续时同时 blocked=true，避免重复调用。不要在 output 声称已发出没有放入 messages 的消息。\n"+
					"当前 inbox 是最新已投递消息；全体任务板的 output 是历史产出，若矛盾请以 inbox 为准。\n"+
					"用户目标："+objective+"\n追加要求："+string(directiveJSON)+"\n你的子任务："+string(tJSON)+"\n全体任务板："+string(boardJSON), t.Model, true, t.ID)
			if err != nil {
				t.Status = "failed"
				t.Error = err.Error()
				o.emit(m)
				return
			}
			resp := response.Parsed
			output, _ := resp["output"].(string)
			complete, _ := resp["complete"].(bool)
			if strings.TrimSpace(output) == "" {
				t.Status = "failed"
				t.Error = "执行结果格式无效"
				o.emit(m)
				return
			}
			var rawMessages []any
			if resp["messages"] != nil {
				rawMessages, _ = resp["messages"].([]any)
				if rawMessages == nil {
					t.Status = "failed"
					t.Error = "执行结果格式无效"
					o.emit(m)
					return
				}
			}
			var rawReads []any
			if resp["reads"] != nil {
				rawReads, _ = resp["reads"].([]any)
			}
			if len(rawReads) > 4 {
				t.Status = "failed"
				t.Error = "文件读取请求无效"
				o.emit(m)
				return
			}
			var reads []map[string]any
			for _, r := range rawReads {
				rm, ok := r.(map[string]any)
				if !ok {
					reads = nil
					break
				}
				p, _ := rm["path"].(string)
				if p == "" {
					reads = nil
					break
				}
				reads = append(reads, rm)
			}
			if resp["reads"] != nil && reads == nil {
				t.Status = "failed"
				t.Error = "文件读取请求无效"
				o.emit(m)
				return
			}
			var messages []map[string]any
			invalid := len(rawMessages) > 8
			if !invalid {
				for _, x := range rawMessages {
					xm, ok := x.(map[string]any)
					if !ok {
						invalid = true
						break
					}
					to, _ := xm["to"].(string)
					text, _ := xm["text"].(string)
					known := false
					m.lock()
					for _, v := range m.Tasks {
						if v.ID == to {
							known = true
						}
					}
					m.unlock()
					if to == t.ID || !known || strings.TrimSpace(text) == "" || len(text) > 6000 {
						invalid = true
						break
					}
					messages = append(messages, xm)
				}
			}
			if invalid {
				t.Status = "failed"
				t.Error = "同僚消息目标或内容无效"
				o.emit(m)
				return
			}
			blocked, _ := resp["blocked"].(bool)
			m.lock()
			t.Output = output
			if blocked {
				t.Status = "blocked"
			} else if complete {
				t.Status = "completed"
			} else {
				t.Status = "queued"
			}
			m.unlock()
			for _, read := range reads {
				content := map[string]any{"path": read["path"]}
				if o.ReadFile == nil {
					content["error"] = "当前执行服务未配置文件读取"
				} else if c, err := o.ReadFile(m, read); err != nil {
					content["error"] = err.Error()
				} else {
					content = c
				}
				m.lock()
				var kept []map[string]any
				for _, f := range t.Files {
					if f["path"] != content["path"] {
						kept = append(kept, f)
					}
				}
				t.Files = append(kept, content)
				if len(t.Files) > 12 {
					t.Files = t.Files[len(t.Files)-12:]
				}
				m.unlock()
				payload, _ := json.Marshal(map[string]any{"path": content["path"], "read": content["error"] == nil, "error": content["error"]})
				o.event(m, "hub", t.AgentID, string(payload), "ctx.sync")
				m.lock()
				t.Status = "queued"
				m.unlock()
			}
			outcomes <- waveOutcome{task: t, messages: messages}
			// Truncate the payload field, never the serialized bytes: cutting
			// JSON at 20000 bytes drops the closing brace and makes the event
			// unparseable downstream.
			resultPayload, _ := json.Marshal(map[string]any{"taskId": t.ID, "complete": complete, "output": textutil.TruncateRunes(t.Output, 18000)})
			o.event(m, t.AgentID, m.ManagerID, string(resultPayload), "task.result")
		}
		m.lock()
		parallel := perm == "readonly"
		m.unlock()
		if parallel {
			var wg sync.WaitGroup
			for _, t := range runnable {
				wg.Add(1)
				go func(t *MissionTask) { defer wg.Done(); execute(t) }(t)
			}
			wg.Wait()
			close(outcomes)
		} else {
			for _, t := range runnable {
				execute(t)
			}
			close(outcomes)
		}
		for out := range outcomes {
			for _, msg := range out.messages {
				to, _ := msg["to"].(string)
				text, _ := msg["text"].(string)
				var target *MissionTask
				m.lock()
				for _, t := range m.Tasks {
					if t.ID == to {
						target = t
					}
				}
				dup := false
				if target != nil {
					for _, e := range m.Events {
						if e.TaskFrom == out.task.ID && e.TaskTo == target.ID && e.Text == text {
							dup = true
						}
					}
				}
				m.unlock()
				if target == nil || dup {
					continue
				}
				if err := o.event(m, out.task.AgentID, target.AgentID, text, ""); err != nil {
					return err
				}
				m.lock()
				if n := len(m.Events); n > 0 {
					m.Events[n-1].TaskFrom = out.task.ID
					m.Events[n-1].TaskTo = target.ID
				}
				target.Inbox = append(target.Inbox, InboxMsg{From: out.task.ID, Text: text})
				if target.Status == "completed" {
					target.Status = "queued"
				}
				m.unlock()
			}
		}
		o.emit(m)
		m.lock()
		allDone := true
		for _, t := range m.Tasks {
			if t.Status != "completed" || len(t.Inbox) > 0 {
				allDone = false
			}
		}
		peer := m.RequiresPeerCommunication && !m.CommunicationRequested && allDone
		m.unlock()
		if peer && !o.peerConfirmed(m) {
			m.lock()
			var first, second *MissionTask
			first = m.Tasks[0]
			for _, t := range m.Tasks {
				if t.AgentID != first.AgentID {
					second = t
				}
			}
			m.unlock()
			if second != nil {
				for _, pair := range [][2]*MissionTask{{first, second}, {second, first}} {
					task, peerTask := pair[0], pair[1]
					m.lock()
					task.Inbox = append(task.Inbox, InboxMsg{From: "宰相", Text: "用户要求实际双向通信。请通过 messages 向子任务 " + peerTask.ID + " 发送你的结论，并处理其回信；不能只引用任务板作为通信证据。"})
					task.Status = "queued"
					m.unlock()
				}
				m.lock()
				m.CommunicationRequested = true
				m.unlock()
				o.emit(m)
			}
		}
		m.lock()
		done := allDone
		pending := len(m.PendingInstructions)
		m.unlock()
		if done && pending == 0 {
			break
		}
	}
	m.lock()
	pending := len(m.PendingInstructions)
	m.unlock()
	if pending > 0 {
		if err := o.adjust(m); err != nil {
			return err
		}
		return o.work(m)
	}
	m.lock()
	complete := true
	for _, t := range m.Tasks {
		if t.Status != "completed" || len(t.Inbox) > 0 {
			complete = false
		}
	}
	if complete && m.RequiresPeerCommunication && !o.peerConfirmedLocked(m) {
		complete = false
	}
	m.Status = "reporting"
	m.unlock()
	o.emit(m)
	// Manager verification & report
	m.lock()
	tasksJSON, _ := json.Marshal(m.Tasks)
	directiveJSON, _ := json.Marshal(m.DirectiveHistory)
	var collab []*MissionEvent
	for _, e := range m.Events {
		if e.Kind == "collab.ask" {
			collab = append(collab, e)
		}
	}
	collabJSON, _ := json.Marshal(collab)
	m.unlock()
	verdict, err := o.call(m, m.ManagerID, "宰相汇总汇报",
		"你是宰相，现在验收真实成果并向用户汇报。优先通过 submit_result 工具提交 JSON（字段作为工具参数传入），工具不可用时才在正文中只输出 JSON：{\"complete\":true,\"reply\":\"直接给用户的自然语言汇报\",\"report\":\"详细验收依据\",\"taskVerdicts\":[{\"id\":\"子任务ID\",\"accepted\":true,\"reason\":\"验收依据或未完成原因\"}]}。"+
			"reply 必须遵守用户要求的格式和长度，默认用2-4句直接汇报成果和未完成项，不要堆砌内部ID或日志；详细证明放在 report。"+
			"逐项验收所有子任务，检查 output 中是否有受阻、未读取或未验证。complete 只有当用户目标实际全部达成时为 true。报告包括分工、成果、证据、失败和未解决事项。"+
			"不得把子任务声明当作独立验证；通信事实只能依据下面的实际消息记录，禁止编造问答。用户明确要求消息交换时，任务板不能作为等效替代；调度器判定未全部完成时必须如实报告未完成。\n"+
			"调度器是否全部完成："+map[bool]string{true: "true", false: "false"}[complete]+"\n用户目标："+m.Objective+"\n追加要求："+string(directiveJSON)+"\n任务板："+string(tasksJSON)+
			"\n实际消息记录："+string(collabJSON), "", false, "")
	if err != nil {
		return err
	}
	m.lock()
	pending = len(m.PendingInstructions)
	m.unlock()
	if pending > 0 {
		if err := o.adjust(m); err != nil {
			return err
		}
		return o.work(m)
	}
	v := verdict.Parsed
	verdictComplete, _ := v["complete"].(bool)
	report, _ := v["report"].(string)
	rawVerdicts, _ := v["taskVerdicts"].([]any)
	if report == "" || len(rawVerdicts) != len(m.Tasks) {
		return errors.New("宰相未返回完整的逐项验收结论")
	}
	seen := map[string]bool{}
	var verdicts []map[string]any
	for _, item := range rawVerdicts {
		vm, ok := item.(map[string]any)
		if !ok {
			return errors.New("宰相未返回完整的逐项验收结论")
		}
		id, _ := vm["id"].(string)
		accepted, _ := vm["accepted"].(bool)
		reason, _ := vm["reason"].(string)
		found := false
		for _, t := range m.Tasks {
			if t.ID == id {
				found = true
			}
		}
		if seen[id] || !found {
			return errors.New("宰相未返回完整的逐项验收结论")
		}
		seen[id] = true
		verdicts = append(verdicts, map[string]any{"id": id, "accepted": accepted, "reason": reason})
	}
	m.lock()
	allAccepted := true
	for _, vd := range verdicts {
		if acc, _ := vd["accepted"].(bool); !acc {
			allAccepted = false
			for _, t := range m.Tasks {
				if t.ID == vd["id"] {
					t.Status = "failed"
					t.Error, _ = vd["reason"].(string)
				}
			}
		}
	}
	m.Report = report
	reply, _ := v["reply"].(string)
	if strings.TrimSpace(reply) == "" {
		reply = report
	}
	m.Reply = reply
	m.Verdicts = verdicts
	if complete && verdictComplete && allAccepted {
		m.Status = "completed"
	} else {
		m.Status = "partial"
	}
	m.FinishedAt = nowMs()
	if m.Status == "partial" && m.RequiresPeerCommunication && !o.peerConfirmedLocked(m) {
		m.Reply = "本轮部分完成：尚无可核验的双向通信记录。\n\n" + m.Reply
	}
	reportSnippet := m.Report
	if len(reportSnippet) > 20000 {
		reportSnippet = reportSnippet[:20000]
	}
	m.unlock()
	o.event(m, m.ManagerID, "hub", reportSnippet, "task.result")
	o.emit(m)
	return nil
}

// CallForHandoff runs a single manager-style call for session handoff briefs.
func (o *Orchestrator) CallForHandoff(m *Mission, agentID, prompt string) (*CallResult, error) {
	return o.call(m, agentID, "会话移交简报", prompt, "", false, "")
}

// peerConfirmed checks for at least one bidirectional exchange in the records.
func (o *Orchestrator) peerConfirmed(m *Mission) bool {
	m.lock()
	defer m.unlock()
	return o.peerConfirmedLocked(m)
}

func (o *Orchestrator) peerConfirmedLocked(m *Mission) bool {
	var messages []*MissionEvent
	for _, e := range m.Events {
		if e.Kind == "collab.ask" && e.From != e.To {
			messages = append(messages, e)
		}
	}
	for _, e := range messages {
		for _, reply := range messages {
			if reply.From == e.To && reply.To == e.From {
				return true
			}
		}
	}
	return false
}

// forkDepth saturating-walks the forkedFrom chain (codeg's depth rule).
func (o *Orchestrator) forkDepth(m *Mission, cap int) int {
	o.mu.Lock()
	defer o.mu.Unlock()
	current := m
	depth := 0
	for depth < cap && current != nil && current.ForkedFrom != "" {
		var next *Mission
		for _, x := range o.missions {
			if x.ID == current.ForkedFrom {
				next = x
			}
		}
		if next == nil {
			break
		}
		current = next
		depth++
	}
	return depth
}

// Fork branches a finished board into a fresh mission. Outputs and files carry
// over as context; every task re-queues and dependencies persist.
func (o *Orchestrator) Fork(id, taskID string) (*Mission, error) {
	o.mu.Lock()
	m := o.missionLocked(id)
	o.mu.Unlock()
	if m == nil {
		return nil, errors.New("任务不存在")
	}
	m.lock()
	terminal := m.Status == "completed" || m.Status == "partial" || m.Status == "failed"
	m.unlock()
	if !terminal {
		return nil, errors.New("任务尚未结束，不能分叉")
	}
	if o.forkDepth(m, 5) >= 5 {
		return nil, errors.New("分叉嵌套已达上限（5 层），请基于更早的任务重新规划")
	}
	keep := map[string]bool{}
	var add func(tid string)
	add = func(tid string) {
		if keep[tid] {
			return
		}
		keep[tid] = true
		m.lock()
		for _, t := range m.Tasks {
			if t.ID == tid {
				for _, dep := range t.DependsOn {
					m.unlock()
					add(dep)
					m.lock()
				}
			}
		}
		m.unlock()
	}
	m.lock()
	if taskID != "" {
		exists := false
		for _, t := range m.Tasks {
			if t.ID == taskID {
				exists = true
			}
		}
		if !exists {
			m.unlock()
			return nil, errors.New("子任务不存在")
		}
		m.unlock()
		add(taskID)
	} else {
		for _, t := range m.Tasks {
			keep[t.ID] = true
		}
		m.unlock()
	}
	m.lock()
	var tasks []*MissionTask
	for _, t := range m.Tasks {
		if !keep[t.ID] {
			continue
		}
		cp := *t
		cp.Status = "queued"
		cp.Inbox = []InboxMsg{}
		cp.Turns = 0
		cp.Error = ""
		tasks = append(tasks, &cp)
	}
	f := &Mission{ID: "mission_" + newOrchID(), Objective: m.Objective, ManagerID: m.ManagerID,
		WorkerIDs: append([]string{}, m.WorkerIDs...), Sensitivity: m.Sensitivity, Permission: m.Permission,
		Workdir: m.BaseWorkdir, Isolate: m.Isolate, SkipPlanning: true, ChatID: m.ChatID,
		Status: "running", Tasks: tasks, Events: []*MissionEvent{}, CreatedAt: nowMs(),
		ManagerModel: m.ManagerModel, PendingInstructions: []*Steering{},
		PinnedTasks: map[string]map[string]any{}, ForkedFrom: m.ID}
	if f.Workdir == "" {
		f.Workdir = m.Workdir
	}
	if m.Models != nil {
		f.Models = map[string]string{}
		for k, v := range m.Models {
			f.Models[k] = v
		}
	}
	m.unlock()
	o.mu.Lock()
	o.missions = append([]*Mission{f}, o.missions...)
	o.mu.Unlock()
	o.emit(f)
	o.launchWork(f)
	return f, nil
}

func (o *Orchestrator) launchWork(m *Mission) {
	go func() {
		defer func() {
			if r := recover(); r != nil {
				o.fail(m, fmt.Sprintf("编排器异常：%v", r))
			}
		}()
		if err := o.work(m); err != nil {
			o.fail(m, err.Error())
		}
	}()
}

// --- accessors ---

func (o *Orchestrator) missionLocked(id string) *Mission {
	for _, m := range o.missions {
		if m.ID == id {
			return m
		}
	}
	return nil
}

// Get returns the mission pointer (mutations go through the orchestrator).
func (o *Orchestrator) Get(id string) *Mission {
	o.mu.Lock()
	defer o.mu.Unlock()
	return o.missionLocked(id)
}

// Marshal snapshots one mission as JSON under its own lock.
func (o *Orchestrator) Marshal(m *Mission) json.RawMessage {
	m.lock()
	defer m.unlock()
	b, _ := json.Marshal(m)
	return b
}

// MarshalAll snapshots the whole mission list.
func (o *Orchestrator) MarshalAll() json.RawMessage {
	o.mu.Lock()
	defer o.mu.Unlock()
	if o.missions == nil {
		o.missions = []*Mission{}
	}
	b, _ := json.Marshal(o.missions)
	return b
}

// Status reads a mission status safely.
func (o *Orchestrator) Status(m *Mission) string {
	m.lock()
	defer m.unlock()
	return m.Status
}

// BaseDir returns the mission's non-isolated workdir.
func (o *Orchestrator) BaseDir(m *Mission) string {
	m.lock()
	defer m.unlock()
	if m.BaseWorkdir != "" {
		return m.BaseWorkdir
	}
	return m.Workdir
}

// Restore loads persisted missions; interrupted runs are marked failed so
// nothing re-executes silently after a restart.
func (o *Orchestrator) Restore(raw json.RawMessage) error {
	var saved []*Mission
	if json.Unmarshal(raw, &saved) != nil {
		return errors.New("任务记录格式无效")
	}
	for _, m := range saved {
		if m.Status == "planning" || m.Status == "running" || m.Status == "reporting" {
			m.Status = "failed"
			m.Error = "服务重启中断了执行，请重新下达目标；不会自动重复执行。"
			for _, t := range m.Tasks {
				if t.Status != "completed" {
					t.Status = "failed"
					t.Error = m.Error
				}
			}
		}
	}
	o.mu.Lock()
	o.missions = saved
	o.mu.Unlock()
	return nil
}
