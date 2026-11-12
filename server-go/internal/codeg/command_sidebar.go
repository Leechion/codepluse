package codeg

import "fmt"

// Sidebar domain: folder groups, conversation tags, the branch-tag toggle,
// assistants and quick messages. Migrated out of command()'s inline switch into
// the store-locked registry so this CRUD surface lives in one file and grows
// without editing server.go. Each handler runs with st.mu held (see
// command_registry.go), so the *Locked store helpers and saveLocked() are safe.

func init() {
	registerLocked(sidebarCommand,
		"list_folder_groups", "create_folder_group", "update_folder_group",
		"delete_folder_group", "set_folder_group", "apply_sidebar_layout",
		"list_conversation_tags", "create_conversation_tag", "update_conversation_tag",
		"delete_conversation_tag", "reorder_conversation_tags",
		"get_conversation_branch_tag", "update_conversation_branch_tag",
		"update_conversation_tags",
		"assistant_list", "assistant_create", "assistant_update",
		"assistant_delete", "assistant_reorder",
		"quick_messages_list", "quick_messages_create", "quick_messages_update",
		"quick_messages_delete", "quick_messages_reorder",
	)
}

func sidebarCommand(s *Server, st *Store, command string, a Row) (any, error) {
	switch command {
	case "list_folder_groups":
		return st.groupsLocked(), nil
	case "create_folder_group":
		g := Row{"id": st.allocate(), "name": text(a["name"]), "color": groupColor(a["color"]), "sort_order": len(st.FolderGroups)}
		st.FolderGroups = append(st.FolderGroups, g)
		return clone(g), st.saveLocked()
	case "update_folder_group":
		g := st.groupLocked(number(a["groupId"]))
		if g == nil {
			return nil, fmt.Errorf("分组不存在")
		}
		if n, ok := a["name"]; ok {
			g["name"] = text(n)
		}
		if c, ok := a["color"]; ok {
			g["color"] = text(c)
		}
		return clone(g), st.saveLocked()
	case "delete_folder_group":
		gid := number(a["groupId"])
		kept := []Row{}
		for _, g := range st.FolderGroups {
			if number(g["id"]) != gid {
				kept = append(kept, g)
			}
		}
		st.FolderGroups = kept
		for _, f := range st.Folders {
			if number(f["group_id"]) == gid {
				f["group_id"] = nil
			}
		}
		return nil, st.saveLocked()
	case "set_folder_group":
		f := st.folderLocked(number(a["folderId"]))
		if f == nil {
			return nil, fmt.Errorf("项目不存在")
		}
		if a["groupId"] == nil {
			f["group_id"] = nil
		} else {
			f["group_id"] = number(a["groupId"])
		}
		return nil, st.saveLocked()
	case "apply_sidebar_layout":
		entries, _ := a["entries"].([]any)
		for i, e := range entries {
			entry, _ := e.(map[string]any)
			if entry == nil {
				continue
			}
			eid := number(entry["id"])
			if text(entry["kind"]) == "group" {
				if g := st.groupLocked(eid); g != nil {
					g["sort_order"] = i
				}
				continue
			}
			if f := st.folderLocked(eid); f != nil {
				f["sort_order"] = i
				if entry["groupId"] == nil {
					f["group_id"] = nil
				} else {
					f["group_id"] = number(entry["groupId"])
				}
			}
		}
		return nil, st.saveLocked()
	case "list_conversation_tags":
		return st.tagsLocked(), nil
	case "create_conversation_tag":
		return st.createTagLocked(a["folderId"], text(a["name"]), text(a["color"])), st.saveLocked()
	case "update_conversation_tag":
		t := st.tagLocked(number(a["tagId"]))
		if t == nil {
			return nil, fmt.Errorf("标签不存在")
		}
		if v, ok := a["name"]; ok {
			t["name"] = text(v)
		}
		if v, ok := a["color"]; ok {
			t["color"] = text(v)
		}
		return clone(t), st.saveLocked()
	case "delete_conversation_tag":
		st.deleteTagLocked(number(a["tagId"]))
		return nil, st.saveLocked()
	case "reorder_conversation_tags":
		st.reorderTagsLocked(ids(a["tagIds"]))
		return nil, st.saveLocked()
	case "get_conversation_branch_tag":
		if saved, ok := st.Settings["conversation_branch_tag"]; ok {
			return clone(saved), nil
		}
		return Row{"enabled": false, "color": "default"}, nil
	case "update_conversation_branch_tag":
		color := text(a["color"])
		if color == "" {
			color = "default"
		}
		setting := Row{"enabled": a["enabled"] == true, "color": color}
		st.Settings["conversation_branch_tag"] = setting
		return clone(setting), st.saveLocked()
	case "update_conversation_tags":
		row := st.conversationLocked(number(a["conversationId"]))
		if row == nil {
			return nil, fmt.Errorf("会话不存在")
		}
		// Delta merge: keep existing order, drop `remove`, append new `add`.
		// Two windows toggling different tags both land.
		cur, _ := row["tag_ids"].([]any)
		remove := map[int]bool{}
		for _, v := range ids(a["remove"]) {
			remove[v] = true
		}
		present := map[int]bool{}
		merged := []any{}
		for _, v := range cur {
			n := number(v)
			if !remove[n] {
				merged = append(merged, n)
				present[n] = true
			}
		}
		for _, v := range ids(a["add"]) {
			if !present[v] && !remove[v] {
				merged = append(merged, v)
				present[v] = true
			}
		}
		row["tag_ids"] = merged
		s.changed(row)
		return clone(row), st.saveLocked()
	case "assistant_list":
		return st.assistantsLocked(), nil
	case "assistant_create":
		draft, _ := a["draft"].(map[string]any)
		return st.createAssistantLocked(draft), st.saveLocked()
	case "assistant_update":
		patch, _ := a["patch"].(map[string]any)
		res := st.updateAssistantLocked(number(a["id"]), patch)
		if res == nil {
			return nil, fmt.Errorf("助手不存在")
		}
		return res, st.saveLocked()
	case "assistant_delete":
		res := st.deleteAssistantLocked(number(a["id"]))
		if res == nil {
			return nil, fmt.Errorf("助手不存在")
		}
		return res, st.saveLocked()
	case "assistant_reorder":
		return st.reorderAssistantsLocked(ids(a["ids"])), st.saveLocked()
	case "quick_messages_list":
		return st.quickMessagesLocked(), nil
	case "quick_messages_create":
		return st.createQuickMessageLocked(text(a["title"]), text(a["content"])), st.saveLocked()
	case "quick_messages_update":
		res := st.updateQuickMessageLocked(number(a["id"]), a["title"], a["content"])
		if res == nil {
			return nil, fmt.Errorf("快捷消息不存在")
		}
		return res, st.saveLocked()
	case "quick_messages_delete":
		st.deleteQuickMessageLocked(number(a["id"]))
		return nil, st.saveLocked()
	case "quick_messages_reorder":
		st.reorderQuickMessagesLocked(ids(a["ids"]))
		return nil, st.saveLocked()
	}
	return nil, fmt.Errorf("Go 移植尚未实现此接口：%s", command)
}
