package codeg

import (
	"fmt"
	"testing"
)

// seedTurns writes deterministic turns (role + timestamp) straight into the
// store so pagination and fingerprint tests are reproducible.
func seedTurns(s *Server, conversationID int, roles []string, millisBase int64) {
	turns := make([]Row, 0, len(roles))
	for i, role := range roles {
		turns = append(turns, Row{
			"id":        fmt.Sprintf("turn-%d", i),
			"role":      role,
			"blocks":    []Row{{"type": "text", "text": fmt.Sprintf("body %d", i)}},
			"timestamp": fmt.Sprintf("2026-01-01T00:%02d:00Z", i),
			// millisBase is unused for timestamps above; kept for future tests.
			"_millis": millisBase + int64(i),
		})
	}
	if err := s.store.replaceTurnsLocked(conversationID, turns); err != nil {
		panic(err)
	}
}

func TestGetFolderConversationTurns(t *testing.T) {
	s := testServer(t)

	command(t, s, "open_folder", Row{"path": s.Root})
	folders := command(t, s, "list_all_folder_details", Row{}).([]any)
	if len(folders) == 0 {
		t.Fatal("应已打开项目文件夹")
	}
	folderID := number(folders[0].(map[string]any)["id"])

	created := command(t, s, "create_conversation", Row{
		"folderId": folderID, "agentType": "codex", "title": "分页测试",
	})
	convID := number(created)

	roles := []string{"user", "assistant", "user", "assistant", "user", "assistant"}
	seedTurns(s, int(convID), roles, 0)

	// Page ending just before index 6 (the end) with limit 4 → [2,6).
	page := command(t, s, "get_folder_conversation_turns", Row{
		"conversationId": convID, "beforeIndex": 6, "limit": 4,
	}).(map[string]any)
	turns := page["turns"].([]any)
	if len(turns) != 4 {
		t.Fatalf("应返回 4 条 turn，实际 %d", len(turns))
	}
	if first := turns[0].(map[string]any)["id"]; first != "turn-2" {
		t.Fatalf("页首应为全局 turn-2，实际 %v", first)
	}
	if got := int(number(page["turns_offset"])); got != 2 {
		t.Fatalf("turns_offset 应为 2，实际 %v", page["turns_offset"])
	}
	if got := int(number(page["turns_total"])); got != 6 {
		t.Fatalf("turns_total 应为 6，实际 %v", page["turns_total"])
	}
	if got := int(number(page["assistant_turns_before_offset"])); got != 1 {
		t.Fatalf("assistant_turns_before_offset 应为 1，实际 %v", page["assistant_turns_before_offset"])
	}
	if got := int(number(page["user_turns_before_offset"])); got != 1 {
		t.Fatalf("user_turns_before_offset 应为 1，实际 %v", page["user_turns_before_offset"])
	}
	prefixHash, ok := page["prefix_hash"].(string)
	if !ok || len(prefixHash) != 16 {
		t.Fatalf("prefix_hash 应为 16 位 hex：%v", page["prefix_hash"])
	}
	boundaryHash := page["prefix_hash_before_index"].(string)
	if boundaryHash == prefixHash {
		t.Fatal("前缀指纹与边界指纹不应相同（前缀为空段 vs 全量段）")
	}

	// The prefix fingerprint must equal hashing the prefix slice (turns[0..2)).
	st := s.store
	st.mu.Lock()
	prefixTurns := st.turnsLocked(int(convID))[:2]
	expected := prefixFingerprint(prefixTurns, fnvSeed)
	st.mu.Unlock()
	if prefixHash != expected {
		t.Fatalf("prefix_hash 应等于前缀指纹 %s，实际 %s", expected, prefixHash)
	}

	// beforeIndex beyond total clamps to total.
	page2 := command(t, s, "get_folder_conversation_turns", Row{
		"conversationId": convID, "beforeIndex": 999, "limit": 3,
	}).(map[string]any)
	if got := int(number(page2["turns_offset"])); got != 3 {
		t.Fatalf("越界 beforeIndex 应钳制，turns_offset 应为 3，实际 %v", page2["turns_offset"])
	}
	if len(page2["turns"].([]any)) != 3 {
		t.Fatal("钳制后页应含 3 条 turn")
	}

	// limit larger than history → whole prefix from 0.
	page3 := command(t, s, "get_folder_conversation_turns", Row{
		"conversationId": convID, "beforeIndex": 6, "limit": 100,
	}).(map[string]any)
	if got := int(number(page3["turns_offset"])); got != 0 {
		t.Fatalf("大 limit 应从 0 开始，实际 %v", page3["turns_offset"])
	}
	if len(page3["turns"].([]any)) != 6 {
		t.Fatal("大 limit 页应含全部 6 条 turn")
	}

	// Empty prefix (offset 0) hashes to the FNV seed.
	if page3["prefix_hash"] != fmt.Sprintf("%016x", fnvSeed) {
		t.Fatalf("空前缀指纹应为 FNV seed：%v", page3["prefix_hash"])
	}

	// Unknown conversation errors.
	if _, err := command2(t, s, "get_folder_conversation_turns", Row{
		"conversationId": 424242, "beforeIndex": 0, "limit": 10,
	}); err == nil {
		t.Fatal("未知会话应报错")
	}
}
