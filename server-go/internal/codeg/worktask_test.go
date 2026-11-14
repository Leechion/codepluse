package codeg

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"
)

// taskOf re-rounds a command result through JSON into the typed wire shape
// (command() returns generic maps because responses are JSON).
func taskOf(t *testing.T, v any) *WorkTask {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	var out WorkTask
	if err := json.Unmarshal(b, &out); err != nil {
		t.Fatal(err)
	}
	return &out
}

func taskListOf(t *testing.T, v any) []*WorkTask {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	var out []*WorkTask
	if err := json.Unmarshal(b, &out); err != nil {
		t.Fatal(err)
	}
	return out
}

func eventListOf(t *testing.T, v any) []*WorkTaskEvent {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	var out []*WorkTaskEvent
	if err := json.Unmarshal(b, &out); err != nil {
		t.Fatal(err)
	}
	return out
}

func settingsOf(t *testing.T, v any) *WorkTaskFolderSettings {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	var out WorkTaskFolderSettings
	if err := json.Unmarshal(b, &out); err != nil {
		t.Fatal(err)
	}
	return &out
}

// initGitRepo turns the temp root into a one-commit git repository so the
// worktree path is exercised. The runtime data dir is ignored like the real
// project does, or the merge preflight would always refuse.
func initGitRepo(t *testing.T, root string) {
	t.Helper()
	_ = os.WriteFile(filepath.Join(root, ".gitignore"), []byte(".data/\n.worktrees/\n"), 0644)
	for _, args := range [][]string{{"init"}, {"config", "user.email", "test@test"}, {"config", "user.name", "test"}, {"add", "-A"}, {"commit", "-m", "init", "--allow-empty"}} {
		cmd := exec.Command("git", args...)
		cmd.Dir = root
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v: %s", args, err, out)
		}
	}
}

func waitForTaskStatus(t *testing.T, s *Server, id int, statuses ...string) *WorkTask {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for {
		task := taskOf(t, command(t, s, "work_task_get", Row{"id": id}))
		for _, want := range statuses {
			if task.Status == want {
				return task
			}
		}
		if time.Now().After(deadline) {
			t.Fatalf("任务 30 秒内未到达 %v，当前 %s", statuses, task.Status)
		}
		time.Sleep(500 * time.Millisecond)
	}
}

// waitForTaskSettledAfter waits for an asynchronous operation to start (the
// task leaves `from`) and then settle on one of `statuses`. waitForTaskStatus
// returns immediately when the current status is already in the accepted set,
// which races a merge goroutine that has not run yet.
func waitForTaskSettledAfter(t *testing.T, s *Server, id int, from string, statuses ...string) *WorkTask {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	left := false
	for {
		task := taskOf(t, command(t, s, "work_task_get", Row{"id": id}))
		if task.Status != from {
			left = true
		}
		if left {
			for _, want := range statuses {
				if task.Status == want {
					return task
				}
			}
		}
		if time.Now().After(deadline) {
			t.Fatalf("任务 30 秒内未从 %s 到达 %v，当前 %s", from, statuses, task.Status)
		}
		time.Sleep(200 * time.Millisecond)
	}
}

func TestWorkTaskBoardOperations(t *testing.T) {
	s := testServer(t)
	command(t, s, "open_folder", Row{"path": s.Root})
	folders := command(t, s, "list_all_folder_details", Row{}).([]any)
	folderID := int(folders[0].(map[string]any)["id"].(float64))
	created := command(t, s, "work_task_create", Row{"draft": Row{"folder_id": folderID, "title": "写报告", "config": Row{"display_text": "写一份报告", "config_values": map[string]any{}}}})
	task := taskOf(t, created)
	if task.Status != "todo" || task.Title != "写报告" {
		t.Fatalf("创建结果异常：%+v", task)
	}
	updated := taskOf(t, command(t, s, "work_task_update", Row{"id": task.ID, "draft": Row{"title": "写周报", "config": Row{"display_text": "写一份周报", "config_values": map[string]any{}}}}))
	if updated.Title != "写周报" {
		t.Fatalf("更新失败：%+v", updated)
	}
	command(t, s, "work_task_reorder", Row{"folderId": folderID, "orderedIds": []int{task.ID}})
	command(t, s, "work_task_archive", Row{"id": task.ID, "archived": true})
	command(t, s, "work_task_defer", Row{"id": task.ID, "deferred": true})
	got := taskOf(t, command(t, s, "work_task_get", Row{"id": task.ID}))
	if got.ArchivedAt == nil || got.DeferredAt == nil {
		t.Fatalf("归档/延后未生效：%+v", got)
	}
	events := eventListOf(t, command(t, s, "work_task_events", Row{"taskId": task.ID, "limit": 50}))
	if len(events) < 2 {
		t.Fatalf("事件应至少 2 条，实际 %d", len(events))
	}
	// Templates round-trip.
	tplRow := command(t, s, "work_task_template_save", Row{"draft": Row{"name": "周报", "title": "写周报", "config": Row{"display_text": "写一份周报", "config_values": map[string]any{}}}})
	tplBytes, _ := json.Marshal(tplRow)
	var tpl WorkTaskTemplate
	if err := json.Unmarshal(tplBytes, &tpl); err != nil {
		t.Fatal(err)
	}
	listBytes, _ := json.Marshal(command(t, s, "work_task_template_list", Row{}))
	var list []WorkTaskTemplate
	if err := json.Unmarshal(listBytes, &list); err != nil {
		t.Fatal(err)
	}
	if len(list) != 1 || list[0].Name != "周报" || list[0].ID != tpl.ID {
		t.Fatalf("模板保存/列表异常：%v", list)
	}
	// Settings round-trip with fallback to defaults.
	effective := settingsOf(t, command(t, s, "work_task_settings_effective", Row{"folderId": folderID}))
	if effective.MaxConcurrent != 1 {
		t.Fatalf("默认并发应为 1：%+v", effective)
	}
	command(t, s, "work_task_settings_set", Row{"folderId": folderID, "settings": Row{"max_concurrent": 3, "merge_strategy": "squash"}})
	effective = settingsOf(t, command(t, s, "work_task_settings_effective", Row{"folderId": folderID}))
	if effective.MaxConcurrent != 3 || effective.MergeStrategy != "squash" {
		t.Fatalf("设置未生效：%+v", effective)
	}
	ownBytes, _ := json.Marshal(command(t, s, "work_task_settings_get_own", Row{"folderId": folderID}))
	var own *WorkTaskFolderSettings
	if err := json.Unmarshal(ownBytes, &own); err != nil {
		t.Fatal(err)
	}
	if own == nil {
		t.Fatal("自有设置应存在")
	}
	command(t, s, "work_task_settings_delete", Row{"folderId": folderID})
	ownBytes, _ = json.Marshal(command(t, s, "work_task_settings_get_own", Row{"folderId": folderID}))
	if err := json.Unmarshal(ownBytes, &own); err != nil {
		t.Fatal(err)
	}
	if own != nil {
		t.Fatal("删除后自有设置应为空")
	}
	command(t, s, "work_task_delete", Row{"id": task.ID})
}

