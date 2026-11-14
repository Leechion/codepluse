package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/codeg"
	"harnesshub/internal/extra"
	"harnesshub/internal/hub"
	"harnesshub/internal/mcpserver"
	"harnesshub/internal/orch"
)

// ----------------------------- small helpers -----------------------------

func str(v any) string {
	s, _ := v.(string)
	return s
}

func toMapSlice(v any) []map[string]any {
	raw, ok := v.([]any)
	if !ok {
		return nil
	}
	var out []map[string]any
	for _, item := range raw {
		if m, ok := item.(map[string]any); ok {
			out = append(out, m)
		}
	}
	return out
}

func toStrSlice(v any) []string {
	raw, ok := v.([]any)
	if !ok {
		return nil
	}
	var out []string
	for _, item := range raw {
		if s, ok := item.(string); ok {
			out = append(out, s)
		}
	}
	return out
}

func mustJSON(v any) json.RawMessage {
	b, _ := json.Marshal(v)
	return b
}

func atoiDefault(s string, def int) int {
	if n, err := strconv.Atoi(s); err == nil && n > 0 {
		return n
	}
	return def
}

// ----------------------------- real status -----------------------------

func toolsStatus() map[string]any {
	out := map[string]any{}
	for _, t := range []string{"codex", "claude"} {
		bin := resolveBin(t)
		out[t] = map[string]any{"available": bin != "", "bin": bin, "label": binLabels[t]}
	}
	return out
}

func (s *server) realStatus() map[string]any {
	realMu.Lock()
	cfg := map[string]any{}
	for k, v := range realConfig {
		cfg[k] = v
	}
	realMu.Unlock()
	warnings := []string{}
	if resolveBin("claude") == "" {
		warnings = append(warnings, "未找到 Claude Code CLI 可执行文件。")
	}
	if resolveBin("codex") == "" {
		warnings = append(warnings, "未找到 Codex CLI 可执行文件。")
	}
	if os.Getenv("HTTPS_PROXY") != "" || os.Getenv("https_proxy") != "" {
		warnings = append(warnings, "检测到 HTTP 代理；Codex 将使用自身的供应商配置。")
	}
	realTasks, realRunningTasks, realDone := 0, 0, 0
	for _, t := range s.hub.Tasks {
		if t.Execution.Mode != "real" {
			continue
		}
		realTasks++
		if t.Status == "running" {
			realRunningTasks++
		}
		if t.Status == "succeeded" {
			realDone++
		}
	}
	return map[string]any{"enabled": REAL_ENABLED, "config": cfg, "tools": toolsStatus(),
		"permissions": permissions, "warnings": warnings,
		"queue": map[string]any{"pending": 0, "running": s.realRunning, "entries": []string{}},
		"stats": map[string]any{"realTasks": realTasks, "realRunning": realRunningTasks, "realCompleted": realDone}}
}

// ----------------------------- ACP registration -----------------------------

// registerAcp creates an agent backed by a custom ACP adapter command.
func (s *server) registerAcp(spec map[string]any) (*hub.Agent, error) {
	if !REAL_ENABLED {
		return nil, fmt.Errorf("本机 Harness 执行已关闭")
	}
	command, _ := spec["command"].(string)
	if command == "" || strings.HasSuffix(strings.ToLower(command), ".cmd") || strings.HasSuffix(strings.ToLower(command), ".bat") {
		return nil, fmt.Errorf("请提供可执行文件或 node 路径，不使用 shell 脚本入口")
	}
	rawArgs, _ := spec["args"].([]any)
	if len(rawArgs) > 40 {
		return nil, fmt.Errorf("args 必须是字符串数组")
	}
	args := []string{}
	for _, a := range rawArgs {
		s, ok := a.(string)
		if !ok {
			return nil, fmt.Errorf("args 必须是字符串数组")
		}
		args = append(args, s)
	}
	readOnlyMode, _ := spec["readOnlyMode"].(string)
	if readOnlyMode != "" && readOnlyMode != "plan" && readOnlyMode != "read-only" && readOnlyMode != "readonly" {
		return nil, fmt.Errorf("只读模式须为 plan、read-only 或 readonly")
	}
	if id, _ := spec["id"].(string); id != "" && s.hub.Agent(id) != nil {
		return nil, fmt.Errorf("实例 ID 已存在")
	}
	raw, _ := json.Marshal(spec)
	var agent hub.Agent
	if json.Unmarshal(raw, &agent) != nil {
		return nil, fmt.Errorf("请求体无效")
	}
	created := s.hub.CreateAgent(&agent)
	created.Real = true
	created.Communication = "acp"
	created.Endpoint = "acp://custom/" + created.ID
	created.AdapterKind = "ACP"
	workspaceMode, _ := spec["workspaceMode"].(string)
	if workspaceMode == "" {
		workspaceMode = "default"
	}
	if readOnlyMode == "" {
		readOnlyMode = "plan"
	}
	created.ACPModes = map[string]string{"readonly": readOnlyMode, "shell": readOnlyMode, "workspace": workspaceMode}
	s.mu.Lock()
	s.customAcp[created.ID] = map[string]any{"command": command, "args": args}
	s.mu.Unlock()
	s.persistCustomAcp()
	return created, nil
}

