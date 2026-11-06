// ChatController is the Go port of core/chat-controller.js: multi-turn chats
// that route every user message through the 宰相 decision stage, with
// codeg-style @-mention delegation.
package orch

import (
	"encoding/json"
	"errors"
	"fmt"
	"regexp"
	"strconv"
	"strings"
	"sync"

	"harnesshub/internal/hub"
)

func chatID() string { return "chat_" + newOrchID() }

// ChatMessage is one turn in a chat transcript.
type ChatMessage struct {
	ID        string `json:"id"`
	Role      string `json:"role"`
	Text      string `json:"text"`
	At        int64  `json:"at"`
	AgentID   string `json:"agentId,omitempty"`
	Model     string `json:"model,omitempty"`
	MissionID string `json:"missionId,omitempty"`
	Key       string `json:"key,omitempty"`
	Note      string `json:"note,omitempty"`
	Details   string `json:"details,omitempty"`
}

// ChatConfig is the per-chat execution setup.
type ChatConfig struct {
	ManagerID    string            `json:"managerId"`
	ManagerModel string            `json:"managerModel"`
	WorkerIDs    []string          `json:"workerIds"`
	Models       map[string]string `json:"models"`
	Permission   string            `json:"permission"`
	Workdir      string            `json:"workdir"`
	Isolate      string            `json:"isolate"`
}

// Chat is one conversation shell around missions.
type Chat struct {
	ID         string         `json:"id"`
	Title      string         `json:"title"`
	Config     ChatConfig     `json:"config"`
	Messages   []*ChatMessage `json:"messages"`
	MissionIDs []string       `json:"missionIds"`
	Pending    int            `json:"pending"`
	CreatedAt  int64          `json:"createdAt"`
	UpdatedAt  int64          `json:"updatedAt"`
}

type chatJob struct {
	chat      *Chat
	user      *ChatMessage
	overrides []map[string]any
	mentions  []string
}

// ChatController owns chats and serialises decision turns per chat.
type ChatController struct {
	Hub  *hub.Hub
	Orch *Orchestrator

	mu     sync.Mutex
	chats  []*Chat
	queues map[string]chan chatJob
	seq    int
}

// NewChatController wires the controller and starts its mission watcher.
func NewChatController(h *hub.Hub, o *Orchestrator) *ChatController {
	c := &ChatController{Hub: h, Orch: o, queues: map[string]chan chatJob{}}
	go c.watchMissions()
	return c
}

func (c *ChatController) emit(chat *Chat) {
	c.mu.Lock()
	chat.UpdatedAt = nowMs()
	c.mu.Unlock()
	c.Hub.Emit("chat", map[string]any{"id": chat.ID, "updatedAt": chat.UpdatedAt})
}

func (c *ChatController) agents() []*hub.Agent {
	var out []*hub.Agent
	for _, a := range c.Hub.Agents() {
		if a.Enabled && (a.Real || a.Communication == "http") {
			out = append(out, a)
		}
	}
	return out
}

