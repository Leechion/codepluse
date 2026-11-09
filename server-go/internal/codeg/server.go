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
	"harnesshub/internal/providers"
)

type Server struct {
	Runtime     *acp.Runtime
	Hub         *hub.Hub
	Root, Base  string
	Resolve     func(string) (string, error)
	store       *Store
	mu          sync.Mutex
	connections map[string]*Connection
	subscribers map[chan Row]bool
	tasks       map[string]*Delegation
	callers     map[string]*Caller
	terminalMu  sync.Mutex
	terminals   map[string]*TerminalSession
	watchMu     sync.Mutex
	watches     map[string]*WorkspaceWatch
	auto        *autoStore
	forge       *forgeStore
	tasks2      *taskStore
	browser     *browserManager
	computer    *computerState
}

func New(root, base string, rt *acp.Runtime, h *hub.Hub, resolve func(string) (string, error)) (*Server, error) {
	st, err := newStore(filepath.Join(root, ".data", "codeg-workspace.json"))
	if err != nil {
		return nil, err
	}
	s := &Server{Runtime: rt, Hub: h, Root: root, Base: base, Resolve: resolve, store: st, connections: map[string]*Connection{}, subscribers: map[chan Row]bool{}, tasks: map[string]*Delegation{}, callers: map[string]*Caller{}, terminals: map[string]*TerminalSession{}}
	s.auto = newAutoStore(root)
	s.forge = newForgeStore(root)
	s.tasks2 = newTaskStore(root)
	s.tasks2.onChange = func(taskID int) { s.emit("task://changed", Row{"id": taskID}) }
	s.browser = &browserManager{console: map[string][]Row{}}
	s.computer = &computerState{}
	if err := s.loadDelegations(); err != nil {
		return nil, err
	}
	rt.OnUpdate = s.nativeUpdate
	rt.ExtraMCP = s.extraMCP
	rt.AllowInternalTool = s.allowInternalTool
	return s, nil
}
func (s *Server) Start() { go s.permissionsLoop() }
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
	for _, a := range s.Hub.Agents() {
		if !a.Real {
			continue
		}
		out = append(out, Row{"agent_type": wireAgent(a), "name": a.Name, "description": "ACP · " + a.Name, "available": a.Enabled, "enabled": a.Enabled, "registry_id": wireAgent(a), "registry_version": nil, "skills_capable": true, "supports_custom_version": false, "distribution_type": "system", "is_acp_adapter": a.RealTool != "", "custom_source": nil, "installed_version": s.installedVersion(a), "sort_order": len(out), "env": Row{}, "config_json": nil, "host_tools_agent_mode": true})
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
	switch command {
	case "codepluse_providers_list":
		return providers.Public(s.Root)
	case "codepluse_provider_save":
		return nil, providers.Save(s.Root, providers.Provider{ID: text(a["id"]), Name: text(a["name"]), Agent: text(a["agent"]), URL: text(a["url"]), Model: text(a["model"]), Key: text(a["key"])})
	case "codepluse_provider_activate":
		return nil, providers.Activate(s.Root, text(a["agent"]), text(a["id"]))
	case "codepluse_provider_delete":
		return nil, providers.Delete(s.Root, text(a["id"]))
	case "acp_list_agents":
		return s.agents(), nil
	case "experts_list", "science_list", "experts_read_content", "science_read_content", "experts_list_all_install_statuses", "science_list_all_install_statuses", "officecli_skill_list_all_install_statuses", "acp_list_agent_runtime_skills", "acp_list_agent_skills":
		return s.skills(command, a)
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
			if c.ConversationID == number(a["conversationId"]) {
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
	switch command {
	case "get_sidebar_data":
		return Row{"folders": st.regularFoldersLocked(), "stats": st.statsLocked()}, nil
	case "get_stats":
		return st.statsLocked(), nil
	case "list_open_folder_details", "load_folder_history":
		return st.regularFoldersLocked(), nil
	case "list_all_folder_details":
		return clone(st.Folders), nil
	case "list_folders":
		out := []Row{}
		for _, f := range st.Folders {
			out = append(out, Row{"path": f["path"], "name": f["name"], "agent_types": []string{}, "conversation_count": 0})
		}
		return out, nil
	case "open_folder", "open_folder_in_workspace":
		p, err := s.Resolve(text(a["path"]))
		if err != nil {
			return nil, err
		}
		if info, err := os.Stat(p); err != nil || !info.IsDir() {
			return nil, fmt.Errorf("项目目录不存在")
		}
		f := st.openFolderLocked(p, "regular")
		return clone(f), st.saveLocked()
	case "get_folder", "open_folder_by_id":
		f := st.folderLocked(number(a["folderId"]))
		if f == nil {
			return nil, fmt.Errorf("项目不存在")
		}
		return clone(f), nil
	case "list_all_conversations", "list_conversations", "list_child_conversations":
		out := []Row{}
		for i := len(st.Conversations) - 1; i >= 0; i-- {
			row := st.Conversations[i]
			if folders := ids(a["folderIds"]); len(folders) > 0 && !hasID(folders, number(row["folder_id"])) {
				continue
			}
			if status := text(a["status"]); status != "" && row["status"] != status {
				continue
			}
			if agent := text(a["agentType"]); agent != "" && row["agent_type"] != agent {
				continue
			}
			if path := text(a["folderPath"]); path != "" {
				f := st.folderLocked(number(row["folder_id"]))
				if f == nil || f["path"] != path {
					continue
				}
			}
			if command == "list_child_conversations" && number(row["parent_id"]) != number(a["parentConversationId"]) {
				continue
			}
			if command == "list_all_conversations" && number(row["parent_id"]) > 0 && a["includeChildren"] != true {
				continue
			}
			if search := text(a["search"]); search != "" && !strings.Contains(strings.ToLower(text(row["title"])), strings.ToLower(search)) {
				continue
			}
			out = append(out, clone(row))
		}
		return out, nil
	case "get_folder_conversation", "get_conversation":
		n := number(a["conversationId"])
		row := st.conversationLocked(n)
		if row == nil {
			return nil, fmt.Errorf("会话不存在")
		}
		turns := st.turnsWithDelegationsLocked(n)
		if turns == nil {
			turns = []Row{}
		}
		return Row{"summary": clone(row), "turns": clone(turns), "turns_offset": 0, "turns_total": len(turns)}, nil
	case "get_folder_conversation_turns":
		n := number(a["conversationId"])
		if st.conversationLocked(n) == nil {
			return nil, fmt.Errorf("会话不存在")
		}
		return st.conversationTurnsPageLocked(n, number(a["beforeIndex"]), number(a["limit"])), nil
	case "create_conversation":
		folder := number(a["folderId"])
		if st.folderLocked(folder) == nil {
			return nil, fmt.Errorf("项目不存在")
		}
		row := st.createConversationLocked(folder, text(a["agentType"]), text(a["title"]), 0)
		s.changed(row)
		return row["id"], st.saveLocked()
	case "create_chat_dir":
		dir, err := os.MkdirTemp(filepath.Join(s.Root, ".data"), "chat-")
		return Row{"path": dir}, err
	case "create_chat_conversation":
		dir := text(a["existingDir"])
		var err error
		if dir == "" {
			dir, err = os.MkdirTemp(filepath.Join(s.Root, ".data"), "chat-")
		} else {
			dir, err = s.Resolve(dir)
		}
		if err != nil {
			return nil, err
		}
		f := st.openFolderLocked(dir, "chat")
		row := st.createConversationLocked(number(f["id"]), text(a["agentType"]), text(a["title"]), 0)
		s.changed(row)
		return Row{"conversationId": row["id"], "folderId": f["id"], "folder": clone(f)}, st.saveLocked()
	case "update_conversation_title", "update_conversation_status", "update_conversation_pinned":
		row := st.conversationLocked(number(a["conversationId"]))
		if row == nil {
			return nil, fmt.Errorf("会话不存在")
		}
		switch command {
		case "update_conversation_title":
			row["title"] = a["title"]
			row["title_locked"] = true
		case "update_conversation_status":
			row["status"] = a["status"]
		case "update_conversation_pinned":
			row["pinned_at"] = nil
			if a["pinned"] == true {
				row["pinned_at"] = timestamp()
			}
		}
		s.changed(row)
		return nil, st.saveLocked()
	case "list_opened_tabs":
		return Row{"items": clone(st.Tabs), "version": st.TabsVersion}, nil
	case "save_opened_tabs":
		accepted := number(a["expectedVersion"]) == st.TabsVersion
		if accepted {
			b, _ := json.Marshal(a["items"])
			_ = json.Unmarshal(b, &st.Tabs)
			if st.Tabs == nil {
				st.Tabs = []Row{}
			}
			st.TabsVersion++
			if err := st.saveLocked(); err != nil {
				return nil, err
			}
		}
		return Row{"accepted": accepted, "version": st.TabsVersion, "tabs": clone(st.Tabs)}, nil
	case "list_folder_groups", "list_conversation_tags", "list_assistants", "assistant_list", "list_remote_connections":
		return []Row{}, nil
	case "get_system_settings", "load_system_settings":
		return clone(st.Settings), nil
	case "update_system_settings", "save_system_settings":
		for k, v := range a {
			st.Settings[k] = v
		}
		return nil, st.saveLocked()
	}
	return nil, fmt.Errorf("Go 移植尚未实现此接口：%s", command)
}
