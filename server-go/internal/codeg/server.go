package codeg

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
	"harnesshub/internal/modelproviders"
	"harnesshub/internal/providers"
)

type Server struct {
	Runtime    *acp.Runtime
	Hub        *hub.Hub
	Root, Base string
	Resolve    func(string) (string, error)
	// AgentAvailable reports whether an agent's backing adapter/executable is
	// actually present on this machine. It is injected by the wiring layer,
	// which owns the binary-resolution logic. When nil, every enabled agent is
	// treated as available (the prior behavior).
	AgentAvailable func(a *hub.Agent) bool
	RegistryURL    string
	RegistryClient *http.Client
	store          *Store
	mu             sync.Mutex
	connections    map[string]*Connection
	subscribers    map[chan Row]bool
	tasks          map[string]*Delegation
	callers        map[string]*Caller
	// splits are the task-split cards waiting for the user (see split.go).
	splits map[string]*taskSplit
	// questions are blocking ask_user_question tool calls waiting on the user
	// (see question.go).
	questions      map[string]*parkedQuestion
	terminalMu     sync.Mutex
	terminals      map[string]*TerminalSession
	watchMu        sync.Mutex
	watches        map[string]*WorkspaceWatch
	auto           *autoStore
	forge          *forgeStore
	tasks2         *taskStore
	folderCommands *folderCommandStore
	browser        *browserManager
	computer       *computerState
	// Warm pool: pre-connected, unused sessions that acp_connect can claim
	// without paying the 15-25s session/new cost on every harness switch.
	warmMu  sync.Mutex
	warm    map[string][]warmEntry
	warming map[string]bool
	started bool // guarded by warmMu; prewarming is a runtime-only behavior
}

