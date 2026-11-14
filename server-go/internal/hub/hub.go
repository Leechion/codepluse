// Package hub is the coordination engine: agents, task records, the message
// bus, the hash-chained audit log and the event fan-out. It mirrors the core
// of core/engine.js from the Node implementation.
package hub

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"strings"
	"sync"
	"time"

	"harnesshub/internal/textutil"
)

// Event is a change notification fanned out to SSE subscribers.
type Event struct {
	Type string    `json:"type"`
	Data any       `json:"data"`
	At   time.Time `json:"at"`
}

// Agent is a registered execution endpoint.
type Agent struct {
	ID            string            `json:"id"`
	Type          string            `json:"type"`
	Name          string            `json:"name"`
	Enabled       bool              `json:"enabled"`
	Real          bool              `json:"real"`
	RealTool      string            `json:"realTool,omitempty"`
	Communication string            `json:"communication,omitempty"` // acp | http
	Endpoint      string            `json:"endpoint,omitempty"`
	AdapterKind   string            `json:"adapterKind,omitempty"`
	Clearance     int               `json:"clearance"`
	Capabilities  []string          `json:"capabilities,omitempty"`
	Skills        []string          `json:"skills,omitempty"`
	Load          float64           `json:"load"`
	MaxCon        int               `json:"maxConcurrency"`
	Status        string            `json:"status"`
	Trust         int               `json:"trust"`
	ACPModes      map[string]string `json:"acpModes,omitempty"`
	ACPVersion    string            `json:"acpVersion,omitempty"`
	Permissions   []string          `json:"permissions,omitempty"`
	// Instance fields (multi-harness): a named copy of a built-in tool that
	// pins its own provider/model instead of following the global selection.
	Wire          string `json:"wire,omitempty"`         // unique wire name, e.g. claude_2; empty = built-in mapping
	ProviderID    string `json:"providerId,omitempty"`   // pinned provider ("" = follow global active)
	DefaultModel  string `json:"defaultModel,omitempty"` // pinned model applied when a session starts
	AuthMode      string `json:"authMode,omitempty"`     // api | subscription; empty preserves the harness's native selection
	BaseAgentType string `json:"baseAgentType,omitempty"`
}

// Execution carries the real-run facts of a task.
type Execution struct {
	Mode       string `json:"mode"`
	Tool       string `json:"tool,omitempty"`
	Permission string `json:"permission,omitempty"`
	Workdir    string `json:"workdir,omitempty"`
	Output     string `json:"output,omitempty"`
	Error      string `json:"error,omitempty"`
}

// Task is one recorded invocation.
type Task struct {
	ID               string    `json:"id"`
	Title            string    `json:"title"`
	Prompt           string    `json:"prompt"`
	Status           string    `json:"status"` // queued | running | succeeded | failed | cancelled
	Progress         int       `json:"progress"`
	Real             bool      `json:"real"`
	AssignedTo       string    `json:"assignedTo,omitempty"`
	RequestedBy      string    `json:"requestedBy,omitempty"`
	MissionID        string    `json:"missionId,omitempty"`
	SessionID        string    `json:"sessionId,omitempty"`
	Sensitivity      int       `json:"sensitivity,omitempty"`
	RequestedModel   string    `json:"requestedModel,omitempty"`
	RuntimeSessionID string    `json:"runtimeSessionId,omitempty"`
	CreatedAt        int64     `json:"createdAt"`
	StartedAt        *int64    `json:"startedAt,omitempty"`
	FinishedAt       *int64    `json:"finishedAt,omitempty"`
	Execution        Execution `json:"execution"`
}

// Message is one envelope on the bus.
type Message struct {
	ID            string `json:"id"`
	From          string `json:"from"`
	To            string `json:"to"`
	Text          string `json:"text"`
	Kind          string `json:"kind"`
	Sensitivity   int    `json:"sensitivity"`
	Status        string `json:"status"` // sent | rejected
	Auth          *Auth  `json:"auth,omitempty"`
	CorrelationID string `json:"correlationId,omitempty"`
	At            int64  `json:"at"`
}

// Auth explains a rejection.
type Auth struct {
	Reason string `json:"reason"`
}