// Config validates (and defaults) a chat config, mirroring the Node config().
func (c *ChatController) Config(value map[string]any) (ChatConfig, error) {
	roster := c.agents()
	lookup := func(v any) string {
		s, _ := v.(string)
		return s
	}
	managerID := lookup(value["managerId"])
	if managerID == "" {
		for _, a := range roster {
			if a.RealTool == "claude" {
				managerID = a.ID
				break
			}
		}
	}
	if managerID == "" && len(roster) > 0 {
		managerID = roster[0].ID
	}
	if managerID == "" {
		return ChatConfig{}, errors.New("请先接入一个真实 Harness")
	}
	if _, err := c.Orch.Eligible(managerID, 1); err != nil {
		return ChatConfig{}, err
	}
	var workerIDs []string
	if raw, ok := value["workerIds"].([]any); ok {
		for _, w := range raw {
			if s, ok := w.(string); ok {
				workerIDs = append(workerIDs, s)
			}
		}
	} else if _, exists := value["workerIds"]; exists && value["workerIds"] != nil {
		return ChatConfig{}, errors.New("请选择 1–8 个执行 Harness")
	}
	if len(workerIDs) == 0 {
		for _, a := range roster {
			workerIDs = append(workerIDs, a.ID)
		}
	}
	if len(workerIDs) == 0 || len(workerIDs) > 8 {
		return ChatConfig{}, errors.New("请选择 1–8 个执行 Harness")
	}
	for _, id := range workerIDs {
		if _, err := c.Orch.Eligible(id, 1); err != nil {
			return ChatConfig{}, err
		}
	}
	seen := map[string]bool{}
	uniqueWorkers := make([]string, 0, len(workerIDs))
	for _, id := range workerIDs {
		if !seen[id] {
			seen[id] = true
			uniqueWorkers = append(uniqueWorkers, id)
		}
	}
	models := map[string]string{}
	if raw, ok := value["models"].(map[string]any); ok {
		for k, v := range raw {
			if s, ok := v.(string); ok {
				models[k] = s
			}
		}
	} else if _, exists := value["models"]; exists && value["models"] != nil {
		return ChatConfig{}, errors.New("模型配置无效")
	}
	modelRe := regexp.MustCompile(`^[a-zA-Z0-9][\w.:/@+\-]{0,119}$`)
	if m, ok := value["managerModel"].(string); ok && m != "" && !modelRe.MatchString(m) {
		return ChatConfig{}, errors.New("模型名称无效")
	}
	for _, model := range models {
		if model != "" && !modelRe.MatchString(model) {
			return ChatConfig{}, errors.New("模型名称无效")
		}
	}
	permission, _ := value["permission"].(string)
	if permission == "" {
		permission = "readonly"
	}
	if permission != "readonly" && permission != "workspace" && permission != "shell" {
		return ChatConfig{}, errors.New("权限配置无效")
	}
	isolate := "shared"
	if raw, ok := value["isolate"].(string); ok && raw != "" {
		if raw != "shared" && raw != "worktree" {
			return ChatConfig{}, errors.New("隔离设置无效")
		}
		isolate = raw
	}
	workdir, _ := value["workdir"].(string)
	managerModel, _ := value["managerModel"].(string)
	return ChatConfig{ManagerID: managerID, ManagerModel: managerModel, WorkerIDs: uniqueWorkers,
		Models: models, Permission: permission, Workdir: workdir, Isolate: isolate}, nil
}

func cfgToMap(cfg ChatConfig) map[string]any {
	b, _ := json.Marshal(cfg)
	var m map[string]any
	_ = json.Unmarshal(b, &m)
	return m
}

// Create opens a new chat.
func (c *ChatController) Create(value map[string]any) (*Chat, error) {
	cfg, err := c.Config(value)
	if err != nil {
		return nil, err
	}
	chat := &Chat{ID: chatID(), Title: "新对话", Config: cfg, Messages: []*ChatMessage{},
		MissionIDs: []string{}, CreatedAt: nowMs()}
	c.mu.Lock()
	c.chats = append([]*Chat{chat}, c.chats...)
	c.mu.Unlock()
	c.emit(chat)
	return chat, nil
}

// Get returns a chat or errors.
func (c *ChatController) Get(id string) (*Chat, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, ch := range c.chats {
		if ch.ID == id {
			return ch, nil
		}
	}
	return nil, errors.New("对话不存在")
}

// Chats returns all chats (snapshot list).
func (c *ChatController) Chats() []*Chat {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]*Chat{}, c.chats...)
}

// active finds the newest non-terminal mission of the chat.
func (c *ChatController) active(chat *Chat) *Mission {
	for i := len(chat.MissionIDs) - 1; i >= 0; i-- {
		m := c.Orch.Get(chat.MissionIDs[i])
		if m == nil {
			continue
		}
		st := c.Orch.Status(m)
		if st != "completed" && st != "partial" && st != "failed" {
			return m
		}
	}
	return nil
}

