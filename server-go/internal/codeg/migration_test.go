package codeg

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

func TestGitPanelUsesRealIndexAndSelectedCommit(t *testing.T) {
	s := testServer(t)
	root := s.Root
	git := func(args ...string) string {
		t.Helper()
		out, err := gitRun(root, args...)
		if err != nil {
			t.Fatal(err)
		}
		return strings.TrimSpace(out)
	}
	command(t, s, "git_init", Row{"path": root})
	initialBranch := git("symbolic-ref", "--short", "HEAD")
	git("config", "user.name", "Migration Test")
	git("config", "user.email", "migration@example.invalid")
	git("config", "commit.gpgsign", "false")
	git("config", "core.hooksPath", filepath.Join(root, ".git", "no-hooks"))
	for _, name := range []string{"selected.txt", "already-staged.txt", "中文 file.txt"} {
		if err := os.WriteFile(filepath.Join(root, name), []byte("before\n"), 0644); err != nil {
			t.Fatal(err)
		}
	}
	git("add", "--", "selected.txt", "already-staged.txt", "中文 file.txt")
	git("commit", "-m", "initial")
	_ = os.WriteFile(filepath.Join(root, "selected.txt"), []byte("after\n"), 0644)
	_ = os.WriteFile(filepath.Join(root, "already-staged.txt"), []byte("staged outside selection\n"), 0644)
	git("add", "--", "already-staged.txt")
	rows := command(t, s, "git_status", Row{"path": root}).([]any)
	if len(rows) != 2 {
		t.Fatal(rows)
	}
	diff := command(t, s, "git_diff", Row{"path": root, "file": "selected.txt"}).(string)
	if !strings.Contains(diff, "+after") {
		t.Fatal(diff)
	}
	command(t, s, "git_commit", Row{"path": root, "message": "only selected", "files": []string{"selected.txt"}})
	if git("show", "HEAD:already-staged.txt") != "before" {
		t.Fatal("unselected index entry was committed")
	}
	if git("diff", "--cached", "--name-only") != "already-staged.txt" {
		t.Fatal("unselected staging was lost")
	}
	history := command(t, s, "git_log", Row{"path": root, "withFiles": true}).(map[string]any)
	entries := history["entries"].([]any)
	if len(entries) != 2 || entries[0].(map[string]any)["message"] != "only selected" {
		t.Fatal(history)
	}
	command(t, s, "git_new_branch", Row{"path": root, "branchName": "migration/test"})
	branch := command(t, s, "get_git_branch", Row{"path": root})
	if branch != "migration/test" {
		t.Fatal(branch)
	}
	command(t, s, "git_checkout", Row{"path": root, "branchName": initialBranch})
	git("rm", "--", "selected.txt")
	command(t, s, "git_commit", Row{"path": root, "message": "delete selected", "files": []string{"selected.txt"}})
	if git("diff", "--cached", "--name-only") != "already-staged.txt" {
		t.Fatal("unselected index changed during deletion commit")
	}
	if _, err := s.gitCommand("git_reset", Row{"path": root, "commit": "--help", "mode": "soft"}); err == nil {
		t.Fatal("Git option accepted as revision")
	}
}

