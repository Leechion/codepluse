package codeg

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// Workspace domain: the store-locked sidebar/folder/conversation/tab/system
// reads and mutations. Migrated verbatim out of command()'s inline switch into
// the store-locked registry so backend data-CRUD work lives here (and in
// sibling domain files) rather than in server.go. Handlers run with st.mu held.

func init() {
	registerLocked(workspaceCommand,
		"get_sidebar_data", "get_stats",
		"list_open_folder_details", "load_folder_history", "list_all_folder_details",
		"list_folders", "open_folder", "open_folder_in_workspace",
		"get_folder", "open_folder_by_id",
		"update_folder_color", "update_folder_alias", "update_folder_default_agent",
		"remove_folder_from_workspace", "list_remote_connections",
		"get_system_settings", "load_system_settings",
		"update_system_settings", "save_system_settings",
		"list_all_conversations", "list_conversations", "list_child_conversations",
		"get_folder_conversation", "get_conversation", "get_folder_conversation_turns",
		"create_conversation", "create_chat_dir", "create_chat_conversation",
		"update_conversation_title", "update_conversation_status", "update_conversation_pinned",
		"list_opened_tabs", "save_opened_tabs",
	)
}

func workspaceCommand(s *Server, st *Store, command string, a Row) (any, error) {
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
		// Granting a folder makes it a read/write root (readableRoots), so leave
		// an audit trail: the local API guard is the boundary, this is the
		// record of when the boundary was moved.
		s.emit("audit://folder_open", Row{"folder_id": f["id"], "path": p})
		return clone(f), st.saveLocked()
	case "get_folder", "open_folder_by_id":
		f := st.folderLocked(number(a["folderId"]))
		if f == nil {
			return nil, fmt.Errorf("项目不存在")
		}
		return clone(f), nil
	case "update_folder_color", "update_folder_alias", "update_folder_default_agent":
		f := st.folderLocked(number(a["folderId"]))
		if f == nil {
			return nil, fmt.Errorf("项目不存在")
		}
		switch command {
		case "update_folder_color":
			f["color"] = text(a["color"])
		case "update_folder_alias":
			f["alias"] = a["alias"]
		case "update_folder_default_agent":
			f["default_agent_type"] = a["defaultAgentType"]
		}
		return clone(f), st.saveLocked()
	case "remove_folder_from_workspace":
		// Close the workspace: hide it from the open list but keep the row in
		// allFolders so by-id cwd lookups (worktrees, history) still resolve.
		f := st.folderLocked(number(a["folderId"]))
		if f == nil {
			return nil, fmt.Errorf("项目不存在")
		}
		f["open"] = false
		return nil, st.saveLocked()
	case "list_remote_connections":
		return []Row{}, nil
	case "get_system_settings", "load_system_settings":
		return clone(st.Settings), nil
	case "update_system_settings", "save_system_settings":
		for k, v := range a {
			st.Settings[k] = v
		}
		return nil, st.saveLocked()
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
	}
	return nil, fmt.Errorf("Go 移植尚未实现此接口：%s", command)
}