// MarshalChat snapshots a chat (with its missions) as JSON.
func (c *ChatController) MarshalChat(chat *Chat) json.RawMessage {
	c.mu.Lock()
	base, _ := json.Marshal(chat)
	var m map[string]any
	_ = json.Unmarshal(base, &m)
	var missions []json.RawMessage
	for _, id := range chat.MissionIDs {
		if mission := c.Orch.Get(id); mission != nil {
			missions = append(missions, c.Orch.Marshal(mission))
		}
	}
	c.mu.Unlock()
	if missions == nil {
		missions = []json.RawMessage{}
	}
	m["missions"] = missions
	out, _ := json.Marshal(m)
	return out
}

// MarshalAll snapshots every chat (list view, without missions).
func (c *ChatController) MarshalAll() json.RawMessage {
	c.mu.Lock()
	defer c.mu.Unlock()
	b, _ := json.Marshal(c.chats)
	return b
}

// Message appends a transcript entry.
func (c *ChatController) Message(chat *Chat, role, text string, extra map[string]any) *ChatMessage {
	item := &ChatMessage{ID: "turn_" + newOrchID(), Role: role, Text: text, At: nowMs()}
	if v, ok := extra["agentId"].(string); ok {
		item.AgentID = v
	}
	if v, ok := extra["model"].(string); ok {
		item.Model = v
	}
	if v, ok := extra["missionId"].(string); ok {
		item.MissionID = v
	}
	if v, ok := extra["key"].(string); ok {
		item.Key = v
	}
	if v, ok := extra["note"].(string); ok {
		item.Note = v
	}
	if v, ok := extra["details"].(string); ok {
		item.Details = v
	}
	c.mu.Lock()
	chat.Messages = append(chat.Messages, item)
	c.mu.Unlock()
	c.emit(chat)
	return item
}

// Configure patches a chat's config.
func (c *ChatController) Configure(id string, patch map[string]any) (*Chat, error) {
	chat, err := c.Get(id)
	if err != nil {
		return nil, err
	}
	c.mu.Lock()
	merged := cfgToMap(chat.Config)
	c.mu.Unlock()
	for k, v := range patch {
		merged[k] = v
	}
	cfg, err := c.Config(merged)
	if err != nil {
		return nil, err
	}
	c.mu.Lock()
	chat.Config = cfg
	c.mu.Unlock()
	c.emit(chat)
	return chat, nil
}

var mentionRe = regexp.MustCompile(`@([^\s@，。、,]+)`)

// ParseMentions resolves @tokens to agent ids (dedup-then-cap happens in Send).
func (c *ChatController) ParseMentions(text string) ([]string, error) {
	tokens := mentionRe.FindAllStringSubmatch(text, -1)
	if len(tokens) == 0 {
		return nil, nil
	}
	roster := c.agents()
	var out []string
	for _, tok := range tokens {
		token := tok[1]
		hit := ""
		for _, a := range roster {
			if a.ID == token || a.Name == token ||
				strings.EqualFold(a.ID, token) || strings.EqualFold(a.Name, token) {
				hit = a.ID
				break
			}
		}
		if hit == "" {
			names := make([]string, 0, len(roster))
			for _, a := range roster {
				names = append(names, a.Name)
			}
			sep := "、"
			if len(names) == 0 {
				return nil, errors.New("未找到执行端 @" + token + "，可用：（无已连接 Harness）")
			}
			return nil, errors.New("未找到执行端 @" + token + "，可用：" + strings.Join(names, sep))
		}
		out = append(out, hit)
	}
	return out, nil
}