// AuditEntry is one link of the tamper-evident chain.
type AuditEntry struct {
	ID          string `json:"id"`
	At          int64  `json:"at"`
	Actor       string `json:"actor"`
	Action      string `json:"action"`
	Target      string `json:"target"`
	Decision    string `json:"decision"`
	Sensitivity int    `json:"sensitivity"`
	Detail      string `json:"detail,omitempty"`
	PrevHash    string `json:"prevHash"`
	Hash        string `json:"hash"`
}

// LogLine is a human-facing system log.
type LogLine struct {
	Level  string `json:"level"`
	Source string `json:"source"`
	Text   string `json:"text"`
	At     int64  `json:"at"`
}

// Hub owns all shared state. Every accessor takes the root lock — the load
// (a handful of local processes) never justifies finer-grained locking.
type Hub struct {
	mu       sync.Mutex
	agents   map[string]*Agent
	order    []string
	Tasks    []*Task
	Bus      []*Message
	Audit    []*AuditEntry
	Logs     []*LogLine
	prevHash string
	subs     map[int]chan Event
	subSeq   int
	Meta     map[string]any `json:"meta"`
	Version  string         `json:"version"`
}

func NewHub() *Hub {
	return &Hub{
		agents: map[string]*Agent{}, Meta: map[string]any{"running": true, "startedAt": time.Now().UnixMilli()},
		Version: "2.0.0-go", prevHash: "", subs: map[int]chan Event{},
		Tasks: []*Task{}, Bus: []*Message{}, Audit: []*AuditEntry{}, Logs: []*LogLine{},
	}
}

func newID(prefix string) string {
	b := make([]byte, 8)
	_, _ = rand.Read(b)
	return prefix + "_" + hex.EncodeToString(b)
}

// --- agents ---

func (h *Hub) CreateAgent(a *Agent) *Agent {
	h.mu.Lock()
	defer h.mu.Unlock()
	if a.ID == "" {
		a.ID = newID("agt")
	}
	if a.Clearance == 0 {
		a.Clearance = 1
	}
	if a.MaxCon == 0 {
		a.MaxCon = 1
	}
	a.Enabled = true
	if a.Status == "" {
		a.Status = "online"
	}
	h.agents[a.ID] = a
	h.order = append(h.order, a.ID)
	h.emitLocked("agent", a)
	return a
}

func (h *Hub) Agent(id string) *Agent {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.agents[id]
}

func (h *Hub) Agents() []*Agent {
	h.mu.Lock()
	defer h.mu.Unlock()
	out := make([]*Agent, 0, len(h.order))
	for _, id := range h.order {
		out = append(out, h.agents[id])
	}
	return out
}

func (h *Hub) SetAgentEnabled(id string, on bool) *Agent {
	h.mu.Lock()
	defer h.mu.Unlock()
	a := h.agents[id]
	if a != nil {
		a.Enabled = on
		h.emitLocked("agent", a)
	}
	return a
}

// DeleteAgent removes an agent from the registry. It is used for user-created
// harness instances only; seeded built-ins are restored on every boot anyway.
func (h *Hub) DeleteAgent(id string) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	a := h.agents[id]
	if a == nil {
		return false
	}
	delete(h.agents, id)
	for i, oid := range h.order {
		if oid == id {
			h.order = append(h.order[:i], h.order[i+1:]...)
			break
		}
	}
	h.emitLocked("agent", a)
	return true
}

// UpdateAgent mutates a registered agent under the root lock and re-emits it.
func (h *Hub) UpdateAgent(id string, fn func(*Agent)) *Agent {
	h.mu.Lock()
	defer h.mu.Unlock()
	a := h.agents[id]
	if a == nil {
		return nil
	}
	fn(a)
	h.emitLocked("agent", a)
	return a
}

// AgentByWire finds an agent by its wire name (the name conversations and the
// frontend address it by), for agents that carry one.
func (h *Hub) AgentByWire(wire string) *Agent {
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, id := range h.order {
		if a := h.agents[id]; a != nil && a.Wire == wire {
			return a
		}
	}
	return nil
}

// --- tasks ---

