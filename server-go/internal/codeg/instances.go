package codeg

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"harnesshub/internal/hub"
	"harnesshub/internal/providers"
)

// Harness instances are named copies of a built-in tool (Claude 1号/2号…)
// that pin their own provider and model instead of following the single
// global active-provider selection. They ride the same ACP binary as the
// built-in (RealTool stays claude/codex); only the wire name, the provider
// env baked at spawn and the default model differ. The registry lives in
// .data/harness-instances.json; launchFn (cmd/serve/wiring.go) reads the
// same hub.Agent fields, so the spawn path needs no extra plumbing.
type instanceSpec struct {
	ID           string `json:"id"`
	Wire         string `json:"wire"`
	Name         string `json:"name"`
	RealTool     string `json:"realTool"`
	ProviderID   string `json:"providerId,omitempty"`
	DefaultModel string `json:"defaultModel,omitempty"`
}

func (s *Server) instancesFile() string {
	return filepath.Join(s.Root, ".data", "harness-instances.json")
}

// loadInstances re-registers persisted instances at startup. Entries whose
// tool is no longer installed are skipped — the built-in would be absent
// too, and an unlaunchable agent is only a broken chip in the UI.
func (s *Server) loadInstances() {
	data, err := os.ReadFile(s.instancesFile())
	if err != nil {
		return
	}
	var saved []instanceSpec
	if json.Unmarshal(data, &saved) != nil {
		return
	}
	for _, spec := range saved {
		if s.pick(spec.RealTool) == nil {
			continue
		}
		// A hub rebuilt over an existing registry (server restart in tests,
		// or a double New) must not duplicate entries.
		if s.Hub.Agent("agt_inst_" + spec.ID) != nil {
			continue
		}
		if a := s.instanceAgent(spec); a != nil {
			s.Hub.CreateAgent(a)
		}
	}
}

// saveInstancesLocked persists every instance (callers hold s.mu). Old
// entries without a stored wire keep round-tripping from the live agent.
func (s *Server) saveInstancesLocked() error {
	var saved []instanceSpec
	for _, a := range s.Hub.Agents() {
		if a.Wire == "" || !a.Real || !strings.HasPrefix(a.Wire, "inst_") {
			continue
		}
		saved = append(saved, instanceSpec{ID: strings.TrimPrefix(a.ID, "agt_inst_"), Wire: a.Wire, Name: a.Name,
			RealTool: a.RealTool, ProviderID: a.ProviderID, DefaultModel: a.DefaultModel})
	}
	if saved == nil {
		saved = []instanceSpec{}
	}
	b, err := json.MarshalIndent(saved, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(s.instancesFile()), 0o700); err != nil {
		return err
	}
	tmp := s.instancesFile() + ".tmp"
	if err := os.WriteFile(tmp, b, 0o600); err != nil {
		return err
	}
	return os.Rename(tmp, s.instancesFile())
}

// instanceAgent builds the hub registry entry for one instance spec. The
// wire is persisted with the spec (not re-derived), so a reload reproduces
// the exact registry the stored conversations address.
func (s *Server) instanceAgent(spec instanceSpec) *hub.Agent {
	if spec.RealTool != "claude" && spec.RealTool != "codex" {
		return nil
	}
	base := s.pick(spec.RealTool)
	if base == nil {
		return nil
	}
	name := strings.TrimSpace(spec.Name)
	if name == "" {
		name = spec.Wire
	}
	clone := *base
	clone.ID = "agt_inst_" + spec.ID
	clone.Name = name
	clone.Wire = spec.Wire
	clone.ProviderID = spec.ProviderID
	clone.DefaultModel = spec.DefaultModel
	clone.Endpoint = "acp://" + spec.RealTool + "/" + spec.ID
	clone.Load = 0
	clone.MaxCon = 4
	clone.Status = "online"
	return &clone
}

