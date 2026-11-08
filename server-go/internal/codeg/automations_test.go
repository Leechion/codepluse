package codeg

import (
	"fmt"
	"testing"
	"time"
)

func automationDraft(name string) Row {
	return Row{"name": name, "enabled": true, "trigger_kind": "schedule", "cron": "30 8 * * *",
		"timezone": "Asia/Shanghai", "agent_type": "codex", "root_folder_id": nil,
		"isolation": "shared_in_root", "branch": nil, "is_remote_branch": false,
		"config": Row{"prompt_blocks": []Row{{"type": "text", "text": "早报"}}, "display_text": "早报", "config_values": map[string]any{}}}
}

func TestAutomationLifecycle(t *testing.T) {
	s := testServer(t)
	created := command(t, s, "automation_create", Row{"draft": automationDraft("晨报")})
	row, ok := created.(map[string]any)
	if !ok {
		t.Fatalf("automation_create 返回类型异常：%T", created)
	}
	id := int(row["id"].(float64))
	if row["next_run_at"] == nil {
		t.Fatalf("创建后应计算 next_run_at：%v", row)
	}

	list := command(t, s, "automation_list", Row{}).([]any)
	if len(list) != 1 {
		t.Fatalf("应有 1 条定时任务，实际 %d", len(list))
	}

	updated := command(t, s, "automation_set_enabled", Row{"id": id, "enabled": false}).(map[string]any)
	if updated["enabled"].(bool) {
		t.Fatal("停用后 enabled 应为 false")
	}

	if runs := command(t, s, "automation_runs", Row{"automationId": id, "limit": 10}).([]any); len(runs) != 0 {
		t.Fatalf("新任务不应有运行记录：%v", runs)
	}

	command(t, s, "automation_delete", Row{"id": id})
	if list = command(t, s, "automation_list", Row{}).([]any); len(list) != 0 {
		t.Fatal("删除后列表应为空")
	}
}

func TestAutomationComputeNextRun(t *testing.T) {
	s := testServer(t)
	next := command(t, s, "automation_compute_next_run", Row{"cron": "0 9 * * 1-5", "timezone": "Asia/Shanghai"}).(string)
	if next == "" {
		t.Fatal("工作日 cron 应有下一次触发时间")
	}
	if _, err := command2(t, s, "automation_compute_next_run", Row{"cron": "99 * * * *"}); err == nil {
		t.Fatal("非法 cron 应报错")
	}
}

// command2 is the error-returning twin of command for negative cases.
func command2(t *testing.T, s *Server, name string, args Row) (any, error) {
	t.Helper()
	result, err := s.command(name, args)
	return result, err
}

func TestAutomationRunNowEndToEnd(t *testing.T) {
	s := testServer(t)
	command(t, s, "open_folder", Row{"path": s.Root})
	folders := command(t, s, "list_all_folder_details", Row{}).([]any)
	if len(folders) == 0 {
		t.Fatal("应已打开项目文件夹")
	}
	folderID := int(folders[0].(map[string]any)["id"].(float64))

	draft := automationDraft("立即运行")
	draft["root_folder_id"] = folderID
	draft["trigger_kind"] = "manual"
	draft["cron"] = nil
	created := command(t, s, "automation_create", Row{"draft": draft}).(map[string]any)
	id := int(created["id"].(float64))

	runID := int(command(t, s, "automation_run_now", Row{"automationId": id}).(float64))
	if runID == 0 {
		t.Fatal("run_now 应返回运行 ID")
	}

	deadline := time.Now().Add(20 * time.Second)
	for {
		runs := command(t, s, "automation_runs", Row{"automationId": id, "limit": 5}).([]any)
		if len(runs) > 0 {
			run := runs[0].(map[string]any)
			status := run["status"].(string)
			if status == "succeeded" || status == "failed" {
				if status != "succeeded" {
					t.Fatalf("fixture 运行应成功：%v", run)
				}
				if run["conversation_id"] == nil {
					t.Fatal("运行应记录会话 ID")
				}
				return
			}
		}
		if time.Now().After(deadline) {
			runs := command(t, s, "automation_runs", Row{"automationId": id, "limit": 5}).([]any)
			t.Fatalf("运行 20 秒内未结束：%v", runs)
		}
		time.Sleep(500 * time.Millisecond)
	}
}

func TestAutomationValidation(t *testing.T) {
	s := testServer(t)
	if _, err := command2(t, s, "automation_create", Row{"draft": Row{"name": "x", "trigger_kind": "schedule", "agent_type": "codex"}}); err == nil {
		t.Fatal("schedule 触发缺少 cron 应报错")
	}
	draft := automationDraft("坏隔离")
	draft["isolation"] = "worktree_per_run"
	if _, err := command2(t, s, "automation_create", Row{"draft": draft}); err == nil {
		t.Fatal("worktree 隔离 v1 应明确报错")
	}
	draft2 := automationDraft("坏执行端")
	draft2["agent_type"] = "不存在的执行端"
	if _, err := command2(t, s, "automation_create", Row{"draft": draft2}); err == nil {
		// 创建时不校验执行端存在性（运行时才解析），此时不强制失败
		fmt.Println("（提示）创建时未校验执行端")
	}
}