func TestWorkTaskLifecycleWithWorktree(t *testing.T) {
	s := testServer(t)
	command(t, s, "open_folder", Row{"path": s.Root})
	folders := command(t, s, "list_all_folder_details", Row{}).([]any)
	folderID := int(folders[0].(map[string]any)["id"].(float64))
	initGitRepo(t, s.Root)

	created := command(t, s, "work_task_create", Row{"draft": Row{"folder_id": folderID, "title": "隔离任务", "config": Row{"display_text": "在隔离工作树里完成任务", "config_values": map[string]any{}}}})
	task := taskOf(t, created)

	command(t, s, "work_task_start", Row{"id": task.ID})
	task = waitForTaskStatus(t, s, task.ID, "review", "failed")
	if task.Status != "review" {
		t.Fatalf("任务应到达评审，当前 %s（错误 %v）", task.Status, task.LastError)
	}
	// The worktree path itself is engine-internal; assert its observable effects.
	wtDir := filepath.Join(s.Root, ".worktrees", "task-"+itoa(task.ID))
	if _, err := os.Stat(wtDir); err != nil {
		t.Fatalf("隔离工作树目录应存在：%v", err)
	}
	if task.WorkBranch == nil || *task.WorkBranch != "hh/task-"+itoa(task.ID) {
		t.Fatalf("工作分支应记录：%v", task.WorkBranch)
	}
	if task.ConversationID == nil {
		t.Fatal("运行应记录会话 ID")
	}
	if task.WorkBranch == nil || *task.WorkBranch != "hh/task-"+itoa(task.ID) {
		t.Fatalf("工作分支应记录：%v", task.WorkBranch)
	}

	// Merge lands the branch and finishes the task.
	command(t, s, "work_task_merge", Row{"id": task.ID, "message": "合并测试", "deleteWorktree": false})
	task = waitForTaskSettledAfter(t, s, task.ID, "review", "done", "failed", "review")
	if task.Status != "done" {
		lastErr := ""
		if task.LastError != nil {
			lastErr = *task.LastError
		}
		t.Fatalf("合并后应完成，当前 %s（错误 %s）", task.Status, lastErr)
	}
	if task.CompletionKind == nil || *task.CompletionKind != "merged" {
		t.Fatalf("完成类型应为 merged：%v", task.CompletionKind)
	}
	if task.MergeCommit == nil {
		t.Fatal("合并应记录提交哈希")
	}
}

func TestWorkTaskCancelAndRequeue(t *testing.T) {
	s := testServer(t)
	command(t, s, "open_folder", Row{"path": s.Root})
	folders := command(t, s, "list_all_folder_details", Row{}).([]any)
	folderID := int(folders[0].(map[string]any)["id"].(float64))

	created := command(t, s, "work_task_create", Row{"draft": Row{"folder_id": folderID, "title": "取消测试", "config": Row{"display_text": "做点事", "config_values": map[string]any{}}}})
	task := taskOf(t, created)
	command(t, s, "work_task_start", Row{"id": task.ID})
	task = waitForTaskStatus(t, s, task.ID, "preparing", "running", "awaiting_input", "review")
	command(t, s, "work_task_cancel", Row{"id": task.ID, "reason": "不需要了"})
	task = waitForTaskStatus(t, s, task.ID, "canceled")
	if task.Status != "canceled" {
		t.Fatalf("取消失败：%s", task.Status)
	}
	// Requeue puts it back on the board.
	command(t, s, "work_task_requeue", Row{"id": task.ID, "note": "重新做"})
	task = taskOf(t, command(t, s, "work_task_get", Row{"id": task.ID}))
	if task.Status != "todo" {
		t.Fatalf("重新排队后应回待办，当前 %s", task.Status)
	}
	// Schedule it for later.
	command(t, s, "work_task_schedule", Row{"id": task.ID, "scheduledAt": "2099-01-01T00:00:00Z"})
	task = taskOf(t, command(t, s, "work_task_get", Row{"id": task.ID}))
	if task.ScheduledAt == nil {
		t.Fatal("计划时间应记录")
	}
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	digits := ""
	for n > 0 {
		digits = string(rune('0'+n%10)) + digits
		n /= 10
	}
	return digits
}