func (s *server) customAcpFile() string {
	return filepath.Join(ROOT, ".data", "acp-agents.json")
}

func (s *server) persistCustomAcp() {
	if STATE_MEMORY {
		return
	}
	s.mu.Lock()
	var saved []map[string]any
	for id, launch := range s.customAcp {
		a := s.hub.Agent(id)
		if a == nil {
			continue
		}
		command, _ := launch["command"].(string)
		args, _ := launch["args"].([]string)
		saved = append(saved, map[string]any{"id": id, "name": a.Name, "type": a.Type,
			"command": command, "args": args,
			"readOnlyMode": a.ACPModes["readonly"], "workspaceMode": a.ACPModes["workspace"]})
	}
	s.mu.Unlock()
	_ = os.MkdirAll(filepath.Dir(s.customAcpFile()), 0o755)
	_ = os.WriteFile(s.customAcpFile(), mustJSON(saved), 0o644)
}

func (s *server) loadCustomAcp() {
	if STATE_MEMORY {
		return
	}
	data, err := os.ReadFile(s.customAcpFile())
	if err != nil {
		return
	}
	var saved []map[string]any
	if json.Unmarshal(data, &saved) != nil {
		return
	}
	for _, spec := range saved {
		_, _ = s.registerAcp(spec)
	}
}

