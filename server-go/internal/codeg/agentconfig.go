package codeg

import (
	"fmt"

	"harnesshub/internal/hub"
)

// This file backs the harness settings page's "保存环境变量" / "保存配置管理"
// buttons (acp_update_agent_env / acp_update_agent_config). codepluse stores
// what the panel edits as a per-agent overlay in its OWN store — never written
// back to the vendor files (~/.claude, the real ~/.codex) — so saving a config
// here can never clobber the user's global CLI setup. The launch path merges
// this overlay on top of the ambient environment (env vars for every tool, and
// codex's isolated CODEX_HOME for its config.toml/auth.json, which is already
// under .data and therefore safe to write).

// updateAgentSettings writes the env or config half of a per-agent overlay and
// returns the number of live sessions on that agent that now hold stale config
// (the panel turns that into an "N sessions need restart" note).
//
// Locking: this runs in the pre-store-lock dispatch section, so it takes
// st.mu and s.mu strictly one at a time and never holds both, avoiding any
// ordering hazard with the connection paths that lock s.mu first.
func (s *Server) updateAgentSettings(command string, a Row) (int, error) {
	agentType := text(a["agentType"])
	ag := s.agentByType(agentType)
	if ag == nil {
		return 0, fmt.Errorf("智能体不存在：%s", agentType)
	}
	wire := wireAgent(ag)

	st := s.store
	st.mu.Lock()
	ov := st.Agents[wire]
	if ov == nil {
		ov = Row{}
		st.Agents[wire] = ov
	}
	var enabled *bool
	switch command {
	case "acp_update_agent_env":
		ov["env"] = stringEnvMap(a["env"])
		if e, ok := a["enabled"].(bool); ok {
			ov["enabled"] = e
			enabled = &e
		}
		if mp, ok := a["modelProviderId"]; ok {
			ov["model_provider_id"] = modelProviderValue(mp)
		}
	default: // acp_update_agent_config
		for _, f := range []struct{ payload, key string }{
			{"configJson", "config_json"},
			{"codexAuthJson", "codex_auth_json"},
			{"codexConfigToml", "codex_config_toml"},
			{"codexModelCatalog", "codex_model_catalog"},
			{"systemInstructionsContent", "system_instructions_content"},
			{"systemInstructionsFilePath", "system_instructions_file_path"},
		} {
			if v, ok := a[f.payload]; ok {
				ov[f.key] = text(v)
			}
		}
	}
	err := st.saveLocked()
	st.mu.Unlock()
	if err != nil {
		return 0, err
	}

	// Mirror an enabled change onto the live registry so the "available" list
	// and new sessions see it without a restart.
	if enabled != nil {
		s.Hub.SetAgentEnabled(ag.ID, *enabled)
	}

	live := 0
	s.mu.Lock()
	for _, c := range s.connections {
		if c.AgentType == wire && !c.Warm {
			live++
		}
	}
	s.mu.Unlock()
	return live, nil
}

// agentByType finds a registered real agent by wire name or id, IGNORING the
// enabled flag — disabling an agent still has to resolve it so the toggle can
// be persisted.
func (s *Server) agentByType(agentType string) *hub.Agent {
	for _, ag := range s.Hub.Agents() {
		if ag.Real && (wireAgent(ag) == agentType || ag.ID == agentType) {
			return ag
		}
	}
	return nil
}

// applyEnabledOverrides re-seeds the persisted enable/disable choices onto the
// freshly-built registry at boot (hub built-ins reset to enabled each start).
func (s *Server) applyEnabledOverrides() {
	st := s.store
	st.mu.Lock()
	want := make(map[string]bool, len(st.Agents))
	for wire, ov := range st.Agents {
		if e, ok := ov["enabled"].(bool); ok {
			want[wire] = e
		}
	}
	st.mu.Unlock()
	for _, ag := range s.Hub.Agents() {
		if !ag.Real {
			continue
		}
		if on, ok := want[wireAgent(ag)]; ok {
			s.Hub.SetAgentEnabled(ag.ID, on)
		}
	}
}

// LaunchOverlay returns what the launch path should layer on top of the ambient
// environment for one agent: the env overlay for any tool, plus the codex
// config.toml/auth.json text to write into its isolated CODEX_HOME. Values are
// copied out so the caller never touches the store without a lock.
func (s *Server) LaunchOverlay(a *hub.Agent) (env map[string]string, codexToml, codexAuth string) {
	if a == nil {
		return nil, "", ""
	}
	st := s.store
	st.mu.Lock()
	defer st.mu.Unlock()
	ov := st.Agents[wireAgent(a)]
	if ov == nil {
		return nil, "", ""
	}
	return overlayEnv(ov["env"]), text(ov["codex_config_toml"]), text(ov["codex_auth_json"])
}

// mergeAgentOverlay folds a persisted overlay row onto an agent-list entry so
// the settings page round-trips what was saved. Callers must hold st.mu
// (agents() runs under the store-locked dispatch).
func mergeAgentOverlay(agents map[string]Row, wire string, row Row) {
	ov := agents[wire]
	if ov == nil {
		return
	}
	if env := overlayEnv(ov["env"]); env != nil {
		row["env"] = env
	}
	for _, key := range []string{
		"config_json", "codex_auth_json", "codex_config_toml",
		"codex_model_catalog", "system_instructions_content",
		"system_instructions_file_path", "model_provider_id",
	} {
		if v, ok := ov[key]; ok {
			row[key] = v
		}
	}
}

// overlayEnv accepts both a fresh map[string]string and a JSON-round-tripped
// map[string]any (what the store holds after a restart).
func overlayEnv(v any) map[string]string {
	switch m := v.(type) {
	case map[string]string:
		if len(m) == 0 {
			return map[string]string{}
		}
		out := make(map[string]string, len(m))
		for k, val := range m {
			out[k] = val
		}
		return out
	case map[string]any:
		out := make(map[string]string, len(m))
		for k, val := range m {
			if s, ok := val.(string); ok {
				out[k] = s
			} else {
				out[k] = fmt.Sprint(val)
			}
		}
		return out
	}
	return nil
}

func stringEnvMap(v any) map[string]string {
	out := map[string]string{}
	if m, ok := v.(map[string]any); ok {
		for k, val := range m {
			if s, ok := val.(string); ok {
				out[k] = s
			} else {
				out[k] = fmt.Sprint(val)
			}
		}
	}
	return out
}

func modelProviderValue(v any) any {
	switch n := v.(type) {
	case nil:
		return nil
	case float64:
		return int(n)
	case int:
		return n
	}
	return nil
}
