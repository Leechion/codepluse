package codeg

import "fmt"

func (s *Server) deleteConversation(conversationID int) error {
	if conversationID <= 0 {
		return fmt.Errorf("会话 ID 无效")
	}
	st := s.store
	st.mu.Lock()
	if st.conversationLocked(conversationID) == nil {
		st.mu.Unlock()
		return nil
	}
	removed := map[int]bool{conversationID: true}
	for changed := true; changed; {
		changed = false
		for _, row := range st.Conversations {
			id := number(row["id"])
			if !removed[id] && removed[number(row["parent_id"])] {
				removed[id] = true
				changed = true
			}
		}
	}
	// Retain a rollback snapshot until the persisted conversation transaction succeeds.
	oldRows, oldTurns, oldTabs, oldVersion := clone(st.Conversations), clone(st.Turns), clone(st.Tabs), st.TabsVersion
	kept := []Row{}
	parents := map[int]bool{}
	for _, row := range st.Conversations {
		id := number(row["id"])
		if removed[id] {
			parents[number(row["parent_id"])] = true
			delete(st.Turns, fmt.Sprint(id))
		} else {
			kept = append(kept, row)
		}
	}
	st.Conversations = kept
	tabs := []Row{}
	for _, tab := range st.Tabs {
		if !removed[number(tab["conversation_id"])] {
			tabs = append(tabs, tab)
		}
	}
	if len(tabs) != len(st.Tabs) {
		st.Tabs = tabs
		st.TabsVersion++
	}
	updates := []Row{}
	for parent := range parents {
		if row := st.conversationLocked(parent); row != nil {
			count := 0
			for _, child := range st.Conversations {
				if number(child["parent_id"]) == parent {
					count++
				}
			}
			row["child_count"] = count
			updates = append(updates, clone(row))
		}
	}
	if err := st.saveLocked(); err != nil {
		st.Conversations, st.Turns, st.Tabs, st.TabsVersion = oldRows, oldTurns, oldTabs, oldVersion
		st.mu.Unlock()
		return err
	}
	st.mu.Unlock()
	s.mu.Lock()
	runtimeIDs := []string{}
	for cid, c := range s.connections {
		if removed[c.ConversationID] {
			runtimeIDs = append(runtimeIDs, c.RuntimeID)
			delete(s.connections, cid)
			delete(s.callers, cid)
		}
	}
	for tid, d := range s.tasks {
		if removed[d.ConversationID] || removed[d.ParentConversationID] {
			d.Status = "canceled"
			delete(s.tasks, tid)
		}
	}
	// Deleted rows also act as tombstones when loading an older task snapshot.
	err := s.saveDelegationsLocked()
	s.mu.Unlock()
	for _, runtimeID := range runtimeIDs {
		_ = s.Runtime.Cancel(runtimeID)
	}
	for id := range removed {
		s.emit("conversation://changed", Row{"kind": "deleted", "id": id})
	}
	for _, row := range updates {
		s.changed(row)
	}
	return err
}
