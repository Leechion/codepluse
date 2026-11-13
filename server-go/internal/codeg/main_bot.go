package codeg

import (
	"crypto/sha256"
	"fmt"
	"harnesshub/internal/hub"
	"harnesshub/internal/providers"
	"strings"
)

func (s *Server) validateProfileProvider(tool, id string) (providers.Provider, error) {
	p, err := providers.Find(s.Root, id)
	if err != nil {
		return p, err
	}
	if p.Agent != tool {
		return p, fmt.Errorf("供应商不属于所选 Harness")
	}
	if !providers.IsAPI(p) {
		return p, fmt.Errorf("请选择有 API 密钥的供应商，官方订阅不能用作此配置")
	}
	if p.Enabled != nil && !*p.Enabled {
		return p, fmt.Errorf("此 API 供应商已停用")
	}
	return p, nil
}

func validateProfileModel(p providers.Provider, model string) error {
	for _, m := range p.Models {
		if m.ID == model && m.Enabled != nil && !*m.Enabled {
			return fmt.Errorf("此模型已停用，请选择其他模型")
		}
	}
	return nil
}

// A dedicated instance binds the main agent to a provider without mutating the
// harness's global credentials. Saved conversations keep their original agent.
func (s *Server) saveMainBotSettings(a Row) (any, error) {
	cfg, _ := a["settings"].(map[string]any)
	if cfg == nil {
		return nil, fmt.Errorf("settings 必须是对象")
	}
	wire := strings.TrimSpace(text(cfg["agent_type"]))
	providerID := strings.TrimSpace(text(cfg["provider_id"]))
	model := strings.TrimSpace(text(cfg["model"]))
	authMode := text(cfg["auth_mode"])
	if providerID != "" && authMode == "" {
		authMode = "api"
	}
	if providerID != "" || authMode == "subscription" {
		base := s.pick(wire)
		if base == nil {
			return nil, fmt.Errorf("所选 Harness 不可用")
		}
		tool := base.RealTool
		if tool == "" {
			tool = wireAgent(base)
		}
		if err := s.validateProfileAuth(tool, authMode, providerID); err != nil {
			return nil, err
		}
		if authMode == "api" {
			p, err := s.validateProfileProvider(tool, providerID)
			if err != nil {
				return nil, err
			}
			if model == "" {
				model = p.Model
			}
			if model == "" {
				return nil, fmt.Errorf("请选择模型")
			}
			if err := validateProfileModel(p, model); err != nil {
				return nil, err
			}
		}
		s.mu.Lock()
		identity := tool + "\x00" + providerID + "\x00" + model
		if authMode == "subscription" {
			identity += "\x00subscription"
		}
		digest := fmt.Sprintf("%x", sha256.Sum256([]byte(identity)))[:12]
		mainWire := "inst_main_" + base.RealTool + "_" + digest
		spec := instanceSpec{ID: "main_" + base.RealTool + "_" + digest, Wire: mainWire, Name: "主智能体 · " + base.Name + " · " + model, RealTool: base.RealTool, ProviderID: providerID, DefaultModel: model, AuthMode: authMode, BaseAgentType: wireAgent(base)}
		agent := s.instanceAgent(spec)
		if agent == nil {
			s.mu.Unlock()
			return nil, fmt.Errorf("主智能体配置无效")
		}
		if s.Hub.Agent(agent.ID) != nil {
			s.Hub.UpdateAgent(agent.ID, func(old *hub.Agent) {
				old.ProviderID = providerID
				old.DefaultModel = model
				old.Name = agent.Name
				old.AuthMode = authMode
			})
		} else {
			s.Hub.CreateAgent(agent)
		}
		err := s.saveInstancesLocked()
		s.mu.Unlock()
		if err != nil {
			return nil, err
		}
		cfg = Row{"agent_type": mainWire, "base_agent_type": wireAgent(base), "provider_id": providerID, "model": model, "auth_mode": authMode}
		s.retireAgentConnections(agent.ID)
	}
	s.store.mu.Lock()
	s.store.Settings["get_main_bot_settings"] = cfg
	err := s.store.saveLocked()
	s.store.mu.Unlock()
	if err != nil {
		return nil, err
	}
	s.emit("main-bot://changed", cfg)
	s.emit("app://acp-agents-updated", Row{})
	return cfg, nil
}
