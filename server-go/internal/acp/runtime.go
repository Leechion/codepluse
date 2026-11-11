// Package acp drives ACP adapter processes over NDJSON JSON-RPC stdio,
// mirroring core/acp-runtime.js: one driver per (agent, permission, cwd),
// native session reuse/restoration, permission cards with a pausable clock,
// model switching and submit_result capture.
package acp

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"harnesshub/internal/hub"
)

func newID() string {
	b := make([]byte, 8)
	_, _ = rand.Read(b)
	return hex.EncodeToString(b)
}

// Launch describes how to spawn an adapter process.
type Launch struct {
	Command string   `json:"command"`
	Args    []string `json:"args"`
	Env     []string `json:"env,omitempty"` // nil inherits the parent environment
}

// Options are the per-turn knobs the orchestrator passes down.
type Options struct {
	SessionKey   string         `json:"sessionKey"`
	Permission   string         `json:"permission"`
	Workdir      string         `json:"workdir"`
	Model        string         `json:"model,omitempty"`
	Mode         string         `json:"mode,omitempty"`
	ChatID       string         `json:"chatId,omitempty"`
	MissionID    string         `json:"missionId,omitempty"`
	TaskID       string         `json:"taskId,omitempty"`
	Role         string         `json:"role,omitempty"`
	Stage        string         `json:"stage,omitempty"`
	Structured   bool           `json:"structured"`
	Sensitivity  int            `json:"sensitivity,omitempty"`
	TimeoutMs    int            `json:"timeoutMs,omitempty"`
	OutputSchema map[string]any `json:"outputSchema,omitempty"`
}

// Result mirrors the Node runtime's turn result.
type Result struct {
	OK         bool           `json:"ok"`
	Output     string         `json:"output"`
	Structured map[string]any `json:"structured,omitempty"`
	Error      string         `json:"error,omitempty"`
	Meta       map[string]any `json:"meta"`
}

var resultKeys = map[string]bool{"output": true, "complete": true, "blocked": true, "messages": true,
	"reads": true, "tasks": true, "reply": true, "action": true, "report": true, "updates": true, "newTasks": true}

func extractSubmitted(tools []map[string]any) map[string]any {
	for i := len(tools) - 1; i >= 0; i-- {
		raw, ok := tools[i]["rawInput"]
		if !ok {
			continue
		}
		switch v := raw.(type) {
		case map[string]any:
			for k := range v {
				if resultKeys[k] {
					return v
				}
			}
		case string:
			var parsed map[string]any
			if json.Unmarshal([]byte(v), &parsed) == nil {
				for k := range parsed {
					if resultKeys[k] {
						return parsed
					}
				}
			}
		}
	}
	return nil
}

// --- wire types ---

type rpcMsg struct {
	ID     *int64          `json:"id"`
	Method string          `json:"method,omitempty"`
	Params json.RawMessage `json:"params,omitempty"`
	Result json.RawMessage `json:"result,omitempty"`
	Error  *rpcError       `json:"error,omitempty"`
}
type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

type catalog struct {
	CurrentModelID  string           `json:"currentModelId"`
	AvailableModels []map[string]any `json:"availableModels,omitempty"`
}
type modesInfo struct {
	CurrentModeID  string           `json:"currentModeId"`
	AvailableModes []map[string]any `json:"availableModes,omitempty"`
}

// PermissionCard is one human decision the harness asked for.
type PermissionCard struct {
	ID        string           `json:"id"`
	SessionID string           `json:"sessionId"`
	NativeID  string           `json:"nativeSessionId"`
	Title     string           `json:"title"`
	ToolCall  map[string]any   `json:"toolCall,omitempty"`
	Options   []map[string]any `json:"options"`
	State     string           `json:"state"`
	Selected  *string          `json:"selected,omitempty"`
	Reason    string           `json:"reason,omitempty"`
	At        int64            `json:"at"`
}

// Turn is one prompt round inside a native session.
type Turn struct {
	ID            string           `json:"id"`
	Stage         string           `json:"stage"`
	Structured    bool             `json:"structured"`
	Output        string           `json:"output"`
	Tools         []map[string]any `json:"tools"`
	Plan          []map[string]any `json:"plan,omitempty"`
	StartedAt     int64            `json:"startedAt"`
	FinishedAt    int64            `json:"finishedAt,omitempty"`
	Status        string           `json:"status"`
	StopReason    string           `json:"stopReason,omitempty"`
	Error         string           `json:"error,omitempty"`
	ReceivedChars int              `json:"receivedChars,omitempty"`
	Usage         map[string]any   `json:"usage,omitempty"`
	lastActivity  time.Time
}

// Tools is always an array, including turns with no native tool calls.
func (t *Turn) MarshalJSON() ([]byte, error) {
	type wireTurn Turn
	tools := t.Tools
	if tools == nil {
		tools = []map[string]any{}
	}
	return json.Marshal(struct {
		*wireTurn
		Tools []map[string]any `json:"tools"`
	}{(*wireTurn)(t), tools})
}

// SessionRecord is the hub's view of one native session.
type SessionRecord struct {
	ID              string            `json:"id"`
	Key             string            `json:"key"`
	AgentID         string            `json:"agentId"`
	ChatID          string            `json:"chatId,omitempty"`
	MissionID       string            `json:"missionId,omitempty"`
	TaskID          string            `json:"taskId,omitempty"`
	Role            string            `json:"role,omitempty"`
	Permission      string            `json:"permission"`
	NativeMode      string            `json:"nativeMode,omitempty"`
	Cwd             string            `json:"cwd"`
	State           string            `json:"state"`
	NativeSessionID string            `json:"nativeSessionId,omitempty"`
	Turns           []*Turn           `json:"turns"`
	Permissions     []*PermissionCard `json:"permissions"`
	Models          *catalog          `json:"models,omitempty"`
	Modes           *modesInfo        `json:"modes,omitempty"`
	ConfigOptions   []map[string]any  `json:"configOptions,omitempty"`
	Notices         []map[string]any  `json:"notices,omitempty"`
	DefaultModel    string            `json:"defaultModel,omitempty"`
	Error           string            `json:"error,omitempty"`
	CreatedAt       int64             `json:"createdAt"`
	HandedOff       bool              `json:"handedOff,omitempty"`
	HandedOffTo     string            `json:"handedOffTo,omitempty"`

	turnMu    sync.Mutex // serialises whole turns per session
	activeMu  sync.Mutex // guards active + Notices during streaming updates
	active    *Turn
	driverRef *Driver
}

// Driver is one spawned adapter process.
type Driver struct {
	cmd      *exec.Cmd
	stdin    io.WriteCloser
	writeMu  sync.Mutex
	pendMu   sync.Mutex
	pending  map[int64]chan *rpcMsg
	nextID   int64
	mu       sync.Mutex
	sessions map[*SessionRecord]bool
	Info     map[string]any
	alive    bool
	Closed   chan struct{}
	stopped  chan struct{}
	stderr   strings.Builder
	perm     string
	cwd      string
	agentID  string
	rt       *Runtime
}

func (d *Driver) send(msg any) error {
	d.writeMu.Lock()
	defer d.writeMu.Unlock()
	line, err := json.Marshal(msg)
	if err != nil {
		return err
	}
	_, err = d.stdin.Write(append(line, '\n'))
	return err
}

