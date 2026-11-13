package codeg

import (
	"encoding/json"
	"fmt"
	"harnesshub/internal/providers"
)

// Existing providers are updated at their original source. Keys never return to the UI.
func (s *Server) saveProviderConfiguration(a Row) (any, error) {
	p := providers.Provider{ID: text(a["id"]), Name: text(a["name"]), Agent: text(a["agent"]), URL: text(a["url"]), Model: text(a["model"]), Key: text(a["key"]), Protocol: text(a["protocol"]), Source: "codepluse"}
	if p.ID == "" {
		p.ID = text(a["sourceId"])
	} // older clients also update the existing source
	if enabled, ok := a["enabled"].(bool); ok {
		p.Enabled = &enabled
	}
	if raw, ok := a["models"]; ok {
		b, _ := json.Marshal(raw)
		if err := json.Unmarshal(b, &p.Models); err != nil {
			return nil, fmt.Errorf("模型列表格式无效")
		}
	}
	if p.Protocol == "" {
		p.Protocol = "openai"
		if p.Agent == "claude" {
			p.Protocol = "anthropic"
		}
		if p.Agent == "codex" {
			p.Protocol = "openai-responses"
		}
	}
	if (p.Agent == "codex" && p.Protocol != "openai-responses" && p.Protocol != "openai") || (p.Agent == "claude" && p.Protocol != "anthropic") || (p.Protocol != "openai" && p.Protocol != "openai-responses" && p.Protocol != "anthropic") {
		return nil, fmt.Errorf("该 Harness 不支持此 API 格式")
	}
	saved, err := providers.SaveConfiguration(s.Root, p)
	if err != nil {
		return nil, err
	}
	saved.HasKey = saved.Key != ""
	saved.API = providers.IsAPI(saved)
	saved.Key = ""
	saved.Env = nil
	saved.Native = nil
	return saved, nil
}