// launchFn builds the ACP adapter launch for one agent.
func (s *server) launchFn(agent *hub.Agent, permission, cwd string) (*acp.Launch, error) {
	if s.codeg != nil {
		if launch, ok, err := s.codeg.CustomAgentLaunch(agent); ok {
			return launch, err
		}
	}
	s.mu.Lock()
	custom := s.customAcp[agent.ID]
	s.mu.Unlock()
	if custom != nil {
		command, _ := custom["command"].(string)
		rawArgs, _ := custom["args"].([]string)
		return &acp.Launch{Command: command, Args: rawArgs, Env: os.Environ()}, nil
	}
	tool := agent.RealTool
	pkg := acpInstalled(tool)
	if pkg == "" {
		return &acp.Launch{Command: ""}, nil
	}
	node, err := exec.LookPath("node")
	if err != nil {
		return nil, fmt.Errorf("未找到 node，无法启动 ACP 适配器")
	}
	env, err := buildEnv(agent)
	if err != nil {
		return nil, err
	}
	// Layer the settings-page overlay (stored in codeg's own data, not the
	// vendor files) onto the launch environment, and materialize codex's
	// config.toml / auth.json into its ISOLATED CODEX_HOME (already under
	// .data, so this never touches the user's real ~/.codex).
	if s.codeg != nil {
		overlayEnv, codexToml, codexAuth := s.codeg.LaunchOverlay(agent)
		for k, v := range overlayEnv {
			env = append(dropEnvKey(env, k), k+"="+v)
		}
		if agent.RealTool == "codex" && (codexToml != "" || codexAuth != "") {
			if dir, err := codexHome(agent.Wire); err == nil {
				if codexToml != "" {
					_ = os.WriteFile(filepath.Join(dir, "config.toml"), []byte(codexToml), 0o600)
				}
				if codexAuth != "" {
					_ = os.WriteFile(filepath.Join(dir, "auth.json"), []byte(codexAuth), 0o600)
				}
			}
		}
	}
	if tool == "codex" {
		mode := "read-only"
		if permission == "workspace" {
			mode = "workspace-write"
		}
		env = append(env, "INITIAL_AGENT_MODE="+mode)
	}
	if tool == "claude" {
		env = append(env, "CLAUDE_CODE_EXECUTABLE="+resolveBin("claude"))
	}
	args := []string{pkg}
	if tool == "claude" {
		// An instance may pin its own provider; resolveProvider falls back to
		// the global active selection, which is what the built-in follows.
		provider, configured, err := resolveProvider(agent)
		if err != nil {
			return nil, fmt.Errorf("读取 API 供应商失败: %w", err)
		}
		if configured {
			env = dropEnvKey(env, "CLAUDE_CODE_OAUTH_TOKEN")
			if provider.Env != nil {
				filtered := env[:0]
				for _, item := range env {
					if !strings.HasPrefix(strings.ToUpper(item), "ANTHROPIC_") && !strings.HasPrefix(strings.ToUpper(item), "CLAUDE_CODE_SUBAGENT_MODEL=") {
						filtered = append(filtered, item)
					}
				}
				env = filtered
				for key, value := range provider.Env {
					if strings.EqualFold(key, "ANTHROPIC_BASE_URL") {
						value = anthropicBaseURL(value)
					}
					env = append(env, key+"="+value)
				}
				if provider.Model != "" && provider.Env["ANTHROPIC_MODEL"] == "" {
					env = append(env, "ANTHROPIC_MODEL="+provider.Model)
				}
			} else {
				for _, key := range []string{"ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "ANTHROPIC_MODEL"} {
					env = dropEnvKey(env, key)
				}
				env = append(env, "ANTHROPIC_BASE_URL="+anthropicBaseURL(provider.URL), "ANTHROPIC_AUTH_TOKEN="+provider.Key, "ANTHROPIC_API_KEY="+provider.Key, "ANTHROPIC_MODEL="+provider.Model)
			}
			if agent.DefaultModel != "" {
				env = append(dropEnvKey(env, "ANTHROPIC_MODEL"), "ANTHROPIC_MODEL="+agent.DefaultModel)
			}
		}
	}
	if tool == "codex" && agent.ProviderID != "" {
		var err error
		env, err = applyCodexProvider(agent, env)
		if err != nil {
			return nil, err
		}
	}
	if agent.AuthMode == "subscription" {
		env, err = applyOfficialAccount(agent, env)
		if err != nil {
			return nil, err
		}
	}
	if tool == "dsh" {
		if permission != "workspace" {
			return nil, fmt.Errorf("DSH 不提供只读会话模式，请使用其原生工作区策略")
		}
		args = append(args, "--profile", "acp")
		var patch string
		var err error
		if agent.ProviderID != "" {
			patch, env, err = dshProviderPatch(agent, env)
		} else {
			patch, err = dshDesktopPatch()
		}
		if err != nil {
			return nil, err
		}
		if patch != "" {
			args = append(args, "--patch", patch)
		}
	}
	if tool == "workbuddy" {
		// The desktop lite bundle registers ACP through its server strategy.
		args = append(args, "--serve", "--acp", "--host", "127.0.0.1")
	}
	return &acp.Launch{Command: node, Args: args, Env: env}, nil
}

// agentAvailable reports whether one registered agent can actually launch on
// this machine, by probing the local environment rather than trusting its
// registration flag.
//
// A built-in ACP harness is available when its adapter entry resolves: the
// adapter npm package either bundles the native CLI or declares it as a
// dependency (@openai/codex, @anthropic-ai/claude-agent-sdk), so a present
// adapter entry is the reliable evidence that the harness is installed. A
// custom ACP instance is available when its pinned launch command still exists.
func (s *server) agentAvailable(a *hub.Agent) bool {
	if s.codeg != nil {
		if available, ok := s.codeg.CustomAgentAvailable(a); ok {
			return available
		}
	}
	s.mu.Lock()
	custom := s.customAcp[a.ID]
	s.mu.Unlock()
	if custom != nil {
		command, _ := custom["command"].(string)
		if command == "" {
			return false
		}
		if _, err := os.Stat(command); err != nil {
			return false
		}
		return true
	}
	if a.RealTool == "" {
		return false
	}
	return acpInstalled(a.RealTool) != ""
}

// ----------------------------- single tasks -----------------------------