func (d *Driver) call(method string, params any, timeout time.Duration) (json.RawMessage, error) {
	d.pendMu.Lock()
	d.nextID++
	id := d.nextID
	ch := make(chan *rpcMsg, 1)
	d.pending[id] = ch
	d.pendMu.Unlock()
	if err := d.send(map[string]any{"jsonrpc": "2.0", "id": id, "method": method, "params": params}); err != nil {
		d.pendMu.Lock()
		delete(d.pending, id)
		d.pendMu.Unlock()
		return nil, err
	}
	if timeout <= 0 {
		// no deadline: the caller owns the clock (session/prompt + pausable turn)
		select {
		case msg := <-ch:
			if msg.Error != nil {
				return nil, fmt.Errorf("%s", msg.Error.Message)
			}
			return msg.Result, nil
		case <-d.Closed:
			return nil, fmt.Errorf("ACP 进程已退出")
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	select {
	case msg := <-ch:
		if msg.Error != nil {
			return nil, fmt.Errorf("%s", msg.Error.Message)
		}
		return msg.Result, nil
	case <-ctx.Done():
		d.pendMu.Lock()
		delete(d.pending, id)
		d.pendMu.Unlock()
		return nil, fmt.Errorf("%s 调用超时", method)
	case <-d.Closed:
		return nil, fmt.Errorf("ACP 进程已退出")
	}
}

// CallWithDeadline issues a request without its own timeout; the caller owns
// the clock (used by session/prompt so the pausable turn deadline applies).
func (d *Driver) CallWithDeadline(method string, params any, done chan<- error) {
	result, err := d.call(method, params, 0)
	if result == nil && err == nil {
		err = fmt.Errorf("空响应")
	}
	done <- err
}

func (d *Driver) notify(method string, params any) error {
	return d.send(map[string]any{"jsonrpc": "2.0", "method": method, "params": params})
}

// --- runtime ---

type approval struct {
	card  *PermissionCard
	rec   *SessionRecord
	reply chan map[string]any
}

type Runtime struct {
	Hub               *hub.Hub
	StateFile         string
	TimeoutMs         int
	LaunchFn          func(agent *hub.Agent, permission, cwd string) (*Launch, error)
	LaunchVersion     func(agent *hub.Agent) string
	ExtraMCP          func(agent *hub.Agent, o Options, sessionID string) []map[string]any
	OnUpdate          func(sessionID string, update map[string]any)
	AllowInternalTool func(sessionID string, toolCall map[string]any) bool

	mu            sync.Mutex
	saveMu        sync.Mutex
	driverStartMu sync.Mutex
	drivers       map[string]*Driver
	records       []*SessionRecord
	bindings      map[string]*Driver
	approvals     map[string]*approval
}

func NewRuntime(h *hub.Hub, stateFile string, timeoutMs int) *Runtime {
	r := &Runtime{Hub: h, StateFile: stateFile, TimeoutMs: timeoutMs,
		drivers: map[string]*Driver{}, bindings: map[string]*Driver{}, approvals: map[string]*approval{}}
	r.load()
	return r
}

func (r *Runtime) load() {
	if r.StateFile == "" || r.StateFile == ":memory:" {
		return
	}
	data, err := os.ReadFile(r.StateFile)
	if err != nil {
		return
	}
	var saved []*SessionRecord
	if json.Unmarshal(data, &saved) != nil {
		return
	}
	for _, rec := range saved {
		rec.State = "disconnected"
		rec.Permissions = nil
		r.records = append(r.records, rec)
	}
}

func (r *Runtime) save() {
	if r.StateFile == "" || r.StateFile == ":memory:" {
		return
	}
	r.saveMu.Lock()
	defer r.saveMu.Unlock()
	records := r.Records("")
	records = records[max(0, len(records)-100):]
	snapshots := make([]json.RawMessage, 0, len(records))
	for _, rec := range records {
		rec.activeMu.Lock()
		data, err := json.Marshal(rec)
		rec.activeMu.Unlock()
		if err != nil {
			return
		}
		snapshots = append(snapshots, data)
	}
	data, err := json.Marshal(snapshots)
	if err != nil {
		return
	}
	if err := os.MkdirAll(filepath.Dir(r.StateFile), 0o755); err != nil {
		return
	}
	tmp := r.StateFile + ".tmp"
	if os.WriteFile(tmp, data, 0o644) == nil {
		_ = os.Rename(tmp, r.StateFile)
	}
}

func (r *Runtime) changed(rec *SessionRecord) {
	rec.activeMu.Lock()
	state := rec.State
	rec.activeMu.Unlock()
	r.Hub.Emit("session", map[string]any{"id": rec.ID, "chatId": rec.ChatID, "agentId": rec.AgentID, "state": state})
}

func (r *Runtime) setState(rec *SessionRecord, state, message string) {
	rec.activeMu.Lock()
	rec.State, rec.Error = state, message
	rec.activeMu.Unlock()
	r.changed(rec)
}

// Records returns all records, optionally filtered by chat.
func (r *Runtime) Records(chatID string) []*SessionRecord {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make([]*SessionRecord, 0)
	for _, rec := range r.records {
		if chatID == "" || rec.ChatID == chatID {
			out = append(out, rec)
		}
	}
	return out
}

func (r *Runtime) record(id string) *SessionRecord {
	for _, rec := range r.records {
		if rec.ID == id {
			return rec
		}
	}
	return nil
}

// --- driver lifecycle ---

func driverKey(agentID, permission, cwd string) string {
	return agentID + "|" + permission + "|" + cwd
}

func (r *Runtime) driver(agent *hub.Agent, permission, cwd string) (*Driver, error) {
	r.driverStartMu.Lock()
	defer r.driverStartMu.Unlock()
	r.mu.Lock()
	key := driverKey(agent.ID, permission, cwd)
	if r.LaunchVersion != nil {
		key += "|" + r.LaunchVersion(agent)
	}
	if d, ok := r.drivers[key]; ok && d.alive {
		r.mu.Unlock()
		return d, nil
	}
	r.mu.Unlock()
	d, err := r.startDriver(agent, permission, cwd)
	if err != nil {
		return nil, err
	}
	r.mu.Lock()
	r.drivers[key] = d
	r.mu.Unlock()
	return d, nil
}

func (r *Runtime) startDriver(agent *hub.Agent, permission, cwd string) (*Driver, error) {
	if r.LaunchFn == nil {
		return nil, fmt.Errorf("未配置适配器启动方式")
	}
	launch, err := r.LaunchFn(agent, permission, cwd)
	if err != nil {
		return nil, err
	}
	if launch.Command == "" {
		return nil, fmt.Errorf("尚未安装此 Harness 的 ACP 适配器")
	}
	cmd := exec.Command(launch.Command, launch.Args...)
	cmd.Dir = cwd
	if launch.Env != nil {
		cmd.Env = launch.Env
	}
	stdin, _ := cmd.StdinPipe()
	stdout, _ := cmd.StdoutPipe()
	stderr, _ := cmd.StderrPipe()
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("适配器进程启动失败：%w", err)
	}
	d := &Driver{cmd: cmd, stdin: stdin, pending: map[int64]chan *rpcMsg{},
		sessions: map[*SessionRecord]bool{}, alive: true, Closed: make(chan struct{}), stopped: make(chan struct{}),
		perm: permission, cwd: cwd, agentID: agent.ID, rt: r}
	stderrDone := make(chan struct{})
	go func() { // stderr tail for error messages
		defer close(stderrDone)
		buf := make([]byte, 8192)
		for {
			n, err := stderr.Read(buf)
			if n > 0 {
				d.mu.Lock()
				d.stderr.WriteString(string(buf[:n]))
				if s := d.stderr.String(); len(s) > 8000 {
					d.stderr.Reset()
					d.stderr.WriteString(s[len(s)-8000:])
				}
				d.mu.Unlock()
			}
			if err != nil {
				return
			}
		}
	}()
	go d.serve(stdout)
	go func() {
		defer close(d.stopped)
		_ = cmd.Wait()
		close(d.Closed)
		d.mu.Lock()
		d.alive = false
		var bound []*SessionRecord
		for rec := range d.sessions {
			bound = append(bound, rec)
		}
		d.mu.Unlock()
		for _, rec := range bound {
			r.cancelPermissions(rec.ID)
			rec.activeMu.Lock()
			rec.State = "disconnected"
			rec.active = nil
			rec.activeMu.Unlock()
			r.mu.Lock()
			delete(r.bindings, rec.ID)
			r.mu.Unlock()
			r.changed(rec)
		}
		r.save()
	}()
	// initialize handshake
	// A cold Claude driver can take well over 30s to answer initialize on
	// first spawn (large native binary plus antivirus scanning), which read as
	// "连接不上"; a dead process still errors out immediately, so the longer
	// deadline only buys slow startups, not hangs.
	result, err := d.call("initialize", map[string]any{
		"protocolVersion": 1,
		"clientInfo":      map[string]any{"name": "harness-hub-go", "version": "2.0.0"},
		"clientCapabilities": map[string]any{"fs": map[string]any{
			"readTextFile": true, "writeTextFile": permission == "workspace"}, "terminal": false, "session": map[string]any{"notices": map[string]any{}}},
	}, 90*time.Second)
	if err != nil {
		d.Kill()
		select {
		case <-stderrDone:
		case <-time.After(time.Second):
		}
		if tail := strings.TrimSpace(d.stderrTail()); tail != "" {
			return nil, fmt.Errorf("ACP 初始化失败：%w: %s", err, tail)
		}
		return nil, fmt.Errorf("ACP 初始化失败：%w", err)
	}
	var info map[string]any
	_ = json.Unmarshal(result, &info)
	d.mu.Lock()
	d.Info = info
	d.mu.Unlock()
	if v, _ := info["protocolVersion"].(float64); v != 1 {
		d.Kill()
		return nil, fmt.Errorf("ACP 协议版本不兼容")
	}
	return d, nil
}

func (d *Driver) stderrTail() string {
	d.mu.Lock()
	defer d.mu.Unlock()
	s := d.stderr.String()
	if len(s) > 400 {
		return s[len(s)-400:]
	}
	return s
}

func (d *Driver) serve(stdout io.Reader) {
	scanner := bufio.NewScanner(stdout)
	scanner.Buffer(make([]byte, 0, 1024*1024), 16*1024*1024)
	for scanner.Scan() {
		var msg rpcMsg
		if json.Unmarshal(scanner.Bytes(), &msg) != nil {
			continue
		}
		switch {
		case msg.ID != nil && msg.Method != "": // agent → client request
			go d.handleRequest(*msg.ID, msg.Method, msg.Params)
		case msg.ID != nil:
			d.pendMu.Lock()
			ch, ok := d.pending[*msg.ID]
			if ok {
				delete(d.pending, *msg.ID)
			}
			d.pendMu.Unlock()
			if ok {
				ch <- &msg
			}
		case msg.Method != "":
			// Updates only take activeMu, never the whole-turn lock. Apply them
			// in wire order before delivering the final response. Requests above
			// remain asynchronous so human permission decisions cannot stall RPC.
			d.handleNotification(msg.Params)
		}
	}
}

func (d *Driver) handleRequest(id int64, method string, params json.RawMessage) {
	var p map[string]any
	_ = json.Unmarshal(params, &p)
	switch method {
	case "fs/read_text_file":
		result, err := d.rt.fsRead(d, p)
		d.reply(id, result, err)
	case "fs/write_text_file":
		result, err := d.rt.fsWrite(d, p)
		d.reply(id, result, err)
	case "session/request_permission":
		result, err := d.rt.requestPermission(d, p)
		if err != nil {
			d.reply(id, nil, err)
		} else {
			d.reply(id, result, nil)
		}
	default:
		d.reply(id, nil, fmt.Errorf("method not supported: %s", method))
	}
}

func (d *Driver) reply(id int64, result any, err error) {
	msg := map[string]any{"jsonrpc": "2.0", "id": id}
	if err != nil {
		msg["error"] = map[string]any{"code": -32603, "message": err.Error()}
	} else {
		msg["result"] = result
	}
	_ = d.send(msg)
}

func (d *Driver) handleNotification(params json.RawMessage) {
	var p struct {
		SessionID string         `json:"sessionId"`
		Update    map[string]any `json:"update"`
	}
	if json.Unmarshal(params, &p) != nil || p.Update == nil {
		return
	}
	d.mu.Lock()
	var rec *SessionRecord
	for s := range d.sessions {
		if s.NativeSessionID == p.SessionID {
			rec = s
		}
	}
	d.mu.Unlock()
	if rec == nil {
		return
	}
	d.rt.applyUpdate(d, rec, p.Update)
}

func (r *Runtime) applyUpdate(d *Driver, rec *SessionRecord, u map[string]any) {
	kind, _ := u["sessionUpdate"].(string)
	rec.activeMu.Lock()
	if rec.active != nil {
		rec.active.lastActivity = time.Now()
	}
	defer func() {
		rec.activeMu.Unlock()
		r.changed(rec)
		if r.OnUpdate != nil {
			r.OnUpdate(rec.ID, u)
		}
	}()
	switch kind {
	case "agent_message_chunk":
		if rec.active != nil {
			if content, ok := u["content"].(map[string]any); ok {
				if text, ok := content["text"].(string); ok {
					rec.active.Output = truncate(rec.active.Output+text, 200000)
					rec.active.ReceivedChars += len(text)
				}
			}
		}
	case "tool_call", "tool_call_update":
		if rec.active != nil {
			id, _ := u["toolCallId"].(string)
			found := false
			for i, t := range rec.active.Tools {
				if t["toolCallId"] == id {
					merged := map[string]any{}
					for k, v := range t {
						merged[k] = v
					}
					for k, v := range u {
						merged[k] = v
					}
					if b, _ := json.Marshal(merged); len(b) > 40000 {
						merged["content"] = []map[string]any{{"type": "content", "content": map[string]any{"type": "text", "text": "此工具输出较大，请查看 Harness 原生记录。"}}}
					}
					rec.active.Tools[i] = merged
					found = true
					break
				}
			}
			if !found {
				if b, _ := json.Marshal(u); len(b) <= 40000 {
					rec.active.Tools = append(rec.active.Tools, u)
				}
				if len(rec.active.Tools) > 40 {
					rec.active.Tools = rec.active.Tools[len(rec.active.Tools)-40:]
				}
			}
		}
	case "plan":
		if rec.active != nil {
			if entries := toMapSlice(u["entries"]); entries != nil {
				rec.active.Plan = entries
			}
		}
	case "notice":
		rec.Notices = append(rec.Notices, u)
		if len(rec.Notices) > 5 {
			rec.Notices = rec.Notices[len(rec.Notices)-5:]
		}
	case "config_option_update":
		applyConfiguration(rec, map[string]any{"configOptions": u["configOptions"]})
		for _, option := range rec.ConfigOptions {
			if option["id"] == "mode" {
				rec.NativeMode, _ = option["currentValue"].(string)
			}
		}
	case "current_mode_update":
		rec.NativeMode, _ = u["currentModeId"].(string)
		if rec.Modes != nil {
			rec.Modes.CurrentModeID, _ = u["currentModeId"].(string)
		}
	}
}

func truncate(s string, n int) string {
	if len(s) > n {
		return s[:n]
	}
	return s
}

// normalizeUsage converts the per-turn usage the adapters attach to the
// session/prompt reply (claude-agent-acp: inputTokens/cachedReadTokens/
// cachedWriteTokens; codex-acp: cachedInputTokens/cacheWriteInputTokens) into
// the persisted TurnUsage wire shape. A reply without usage yields nil.
func normalizeUsage(u map[string]any) map[string]any {
	if u == nil {
		return nil
	}
	num := func(keys ...string) float64 {
		for _, k := range keys {
			if f, ok := u[k].(float64); ok {
				return f
			}
		}
		return 0
	}
	out := map[string]any{
		"input_tokens":                num("inputTokens", "input_tokens"),
		"output_tokens":               num("outputTokens", "output_tokens"),
		"cache_creation_input_tokens": num("cachedWriteTokens", "cacheWriteInputTokens", "cache_creation_input_tokens"),
		"cache_read_input_tokens":     num("cachedReadTokens", "cachedInputTokens", "cache_read_input_tokens"),
	}
	total := 0.0
	for _, v := range out {
		total += v.(float64)
	}
	if total <= 0 {
		return nil
	}
	return out
}

// --- permission cards ---

func (r *Runtime) requestPermission(d *Driver, p map[string]any) (map[string]any, error) {
	nativeID, _ := p["sessionId"].(string)
	d.mu.Lock()
	var rec *SessionRecord
	for s := range d.sessions {
		if s.NativeSessionID == nativeID {
			rec = s
		}
	}
	d.mu.Unlock()
	if rec == nil {
		return map[string]any{"outcome": map[string]any{"outcome": "cancelled"}}, nil
	}
	rec.activeMu.Lock()
	noActive := rec.active == nil
	rec.activeMu.Unlock()
	if noActive {
		return map[string]any{"outcome": map[string]any{"outcome": "cancelled"}}, nil
	}
	title := "Harness 请求权限"
	if tc, ok := p["toolCall"].(map[string]any); ok {
		if t, ok := tc["title"].(string); ok && t != "" {
			title = t
		}
	}
	opts := toMapSlice(p["options"])
	card := &PermissionCard{ID: "permission_" + newID(), SessionID: rec.ID, NativeID: nativeID,
		Title: title, Options: opts, State: "pending", At: time.Now().UnixMilli()}
	if tc, ok := p["toolCall"].(map[string]any); ok {
		card.ToolCall = tc
	}
	if r.AllowInternalTool != nil && r.AllowInternalTool(rec.ID, card.ToolCall) {
		for _, option := range opts {
			if option["kind"] == "allow_once" {
				choice, _ := option["optionId"].(string)
				card.State = "answered"
				card.Selected = &choice
				card.Reason = "工作台内置协作工具，授权一次"
				rec.activeMu.Lock()
				rec.Permissions = append(rec.Permissions, card)
				rec.activeMu.Unlock()
				r.changed(rec)
				return map[string]any{"outcome": map[string]any{"outcome": "selected", "optionId": choice}}, nil
			}
		}
	}
	rec.activeMu.Lock()
	rec.Permissions = append(rec.Permissions, card)
	if !rec.canWrite() {
		card.State = "denied"
		card.Reason = "当前为只读会话，权限提升已拒绝。需要写入时请选择目录内读写权限启动新任务。"
		rec.activeMu.Unlock()
		r.changed(rec)
		return map[string]any{"outcome": map[string]any{"outcome": "cancelled"}}, nil
	}
	rec.State = "requires_action"
	rec.activeMu.Unlock()
	r.changed(rec)
	r.save()
	app := &approval{card: card, rec: rec, reply: make(chan map[string]any, 1)}
	r.mu.Lock()
	r.approvals[card.ID] = app
	r.mu.Unlock()
	select {
	case outcome := <-app.reply:
		return outcome, nil
	case <-d.Closed:
		return map[string]any{"outcome": map[string]any{"outcome": "cancelled"}}, nil
	}
}

// RespondPermission answers a pending card; nil optionID cancels.
func (r *Runtime) RespondPermission(id string, optionID *string) error {
	r.mu.Lock()
	app, ok := r.approvals[id]
	if !ok {
		r.mu.Unlock()
		return fmt.Errorf("该权限请求已失效")
	}
	r.mu.Unlock()
	if app.card.State != "pending" {
		return fmt.Errorf("该权限请求已失效")
	}
	if optionID != nil {
		valid := false
		for _, o := range app.card.Options {
			if o["optionId"] == *optionID {
				valid = true
			}
		}
		if !valid {
			return fmt.Errorf("无效的权限选项")
		}
	}
	r.mu.Lock()
	delete(r.approvals, id)
	r.mu.Unlock()
	rec := app.rec
	if optionID == nil {
		app.card.State = "cancelled"
		app.reply <- map[string]any{"outcome": map[string]any{"outcome": "cancelled"}}
	} else {
		app.card.State = "answered"
		app.card.Selected = optionID
		app.reply <- map[string]any{"outcome": map[string]any{"outcome": "selected", "optionId": *optionID}}
	}
	rec.activeMu.Lock()
	if rec.State != "cancelling" {
		rec.State = "running"
	}
	rec.activeMu.Unlock()
	r.changed(rec)
	r.save()
	return nil
}

func (r *Runtime) cancelPermissions(sessionID string) {
	r.mu.Lock()
	ids := []string{}
	for id, app := range r.approvals {
		if app.rec.ID == sessionID {
			ids = append(ids, id)
		}
	}
	r.mu.Unlock()
	for _, id := range ids {
		_ = r.RespondPermission(id, nil)
	}
}

func (r *Runtime) pendingCount(recID string) int {
	r.mu.Lock()
	defer r.mu.Unlock()
	n := 0
	for _, app := range r.approvals {
		if app.rec.ID == recID {
			n++
		}
	}
	return n
}

// --- fs handlers ---

// splitLines splits on \n and trims a trailing \r (Windows line endings).
func splitLines(s string) []string {
	raw := strings.Split(s, "\n")
	out := make([]string, len(raw))
	for i, l := range raw {
		out[i] = strings.TrimSuffix(l, "\r")
	}
	return out
}

// toMapSlice converts a decoded []any of objects into []map[string]any.
func toMapSlice(v any) []map[string]any {
	raw, ok := v.([]any)
	if !ok {
		return nil
	}
	out := make([]map[string]any, 0, len(raw))
	for _, item := range raw {
		if m, ok := item.(map[string]any); ok {
			out = append(out, m)
		}
	}
	return out
}

func withinDir(root, candidate string) (string, error) {
	abs, err := filepath.Abs(candidate)
	if err != nil {
		return "", err
	}
	realRoot, err := filepath.EvalSymlinks(root)
	if err != nil {
		realRoot = root
	}
	real, err := filepath.EvalSymlinks(abs)
	if err != nil {
		// target may not exist yet (write); resolve the deepest existing ancestor
		dir := filepath.Dir(abs)
		if r, err2 := filepath.EvalSymlinks(dir); err2 == nil {
			real = filepath.Join(r, filepath.Base(abs))
		} else {
			real = abs
		}
	}
	rel, err := filepath.Rel(realRoot, real)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) || filepath.IsAbs(rel) {
		return "", fmt.Errorf("文件路径超出此会话工作目录")
	}
	return real, nil
}