// instanceCommand serves codepluse_instance_* — the CRUD surface the
// settings UI uses to curate named harness instances.
func (s *Server) instanceCommand(command string, a Row) (any, error) {
	switch command {
	case "codepluse_instance_list":
		out := []Row{}
		for _, row := range s.agents() {
			if instance, _ := row["instance"].(bool); instance {
				out = append(out, row)
			}
		}
		return out, nil
	case "codepluse_instance_create":
		name := strings.TrimSpace(text(a["name"]))
		tool := text(a["realTool"])
		if tool == "" {
			tool = text(a["agentType"])
		}
		if tool == "claude_code" {
			tool = "claude"
		}
		if tool != "claude" && tool != "codex" {
			return nil, fmt.Errorf("实例只能基于 Claude Code 或 Codex")
		}
		if s.pick(tool) == nil {
			return nil, fmt.Errorf("本机未安装 %s，无法创建实例", tool)
		}
		providerID := strings.TrimSpace(text(a["providerId"]))
		defaultModel := strings.TrimSpace(text(a["defaultModel"]))
		if providerID != "" {
			if tool != "claude" {
				return nil, fmt.Errorf("只有 Claude Code 支持钉住供应商，Codex 请使用默认模型")
			}
			if _, err := providers.Find(s.Root, providerID); err != nil {
				return nil, fmt.Errorf("钉住供应商失败: %w", err)
			}
		}
		b := make([]byte, 6)
		if _, err := rand.Read(b); err != nil {
			return nil, err
		}
		spec := instanceSpec{ID: hex.EncodeToString(b), RealTool: tool, Name: name,
			ProviderID: providerID, DefaultModel: defaultModel, Wire: s.freeWire(tool)}
		agent := s.instanceAgent(spec)
		if agent == nil {
			return nil, fmt.Errorf("实例配置无效")
		}
		s.mu.Lock()
		s.Hub.CreateAgent(agent)
		err := s.saveInstancesLocked()
		s.mu.Unlock()
		if err != nil {
			return nil, err
		}
		s.emit("agent://changed", Row{"kind": "upsert", "agent": agent.Wire})
		return s.instanceView(agent), nil
	case "codepluse_instance_update":
		agentID := s.resolveInstanceID(a)
		providerID, hasProvider := a["providerId"].(string)
		providerID = strings.TrimSpace(providerID)
		defaultModel, hasModel := a["defaultModel"].(string)
		name := strings.TrimSpace(text(a["name"]))
		if providerID != "" {
			if _, err := providers.Find(s.Root, providerID); err != nil {
				return nil, fmt.Errorf("钉住供应商失败: %w", err)
			}
		}
		if existing := s.Hub.Agent(agentID); existing == nil || !strings.HasPrefix(existing.Wire, "inst_") {
			return nil, fmt.Errorf("实例不存在")
		} else if providerID != "" && existing.RealTool != "claude" {
			return nil, fmt.Errorf("只有 Claude Code 支持钉住供应商，Codex 请使用默认模型")
		}
		var updated *hub.Agent
		s.mu.Lock()
		s.Hub.UpdateAgent(agentID, func(ag *hub.Agent) {
			if ag.Wire == "" || !strings.HasPrefix(ag.Wire, "inst_") {
				return // never mutate a built-in
			}
			updated = ag
			if name != "" {
				ag.Name = name
			}
			if hasProvider {
				ag.ProviderID = providerID
			}
			if hasModel {
				ag.DefaultModel = strings.TrimSpace(defaultModel)
			}
		})
		if updated == nil {
			s.mu.Unlock()
			return nil, fmt.Errorf("实例不存在")
		}
		view := s.instanceView(updated)
		err := s.saveInstancesLocked()
		s.mu.Unlock()
		if err != nil {
			return nil, err
		}
		// Provider or model changed: warm entries keyed on the old values are
		// stale now. A driver with the new env comes up on the next connect
		// via LaunchVersion, so retire the idle pre-builts to avoid a race
		// where a claim still carries the previous provider.
		s.retireAgentConnections(agentID)
		s.emit("agent://changed", Row{"kind": "upsert", "agent": updated.Wire})
		return view, nil
	case "codepluse_instance_delete":
		agentID := s.resolveInstanceID(a)
		s.mu.Lock()
		agent := s.Hub.Agent(agentID)
		if agent == nil || !strings.HasPrefix(agent.Wire, "inst_") {
			s.mu.Unlock()
			return nil, fmt.Errorf("实例不存在")
		}
		s.Hub.DeleteAgent(agentID)
		wire := agent.Wire
		err := s.saveInstancesLocked()
		s.mu.Unlock()
		if err != nil {
			return nil, err
		}
		s.retireAgentConnections(agentID)
		s.emit("agent://changed", Row{"kind": "remove", "agent": wire})
		return nil, nil
	}
	return nil, fmt.Errorf("未知命令: %s", command)
}

// resolveInstanceID maps the frontend's agentType (an instance wire like
// inst_claude2) or an explicit agentId to the registry id.
func (s *Server) resolveInstanceID(a Row) string {
	if id := text(a["agentId"]); id != "" {
		return id
	}
	if wire := text(a["agentType"]); strings.HasPrefix(wire, "inst_") {
		if found := s.Hub.AgentByWire(wire); found != nil {
			return found.ID
		}
	}
	return ""
}

// freeWire picks the lowest unused inst_<tool><N> name, starting at 2 so the
// built-in keeps reading as "1号".
func (s *Server) freeWire(tool string) string {
	agents := s.Hub.Agents()
	for n := 2; n < 10000; n++ {
		wire := fmt.Sprintf("inst_%s%d", tool, n)
		taken := false
		for _, a := range agents {
			if a.Wire == wire {
				taken = true
				break
			}
		}
		if !taken {
			return wire
		}
	}
	return ""
}

func (s *Server) instanceView(agent *hub.Agent) Row {
	for _, row := range s.agents() {
		if row["agent_type"] == agent.Wire {
			return row
		}
	}
	return Row{}
}

// retireAgentConnections releases every warm pool entry belonging to one
// agent. Live conversations are left alone — their driver process keeps the
// old env until the runtime rotates it through LaunchVersion.
func (s *Server) retireAgentConnections(agentID string) {
	s.mu.Lock()
	var cids []string
	for _, c := range s.connections {
		if c.Agent != nil && c.Agent.ID == agentID && c.Warm {
			cids = append(cids, c.ID)
		}
	}
	s.mu.Unlock()
	for _, cid := range cids {
		s.releaseWarm(cid)
	}
}
