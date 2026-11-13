package codeg

import (
	"fmt"
	"harnesshub/internal/accountchannels"
	"harnesshub/internal/hub"
	"harnesshub/internal/providers"
	"path/filepath"
	"strings"
)

func (s *Server) profileBase(tool, base string) *hub.Agent {
	if base != "" {
		return s.pick(base)
	}
	wire := baseWire(tool)
	if wire == "" {
		wire = tool
	}
	return s.pick(wire)
}

func (s *Server) validateProfileAuth(tool, mode, pin string) error {
	if mode != "" && mode != "api" && mode != "subscription" {
		return fmt.Errorf("连接方式无效")
	}
	if mode == "subscription" {
		if pin != "" {
			return fmt.Errorf("官网订阅不能同时指定 API 供应商")
		}
		if !providers.SupportsSubscription(tool) {
			return fmt.Errorf("此 Harness 没有已接入的官网订阅登录方式，请使用其原生 API 配置")
		}
	}
	if mode == "api" && pin == "" {
		return fmt.Errorf("请选择 API 供应商")
	}
	return nil
}

func (s *Server) connectionCatalog() (any, error) {
	data, err := providers.Public(s.Root)
	if err != nil {
		return nil, err
	}
	rows := []Row{}
	var accountRows []accountchannels.Channel
	accountsLoaded := false
	for _, row := range s.agents() {
		if row["instance"] == true {
			continue
		}
		wire := text(row["agent_type"])
		tool := text(row["real_tool"])
		if tool == "" {
			tool = wire
		}
		s.store.mu.Lock()
		defaults, _ := s.store.Settings["harness_connection:"+wire].(map[string]any)
		defaults = clone(defaults)
		s.store.mu.Unlock()
		if defaults == nil {
			defaults = Row{"auth_mode": "", "provider_id": "", "model": ""}
			for _, p := range data.Providers {
				if p.Agent == tool && p.CCCurrent && p.API {
					defaults = Row{"auth_mode": "api", "provider_id": p.ID, "model": p.Model}
					break
				}
			}
		}
		account := providers.Account(tool)
		if strings.HasPrefix(tool, "account:") {
			account.Managed = true
			account.Source = filepath.Join(s.Root, ".data", "account-channels")
			if !accountsLoaded {
				accountRows, _, _ = accountchannels.List(s.Root)
				accountsLoaded = true
			}
			if accountRows != nil {
				for _, channel := range accountRows {
					if channel.ID == strings.TrimPrefix(tool, "account:") {
						account.Configured = channel.Configured
						for _, model := range channel.Models {
							account.Models = append(account.Models, providers.Model{ID: model.ID, Name: model.Name})
						}
					}
				}
			}
		}
		rows = append(rows, Row{"agent_type": wire, "tool": tool, "account": account, "defaults": defaults})
	}
	return rows, nil
}

func (s *Server) saveHarnessConnection(a Row) (any, error) {
	wire := text(a["agentType"])
	agent := s.agentByType(wire)
	if agent == nil || text(a["authMode"]) == "" {
		return nil, fmt.Errorf("请选择智能体与连接方式")
	}
	mode, pin, model := text(a["authMode"]), text(a["providerId"]), text(a["model"])
	tool := agent.RealTool
	if tool == "" {
		tool = wire
	}
	if err := s.validateProfileAuth(tool, mode, pin); err != nil {
		return nil, err
	}
	if mode == "api" {
		if p, err := s.validateProfileProvider(tool, pin); err != nil {
			return nil, err
		} else if err = validateProfileModel(p, model); err != nil {
			return nil, err
		}
	}
	cfg := Row{"auth_mode": mode, "provider_id": pin, "model": model}
	s.store.mu.Lock()
	old := s.store.Settings["harness_connection:"+wire]
	s.store.Settings["harness_connection:"+wire] = cfg
	err := s.store.saveLocked()
	if err != nil {
		s.store.Settings["harness_connection:"+wire] = old
	}
	s.store.mu.Unlock()
	if err != nil {
		return nil, err
	}
	s.Hub.UpdateAgent(agent.ID, func(ag *hub.Agent) { ag.AuthMode = mode; ag.ProviderID = pin; ag.DefaultModel = model })
	s.retireAgentConnections(agent.ID)
	s.emit("app://acp-agents-updated", Row{})
	return cfg, nil
}

func (s *Server) loadHarnessConnections() {
	for _, agent := range s.Hub.Agents() {
		if agent.Wire != "" && len(agent.Wire) > 5 && agent.Wire[:5] == "inst_" {
			continue
		}
		cfg, _ := s.store.Settings["harness_connection:"+wireAgent(agent)].(map[string]any)
		if cfg == nil {
			continue
		}
		s.Hub.UpdateAgent(agent.ID, func(ag *hub.Agent) {
			ag.AuthMode = text(cfg["auth_mode"])
			ag.ProviderID = text(cfg["provider_id"])
			ag.DefaultModel = text(cfg["model"])
		})
	}
}