func (r *Runtime) fsRead(d *Driver, p map[string]any) (map[string]any, error) {
	path, _ := p["path"].(string)
	file, err := withinDir(d.cwd, filepath.Join(d.cwd, path))
	if err != nil {
		return nil, err
	}
	st, err := os.Stat(file)
	if err != nil {
		return nil, err
	}
	if st.Size() > 1024*1024 {
		return nil, fmt.Errorf("文件超过 1 MiB，请使用分段读取")
	}
	data, err := os.ReadFile(file)
	if err != nil {
		return nil, err
	}
	lines := splitLines(string(data))
	line := 1
	if v, ok := p["line"].(float64); ok && int(v) > 1 {
		line = int(v)
	}
	var limit int
	if v, ok := p["limit"].(float64); ok && v > 0 {
		limit = int(v)
	}
	start := line - 1
	if start < 0 {
		start = 0
	}
	end := len(lines)
	if limit > 0 && start+limit < end {
		end = start + limit
	}
	return map[string]any{"content": strings.Join(lines[start:end], "\n")}, nil
}

func (r *Runtime) fsWrite(d *Driver, p map[string]any) (map[string]any, error) {
	allowed := d.perm == "workspace"
	nativeID, _ := p["sessionId"].(string)
	d.mu.Lock()
	var record *SessionRecord
	for rec := range d.sessions {
		if rec.NativeSessionID == nativeID {
			record = rec
			break
		}
	}
	d.mu.Unlock()
	if record != nil {
		record.activeMu.Lock()
		allowed = record.canWrite()
		record.activeMu.Unlock()
	}
	if !allowed {
		return nil, fmt.Errorf("此会话未授予文件写入权限")
	}
	path, _ := p["path"].(string)
	content, _ := p["content"].(string)
	if len(content) > 1024*1024 {
		return nil, fmt.Errorf("写入内容超过 1 MiB")
	}
	target := filepath.Join(d.cwd, path)
	check := target
	if _, err := os.Stat(target); err != nil {
		check = filepath.Dir(target)
	}
	if _, err := withinDir(d.cwd, check); err != nil {
		return nil, err
	}
	if err := os.WriteFile(target, []byte(content), 0o644); err != nil {
		return nil, err
	}
	return map[string]any{}, nil
}

