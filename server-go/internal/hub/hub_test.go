package hub

import (
	"encoding/json"
	"testing"
)

func TestAuditChainVerifies(t *testing.T) {
	h := NewHub()
	h.AddAudit("user", "a", "t1", "allowed", 1, "one")
	h.AddAudit("user", "b", "t2", "allowed", 1, "two")
	h.AddAudit("hub", "c", "t3", "denied", 2, "three")
	res := h.VerifyAuditChain()
	if !res["ok"].(bool) || res["links"].(int) != 3 || res["broken"].(int) != 0 {
		t.Fatalf("审计链验证失败：%v", res)
	}
	// retro-edit breaks the edited link's own hash
	h.Audit[1].Detail = "tampered"
	res = h.VerifyAuditChain()
	if res["ok"].(bool) || res["broken"].(int) < 1 {
		t.Fatalf("篡改应破坏验证：%v", res)
	}
}

func TestSendMessageClearanceRejection(t *testing.T) {
	h := NewHub()
	h.CreateAgent(&Agent{ID: "low", Name: "Low", Clearance: 1})
	h.CreateAgent(&Agent{ID: "high", Name: "High", Clearance: 3})
	msg := h.SendMessage("low", "high", "secret", "collab.ask", 2, "")
	if msg.Status != "rejected" || msg.Auth == nil {
		t.Fatalf("低密级发送应被拒绝：%v", msg.Status)
	}
	msg = h.SendMessage("high", "low", "fine", "collab.ask", 2, "")
	if msg.Status != "sent" {
		t.Fatalf("高密级发送应放行：%v", msg.Status)
	}
	msg = h.SendMessage("ghost", "low", "hi", "", 1, "")
	if msg.Status != "rejected" {
		t.Fatalf("未知发送方应被拒绝")
	}
}

func TestTaskLifecycle(t *testing.T) {
	h := NewHub()
	t1 := h.SubmitTask(&Task{Title: "t", Prompt: "p", Real: true})
	if t1.Status != "queued" || t1.Execution.Mode != "real" {
		t.Fatalf("新任务状态错误：%s %s", t1.Status, t1.Execution.Mode)
	}
	h.StartTask(t1.ID, "a1", Execution{Tool: "claude"})
	if h.Task(t1.ID).Status != "running" {
		t.Fatalf("开始后应为 running")
	}
	h.CompleteTask(t1.ID, "done", "")
	if h.Task(t1.ID).Status != "succeeded" || h.Task(t1.ID).Progress != 100 {
		t.Fatalf("完成状态错误")
	}
	h.CancelTask(t1.ID)
	if h.Task(t1.ID).Status != "succeeded" {
		t.Fatalf("终态任务不可取消")
	}
	h.SetTaskMeta("task_none", "m", "s")
	b, _ := json.Marshal(h.Snapshot())
	if len(b) == 0 {
		t.Fatalf("快照为空")
	}
}

func TestSubscribeFanOut(t *testing.T) {
	h := NewHub()
	id, ch := h.Subscribe()
	defer h.Unsubscribe(id)
	h.Emit("test", map[string]any{"x": 1})
	evt := <-ch
	if evt.Type != "test" {
		t.Fatalf("应收到事件：%s", evt.Type)
	}
}