func TestCanvasTransactionsPersistAndBroadcastDenseRevisions(t *testing.T) {
	s := testServer(t)
	events := make(chan Row, 32)
	s.mu.Lock()
	s.subscribers[events] = true
	s.mu.Unlock()
	board := command(t, s, "canvas_create_board", Row{"input": Row{"name": "迁移验收"}}).(map[string]any)
	boardID := number(board["id"])
	create := func(kind string, extra Row) Row {
		args := Row{"boardId": boardID, "kind": kind, "x": 10, "y": 20, "width": 400, "height": 260}
		for k, v := range extra {
			args[k] = v
		}
		return command(t, s, "canvas_create_node", Row{"input": args}).(map[string]any)
	}
	note := create("note", Row{"content": "persistent note"})["value"].(map[string]any)
	command(t, s, "canvas_update_node", Row{"nodeId": note["id"], "patch": Row{"x": 125, "content": "saved note"}})
	s.store.mu.Lock()
	f := s.store.openFolderLocked(s.Root, "regular")
	conversation := s.store.createConversationLocked(number(f["id"]), "codex", "member", 0)
	s.store.mu.Unlock()
	pin := create("conversation", Row{"conversationId": conversation["id"]})["value"].(map[string]any)
	grouped := command(t, s, "canvas_group_into_region", Row{"input": Row{"boardId": boardID, "memberIds": []int{number(conversation["id"])}, "consumeNodeIds": []int{number(pin["id"])}, "x": 300, "y": 20, "width": 700, "height": 500}}).(map[string]any)["value"].(map[string]any)
	region := grouped["node"].(map[string]any)
	if len(grouped["deletedIds"].([]any)) != 1 {
		t.Fatal(grouped)
	}
	command(t, s, "canvas_detach_member", Row{"regionId": region["id"], "conversationId": conversation["id"], "x": 100, "y": 600})
	restored := reloadStore(t, s.store.file)
	if len(restored.CanvasNodes) != 3 {
		t.Fatal(restored.CanvasNodes)
	}
	saved := findRow(restored.CanvasNodes, number(note["id"]))
	if saved["content"] != "saved note" || float(saved["x"]) != 125 {
		t.Fatal(saved)
	}
	last := 0
	for len(events) > 0 {
		event := <-events
		if event["channel"] != "canvas://changed" {
			continue
		}
		n := number(event["payload"].(map[string]any)["revision"])
		if n != last+1 {
			t.Fatalf("revision gap %d -> %d", last, n)
		}
		last = n
	}
	if last != 5 {
		t.Fatalf("missing canvas events: %d", last)
	}
	before := s.store.CanvasRevision
	if _, err := s.canvasCommand("canvas_update_node", Row{"nodeId": note["id"], "patch": Row{"content": "must roll back", "memberAdd": conversation["id"]}}); err == nil {
		t.Fatal("note accepted region membership")
	}
	if findRow(s.store.CanvasNodes, number(note["id"]))["content"] != "saved note" || s.store.CanvasRevision != before {
		t.Fatal("failed mutation leaked into memory")
	}
}

func TestWorkspaceStreamFindsRealFileAndGitChanges(t *testing.T) {
	s := testServer(t)
	defer s.Close()
	first := command(t, s, "start_workspace_state_stream", Row{"rootPath": s.Root}).(map[string]any)
	if first["is_git_repo"] != false {
		t.Fatal(first)
	}
	if err := os.WriteFile(filepath.Join(s.Root, "new.txt"), []byte("real data"), 0644); err != nil {
		t.Fatal(err)
	}
	next := command(t, s, "get_workspace_snapshot", Row{"rootPath": s.Root}).(map[string]any)
	if number(next["seq"]) <= number(first["seq"]) {
		t.Fatal("snapshot did not advance")
	}
	b, _ := json.Marshal(next["tree_snapshot"])
	if !strings.Contains(string(b), "new.txt") {
		t.Fatal(string(b))
	}
	command(t, s, "stop_workspace_state_stream", Row{"rootPath": s.Root})
}

func TestTerminalIsRealInteractivePTYAndRetainsExitOutput(t *testing.T) {
	if runtime.GOOS != "windows" {
		t.Skip("Windows ConPTY integration")
	}
	s := testServer(t)
	defer s.Close()
	tid := command(t, s, "terminal_spawn", Row{"workingDir": s.Root, "shell": "cmd.exe", "terminalId": "real-terminal"}).(string)
	command(t, s, "terminal_resize", Row{"terminalId": tid, "cols": 100, "rows": 28})
	command(t, s, "terminal_write", Row{"terminalId": tid, "data": "echo CODEG_REAL_PTY> native-output.txt\r"})
	deadline := time.Now().Add(12 * time.Second)
	for {
		b, err := os.ReadFile(filepath.Join(s.Root, "native-output.txt"))
		if err == nil && strings.Contains(string(b), "CODEG_REAL_PTY") {
			break
		}
		if time.Now().After(deadline) {
			snap, _ := s.terminalCommand("terminal_snapshot", Row{"terminalId": tid})
			t.Fatalf("terminal did not execute input: %v", snap)
		}
		time.Sleep(50 * time.Millisecond)
	}
	command(t, s, "terminal_write", Row{"terminalId": tid, "data": "type native-output.txt & exit /b 7\r"})
	for {
		snap := command(t, s, "terminal_snapshot", Row{"terminalId": tid}).(map[string]any)
		if snap["alive"] == false {
			if number(snap["exit_code"]) != 7 || !strings.Contains(text(snap["data"]), "CODEG_REAL_PTY") {
				t.Fatal(snap)
			}
			if number(snap["seq"]) < 1 || snap["generation"] == nil {
				t.Fatal(snap)
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatal(fmt.Sprint("PTY did not exit: ", snap))
		}
		time.Sleep(50 * time.Millisecond)
	}
	command(t, s, "terminal_kill", Row{"terminalId": tid})
	missing := command(t, s, "terminal_snapshot", Row{"terminalId": tid}).(map[string]any)
	if missing["exists"] != false {
		t.Fatal(missing)
	}
}