// --- sessions and turns ---

func optionsKey(o Options) string {
	key := o.SessionKey
	if key == "" {
		key = "direct"
	}
	return key + "|" + o.Workdir
}

func (r *Runtime) ensure(agent *hub.Agent, o Options) (*SessionRecord, *Driver, error) {
	permission := o.Permission
	if permission == "" {
		permission = "readonly"
	}
	if o.Workdir == "" {
		return nil, nil, fmt.Errorf("缺少工作目录")
	}
	key := optionsKey(o) + "|" + agent.ID + "|" + permission
	r.mu.Lock()
	var rec *SessionRecord
	for _, cand := range r.records {
		if cand.Key == key {
			rec = cand
		}
	}
	if rec == nil {
		rec = &SessionRecord{ID: "session_" + newID(), Key: key, AgentID: agent.ID, ChatID: o.ChatID,
			MissionID: o.MissionID, TaskID: o.TaskID, Role: o.Role, Permission: permission, Cwd: o.Workdir,
			State: "connecting", CreatedAt: time.Now().UnixMilli()}
		r.records = append(r.records, rec)
	}
	r.mu.Unlock()
	r.changed(rec)

	r.mu.Lock()
	bound := r.bindings[rec.ID]
	r.mu.Unlock()
	if bound != nil && bound.alive {
		return rec, bound, nil
	}
	d, err := r.driver(agent, permission, o.Workdir)
	if err != nil {
		r.setState(rec, "error", err.Error())
		return nil, nil, err
	}
	rec.turnMu.Lock()
	defer rec.turnMu.Unlock()
	r.setState(rec, map[bool]string{true: "restoring", false: "connecting"}[rec.NativeSessionID != ""], "")
	var result json.RawMessage
	var callErr error
	mcpServers := r.mcpServers(agent, o)
	if r.ExtraMCP != nil {
		mcpServers = append(mcpServers, r.ExtraMCP(agent, o, rec.ID)...)
	}
	// Same cold-start allowance as the initialize handshake above: the first
	// session/new on a fresh driver can exceed 30s while the CLI initializes.
	if rec.NativeSessionID != "" {
		caps, _ := d.Info["agentCapabilities"].(map[string]any)
		method := "session/load"
		load, _ := caps["loadSession"].(bool)
		sessionCaps, _ := caps["sessionCapabilities"].(map[string]any)
		_, resume := sessionCaps["resume"]
		if !load && resume {
			method = "session/resume"
		}
		if !load && !resume {
			r.setState(rec, "error", "该 Harness 不支持恢复旧会话，请新建对话")
			return nil, nil, fmt.Errorf("该 Harness 不支持恢复旧会话，请新建对话")
		}
		result, callErr = d.call(method, map[string]any{"sessionId": rec.NativeSessionID, "cwd": o.Workdir, "mcpServers": mcpServers}, 90*time.Second)
	} else {
		result, callErr = d.call("session/new", map[string]any{"cwd": o.Workdir, "mcpServers": mcpServers}, 90*time.Second)
	}
	if callErr != nil {
		r.setState(rec, "error", callErr.Error())
		return nil, nil, callErr
	}
	var newRes map[string]any
	_ = json.Unmarshal(result, &newRes)
	rec.activeMu.Lock()
	if sid, ok := newRes["sessionId"].(string); ok && sid != "" {
		rec.NativeSessionID = sid
	}
	applyConfiguration(rec, newRes)
	if rec.DefaultModel == "" && rec.Models != nil {
		rec.DefaultModel = rec.Models.CurrentModelID
	}
	rec.activeMu.Unlock()
	d.mu.Lock()
	d.sessions[rec] = true
	d.mu.Unlock()
	wanted := rec.NativeMode
	if wanted == "" {
		wanted = o.Mode
	}
	if wanted == "" {
		wanted = wantedMode(agent, permission)
	}
	if wanted != "" {
		if rec.Modes != nil && len(rec.Modes.AvailableModes) > 0 {
			found := false
			for _, m := range rec.Modes.AvailableModes {
				if m["id"] == wanted {
					found = true
				}
			}
			if !found {
				r.setState(rec, "error", "该 Harness 未提供所需权限模式："+permission)
				return nil, nil, fmt.Errorf("该 Harness 未提供所需权限模式：%s", permission)
			}
		}
		if _, err := d.call("session/set_mode", map[string]any{"sessionId": rec.NativeSessionID, "modeId": wanted}, 15*time.Second); err != nil {
			r.setState(rec, "error", err.Error())
			return nil, nil, err
		}
		rec.activeMu.Lock()
		rec.NativeMode = wanted
		if rec.Modes != nil {
			rec.Modes.CurrentModeID = wanted
		}
		for _, option := range rec.ConfigOptions {
			if option["category"] == "mode" || option["id"] == "mode" {
				option["currentValue"] = wanted
			}
		}
		rec.activeMu.Unlock()
	}
	d.mu.Lock()
	d.sessions[rec] = true
	d.mu.Unlock()
	r.mu.Lock()
	r.bindings[rec.ID] = d
	r.mu.Unlock()
	r.setState(rec, "idle", "")
	r.save()
	return rec, d, nil
}