// delegate starts one skipPlanning mission per mentioned agent (codeg-style).
func (c *ChatController) delegate(chat *Chat, user *ChatMessage, agentIDs []string) []*Mission {
	stripRe := regexp.MustCompile(`@[^\s@，。、,]+`)
	objective := strings.TrimSpace(stripRe.ReplaceAllString(user.Text, ""))
	if objective == "" {
		objective = strings.TrimSpace(user.Text)
	}
	if len(objective) > 12000 {
		objective = objective[:12000]
	}
	c.mu.Lock()
	cfg := chat.Config
	var created []*Mission
	c.mu.Unlock()
	for _, agentID := range agentIDs {
		spec := Spec{
			Objective: objective, ManagerID: cfg.ManagerID, WorkerIDs: []string{agentID},
			Sensitivity: intPtr(1), Permission: cfg.Permission, Workdir: cfg.Workdir,
			Isolate: cfg.Isolate, ManagerModel: cfg.ManagerModel, Models: cfg.Models,
			ChatID: chat.ID, SkipPlanning: true,
		}
		m, err := c.Orch.Create(spec)
		if err != nil {
			c.Message(chat, "error", err.Error(), nil)
			continue
		}
		c.mu.Lock()
		chat.MissionIDs = append(chat.MissionIDs, m.ID)
		c.mu.Unlock()
		created = append(created, m)
	}
	c.emit(chat)
	return created
}

func intPtr(v int) *int { return &v }

// Send records the user message and queues the decision turn.
func (c *ChatController) Send(id, text string, overrides []map[string]any, mentions []string) (*ChatMessage, error) {
	chat, err := c.Get(id)
	if err != nil {
		return nil, err
	}
	trimmed := strings.TrimSpace(text)
	if trimmed == "" || len(text) > 12000 {
		return nil, errors.New("请输入 1–12000 字符的消息")
	}
	if len(overrides) > 8 {
		return nil, errors.New("人工分工无效")
	}
	if len(mentions) > 4 {
		return nil, errors.New("委派名单无效")
	}
	for _, m := range mentions {
		if strings.TrimSpace(m) == "" {
			return nil, errors.New("委派名单无效")
		}
	}
	// Visible-mention routing semantics (codeg): pasted mentions route exactly
	// like fresh ones — dedup-then-cap before delegating.
	extraMentions, err := c.ParseMentions(trimmed)
	if err != nil {
		return nil, err
	}
	seen := map[string]bool{}
	var merged []string
	for _, id := range append(append([]string{}, extraMentions...), mentions...) {
		if !seen[id] {
			seen[id] = true
			merged = append(merged, id)
		}
	}
	if len(merged) > 4 {
		merged = merged[:4]
	}
	user := c.Message(chat, "user", trimmed, nil)
	c.mu.Lock()
	if chat.Title == "新对话" {
		runes := []rune(trimmed)
		if len(runes) > 36 {
			chat.Title = string(runes[:36])
		} else {
			chat.Title = trimmed
		}
	}
	chat.Pending++
	c.mu.Unlock()
	c.emit(chat)

	job := chatJob{chat: chat, user: user, overrides: overrides, mentions: merged}
	c.mu.Lock()
	q, ok := c.queues[id]
	if !ok {
		q = make(chan chatJob, 64)
		c.queues[id] = q
		c.mu.Unlock()
		go c.worker(id, q)
	} else {
		c.mu.Unlock()
	}
	q <- job
	return user, nil
}

// worker serialises decision turns per chat, like the Node promise queue.
// One worker lives per active chat; it exits only when the controller dies.
func (c *ChatController) worker(id string, q chan chatJob) {
	for job := range q {
		func() {
			defer func() {
				if r := recover(); r != nil {
					c.Message(job.chat, "error", "对话处理异常："+fmt.Sprintf("%v", r), nil)
				}
				c.mu.Lock()
				job.chat.Pending--
				c.mu.Unlock()
				c.emit(job.chat)
			}()
			if err := c.decide(job); err != nil {
				c.Message(job.chat, "error", err.Error(), nil)
			}
		}()
	}
}