// runSingleTask executes a one-off real task through the ACP runtime.
func (s *server) runSingleTask(t *hub.Task) {
	permission := t.Execution.Permission
	if permission == "" {
		permission = cfgString("permission")
	}
	wd, err := resolveWorkdir(t.Execution.Workdir)
	if err != nil {
		s.hub.CompleteTask(t.ID, "", err.Error())
		return
	}
	tool := t.Execution.Tool
	if tool != "codex" && tool != "claude" {
		tool = cfgString("defaultTool")
		if tool != "codex" && tool != "claude" {
			if resolveBin("claude") != "" {
				tool = "claude"
			} else {
				tool = "codex"
			}
		}
	}
	var agent *hub.Agent
	for _, a := range s.hub.Agents() {
		if a.RealTool == tool && a.Enabled {
			agent = a
			break
		}
	}
	if agent == nil {
		s.hub.CompleteTask(t.ID, "", "没有已启用的真实执行端："+tool)
		return
	}
	s.hub.StartTask(t.ID, agent.ID, hub.Execution{Tool: tool, Permission: permission, Workdir: wd})
	result := s.invoke(agent, t.Prompt, acp.Options{Permission: permission, Workdir: wd,
		SessionKey: "single:" + t.ID, Stage: t.Title, Structured: false,
		TimeoutMs: cfgInt("timeoutMs")})
	if result.OK {
		s.hub.CompleteTask(t.ID, hub.TrimResult(result.Output, 20000), "")
	} else {
		s.hub.CompleteTask(t.ID, "", result.Error)
	}
}

// pendingApprovals lists permission cards awaiting an answer.
func (s *server) pendingApprovals() []map[string]any {
	var out []map[string]any
	for _, rec := range s.rt.Records("") {
		for _, card := range rec.Permissions {
			if card.State == "pending" {
				b, _ := json.Marshal(card)
				var m map[string]any
				_ = json.Unmarshal(b, &m)
				out = append(out, m)
			}
		}
	}
	if out == nil {
		out = []map[string]any{}
	}
	return out
}

// ----------------------------- persistence -----------------------------

func dataFile(name string) string {
	return filepath.Join(ROOT, ".data", name)
}

func readJSONFile(path string) json.RawMessage {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	return json.RawMessage(strings.TrimPrefix(string(data), "\uFEFF"))
}

func writeJSONFile(path string, data json.RawMessage) {
	_ = os.MkdirAll(filepath.Dir(path), 0o755)
	tmp := path + ".tmp"
	if os.WriteFile(tmp, data, 0o644) == nil {
		_ = os.Rename(tmp, path)
	}
}

// persistenceLoop mirrors the Node side: missions and chats save on change.
func (s *server) persistenceLoop() {
	_, events := s.hub.Subscribe()
	lastMission, lastChat := time.Time{}, time.Time{}
	for evt := range events {
		switch evt.Type {
		case "mission":
			if time.Since(lastMission) < 300*time.Millisecond {
				continue
			}
			lastMission = time.Now()
			writeJSONFile(dataFile("missions.json"), s.orch.MarshalAll())
		case "chat":
			if time.Since(lastChat) < 300*time.Millisecond {
				continue
			}
			lastChat = time.Now()
			writeJSONFile(dataFile("chats.json"), s.chats.MarshalAll())
		}
	}
}

