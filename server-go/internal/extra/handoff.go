package extra

import (
	"fmt"
	"strings"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
	"harnesshub/internal/orch"
)

// HandoffSession moves an ongoing native conversation from one agent to
// another (codeg-style): instead of parsing each vendor's private on-disk
// history, the hub compresses the transcript it already holds and opens a
// fresh native session on the target agent.
func HandoffSession(rt *acp.Runtime, o *orch.Orchestrator, h *hub.Hub, chats *orch.ChatController, sessionID, toAgentID string) (map[string]any, error) {
	var source *acp.SessionRecord
	for _, rec := range rt.Records("") {
		if rec.ID == sessionID {
			source = rec
		}
	}
	if source == nil {
		return nil, fmt.Errorf("原生会话不存在")
	}
	if source.HandedOff {
		return nil, fmt.Errorf("此会话已移交过，不能再移交")
	}
	transcript := rt.ExportTranscript(sessionID)
	if strings.TrimSpace(transcript) == "" {
		return nil, fmt.Errorf("源会话没有可移交的执行记录")
	}
	target := h.Agent(toAgentID)
	if target == nil || !target.Enabled || !target.Real {
		return nil, fmt.Errorf("目标执行端未连接或已禁用")
	}
	if toAgentID == source.AgentID {
		return nil, fmt.Errorf("目标执行端就是当前会话的执行端")
	}
	fromAgent := h.Agent(source.AgentID)
	fromName := source.AgentID
	if fromAgent != nil {
		fromName = fromAgent.Name
	}

	managerID, managerModel := pickManager(source, chats, h, target)
	control := &orch.Mission{ID: "handoff_" + sessionID, ChatID: source.ChatID,
		ManagerID: managerID, ManagerModel: managerModel, Models: map[string]string{},
		Sensitivity: 1, Workdir: source.Cwd, Permission: "readonly"}
	briefing := ""
	res, err := o.CallForHandoff(control, managerID,
		"你是宰相。以下是一次 Harness 会话的执行纪要，请把它压缩为给接手执行端的移交简报。优先通过 submit_result 工具提交，字段：{\"summary\":\"简报\"}。\n"+
			"简报包含：正在进行的任务与目标、已完成的关键结论与证据、未完成事项与下一步、需要注意的约束。用中文，不超过 600 字。\n"+
			"会话纪要："+clip(transcript, 60000))
	if err == nil {
		if s, ok := res.Parsed["summary"].(string); ok {
			briefing = s
		} else if s, ok := res.Parsed["reply"].(string); ok {
			briefing = s
		} else {
			briefing = res.Output
		}
		briefing = clip(briefing, 6000)
	}
	opening := "你将接手一位同僚（" + fromName + "）此前负责的工作。" +
		"以下是移交简报与原始执行纪要；不要重复已完成的工作，基于这些上下文继续。\n\n" +
		"## 移交简报\n" + firstNonEmpty(briefing, "（简报生成失败，请直接依据纪要判断。）") + "\n\n## 原始执行纪要\n" + clip(transcript, 60000) +
		"\n\n请先用一两句话确认你已理解当前状态。"
	result := rt.Prompt(target, opening, acp.Options{SessionKey: "handoff:" + source.ID,
		Permission: source.Permission, Workdir: source.Cwd, ChatID: source.ChatID,
		Structured: false, Stage: "会话移交"})
	if !result.OK {
		return nil, fmt.Errorf("会话移交失败：%s", firstNonEmpty(result.Error, "目标执行端未确认接手"))
	}
	source.HandedOff = true
	source.HandedOffTo = toAgentID
	rt.SaveRecords()
	return map[string]any{"sessionId": source.ID, "handedOffTo": toAgentID, "briefing": briefing,
		"newSessionKey": "handoff:" + source.ID, "output": result.Output}, nil
}

func pickManager(source *acp.SessionRecord, chats *orch.ChatController, h *hub.Hub, fallback *hub.Agent) (string, string) {
	if source.ChatID != "" {
		if chat, err := chats.Get(source.ChatID); err == nil {
			a := h.Agent(chat.Config.ManagerID)
			if a != nil && a.Enabled {
				return a.ID, chat.Config.ManagerModel
			}
		}
	}
	for _, a := range h.Agents() {
		if a.Enabled && a.Real {
			return a.ID, ""
		}
	}
	if fallback != nil {
		return fallback.ID, ""
	}
	return "", ""
}

func clip(s string, n int) string {
	if len(s) > n {
		return s[:n]
	}
	return s
}

func firstNonEmpty(a, b string) string {
	if strings.TrimSpace(a) != "" {
		return a
	}
	return b
}