func wantedMode(agent *hub.Agent, permission string) string {
	if agent.ACPModes != nil {
		if m, ok := agent.ACPModes[permission]; ok {
			return m
		}
	}
	switch agent.RealTool {
	case "codex":
		return map[string]string{"readonly": "read-only", "workspace": "workspace-write", "shell": "read-only"}[permission]
	case "claude":
		return map[string]string{"readonly": "plan", "workspace": "default", "shell": "plan"}[permission]
	}
	return ""
}

// ModeLevel preserves the difference between workspace access and full access.
func ModeLevel(mode string) string {
	switch mode {
	case "read-only", "readonly", "plan":
		return "readonly"
	case "workspace-write", "default", "acceptEdits", "auto", "agent":
		return "workspace"
	case "danger-full-access", "agent-full-access", "bypassPermissions":
		return "full"
	}
	return ""
}

// The caller holds activeMu. This affects host callbacks, not native approvals.
func (rec *SessionRecord) canWrite() bool {
	if level := ModeLevel(rec.NativeMode); level != "" {
		return level != "readonly"
	}
	return rec.Permission == "workspace"
}

func DelegatedMode(agent *hub.Agent, level string) string {
	if level != "full" && agent.ACPModes != nil {
		if mode := agent.ACPModes[level]; mode != "" {
			return mode
		}
	}
	if agent.RealTool == "codex" {
		return map[string]string{"readonly": "read-only", "workspace": "workspace-write", "full": "agent-full-access"}[level]
	}
	if agent.RealTool == "claude" || agent.RealTool == "workbuddy" {
		return map[string]string{"readonly": "plan", "workspace": "default", "full": "bypassPermissions"}[level]
	}
	return ""
}

func applyConfiguration(rec *SessionRecord, res map[string]any) {
	if res == nil {
		return
	}
	if modes, ok := res["modes"].(map[string]any); ok {
		b, _ := json.Marshal(modes)
		var m modesInfo
		if json.Unmarshal(b, &m) == nil {
			rec.Modes = &m
		}
	}
	if models, ok := res["models"].(map[string]any); ok {
		b, _ := json.Marshal(models)
		var c catalog
		if json.Unmarshal(b, &c) == nil {
			rec.Models = &c
		}
	}
	if opts, ok := res["configOptions"].([]any); ok {
		var clean []map[string]any
		for _, raw := range opts {
			o, ok := raw.(map[string]any)
			if !ok {
				continue
			}
			cat, _ := o["category"].(string)
			id, _ := o["id"].(string)
			if cat == "model" || cat == "mode" || cat == "thought_level" || id == "model" || id == "mode" || id == "reasoning_effort" {
				clean = append(clean, o)
			}
		}
		rec.ConfigOptions = clean
		for _, o := range clean {
			if o["category"] == "model" || o["id"] == "model" {
				if cur, ok := o["currentValue"].(string); ok {
					if rec.Models == nil {
						rec.Models = &catalog{}
					}
					rec.Models.CurrentModelID = cur
				}
			}
		}
	}
}