// ----------------------------- main -----------------------------

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "mcp-result":
			os.Exit(mcpserver.MainResult(os.Args[2:]))
		case "mcp-files":
			os.Exit(mcpserver.MainFiles(os.Args[2:]))
		case "mcp-codeg":
			os.Exit(codeg.MainCompanion(os.Args[2:]))
		}
	}

	_ = os.MkdirAll(filepath.Join(ROOT, ".data"), 0o755)
	_ = os.MkdirAll(cfgString("workdir"), 0o755)

	h := hub.NewHub()
	sessionFile := dataFile("sessions.json")
	if CODEG_MODE {
		sessionFile = dataFile("codeg-sessions.json")
	}
	rt := acp.NewRuntime(h, mapStr(STATE_MEMORY, ":memory:", sessionFile), cfgInt("timeoutMs"))
	skills := extra.NewSkills(filepath.Join(ROOT, "skills"))
	worktrees := extra.NewWorktrees(ROOT, "")
	srv := &server{hub: h, rt: rt, skills: skills, worktrees: worktrees,
		customAcp: map[string]map[string]any{}, agentLocks: map[string]*sync.Mutex{},
		sseClients: map[chan []byte]bool{}}
	rt.LaunchFn = srv.launchFn
	rt.LaunchVersion = func(agent *hub.Agent) string { return providerVersion(agent) }

	logFn := func(level, source, text string) { h.Log(level, source, text) }
	orchestrator := &orch.Orchestrator{Hub: h, Invoke: srv.invoke,
		ReadFile: func(m *orch.Mission, request map[string]any) (map[string]any, error) {
			wd, err := resolveWorkdir(m.Workdir)
			if err != nil {
				return nil, err
			}
			files, err := mcpserver.NewFiles(wd)
			if err != nil {
				return nil, err
			}
			result, err := files.Call("read_file", request)
			if err != nil {
				return nil, err
			}
			if text, ok := result["text"].(string); ok && len(text) > 30000 {
				result["text"] = text[:30000]
			}
			return result, nil
		},
		Worktrees: worktrees, Skills: skills}
	srv.orch = orchestrator
	srv.chats = orch.NewChatController(h, orchestrator)
	srv.automations = orch.NewAutomations(srv.chats,
		mapStr(STATE_MEMORY || CODEG_MODE, ":memory:", dataFile("automations.json")), logFn)

	extra.NewNotifier(h, os.Getenv("HARNESS_HUB_WEBHOOK_URL"), logFn)

	// seed real CLI agents
	if REAL_ENABLED {
		for _, tool := range []string{"codex", "claude", "dsh", "workbuddy"} {
			if acpInstalled(tool) == "" {
				continue
			}
			agent := h.CreateAgent(&hub.Agent{ID: "agt_" + tool + "_01",
				Type: map[string]string{"codex": "codex", "claude": "claude-code", "dsh": "deepseek", "workbuddy": "workbuddy"}[tool],
				Name: map[string]string{"codex": "Codex", "claude": "Claude Code", "dsh": "DeepSeek (DSH)", "workbuddy": "WorkBuddy"}[tool],
				Load: 0, Trust: 1})
			agent.Real = true
			agent.RealTool = tool
			agent.ACPVersion = adapterVersion(acpInstalled(tool))
			if tool == "workbuddy" {
				agent.ACPModes = map[string]string{"readonly": "plan", "workspace": "default", "shell": "plan"}
			}
			agent.Communication = "acp"
			agent.Endpoint = "acp://" + tool
			agent.AdapterKind = "ACP"
		}
	}
	srv.loadCustomAcp()

	// restore persisted state
	if !STATE_MEMORY && !CODEG_MODE {
		if raw := readJSONFile(dataFile("missions.json")); raw != nil {
			if err := orchestrator.Restore(raw); err != nil {
				fmt.Fprintln(os.Stderr, "[hub] 读取任务记录失败："+err.Error())
			}
		}
		if raw := readJSONFile(dataFile("chats.json")); raw != nil {
			if err := srv.chats.Restore(raw); err != nil {
				fmt.Fprintln(os.Stderr, "[hub] 读取对话失败："+err.Error())
			}
		}
	}

	go srv.sseFanOut()
	if !CODEG_MODE {
		go srv.persistenceLoop()
	}
	go func() {
		for {
			time.Sleep(30 * time.Second)
			func() {
				defer func() { recover() }() // tick must never crash the server
				srv.automations.Tick()
				if srv.codeg != nil {
					// TickAutomations 已随 H 组翻转 Rust（automations.rs 的
					// spawn_scheduler）；保留 TickWorkTasks（D 组仍在 Go）。
					srv.codeg.TickWorkTasks()
				}
			}()
		}
	}()

	mux := http.NewServeMux()
	if os.Getenv("HARNESS_HUB_CODEG") == "1" {
		var err error
		srv.codeg, err = codeg.New(ROOT, "http://127.0.0.1:"+strconv.Itoa(PORT), rt, h, resolveWorkdir)
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		// Drive the agents' "available" flag from a live probe of the local
		// environment so the new-conversation page lists only harnesses that
		// are actually installed on this machine.
		srv.codeg.AgentAvailable = srv.agentAvailable
		// The directory picker may browse the whole disk when the browse flag
		// is on, but that resolver is never used for workdirs or file access.
		srv.codeg.BrowseResolve = resolveBrowseWorkdir
		srv.codeg.Start()
	}
	mux.HandleFunc("/", srv.handle)
	addr := HOST + ":" + strconv.Itoa(PORT)
	httpSrv := &http.Server{Addr: addr, Handler: newLocalGuard(PORT).wrap(mux), ReadHeaderTimeout: 10 * time.Second}

	appName := "Harness Hub"
	if CODEG_MODE {
		appName = "codepluse"
	}
	fmt.Printf("[hub] %s (Go) 监听 http://%s\n", appName, addr)
	go func() {
		if err := httpSrv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			fmt.Fprintln(os.Stderr, "[hub] 服务异常退出："+err.Error())
			os.Exit(1)
		}
	}()

	sig := make(chan os.Signal, 1)
	signal.Notify(sig, os.Interrupt, syscall.SIGTERM)
	<-sig
	fmt.Println("[hub] 正在关闭…")
	if srv.codeg != nil {
		srv.codeg.Close()
	}
	rt.Shutdown()
}

func mapStr(cond bool, a, b string) string {
	if cond {
		return a
	}
	return b
}
