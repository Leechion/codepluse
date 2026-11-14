package codeg

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestLegacyTurnsMigrateToSQLite(t *testing.T) {
	dir := t.TempDir()
	file := filepath.Join(dir, "codeg-workspace.json")
	legacy := Row{
		"next": 2,
		"folders": []Row{{"id": 1, "path": dir, "kind": "regular"}},
		"conversations": []Row{{"id": 1, "folder_id": 1, "agent_type": "codex", "title": "legacy", "status": "pending_review", "kind": "regular"}},
		"turns": map[string]any{"1": []Row{
			{"id": "t1", "role": "user", "blocks": []Row{{"type": "text", "text": "hi"}}, "timestamp": "2026-01-01T00:00:00Z"},
			{"id": "t2", "role": "assistant", "blocks": []Row{{"type": "text", "text": "yo"}}, "timestamp": "2026-01-01T00:00:01Z"},
			{"id": "t3", "role": "user", "blocks": []Row{{"type": "text", "text": "again"}}, "timestamp": "2026-01-01T00:00:02Z"},
		}},
	}
	raw, _ := json.Marshal(legacy)
	if err := os.WriteFile(file, raw, 0o600); err != nil {
		t.Fatal(err)
	}

	s, err := newStore(file)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = s.Close() }()

	if n := s.turnCountLocked(1); n != 3 {
		t.Fatalf("migrated %d turns, want 3", n)
	}
	if s.Turns != nil {
		t.Fatal("legacy turns map should be cleared after import")
	}
	turns := s.turnsLocked(1)
	if len(turns) != 3 || text(turns[1]["id"]) != "t2" {
		t.Fatalf("unexpected migrated turns: %v", turns)
	}

	// The JSON document must no longer carry the history blob.
	after, err := os.ReadFile(file)
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(after, []byte(`"turns"`)) {
		t.Fatal("workspace JSON still contains the turns map after migration")
	}
	backups, _ := filepath.Glob(file + ".bak-*")
	if len(backups) != 1 {
		t.Fatalf("expected one backup, got %v", backups)
	}

	// Cumulative columns must reproduce the exact prefix fingerprint.
	if st, ok := s.prefixAtLocked(1, 1); !ok || st.hash != prefixFingerprint(turns[:2], fnvSeed) {
		t.Fatalf("prefix hash mismatch: %+v", st)
	}
	if st, ok := s.prefixAtLocked(1, 1); !ok || st.assistant != 1 || st.user != 1 {
		t.Fatalf("prefix counts mismatch: %+v", st)
	}
	page := s.conversationTurnsPageLocked(1, 3, 1)
	if page["prefix_hash"] != prefixFingerprint(turns[:2], fnvSeed) {
		t.Fatalf("page prefix hash %v", page["prefix_hash"])
	}
	if number(page["assistant_turns_before_offset"]) != 1 || number(page["user_turns_before_offset"]) != 1 {
		t.Fatalf("page prefix counts: %v / %v", page["assistant_turns_before_offset"], page["user_turns_before_offset"])
	}

	// A second open must not re-import (which would duplicate rows).
	again, err := newStore(file)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = again.Close() }()
	if n := again.turnCountLocked(1); n != 3 {
		t.Fatalf("re-open migrated %d turns, want 3", n)
	}

	// Appending keeps message_count in step with the database.
	if _, err := s.appendTurnLocked(1, "assistant", "done", nil); err != nil {
		t.Fatal(err)
	}
	if got := number(s.conversationLocked(1)["message_count"]); got != 4 {
		t.Fatalf("message_count = %d, want 4", got)
	}
}

func TestTurnStoreLazyAndClosed(t *testing.T) {
	dir := t.TempDir()
	file := filepath.Join(dir, "workspace.json")
	s, err := newStore(file)
	if err != nil {
		t.Fatal(err)
	}
	// No database file until something actually needs it.
	if _, err := os.Stat(filepath.Join(dir, "codeg.db")); !os.IsNotExist(err) {
		t.Fatalf("database created eagerly: %v", err)
	}
	if n := s.turnCountLocked(1); n != 0 {
		t.Fatalf("empty store reported %d turns", n)
	}
	if _, err := os.Stat(filepath.Join(dir, "codeg.db")); err != nil {
		t.Fatalf("database not created on first use: %v", err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	if err := s.insertTurnLocked(1, Row{"id": "x", "role": "user", "blocks": []Row{}}); err == nil {
		t.Fatal("closed store accepted a turn")
	}
}