func (r *Runtime) mcpServers(agent *hub.Agent, o Options) []map[string]any {
	if !o.Structured || (agent.RealTool != "codex" && agent.RealTool != "claude") {
		return []map[string]any{}
	}
	exe, _ := os.Executable()
	args := []string{"mcp-result"}
	if o.OutputSchema != nil {
		if b, err := json.Marshal(o.OutputSchema); err == nil {
			args = append(args, string(b))
		}
	}
	return []map[string]any{{"name": "harness-result", "command": exe, "args": args, "env": []map[string]string{}}}
}

// Prompt serializes turns per session record, like the Node turnQueues.
func (r *Runtime) Connect(agent *hub.Agent, o Options) (string, error) {
	rec, _, err := r.ensure(agent, o)
	if err != nil {
		return "", err
	}
	return rec.ID, nil
}

// SupportsFork reports the native ACP capability, without implying that an
// arbitrary local history row has a usable native fork point.
// HasActiveTurn reports whether a prompt turn is genuinely in flight on the
// session record. The codeg layer uses it to tell a live turn (real work in
// progress) from a leaked connection-level busy flag, so a stuck connection
// can be reset without ever killing running work.
func (r *Runtime) HasActiveTurn(sessionID string) bool {
	r.mu.Lock()
	rec := r.record(sessionID)
	r.mu.Unlock()
	if rec == nil {
		return false
	}
	rec.activeMu.Lock()
	defer rec.activeMu.Unlock()
	return rec.active != nil
}

// ForgetSession releases one session record: it is removed from the runtime
// and unbound from its pooled driver. The driver itself stays alive while other
// sessions still use it (drivers are shared per agent+permission+cwd).
func (r *Runtime) ForgetSession(sessionID string) {
	r.ForgetFork(sessionID)
}