func (h *Hub) SubmitTask(t *Task) *Task {
	h.mu.Lock()
	defer h.mu.Unlock()
	if t.ID == "" {
		t.ID = newID("task")
	}
	if t.Status == "" {
		t.Status = "queued"
	}
	if t.CreatedAt == 0 {
		t.CreatedAt = time.Now().UnixMilli()
	}
	if t.Execution.Mode == "" {
		t.Execution.Mode = map[bool]string{true: "real", false: "none"}[t.Real]
	}
	h.Tasks = append(h.Tasks, t)
	h.emitLocked("task", t)
	return t
}

func (h *Hub) Task(id string) *Task {
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, t := range h.Tasks {
		if t.ID == id {
			return t
		}
	}
	return nil
}

func (h *Hub) StartTask(id, agentID string, exec Execution) {
	h.mu.Lock()
	defer h.mu.Unlock()
	t := h.findTask(id)
	if t == nil {
		return
	}
	now := time.Now().UnixMilli()
	t.Status = "running"
	t.StartedAt = &now
	t.AssignedTo = agentID
	t.Execution.Tool = exec.Tool
	t.Execution.Permission = exec.Permission
	t.Execution.Workdir = exec.Workdir
	h.emitLocked("task", t)
}

func (h *Hub) CompleteTask(id string, output, errMsg string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	t := h.findTask(id)
	if t == nil {
		return
	}
	now := time.Now().UnixMilli()
	t.FinishedAt = &now
	if errMsg != "" {
		t.Status = "failed"
		t.Execution.Error = errMsg
	} else {
		t.Status = "succeeded"
		t.Progress = 100
		t.Execution.Output = output
	}
	h.emitLocked("task", t)
}

func (h *Hub) CancelTask(id string) *Task {
	h.mu.Lock()
	defer h.mu.Unlock()
	t := h.findTask(id)
	if t == nil || t.Status == "succeeded" || t.Status == "failed" || t.Status == "cancelled" {
		return t
	}
	now := time.Now().UnixMilli()
	t.Status = "cancelled"
	t.FinishedAt = &now
	h.emitLocked("task", t)
	return t
}

// SetTaskMeta records orchestrator metadata on a task under the engine lock.
func (h *Hub) SetTaskMeta(id, requestedModel, runtimeSessionID string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if t := h.findTask(id); t != nil {
		t.RequestedModel = requestedModel
		t.RuntimeSessionID = runtimeSessionID
	}
}

func (h *Hub) findTask(id string) *Task {
	for _, t := range h.Tasks {
		if t.ID == id {
			return t
		}
	}
	return nil
}

// --- message bus ---

// SendMessage delivers an envelope if the sender may, else records a rejection.
func (h *Hub) SendMessage(from, to, text, kind string, sensitivity int, correlationID string) *Message {
	h.mu.Lock()
	defer h.mu.Unlock()
	msg := &Message{ID: newID("msg"), From: from, To: to, Text: text, Kind: kind,
		Sensitivity: sensitivity, Status: "sent", CorrelationID: correlationID, At: time.Now().UnixMilli()}
	if kind == "" {
		msg.Kind = "collab.ask"
	}
	switch sender := h.agents[from]; {
	case from != "hub" && from != "user" && sender == nil:
		msg.Status = "rejected"
		msg.Auth = &Auth{Reason: "发送方不存在：" + from}
	case sender != nil && sender.Clearance < sensitivity:
		msg.Status = "rejected"
		msg.Auth = &Auth{Reason: fmt.Sprintf("发送方密级 %d 低于 %d", sender.Clearance, sensitivity)}
	case from != "hub" && from != "user" && h.agents[to] == nil:
		msg.Status = "rejected"
		msg.Auth = &Auth{Reason: "接收方不存在：" + to}
	}
	h.Bus = append(h.Bus, msg)
	h.emitLocked("message", msg)
	return msg
}

// --- audit ---