// Decide runs the 宰相对话 stage and routes the result (execute/steer/reply).
func (c *ChatController) decide(job chatJob) error {
	chat, user, overrides, extraMentions := job.chat, job.user, job.overrides, job.mentions
	c.mu.Lock()
	cfg := chat.Config
	msgs := append([]*ChatMessage{}, chat.Messages...)
	c.mu.Unlock()

	mentioned, err := c.ParseMentions(user.Text)
	if err != nil {
		return err
	}
	seen := map[string]bool{}
	var merged []string
	for _, id := range append(append([]string{}, mentioned...), extraMentions...) {
		if !seen[id] {
			seen[id] = true
			merged = append(merged, id)
		}
	}
	if len(merged) > 0 {
		c.delegate(chat, user, merged)
		return nil
	}
	active := c.active(chat)
	control := &Mission{ID: chat.ID, ChatID: chat.ID, ManagerID: cfg.ManagerID,
		ManagerModel: cfg.ManagerModel, Models: cfg.Models, Sensitivity: 1,
		Workdir: cfg.Workdir, Permission: "readonly"}
	cfgJSON, _ := json.Marshal(cfg)
	roster := c.agents()
	rosterList := make([]map[string]any, 0, len(roster))
	for _, a := range roster {
		rosterList = append(rosterList, map[string]any{"id": a.ID, "name": a.Name, "capabilities": a.Capabilities})
	}
	rosterJSON, _ := json.Marshal(rosterList)
	activeJSON := []byte("null")
	if active != nil {
		activeJSON = c.Orch.Marshal(active)
	}
	// transcript slice up to and including this user message
	userIdx := -1
	for i, msg := range msgs {
		if msg == user {
			userIdx = i
		}
	}
	if userIdx < 0 {
		userIdx = len(msgs) - 1
	}
	start := userIdx + 1 - 24
	if start < 0 {
		start = 0
	}
	transcript, _ := json.Marshal(msgs[start : userIdx+1])
	overridesJSON, _ := json.Marshal(overrides)
	prompt := "你是用户的宰相和主要负责人。以自然中文与用户对话，主动调用执行 Harness 完成明确的工作请求，不要仅表示能够做。" +
		"你可以自主挑选名单中的 Harness，规划器随后会根据能力和模型分工；尊重用户指定的执行端、模型和任务。\n" +
		"优先通过 submit_result 工具提交 JSON（字段作为工具参数传入），工具不可用时才在正文中只输出 JSON：{\"reply\":\"直接对用户说的话\",\"action\":\"reply 或 execute 或 steer\",\"objective\":\"本次要执行或调整的具体要求\",\"models\":{},\"assignments\":[]}。" +
		"用户明确指定模型时，models 可填写 {\"Harness ID\":\"用户给出的完整模型名\"}；不得自行发明或改写模型名称。" +
		"用户指定正在执行的子任务由谁做时，assignments 填写 [{\"taskId\":\"已有子任务ID\",\"agentId\":\"允许的Harness ID\",\"model\":\"可选模型名\",\"instructions\":\"调整后要求\"}]。其他情况留空。" +
		"闲聊或确实缺少必要信息才 reply；新的明确任务 execute；有执行中的任务且用户补充要求或调整分工时 steer。" +
		"不要向用户展示内部 JSON。不能声称任务已经执行或消息已发出。\n" +
		"当前设置：" + string(cfgJSON) + "\n真实 Harness 名单：" + string(rosterJSON) +
		"\n执行中的任务：" + string(activeJSON) +
		"\n对话记录（当前消息及之前）：" + string(transcript) +
		"\n人工指定分工（必须遵守）：" + string(overridesJSON)
	decision, err := c.Orch.call(control, cfg.ManagerID, "宰相对话", prompt, "", false, "")
	if err != nil {
		return err
	}
	reply, _ := decision.Parsed["reply"].(string)
	action, _ := decision.Parsed["action"].(string)
	if strings.TrimSpace(reply) == "" || (action != "reply" && action != "execute" && action != "steer") {
		return errors.New("宰相未返回有效的对话决策")
	}
	objective, _ := decision.Parsed["objective"].(string)
	if action != "reply" && strings.TrimSpace(objective) == "" {
		return errors.New("宰相未给出具体执行要求")
	}
	current := c.active(chat)
	instruction := user.Text + "\n宰相理解：" + objective
	if len(instruction) > 12000 {
		instruction = instruction[:12000]
	}
	var suggested []map[string]any
	if raw, ok := decision.Parsed["assignments"].([]any); ok {
		for _, s := range raw {
			if sm, ok := s.(map[string]any); ok {
				suggested = append(suggested, sm)
			}
		}
	} else if decision.Parsed["assignments"] != nil {
		return errors.New("宰相返回的人工分工格式无效")
	}
	if len(suggested) > 8 {
		return errors.New("宰相返回的人工分工格式无效")
	}
	changes := append(append([]map[string]any{}, suggested...), overrides...)
	decisionModels := map[string]string{}
	if raw, ok := decision.Parsed["models"].(map[string]any); ok {
		for k, v := range raw {
			if s, ok := v.(string); ok {
				decisionModels[k] = s
			}
		}
	} else if decision.Parsed["models"] != nil {
		return errors.New("宰相返回的模型设置无效")
	}
	for id, model := range decisionModels {
		allowed := false
		for _, w := range cfg.WorkerIDs {
			if w == id {
				allowed = true
			}
		}
		if !allowed || model == "" || !strings.Contains(strings.ToLower(user.Text), strings.ToLower(model)) {
			return errors.New("请写出完整模型名称，或在模型设置中指定")
		}
	}
	// effective config: merge decision models into chat config and re-validate
	mergedCfg := cfgToMap(cfg)
	if mergedCfg["models"] == nil {
		mergedCfg["models"] = map[string]any{}
	}
	if mm, ok := mergedCfg["models"].(map[string]any); ok {
		for k, v := range decisionModels {
			mm[k] = v
		}
	}
	effective, err := c.Config(mergedCfg)
	if err != nil {
		return err
	}
	c.mu.Lock()
	chat.Config = effective
	c.mu.Unlock()
	if len(changes) > 0 && current == nil {
		return errors.New("原任务已经结束，请在对话中发起后续工作")
	}
	if (action == "steer" || len(changes) > 0 || len(decisionModels) > 0) && current != nil {
		var modelChanges []map[string]any
		current.lock()
		for _, t := range current.Tasks {
			if model, ok := decisionModels[t.AgentID]; ok {
				modelChanges = append(modelChanges, map[string]any{"taskId": t.ID, "model": model})
			}
		}
		current.unlock()
		all := append(append([]map[string]any{}, modelChanges...), changes...)
		if err := c.Orch.Steer(current.ID, instruction, all); err != nil {
			return err
		}
		c.Message(chat, "assistant", reply, map[string]any{"agentId": cfg.ManagerID, "model": cfg.ManagerModel,
			"note": "调整将在当前调用结束后生效", "missionId": current.ID})
		return nil
	}
	if action != "reply" {
		if current != nil {
			if err := c.Orch.Steer(current.ID, instruction, overrides); err != nil {
				return err
			}
			c.Message(chat, "assistant", reply, map[string]any{"agentId": cfg.ManagerID,
				"missionId": current.ID, "note": "追加工作已进入后续调度"})
			return nil
		}
		c.Message(chat, "assistant", reply, map[string]any{"agentId": cfg.ManagerID, "model": cfg.ManagerModel})
		spec := Spec{Objective: instruction, ManagerID: effective.ManagerID, WorkerIDs: effective.WorkerIDs,
			Sensitivity: intPtr(1), Permission: effective.Permission, Workdir: effective.Workdir,
			Isolate: effective.Isolate, ManagerModel: effective.ManagerModel, Models: effective.Models,
			ChatID: chat.ID}
		m, err := c.Orch.Create(spec)
		if err != nil {
			return err
		}
		c.mu.Lock()
		chat.MissionIDs = append(chat.MissionIDs, m.ID)
		c.mu.Unlock()
		c.emit(chat)
		return nil
	}
	c.Message(chat, "assistant", reply, map[string]any{"agentId": cfg.ManagerID, "model": cfg.ManagerModel})
	return nil
}

