package codeg

import "sort"

// P1 sidebar entities: conversation tags, assistants, quick messages. Each
// helper assumes the caller holds st.mu (they run inside the store-backed
// command switch); persistence is the caller's saveLocked() after the mutation.

// ---- conversation tags ----

func (s *Store) tagsLocked() []Row {
	out := []Row{}
	for _, t := range s.ConversationTags {
		out = append(out, clone(t))
	}
	sort.SliceStable(out, func(i, j int) bool {
		if number(out[i]["sort_order"]) != number(out[j]["sort_order"]) {
			return number(out[i]["sort_order"]) < number(out[j]["sort_order"])
		}
		return number(out[i]["id"]) < number(out[j]["id"])
	})
	return out
}

func (s *Store) tagLocked(n int) Row {
	for _, t := range s.ConversationTags {
		if number(t["id"]) == n {
			return t
		}
	}
	return nil
}

func (s *Store) createTagLocked(folderID any, name, color string) Row {
	t := Row{"id": s.allocate(), "folder_id": folderID, "name": name, "color": color, "sort_order": len(s.ConversationTags)}
	s.ConversationTags = append(s.ConversationTags, t)
	return clone(t)
}

func (s *Store) deleteTagLocked(tagID int) {
	kept := []Row{}
	for _, t := range s.ConversationTags {
		if number(t["id"]) != tagID {
			kept = append(kept, t)
		}
	}
	s.ConversationTags = kept
	// The tag comes off every conversation that carried it.
	for _, c := range s.Conversations {
		if existing, ok := c["tag_ids"].([]any); ok {
			nxt := []any{}
			for _, v := range existing {
				if number(v) != tagID {
					nxt = append(nxt, v)
				}
			}
			c["tag_ids"] = nxt
		}
	}
}

// reorderTagsLocked applies one scope's full ordering (tagIds first..last).
func (s *Store) reorderTagsLocked(tagIDs []int) {
	for i, id := range tagIDs {
		if t := s.tagLocked(id); t != nil {
			t["sort_order"] = i
		}
	}
}

// ---- quick messages ----

func (s *Store) quickMessagesLocked() []Row {
	out := []Row{}
	for _, m := range s.QuickMessages {
		out = append(out, clone(m))
	}
	sort.SliceStable(out, func(i, j int) bool {
		if number(out[i]["sort_order"]) != number(out[j]["sort_order"]) {
			return number(out[i]["sort_order"]) < number(out[j]["sort_order"])
		}
		return number(out[i]["id"]) < number(out[j]["id"])
	})
	return out
}

func (s *Store) quickMessageLocked(n int) Row {
	for _, m := range s.QuickMessages {
		if number(m["id"]) == n {
			return m
		}
	}
	return nil
}

func (s *Store) createQuickMessageLocked(title, content string) Row {
	now := timestamp()
	m := Row{"id": s.allocate(), "title": title, "content": content, "sort_order": len(s.QuickMessages), "created_at": now, "updated_at": now}
	s.QuickMessages = append(s.QuickMessages, m)
	return clone(m)
}

func (s *Store) updateQuickMessageLocked(id int, title, content any) Row {
	m := s.quickMessageLocked(id)
	if m == nil {
		return nil
	}
	if title != nil {
		m["title"] = text(title)
	}
	if content != nil {
		m["content"] = text(content)
	}
	m["updated_at"] = timestamp()
	return clone(m)
}

func (s *Store) deleteQuickMessageLocked(id int) {
	kept := []Row{}
	for _, m := range s.QuickMessages {
		if number(m["id"]) != id {
			kept = append(kept, m)
		}
	}
	s.QuickMessages = kept
}

func (s *Store) reorderQuickMessagesLocked(ids []int) {
	for i, id := range ids {
		if m := s.quickMessageLocked(id); m != nil {
			m["sort_order"] = i
		}
	}
}

// ---- assistants ----

func (s *Store) assistantsLocked() []Row {
	out := []Row{}
	for _, a := range s.Assistants {
		out = append(out, clone(a))
	}
	sort.SliceStable(out, func(i, j int) bool {
		if number(out[i]["sort_order"]) != number(out[j]["sort_order"]) {
			return number(out[i]["sort_order"]) < number(out[j]["sort_order"])
		}
		return number(out[i]["id"]) < number(out[j]["id"])
	})
	return out
}

func (s *Store) assistantLocked(n int) Row {
	for _, a := range s.Assistants {
		if number(a["id"]) == n {
			return a
		}
	}
	return nil
}

// flattenAgentConfig maps an AssistantDraft/Patch's agent_config onto the
// agent_type / agent_options / label_snapshot fields of AssistantInfo.
func flattenAgentConfig(row Row, src Row) {
	if cfg, ok := src["agent_config"].(map[string]any); ok {
		row["agent_type"] = cfg["agent_type"]
		row["agent_options"] = cfg["agent_options"]
		row["label_snapshot"] = cfg["label_snapshot"]
	}
}

func (s *Store) createAssistantLocked(draft Row) Row {
	now := timestamp()
	a := Row{
		"id": s.allocate(), "uid": id(), "name": text(draft["name"]), "description": text(draft["description"]),
		"avatar": draft["avatar"], "instructions": draft["instructions"], "enabled": draft["enabled"],
		"sort_order": len(s.Assistants), "revision": 1, "created_at": now, "updated_at": now, "deleted_at": nil,
	}
	flattenAgentConfig(a, draft)
	s.Assistants = append(s.Assistants, a)
	return clone(a)
}

func (s *Store) updateAssistantLocked(aid int, patch Row) Row {
	a := s.assistantLocked(aid)
	if a == nil {
		return nil
	}
	if v, ok := patch["name"]; ok {
		a["name"] = text(v)
	}
	if v, ok := patch["description"]; ok {
		a["description"] = text(v)
	}
	if v, ok := patch["avatar"]; ok {
		a["avatar"] = v
	}
	if v, ok := patch["instructions"]; ok {
		a["instructions"] = v
	}
	if v, ok := patch["enabled"]; ok {
		a["enabled"] = v
	}
	if _, ok := patch["agent_config"]; ok {
		flattenAgentConfig(a, patch)
	}
	a["revision"] = number(a["revision"]) + 1
	a["updated_at"] = timestamp()
	return clone(a)
}

func (s *Store) deleteAssistantLocked(aid int) Row {
	a := s.assistantLocked(aid)
	if a == nil {
		return nil
	}
	now := timestamp()
	a["deleted_at"] = now
	a["revision"] = number(a["revision"]) + 1
	a["updated_at"] = now
	return clone(a)
}

// reorderAssistantsLocked applies the full ordering and returns only the rows
// whose position changed (the assistant_reorder contract).
func (s *Store) reorderAssistantsLocked(ids []int) []Row {
	changed := []Row{}
	for i, idd := range ids {
		if a := s.assistantLocked(idd); a != nil && number(a["sort_order"]) != i {
			a["sort_order"] = i
			a["revision"] = number(a["revision"]) + 1
			a["updated_at"] = timestamp()
			changed = append(changed, clone(a))
		}
	}
	return changed
}