func (r *Runtime) SupportsFork(sessionID string) bool {
	r.mu.Lock()
	d := r.bindings[sessionID]
	r.mu.Unlock()
	if d == nil {
		return false
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	if !d.alive {
		return false
	}
	caps, _ := d.Info["agentCapabilities"].(map[string]any)
	sessions, _ := caps["sessionCapabilities"].(map[string]any)
	fork, ok := sessions["fork"].(map[string]any)
	return ok && fork != nil
}

func (r *Runtime) PromptCapabilities(sessionID string) map[string]bool {
	out := map[string]bool{"image": false, "audio": false, "embedded_context": false}
	r.mu.Lock()
	d := r.bindings[sessionID]
	r.mu.Unlock()
	if d == nil {
		return out
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	if !d.alive {
		return out
	}
	caps, _ := d.Info["agentCapabilities"].(map[string]any)
	prompt, _ := caps["promptCapabilities"].(map[string]any)
	out["image"], _ = prompt["image"].(bool)
	out["embedded_context"], _ = prompt["embeddedContext"].(bool)
	return out
}

// SupportsNativeSteering requires both the ACP extension declaration and a
// locally verified adapter implementation of the no-detached-turn fallback.
func (r *Runtime) SupportsNativeSteering(sessionID string) bool {
	r.mu.Lock()
	d := r.bindings[sessionID]
	r.mu.Unlock()
	if d == nil {
		return false
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	if !d.alive {
		return false
	}
	meta, _ := d.Info["_meta"].(map[string]any)
	steering, _ := meta["steering"].(map[string]any)
	supported, _ := steering["supported"].(bool)
	info, _ := d.Info["agentInfo"].(map[string]any)
	name, _ := info["name"].(string)
	version, _ := info["version"].(string)
	return supported && ((name == "@agentclientprotocol/codex-acp" && version == "2.5.0") || (name == "@agentclientprotocol/claude-agent-acp" && version == "0.101.0"))
}

// Steer requests an in-flight injection; promptRequired means the caller keeps
// the draft and may queue it. It must never start a detached turn implicitly.
func (r *Runtime) Steer(sessionID string, blocks []map[string]any) error {
	if !r.SupportsNativeSteering(sessionID) {
		return fmt.Errorf("native steering unavailable")
	}
	r.mu.Lock()
	d, rec := r.bindings[sessionID], r.record(sessionID)
	r.mu.Unlock()
	if d == nil || rec == nil || rec.NativeSessionID == "" {
		return fmt.Errorf("no active turn")
	}
	response, err := d.call("_session/steering", map[string]any{"sessionId": rec.NativeSessionID, "prompt": blocks, "_meta": map[string]any{"steering": map[string]any{"idleBehavior": "promptRequired"}}}, 20*time.Second)
	if err != nil {
		return err
	}
	var result struct {
		Outcome string `json:"outcome"`
	}
	if err := json.Unmarshal(response, &result); err != nil {
		return err
	}
	if result.Outcome == "promptRequired" {
		return fmt.Errorf("no active turn")
	}
	if result.Outcome != "injected" {
		return fmt.Errorf("unexpected steering outcome: %s", result.Outcome)
	}
	return nil
}

// SupportsPointFork is narrower than ACP's fork flag: the standard capability
// does not promise historical message selection. These adapter versions are
// the ones whose AIR messageId extension is known to be implemented locally.
func (r *Runtime) SupportsPointFork(sessionID string, agent *hub.Agent) bool {
	if agent == nil || !r.SupportsFork(sessionID) {
		return false
	}
	r.mu.Lock()
	d := r.bindings[sessionID]
	r.mu.Unlock()
	if d == nil {
		return false
	}
	d.mu.Lock()
	info, _ := d.Info["agentInfo"].(map[string]any)
	name, _ := info["name"].(string)
	version, _ := info["version"].(string)
	d.mu.Unlock()
	// Restrict this to the locally inspected adapter releases; an ACP agent may
	// legally ignore unknown _meta and otherwise silently fork the wrong point.
	return (agent.RealTool == "codex" && name == "@agentclientprotocol/codex-acp" && version == "2.5.0") ||
		(agent.RealTool == "claude" && name == "@agentclientprotocol/claude-agent-acp" && version == "0.101.0")
}

// Fork creates a new ACP session at a native message ID. The caller owns the
// conversation-store transaction and must not substitute a local row ID.
func (r *Runtime) Fork(agent *hub.Agent, sourceID string, source Options, newSessionKey, messageID string, extraMCP []map[string]any) (string, error) {
	if agent == nil || newSessionKey == "" || messageID == "" {
		return "", fmt.Errorf("分支需要执行端、新会话标识和原生消息标识")
	}
	// The standard ACP request does not define a historical fork point. Only
	// these adapters are known to implement the AIR messageId extension.
	if agent.RealTool != "codex" && agent.RealTool != "claude" {
		return "", fmt.Errorf("该执行端未确认支持按消息位置分支")
	}
	r.mu.Lock()
	rec := r.record(sourceID)
	d := r.bindings[sourceID]
	r.mu.Unlock()
	if rec == nil || d == nil || rec.AgentID != agent.ID || !r.SupportsPointFork(sourceID, agent) {
		return "", fmt.Errorf("当前执行端未声明会话分支能力")
	}
	if !rec.turnMu.TryLock() {
		return "", fmt.Errorf("turn_in_progress")
	}
	defer rec.turnMu.Unlock()
	if source.Workdir != rec.Cwd || source.Permission != rec.Permission || optionsKey(source)+"|"+agent.ID+"|"+rec.Permission != rec.Key {
		return "", fmt.Errorf("分支工作目录或权限与原会话不一致")
	}
	destOptions := source
	destOptions.SessionKey = newSessionKey
	destKey := optionsKey(destOptions) + "|" + agent.ID + "|" + rec.Permission
	r.mu.Lock()
	for _, existing := range r.records {
		if existing.Key == destKey {
			r.mu.Unlock()
			return "", fmt.Errorf("分支会话标识已存在")
		}
	}
	r.mu.Unlock()
	servers := r.mcpServers(agent, source)
	if extraMCP != nil {
		servers = append(servers, extraMCP...)
	} else if r.ExtraMCP != nil {
		servers = append(servers, r.ExtraMCP(agent, source, rec.ID)...)
	}
	params := map[string]any{
		"sessionId": rec.NativeSessionID, "cwd": rec.Cwd, "mcpServers": servers,
		"_meta": map[string]any{"jetbrains": map[string]any{"air": map[string]any{
			"fork": map[string]any{"version": 1, "messageId": messageID},
		}}},
	}
	result, err := d.call("session/fork", params, 30*time.Second)
	if err != nil {
		return "", err
	}
	var response map[string]any
	if err := json.Unmarshal(result, &response); err != nil {
		return "", fmt.Errorf("分支响应无效: %w", err)
	}
	nativeID, _ := response["sessionId"].(string)
	if nativeID == "" || nativeID == rec.NativeSessionID {
		return "", fmt.Errorf("分支未返回新的原生会话标识")
	}
	// Clone persisted settings without copying the source record's mutexes.
	rec.activeMu.Lock()
	encoded, err := json.Marshal(rec)
	rec.activeMu.Unlock()
	if err != nil {
		return "", err
	}
	var fork SessionRecord
	if err := json.Unmarshal(encoded, &fork); err != nil {
		return "", err
	}
	fork.ID, fork.Key, fork.NativeSessionID = "session_"+newID(), destKey, nativeID
	fork.State, fork.Error, fork.CreatedAt = "idle", "", time.Now().UnixMilli()
	fork.Turns, fork.Permissions, fork.Notices = nil, nil, nil
	fork.HandedOff, fork.HandedOffTo = false, ""
	applyConfiguration(&fork, response)
	r.mu.Lock()
	for _, existing := range r.records {
		if existing.Key == destKey || existing.NativeSessionID == nativeID {
			r.mu.Unlock()
			return "", fmt.Errorf("分支会话标识发生冲突")
		}
	}
	r.records = append(r.records, &fork)
	r.bindings[fork.ID] = d
	r.mu.Unlock()
	d.mu.Lock()
	d.sessions[&fork] = true
	d.mu.Unlock()
	r.changed(&fork)
	r.save()
	return fork.ID, nil
}

// ForgetFork removes an unpublished local record if the caller cannot commit
// its conversation-store transaction. It does not delete the native session.
func (r *Runtime) ForgetFork(sessionID string) {
	r.mu.Lock()
	rec, driver := r.record(sessionID), r.bindings[sessionID]
	if rec != nil {
		for i, candidate := range r.records {
			if candidate == rec {
				r.records = append(r.records[:i], r.records[i+1:]...)
				break
			}
		}
		delete(r.bindings, sessionID)
	}
	r.mu.Unlock()
	if rec != nil && driver != nil {
		driver.mu.Lock()
		delete(driver.sessions, rec)
		driver.mu.Unlock()
	}
	r.save()
}

func (r *Runtime) Configure(sessionID, configID string, value any) error {
	r.mu.Lock()
	rec := r.record(sessionID)
	d := r.bindings[sessionID]
	r.mu.Unlock()
	if rec == nil || d == nil {
		return fmt.Errorf("原生会话未连接")
	}
	if !rec.turnMu.TryLock() {
		return fmt.Errorf("请等待当前调用结束后切换模型")
	}
	defer rec.turnMu.Unlock()
	result, err := d.call("session/set_config_option", map[string]any{"sessionId": rec.NativeSessionID, "configId": configID, "value": value}, 20*time.Second)
	if err != nil {
		return err
	}
	var configured map[string]any
	if err = json.Unmarshal(result, &configured); err != nil {
		return err
	}
	rec.activeMu.Lock()
	applyConfiguration(rec, configured)
	if configID == "mode" {
		if mode, ok := value.(string); ok {
			rec.NativeMode = mode
			if rec.Modes != nil {
				rec.Modes.CurrentModeID = mode
			}
		}
	}
	rec.activeMu.Unlock()
	r.changed(rec)
	r.save()
	return nil
}

func (r *Runtime) Prompt(agent *hub.Agent, text string, o Options) Result {
	return r.PromptBlocks(agent, []map[string]any{{"type": "text", "text": text}}, o)
}

func (r *Runtime) PromptBlocks(agent *hub.Agent, blocks []map[string]any, o Options) Result {
	rec, _, err0 := r.ensure(agent, o)
	if err0 != nil {
		return Result{OK: false, Error: err0.Error(), Meta: map[string]any{}}
	}
	rec.turnMu.Lock()
	defer rec.turnMu.Unlock()
	return r.turn(rec, agent, blocks, o)
}

func (r *Runtime) turn(rec *SessionRecord, agent *hub.Agent, blocks []map[string]any, o Options) Result {
	r.mu.Lock()
	d := r.bindings[rec.ID]
	r.mu.Unlock()
	if d == nil || !d.alive {
		return Result{OK: false, Error: "会话驱动已断开", Meta: map[string]any{}}
	}
	model := o.Model
	if model == "" {
		model = rec.DefaultModel
	}
	if model != "" && (rec.Models == nil || rec.Models.CurrentModelID != model) {
		cfg, err := d.call("session/set_config_option", map[string]any{"sessionId": rec.NativeSessionID, "configId": "model", "value": model}, 20*time.Second)
		if err != nil {
			return Result{OK: false, Error: err.Error(), Meta: map[string]any{}}
		}
		var configured map[string]any
		_ = json.Unmarshal(cfg, &configured)
		rec.activeMu.Lock()
		applyConfiguration(rec, configured)
		rec.activeMu.Unlock()
	}
	turn := &Turn{ID: "turn_" + newID(), Stage: o.Stage, Structured: o.Structured, StartedAt: time.Now().UnixMilli(), Status: "running", lastActivity: time.Now()}
	rec.activeMu.Lock()
	rec.active = turn
	rec.State = "running"
	rec.Error = ""
	rec.activeMu.Unlock()
	r.changed(rec)
	timeout := time.Duration(o.TimeoutMs) * time.Millisecond
	if timeout == 0 {
		timeout = time.Duration(r.TimeoutMs) * time.Millisecond
	}
	// Only inactivity consumes the timeout; streaming and tool updates renew it.
	deadline := time.Now().Add(timeout)
	type promptReply struct {
		Raw json.RawMessage
		Err error
	}
	done := make(chan promptReply, 1)
	go func() {
		result, err := d.call("session/prompt", map[string]any{"sessionId": rec.NativeSessionID,
			"prompt": blocks}, 0)
		if result == nil && err == nil {
			err = fmt.Errorf("空响应")
		}
		done <- promptReply{result, err}
	}()
	ticker := time.NewTicker(500 * time.Millisecond)
	defer ticker.Stop()
	var promptErr error
	var stopReason string
	hardKilled := false
	var cancelledAt time.Time
clock:
	for {
		select {
		case reply := <-done:
			promptErr = reply.Err
			var result struct {
				StopReason string         `json:"stopReason"`
				Meta       map[string]any `json:"_meta"`
				Usage      map[string]any `json:"usage"`
			}
			if promptErr == nil {
				promptErr = json.Unmarshal(reply.Raw, &result)
				stopReason = result.StopReason
				turn.Usage = normalizeUsage(result.Usage)
				if stopReason == "refusal" {
					message := "执行端拒绝了本次请求"
					if raw, ok := result.Meta["codebuddy.ai/errorMessage"].(string); ok {
						var detail struct {
							Message string `json:"message"`
							Data    struct {
								Details string `json:"details"`
							} `json:"data"`
						}
						if json.Unmarshal([]byte(raw), &detail) == nil {
							if detail.Message != "" {
								message = detail.Message
							}
							if detail.Data.Details != "" {
								message = detail.Data.Details
							}
						}
					}
					promptErr = fmt.Errorf("%s", message)
				}
			}
			break clock
		case <-ticker.C:
			if !hardKilled {
				rec.activeMu.Lock()
				lastActivity := turn.lastActivity
				rec.activeMu.Unlock()
				if candidate := lastActivity.Add(timeout); candidate.After(deadline) {
					deadline = candidate
				}
				if r.pendingCount(rec.ID) > 0 {
					deadline = time.Now().Add(timeout)
				}
			}
			if !hardKilled && time.Now().After(deadline) {
				_ = d.notify("session/cancel", map[string]any{"sessionId": rec.NativeSessionID})
				hardKilled = true
				cancelledAt = time.Now()
			}
			if hardKilled && time.Since(cancelledAt) > 10*time.Second {
				d.Kill()
				promptErr = fmt.Errorf("ACP 会话超时")
				break clock
			}
		case <-d.Closed:
			promptErr = fmt.Errorf("ACP 进程已退出")
			break clock
		}
	}
	if hardKilled && promptErr == nil {
		promptErr = fmt.Errorf("ACP 会话超时")
	}
	r.cancelPermissions(rec.ID)
	rec.activeMu.Lock()
	turn.FinishedAt = time.Now().UnixMilli()
	rec.Turns = append(rec.Turns, turn)
	if len(rec.Turns) > 20 {
		rec.Turns = rec.Turns[len(rec.Turns)-20:]
	}
	rec.active = nil
	if promptErr != nil {
		turn.Status = "failed"
		turn.Error = promptErr.Error()
		if d.alive {
			rec.State = "idle"
		} else {
			rec.State = "disconnected"
		}
		rec.activeMu.Unlock()
		r.changed(rec)
		r.save()
		return Result{OK: false, Error: promptErr.Error(), Meta: map[string]any{"runtimeSessionId": rec.ID}}
	}
	turn.Status = "completed"
	turn.StopReason = stopReason
	if stopReason == "cancelled" {
		turn.Status = "cancelled"
	}
	rec.State = "idle"
	rec.activeMu.Unlock()
	r.changed(rec)
	r.save()
	structured := extractSubmitted(turn.Tools)
	meta := map[string]any{"sessionId": rec.NativeSessionID, "runtimeSessionId": rec.ID,
		"model": model, "permission": rec.Permission, "tool": agent.RealTool,
		"durationMs": turn.FinishedAt - turn.StartedAt, "stopReason": stopReason}
	if turn.Usage != nil {
		meta["usage"] = turn.Usage
	}
	if hardKilled {
		return Result{OK: false, Output: turn.Output, Error: "ACP 会话超时", Meta: meta}
	}
	if stopReason == "cancelled" {
		return Result{OK: false, Output: turn.Output, Error: "会话已取消", Meta: meta}
	}
	ok := turn.Output != "" || structured != nil
	return Result{OK: ok, Output: turn.Output, Structured: structured, Meta: meta,
		Error: map[bool]string{true: "", false: "会话已停止或未返回有效结果"}[ok]}
}

// Cancel stops the active turn of a record.
func (r *Runtime) Cancel(id string) error {
	r.mu.Lock()
	rec := r.record(id)
	d := r.bindings[id]
	r.mu.Unlock()
	if rec == nil || d == nil {
		return fmt.Errorf("此会话没有正在执行的调用")
	}
	rec.activeMu.Lock()
	noActive := rec.active == nil
	rec.activeMu.Unlock()
	if noActive {
		return fmt.Errorf("此会话没有正在执行的调用")
	}
	rec.activeMu.Lock()
	rec.State = "cancelling"
	rec.activeMu.Unlock()
	r.cancelPermissions(id)
	r.changed(rec)
	return d.notify("session/cancel", map[string]any{"sessionId": rec.NativeSessionID})
}

// Kill force-terminates the driver process.
func (d *Driver) Kill() {
	if d.cmd != nil && d.cmd.Process != nil {
		_ = d.cmd.Process.Kill()
	}
}

// ListJSON returns the session records as JSON, optionally filtered by chat.
func (r *Runtime) ListJSON(chatID string) json.RawMessage {
	type wireSession SessionRecord
	snapshots := make([]json.RawMessage, 0)
	for _, rec := range r.Records(chatID) {
		rec.activeMu.Lock()
		turns, permissions := rec.Turns, rec.Permissions
		if turns == nil {
			turns = []*Turn{}
		}
		if permissions == nil {
			permissions = []*PermissionCard{}
		}
		b, _ := json.Marshal(struct {
			*wireSession
			Turns       []*Turn           `json:"turns"`
			Permissions []*PermissionCard `json:"permissions"`
			ActiveTurn  *Turn             `json:"activeTurn"`
		}{(*wireSession)(rec), turns, permissions, rec.active})
		rec.activeMu.Unlock()
		snapshots = append(snapshots, b)
	}
	b, _ := json.Marshal(snapshots)
	return b
}

// ExportTranscript renders a readable transcript of one session for handoff.
func (r *Runtime) ExportTranscript(id string) string {
	r.mu.Lock()
	rec := r.record(id)
	r.mu.Unlock()
	if rec == nil {
		return ""
	}
	rec.turnMu.Lock()
	defer rec.turnMu.Unlock()
	var b strings.Builder
	for _, t := range rec.Turns {
		stage := t.Stage
		if stage == "" {
			stage = "执行"
		}
		fmt.Fprintf(&b, "### 回合 %s（%s）\n", t.ID, stage)
		if t.Output != "" {
			b.WriteString(t.Output)
			b.WriteString("\n")
		}
		for _, tool := range t.Tools {
			title, _ := tool["title"].(string)
			kind, _ := tool["kind"].(string)
			fmt.Fprintf(&b, "- 工具 %s（%s）\n", title, kind)
			if raw, ok := tool["rawInput"]; ok {
				j, _ := json.Marshal(raw)
				if len(j) > 2000 {
					j = j[:2000]
				}
				b.WriteString("  参数: " + string(j) + "\n")
			}
		}
		if len(t.Plan) > 0 {
			j, _ := json.Marshal(t.Plan)
			if len(j) > 2000 {
				j = j[:2000]
			}
			b.WriteString("  计划: " + string(j) + "\n")
		}
	}
	return b.String()
}

// SaveRecords persists session records (public wrapper for the handoff flow).
func (r *Runtime) SaveRecords() { r.save() }

// Shutdown stops every driver.
func (r *Runtime) Shutdown() {
	r.mu.Lock()
	drivers := make([]*Driver, 0, len(r.drivers))
	for _, d := range r.drivers {
		drivers = append(drivers, d)
	}
	r.mu.Unlock()
	for _, d := range drivers {
		for rec := range d.sessions {
			r.cancelPermissions(rec.ID)
		}
		d.Kill()
		select {
		case <-d.stopped:
		case <-time.After(3 * time.Second):
		}
	}
}

// KillDriversAt terminates pooled drivers whose working directory is the given
// path, waiting for the processes to exit — used before removing a worktree so
// the agent process does not pin the directory on Windows.
func (r *Runtime) KillDriversAt(dir string) {
	r.mu.Lock()
	var doomed []*Driver
	for key, d := range r.drivers {
		if d.cwd == dir {
			doomed = append(doomed, d)
			delete(r.drivers, key)
		}
	}
	r.mu.Unlock()
	for _, d := range doomed {
		for rec := range d.sessions {
			r.cancelPermissions(rec.ID)
		}
		d.Kill()
		select {
		case <-d.stopped:
		case <-time.After(5 * time.Second):
		}
	}
}