// Restore loads persisted chats (pending counters reset to zero).
func (c *ChatController) Restore(raw json.RawMessage) error {
	var saved []*Chat
	if json.Unmarshal(raw, &saved) != nil {
		return errors.New("对话记录无效")
	}
	for _, ch := range saved {
		ch.Pending = 0
	}
	c.mu.Lock()
	c.chats = saved
	c.mu.Unlock()
	return nil
}

// watchMissions folds mission updates into chat transcripts (plan/adjust/final).
func (c *ChatController) watchMissions() {
	_, events := c.Hub.Subscribe()
	for evt := range events {
		raw, ok := evt.Data.(json.RawMessage)
		if !ok {
			continue
		}
		var m Mission
		if json.Unmarshal(raw, &m) != nil || m.ChatID == "" {
			continue
		}
		c.onMission(&m)
	}
}

func (c *ChatController) onMission(m *Mission) {
	c.mu.Lock()
	var chat *Chat
	for _, ch := range c.chats {
		if ch.ID == m.ChatID {
			chat = ch
		}
	}
	if chat == nil {
		c.mu.Unlock()
		return
	}
	has := func(key string) bool {
		for _, t := range chat.Messages {
			if t.Key == key {
				return true
			}
		}
		return false
	}
	add := func(role, text string, extra map[string]any) {
		item := &ChatMessage{ID: "turn_" + newOrchID(), Role: role, Text: text, At: nowMs()}
		if v, ok := extra["key"].(string); ok {
			item.Key = v
		}
		if v, ok := extra["missionId"].(string); ok {
			item.MissionID = v
		}
		if v, ok := extra["agentId"].(string); ok {
			item.AgentID = v
		}
		if v, ok := extra["note"].(string); ok {
			item.Note = v
		}
		if v, ok := extra["details"].(string); ok {
			item.Details = v
		}
		chat.Messages = append(chat.Messages, item)
	}
	if m.PlanMessage != "" && !has(m.ID+":plan") {
		add("assistant", m.PlanMessage, map[string]any{"key": m.ID + ":plan", "missionId": m.ID, "agentId": m.ManagerID})
	}
	for i, a := range m.Adjustments {
		key := m.ID + ":adjust:" + strconv.Itoa(i)
		if reply, ok := a["reply"].(string); ok && !has(key) {
			add("assistant", reply, map[string]any{"key": key, "missionId": m.ID, "agentId": m.ManagerID})
		}
	}
	terminal := m.Status == "completed" || m.Status == "partial" || m.Status == "failed"
	if terminal && !has(m.ID+":final") {
		text := m.Reply
		if text == "" {
			text = m.Report
		}
		if text == "" {
			text = m.Error
		}
		role := "assistant"
		if m.Status == "failed" {
			role = "error"
		}
		details := ""
		if m.Reply != "" && m.Reply != m.Report {
			details = m.Report
		}
		note := "包含未完成事项"
		if m.Status == "completed" {
			note = "验收完成"
		}
		add(role, text, map[string]any{"key": m.ID + ":final", "missionId": m.ID,
			"agentId": m.ManagerID, "details": details, "note": note})
	}
	c.mu.Unlock()
	c.emit(chat)
}

func itoa(i int) string { return strconv.Itoa(i) }
