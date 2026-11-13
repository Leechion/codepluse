package codeg

import (
	"fmt"
	"strings"
)

// pinnableTool reports whether a harness row can carry provider/model
// profiles. Only the three tools whose spawn env the instance layer rewrites
// can; a custom ACP agent manages its own credentials, so no profile will ever
// exist for it.
func pinnableTool(row Row) bool {
	switch text(row["real_tool"]) {
	case "claude", "codex", "dsh":
		return true
	}
	return false
}

// Once API profiles are configured the manager sees those precise provider /
// model targets, rather than an unpinned harness that could use a subscription.
func (s *Server) delegationAgents() []Row {
	all := s.agents()
	profiles, standalone := []Row{}, []Row{}
	for _, row := range all {
		if row["instance"] == true {
			if text(row["provider_id"]) != "" && !strings.HasPrefix(text(row["agent_type"]), "inst_main_") {
				profiles = append(profiles, row)
			}
			continue
		}
		// Harnesses that can never have a profile are their own delegation
		// target: waiting for a configuration that does not exist would hide
		// every agent the user adds in Settings → Agents from the
		// collaboration side. Disabled or not-yet-installed ones stay out,
		// because a delegate to them can only fail.
		if !pinnableTool(row) && row["enabled"] == true && row["available"] == true {
			standalone = append(standalone, row)
		}
	}
	if len(profiles) > 0 {
		return append(profiles, standalone...)
	}
	s.store.mu.Lock()
	main, _ := s.store.Settings["get_main_bot_settings"].(map[string]any)
	strict := text(main["provider_id"]) != ""
	s.store.mu.Unlock()
	if strict {
		return standalone
	}
	return all
}

func (s *Server) delegationProfile(wire, model string) (string, error) {
	all := s.delegationAgents()
	matches := []string{}
	// A named profile is exact; an unrelated model must select another profile.
	for _, row := range all {
		if text(row["agent_type"]) == wire {
			if row["instance"] == true && text(row["provider_id"]) != "" && model != "" && model != text(row["default_model"]) {
				return "", fmt.Errorf("模型与已保存配置不一致，请从 list_agents 选择对应的供应商/模型组合")
			}
			return wire, nil
		}
	}
	base := s.pick(wire)
	if base != nil {
		for _, row := range all {
			if text(row["real_tool"]) == base.RealTool && (model == "" || model == text(row["default_model"])) {
				matches = append(matches, text(row["agent_type"]))
			}
		}
	}
	if len(matches) == 1 {
		return matches[0], nil
	}
	if len(matches) > 1 {
		return "", fmt.Errorf("该 Harness 有多个供应商/模型配置，请使用 list_agents 返回的 agent_type 指定目标")
	}
	return "", fmt.Errorf("未找到可委派的供应商/模型配置，请在协作设置中添加子智能体")
}