func New(root, base string, rt *acp.Runtime, h *hub.Hub, resolve func(string) (string, error)) (*Server, error) {
	st, err := newStore(filepath.Join(root, ".data", "codeg-workspace.json"))
	if err != nil {
		return nil, err
	}
	s := &Server{Runtime: rt, Hub: h, Root: root, Base: base, Resolve: resolve, store: st, connections: map[string]*Connection{}, subscribers: map[chan Row]bool{}, tasks: map[string]*Delegation{}, callers: map[string]*Caller{}, splits: map[string]*taskSplit{}, questions: map[string]*parkedQuestion{}, terminals: map[string]*TerminalSession{}, warm: map[string][]warmEntry{}, warming: map[string]bool{}}
	s.auto = newAutoStore(root)
	s.forge = newForgeStore(root)
	s.tasks2 = newTaskStore(root)
	s.folderCommands = newFolderCommandStore(root)
	s.tasks2.onChange = func(taskID int) { s.emit("task://changed", Row{"id": taskID}) }
	s.browser = &browserManager{console: map[string][]Row{}}
	s.computer = &computerState{}
	if err := s.loadDelegations(); err != nil {
		return nil, err
	}
	s.loadCustomAgents()
	s.loadInstances()
	s.loadSplits()
	s.applyEnabledOverrides()
	rt.OnUpdate = s.nativeUpdate
	rt.ExtraMCP = s.extraMCP
	rt.AllowInternalTool = s.allowInternalTool
	return s, nil
}
func (s *Server) Start() {
	go s.permissionsLoop()
	if os.Getenv("HARNESS_HUB_WARM") != "0" {
		s.warmMu.Lock()
		s.started = true
		s.warmMu.Unlock()
		go s.warmAll()
		go s.warmSweeper()
	}
}
func respond(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}
func (s *Server) emit(channel string, payload any) {
	s.mu.Lock()
	defer s.mu.Unlock()
	event := Row{"channel": channel, "payload": payload}
	for ch := range s.subscribers {
		select {
		case ch <- event:
		default:
		}
	}
}
func (s *Server) changed(row Row) {
	s.emit("conversation://changed", Row{"kind": "upsert", "summary": clone(row)})
}
func (s *Server) pick(agentType string) *hub.Agent {
	for _, a := range s.Hub.Agents() {
		wire := wireAgent(a)
		if a.Enabled && a.Real && (wire == agentType || a.ID == agentType) {
			return a
		}
	}
	return nil
}
func wireAgent(a *hub.Agent) string {
	if a.Wire != "" {
		return a.Wire
	}
	if a.RealTool == "dsh" {
		return "deepseek"
	}
	if a.RealTool == "workbuddy" {
		return "custom:workbuddy"
	}
	if a.RealTool == "claude" {
		return "claude_code"
	}
	if a.RealTool == "codex" {
		return "codex"
	}
	return "custom:" + a.ID
}
func (s *Server) agents() []Row {
	out := []Row{}
	// Snapshot the settings-page overlay under the store lock, then release it:
	// agents() runs on the pre-store-lock dispatch path (acp_list_agents), and
	// the per-agent installedVersion/AgentAvailable probes do file I/O that must
	// not run while holding st.mu. Cloning each overlay Row also means the loop
	// below never touches the live map a concurrent acp_update_agent_env may
	// mutate.
	s.store.mu.Lock()
	overlay := make(map[string]Row, len(s.store.Agents))
	for wire, ov := range s.store.Agents {
		overlay[wire] = clone(ov)
	}
	s.store.mu.Unlock()
	for _, a := range s.Hub.Agents() {
		if !a.Real {
			continue
		}
		// "available" is a live probe of the local environment, not the
		// registration flag: the agent is available only when its backing
		// adapter/executable is actually present on this machine. "enabled"
		// stays the user/config switch, so the two are no longer the same value.
		available := a.Enabled
		if s.AgentAvailable != nil {
			available = s.AgentAvailable(a)
		}
		row := Row{"agent_type": wireAgent(a), "name": a.Name, "description": "ACP · " + a.Name, "available": available, "enabled": a.Enabled, "registry_id": wireAgent(a), "registry_version": nil, "skills_capable": true, "supports_custom_version": false, "distribution_type": "system", "is_acp_adapter": a.RealTool != "", "custom_source": nil, "installed_version": s.installedVersion(a), "sort_order": len(out), "env": Row{}, "config_json": nil, "config_file_path": nil, "codex_auth_json": nil, "codex_config_toml": nil, "codex_model_catalog": nil, "system_instructions_content": nil, "system_instructions_file_path": nil, "model_provider_id": nil, "host_tools_agent_mode": true, "instance": a.Wire != "", "real_tool": a.RealTool, "provider_id": a.ProviderID, "default_model": a.DefaultModel}
		mergeAgentOverlay(overlay, wireAgent(a), row)
		// Go injects the collaboration MCP alongside each harness's native
		// file/command tools; native tools do not withhold delegation here.
		row["host_tools_supports_delegation"] = true
		if d, ok := s.customDef(wireAgent(a)); ok {
			available, _ = s.CustomAgentAvailable(a)
			row["available"] = available
			row["instance"] = false
			row["description"] = d.Description
			row["registry_id"] = d.RegistryID
			row["registry_version"] = nil
			if d.Source == "registry" && d.Version != "" {
				row["registry_version"] = d.Version
			}
			row["custom_source"] = d.Source
			row["icon_url"] = d.IconURL
			row["distribution_type"] = d.Kind
			row["is_acp_adapter"] = false
			row["skills_capable"] = d.SkillsSharedStore || d.SkillsDir != nil
			row["host_tools_supports_delegation"] = d.SupportsMCP
			row["installed_version"] = s.customInstalledVersion(d)
		}
		out = append(out, row)
	}
	return out
}
func (s *Server) installedVersion(a *hub.Agent) string {
	if a.ACPVersion != "" {
		return a.ACPVersion
	}
	name := "codex-acp"
	if a.RealTool == "claude" {
		name = "claude-agent-acp"
	}
	if a.RealTool == "" {
		return "local"
	}
	home, _ := os.UserHomeDir()
	for _, base := range []string{filepath.Join(home, "AppData", "Roaming", "npm", "node_modules"), filepath.Join(s.Root, "node_modules")} {
		b, err := os.ReadFile(filepath.Join(base, "@agentclientprotocol", name, "package.json"))
		if err != nil {
			continue
		}
		var pkg struct {
			Version string `json:"version"`
		}
		if json.Unmarshal(b, &pkg) == nil && pkg.Version != "" {
			return pkg.Version
		}
	}
	return "local"
}
func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path == "/ws" || r.URL.Path == "/ws/events" {
		s.websocket(w, r)
		return
	}
	if r.URL.Path == "/api/codeg_mcp" {
		s.companionRequest(w, r)
		return
	}
	if r.URL.Path == "/api/health" {
		respond(w, 200, Row{"ok": true, "app": "codepluse", "data": Row{"status": "ok", "version": "2.0.0-go", "sessionRuntime": "acp-v1"}, "backend": "go", "codeg_version": "0.42.0"})
		return
	}
	if r.URL.Path == "/api/upload_attachment" {
		s.uploadAttachment(w, r)
		return
	}
	if !strings.HasPrefix(r.URL.Path, "/api/") {
		s.static(w, r)
		return
	}
	if r.Method != "POST" {
		respond(w, 405, Row{"code": "invalid_input", "message": "Use POST"})
		return
	}
	var args Row
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 2<<20)).Decode(&args); err != nil {
		respond(w, 400, Row{"code": "invalid_input", "message": err.Error()})
		return
	}
	command := strings.TrimPrefix(r.URL.Path, "/api/")
	result, err := s.command(command, args)
	if err != nil {
		fmt.Fprintln(os.Stderr, "[codeg-api]", command, err)
		// A 409 here always means a genuine turn is running: prompt() takes a
		// leaked busy flag over in place (past the stale grace, no runtime
		// active turn) instead of rejecting it, so the client never sees a
		// healable rejection and must never disconnect to "recover".
		if err.Error() == "turn_in_progress" {
			respond(w, 409, Row{"code": "turn_in_progress", "message": "A turn is already in progress"})
			return
		}
		respond(w, 400, Row{"code": "task_execution_failed", "message": err.Error()})
		return
	}
	respond(w, 200, result)
}
func (s *Server) static(w http.ResponseWriter, r *http.Request) {
	static := filepath.Join(s.Root, "codeg-ui", "out")
	request := r.URL.Path
	if request == "/" {
		request = "/index.html"
	}
	file := filepath.Join(static, filepath.Clean("/"+request))
	if r.Header.Get("Rsc") == "1" {
		if _, err := os.Stat(file + ".txt"); err == nil {
			file += ".txt"
			w.Header().Set("Content-Type", "text/x-component")
		}
	} else if info, err := os.Stat(file); err != nil || info.IsDir() {
		if _, err := os.Stat(file + ".html"); err == nil {
			file += ".html"
		} else {
			file = filepath.Join(file, "index.html")
		}
	}
	http.ServeFile(w, r, file)
}
func (s *Server) command(command string, a Row) (any, error) {
	if command == "acp_current_platform" || command == "acp_fetch_registry_catalog" || command == "acp_add_registry_agent" || command == "acp_list_custom_agents" || command == "acp_save_custom_agent" || command == "acp_delete_custom_agent" {
		return s.customAgentCommand(command, a)
	}
	if command == "acp_download_agent_binary" || command == "acp_prepare_npx_agent" || command == "acp_uninstall_agent" || command == "acp_clear_binary_cache" {
		if d, ok := s.customDef(text(a["agentType"])); ok {
			return s.customInstallCommand(command, a, d)
		}
	}
	if command == "delete_conversation" {
		return nil, s.deleteConversation(number(a["conversationId"]))
	}
	if command == "get_commit_message_settings" || command == "set_commit_message_settings" || command == "delete_commit_message_settings" {
		return s.commitSettings(command, a)
	}
	if command == "start_workspace_state_stream" || command == "stop_workspace_state_stream" || command == "get_workspace_snapshot" {
		return s.workspaceCommand(command, a)
	}
	if command == "open_commit_window" || command == "open_push_window" || command == "open_stash_window" || command == "open_merge_window" {
		return gitWindow(command, a), nil
	}
	if strings.HasPrefix(command, "terminal_") || command == "get_available_terminal_shells" || command == "probe_terminal_shell_path" {
		return s.terminalCommand(command, a)
	}
	if strings.HasPrefix(command, "canvas_") {
		return s.canvasCommand(command, a)
	}
	if strings.HasPrefix(command, "automation_") {
		return s.automationCommand(command, a)
	}
	if strings.HasPrefix(command, "work_task_") {
		return s.workTaskCommand(command, a)
	}
	if command == "list_folder_commands" || command == "create_folder_command" ||
		command == "update_folder_command" || command == "delete_folder_command" ||
		command == "reorder_folder_commands" || command == "bootstrap_folder_commands_from_package_json" {
		return s.folderCommandCommand(command, a)
	}
	if strings.HasPrefix(command, "officecli_") || strings.HasPrefix(command, "office_") ||
		command == "start_office_watch" || command == "stop_office_watch" {
		return s.officeCommand(command, a)
	}
	if strings.HasPrefix(command, "browser_") {
		return s.browserCommand(command, a)
	}
	if strings.HasPrefix(command, "computer_") || command == "get_computer_tools_settings" ||
		command == "set_computer_tools_enabled" || command == "set_computer_tools_preferences" {
		return s.computerCommand(command, a)
	}
	if strings.HasPrefix(command, "app_update") || command == "check_app_update" ||
		command == "perform_app_update" || command == "rollback_app" || command == "restart_app" {
		return s.updaterCommand(command, a)
	}
	if strings.HasPrefix(command, "forge_") || command == "get_github_accounts" || command == "validate_github_token" || command == "update_github_accounts" || command == "folder_forge_remote" {
		return s.forgeCommand(command, a)
	}
	if strings.HasPrefix(command, "git_") || command == "get_git_branch" {
		return s.gitCommand(command, a)
	}
	if strings.HasPrefix(command, "codepluse_instance_") {
		return s.instanceCommand(command, a)
	}
	switch command {
	case "codepluse_providers_list":
		return providers.Public(s.Root)
	case "codepluse_provider_models":
		return s.harnessProviderModels(text(a["providerId"]))
	case "update_main_bot_settings":
		return s.saveMainBotSettings(a)
	case "codepluse_provider_save":
		return nil, providers.Save(s.Root, providers.Provider{ID: text(a["id"]), Name: text(a["name"]), Agent: text(a["agent"]), URL: text(a["url"]), Model: text(a["model"]), Key: text(a["key"])})
	case "codepluse_provider_activate":
		return nil, providers.Activate(s.Root, text(a["agent"]), text(a["id"]))
	case "codepluse_provider_delete":
		return nil, providers.Delete(s.Root, text(a["id"]))
	case "list_model_providers":
		return modelproviders.List(s.Root)
	case "list_provider_models":
		return s.listProviderModels(a)
	case "create_model_provider":
		info, e := modelproviders.Create(s.Root, text(a["name"]), text(a["apiUrl"]), text(a["apiKey"]), text(a["agentType"]), optStr(a["model"]))
		if e != nil {
			return nil, e
		}
		return info, nil
	case "update_model_provider":
		info, affected, e := modelproviders.Update(s.Root, int64(number(a["id"])), optStr(a["name"]), optStr(a["apiUrl"]), optStr(a["apiKey"]), optStr(a["agentType"]), optStr(a["model"]))
		if e != nil {
			return nil, e
		}
		return Row{"provider": info, "affectedRunningSessions": affected}, nil
	case "delete_model_provider":
		return nil, modelproviders.Delete(s.Root, int64(number(a["id"])))
	case "ccswitch_provider_config":
		// 取回单个 CC Switch 供应商的明文配置（仅限 ccswitch: 前缀，防止借此读 store 密钥）。
		id := text(a["id"])
		if !strings.HasPrefix(id, "ccswitch:") {
			return nil, fmt.Errorf("仅支持 CC Switch 供应商")
		}
		p, e := providers.Find(s.Root, id)
		if e != nil {
			return nil, e
		}
		key := p.Env["ANTHROPIC_AUTH_TOKEN"]
		if key == "" {
			key = p.Env["ANTHROPIC_API_KEY"]
		}
		return Row{"url": p.Env["ANTHROPIC_BASE_URL"], "apiKey": key, "model": p.Model, "hasKey": key != ""}, nil
	case "acp_list_agents":
		return s.agents(), nil
	case "acp_detect_agent_local_version":
		ag := s.pick(text(a["agentType"]))
		if ag == nil {
			return nil, nil
		}
		return s.installedVersion(ag), nil
	case "acp_preflight":
		return s.preflight(text(a["agentType"]))
	case "token_usage_report":
		return s.tokenUsageReport(a)
	case "token_usage_facets":
		return s.tokenUsageFacets()
	case "token_usage_status":
		return s.tokenUsageStatus()
	case "token_usage_sync":
		return s.tokenUsageSync(a)
	case "experts_list", "science_list", "experts_read_content", "science_read_content", "experts_list_all_install_statuses", "science_list_all_install_statuses", "officecli_skill_list_all_install_statuses", "acp_list_agent_runtime_skills", "acp_list_agent_skills":
		return s.skills(command, a)
	case "pick_directory":
		return s.pickDirectory()
	case "get_home_directory", "list_directory_entries", "list_directory_with_files", "get_file_tree", "list_workspace_files", "read_file_preview", "read_file_for_edit", "read_file_base64", "read_workspace_file_base64", "save_file_content", "save_file_copy", "get_git_head":
		return s.files(command, a)
	case "computer_available":
		return s.computerAvailable(), nil
	case "acp_get_agent_status":
		for _, agent := range s.agents() {
			if agent["agent_type"] == a["agentType"] {
				return agent, nil
			}
		}
		return Row{"agent_type": a["agentType"], "available": false, "enabled": false, "installed_version": nil, "is_acp_adapter": true}, nil
	case "get_codeg_mcp_service_status":
		s.mu.Lock()
		active := 0
		for _, task := range s.tasks {
			if task.Status == "running" {
				active++
			}
		}
		companions, sessions := len(s.callers), len(s.connections)
		s.mu.Unlock()
		exe, _ := os.Executable()
		return Row{"state": "running", "listening": true, "socket_path": s.Base + "/api/codeg_mcp", "binary_path": exe, "tool_groups": []Row{{"key": "delegation", "enabled": true}, {"key": "feedback", "enabled": true}}, "companion_count": companions, "session_count": sessions, "active_delegations": active, "depth_limit": 4, "started_at": nil, "last_error": nil, "can_start": false}, nil
	case "acp_connect":
		return s.connect(a)
	case "acp_fork":
		return s.forkConversation(a)
	case "acp_set_config_option":
		s.mu.Lock()
		c := s.connections[text(a["connectionId"])]
		s.mu.Unlock()
		if c == nil {
			return nil, fmt.Errorf("连接不存在")
		}
		key := text(a["configId"])
		if err := s.Runtime.Configure(c.RuntimeID, key, a["value"]); err != nil {
			return nil, err
		}
		if key == "model" || key == "mode" {
			s.mu.Lock()
			if key == "model" {
				c.Options.Model = text(a["value"])
			} else {
				c.Options.Mode = text(a["value"])
			}
			s.mu.Unlock()
		}
		s.publishSession(c.ID)
		return nil, nil
	case "submit_session_feedback":
		return s.submitFeedback(a)
	case "acp_describe_agent_options":
		agent := s.pick(text(a["agentType"]))
		if agent == nil {
			return nil, fmt.Errorf("执行端不可用")
		}
		cwd, err := s.Resolve(text(a["workingDir"]))
		if err != nil {
			return nil, err
		}
		permission := "readonly"
		if agent.RealTool == "dsh" {
			permission = "workspace"
		}
		rid, err := s.Runtime.Connect(agent, acp.Options{SessionKey: "probe:" + id(), Permission: permission, Workdir: cwd})
		if err != nil {
			return nil, err
		}
		v := s.runtimeView(rid)
		return Row{"modes": modeWire(v["modes"]), "config_options": configWire(v), "available_commands": []Row{}, "unapplied_config_ids": []string{}, "prompt_capabilities": s.Runtime.PromptCapabilities(rid)}, nil
	case "acp_prompt":
		return nil, s.prompt(a)
	case "acp_cancel":
		return nil, s.cancel(text(a["connectionId"]))
	case "acp_disconnect":
		return nil, s.disconnect(text(a["connectionId"]))
	case "acp_list_connections":
		return s.connectionList(), nil
	case "acp_touch_connection":
		s.mu.Lock()
		_, ok := s.connections[text(a["connectionId"])]
		s.mu.Unlock()
		return ok, nil
	case "acp_get_session_snapshot":
		return s.snapshot(text(a["connectionId"])), nil
	case "acp_get_session_snapshot_by_conversation", "acp_find_connection_for_conversation":
		s.mu.Lock()
		cid := ""
		for _, c := range s.connections {
			// A warm pre-built connection has no conversation yet and can be
			// claimed (or released) at any moment; binding a conversation to it
			// here would hand out an idle session that is about to move.
			if c.ConversationID == number(a["conversationId"]) && !c.Warm {
				cid = c.ID
			}
		}
		s.mu.Unlock()
		if cid == "" {
			return nil, nil
		}
		if command == "acp_find_connection_for_conversation" {
			return Row{"connection_id": cid, "event_seq": s.snapshot(cid)["event_seq"]}, nil
		}
		return s.snapshot(cid), nil
	case "acp_respond_permission":
		choice := text(a["optionId"])
		return nil, s.Runtime.RespondPermission(text(a["requestId"]), &choice)
	case "acp_answer_question":
		s.answerQuestion(a)
		return nil, nil
	case "acp_respond_task_split":
		return s.respondTaskSplit(a)
	case "acp_set_mode":
		s.mu.Lock()
		c := s.connections[text(a["connectionId"])]
		s.mu.Unlock()
		if c == nil {
			return nil, fmt.Errorf("连接不存在")
		}
		if err := s.Runtime.SetMode(c.RuntimeID, text(a["modeId"])); err != nil {
			return nil, err
		}
		s.publishSession(c.ID)
		return nil, nil
	case "acp_goal_control":
		s.mu.Lock()
		c := s.connections[text(a["connectionId"])]
		s.mu.Unlock()
		if c == nil {
			return nil, fmt.Errorf("连接不存在")
		}
		return nil, s.Runtime.GoalControl(c.RuntimeID, text(a["action"]))
	case "acp_stop_async_task":
		s.mu.Lock()
		c := s.connections[text(a["connectionId"])]
		s.mu.Unlock()
		if c == nil {
			return nil, fmt.Errorf("连接不存在")
		}
		return s.Runtime.StopAsyncTask(c.RuntimeID, text(a["taskId"]))
	case "acp_update_agent_env":
		return s.updateAgentSettings("acp_update_agent_env", a)
	case "acp_update_agent_config":
		return s.updateAgentSettings("acp_update_agent_config", a)
	}
	st := s.store
	st.mu.Lock()
	defer st.mu.Unlock()
	if fallback, ok := settingDefaults[command]; ok {
		if saved, ok := st.Settings[command]; ok {
			return clone(saved), nil
		}
		return clone(fallback), nil
	}
	if getCommand, ok := settingUpdates[command]; ok {
		settings, ok := a["settings"].(map[string]any)
		if !ok {
			return nil, fmt.Errorf("settings 必须是对象")
		}
		st.Settings[getCommand] = settings
		return clone(settings), st.saveLocked()
	}
	// Registry-dispatched domains (store-locked). New feature domains register
	// here from their own file and no longer edit this switch.
	if fn, ok := lockedCommands[command]; ok {
		return fn(s, st, command, a)
	}
	return nil, fmt.Errorf("Go 移植尚未实现此接口：%s", command)
}