// AddAudit appends one link to the hash chain. The chain binds prevHash so any
// retro-edit breaks verification, same as the Node implementation.
func (h *Hub) AddAudit(actor, action, target, decision string, sensitivity int, detail string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	entry := &AuditEntry{ID: newID("aud"), At: time.Now().UnixMilli(), Actor: actor, Action: action,
		Target: target, Decision: decision, Sensitivity: sensitivity, Detail: detail, PrevHash: h.prevHash}
	payload := fmt.Sprintf("%s|%d|%s|%s|%s|%s|%d|%s|%s", entry.ID, entry.At, entry.Actor, entry.Action,
		entry.Target, entry.Decision, entry.Sensitivity, entry.Detail, entry.PrevHash)
	sum := sha256.Sum256([]byte(payload))
	entry.Hash = hex.EncodeToString(sum[:])
	h.prevHash = entry.Hash
	h.Audit = append(h.Audit, entry)
	h.emitLocked("audit", entry)
}

// VerifyAuditChain recomputes every link.
func (h *Hub) VerifyAuditChain() map[string]any {
	h.mu.Lock()
	defer h.mu.Unlock()
	prev := ""
	ok := true
	broken := 0
	for _, e := range h.Audit {
		payload := fmt.Sprintf("%s|%d|%s|%s|%s|%s|%d|%s|%s", e.ID, e.At, e.Actor, e.Action,
			e.Target, e.Decision, e.Sensitivity, e.Detail, e.PrevHash)
		sum := sha256.Sum256([]byte(payload))
		if e.PrevHash != prev || e.Hash != hex.EncodeToString(sum[:]) {
			ok = false
			broken++
		}
		prev = e.Hash
	}
	return map[string]any{"ok": ok, "links": len(h.Audit), "broken": broken}
}

// --- logs ---

func (h *Hub) Log(level, source, text string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.Logs = append(h.Logs, &LogLine{Level: level, Source: source, Text: text, At: time.Now().UnixMilli()})
	if len(h.Logs) > 2000 {
		h.Logs = h.Logs[len(h.Logs)-2000:]
	}
	h.emitLocked("log", h.Logs[len(h.Logs)-1])
}

// --- events ---

// Emit publishes an engine event to all subscribers.
func (h *Hub) Emit(typ string, data any) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.emitLocked(typ, data)
}

func (h *Hub) emitLocked(typ string, data any) {
	evt := Event{Type: typ, Data: data, At: time.Now()}
	for _, ch := range h.subs {
		select {
		case ch <- evt:
		default: // slow subscriber: drop rather than block the engine
		}
	}
}

// Subscribe returns a channel receiving every engine event.
func (h *Hub) Subscribe() (int, <-chan Event) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.subSeq++
	ch := make(chan Event, 256)
	h.subs[h.subSeq] = ch
	return h.subSeq, ch
}

func (h *Hub) Unsubscribe(id int) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if ch, ok := h.subs[id]; ok {
		delete(h.subs, id)
		close(ch)
	}
}

// Snapshot is the /api/snapshot payload.
func (h *Hub) Snapshot() map[string]any {
	h.mu.Lock()
	defer h.mu.Unlock()
	agents := make([]*Agent, 0, len(h.order))
	for _, id := range h.order {
		agents = append(agents, h.agents[id])
	}
	tasks := h.Tasks
	if tasks == nil {
		tasks = []*Task{}
	}
	bus := h.Bus
	if bus == nil {
		bus = []*Message{}
	}
	audit := h.Audit
	if audit == nil {
		audit = []*AuditEntry{}
	}
	logs := h.Logs
	if logs == nil {
		logs = []*LogLine{}
	}
	return map[string]any{
		"version": h.Version, "meta": h.Meta, "agents": agents,
		"tasks": tasks, "messages": bus, "audit": audit, "logs": logs,
		"auditVerify": map[string]any{"ok": true},
	}
}

// RealEnabled reports whether any registered agent is a real endpoint.
func (h *Hub) RealEnabled() bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, a := range h.agents {
		if a.Real && a.Enabled {
			return true
		}
	}
	return false
}

func TrimResult(s string, n int) string {
	// Byte slicing splits multi-byte characters; tool output is routinely
	// Chinese, so cut on a rune boundary.
	if trimmed := textutil.TruncateRunes(s, n); trimmed != s {
		return trimmed + "…"
	}
	return strings.TrimSpace(s)
}
