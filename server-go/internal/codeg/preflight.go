package codeg

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
)

// jsonVersion reads the "version" field out of a package.json body.
func jsonVersion(b []byte) string {
	var pkg struct {
		Version string `json:"version"`
	}
	if json.Unmarshal(b, &pkg) == nil {
		return pkg.Version
	}
	return ""
}

// acpPreflight answers the settings panel's launch-readiness probe for one
// agent. The panel builds two cards entirely on the client (the ACP-adapter
// explainer from `adapter`, and the version status from the agent row), so the
// backend's job is to ship facts only: whether the thing a launch would run is
// present, where the vendor CLI is, and which config dir the adapter shares
// with it so installing the bridge needs no second login.
func (s *Server) preflight(agentType string) (Row, error) {
	ag := s.pick(agentType)
	if ag == nil {
		// A deleted/disabled agent: report the miss rather than erroring, so the
		// panel degrades to "not available" instead of an unhandled rejection.
		return Row{
			"agent_type": agentType,
			"agent_name": agentType,
			"passed":     false,
			"checks":     []Row{{"check_id": "system_cli", "label": "Launch target", "status": "fail", "message": "此智能体未注册或已停用。", "fixes": []Row{}}},
			"adapter":    nil,
		}, nil
	}

	name, nativeCmd, nativeLabel, docsURL, isAdapter := adapterIdentity(ag.RealTool)
	adapter := Row(nil)
	checks := []Row{}

	if isAdapter {
		version, entryDir, installed := s.adapterLookup(ag.RealTool)
		if installed {
			adapter = s.adapterInfo(name, nativeCmd, nativeLabel, version, docsURL, true)
			checks = append(checks, Row{
				"check_id": "system_cli",
				"label":    "Launch target",
				"status":   "pass",
				"message":  "找到 ACP 适配器 " + entryDir + "，启动时将运行它。",
				"fixes":    []Row{},
			})
		} else {
			adapter = s.adapterInfo(name, nativeCmd, nativeLabel, version, docsURL, false)
			checks = append(checks, Row{
				"check_id": "system_cli",
				"label":    "Launch target",
				"status":   "fail",
				"message":  "尚未安装 ACP 适配器 " + "@agentclientprotocol/" + name + "，下方可安装。",
				"fixes":    []Row{{"label": "了解更多", "kind": "open_url", "payload": docsURL}},
			})
		}
	} else {
		available := ag.Enabled
		if s.AgentAvailable != nil {
			available = s.AgentAvailable(ag)
		}
		status := "fail"
		message := "未找到可启动的运行环境。"
		if available {
			status = "pass"
			message = "运行环境已就绪。"
		}
		checks = append(checks, Row{"check_id": "system_cli", "label": "Launch target", "status": status, "message": message, "fixes": []Row{}})
	}

	passed := ag.Enabled
	if s.AgentAvailable != nil {
		passed = s.AgentAvailable(ag)
	}

	return Row{
		"agent_type": wireAgent(ag),
		"agent_name": ag.Name,
		"passed":     passed,
		"checks":     checks,
		"adapter":    adapter,
	}, nil
}

// adapterIdentity maps a hub RealTool onto the third-party ACP-adapter facts
// the panel explains. Only Claude Code and Codex route through an
// @agentclientprotocol bridge; every other tool returns isAdapter=false so the
// caller skips the adapter card (the backend, not i18n, decides it is null).
func adapterIdentity(realTool string) (name, nativeCmd, nativeLabel, docsURL string, isAdapter bool) {
	switch realTool {
	case "claude":
		return "claude-agent-acp", "claude", "Claude Code CLI", "https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp", true
	case "codex":
		return "codex-acp", "codex", "Codex CLI", "https://www.npmjs.com/package/@agentclientprotocol/codex-acp", true
	}
	return "", "", "", "", false
}

// adapterInfo assembles the AdapterInfo payload. `installed` mirrors the
// adapterLookup verdict; native_path is the vendor CLI resolved on PATH (may be
// empty — the bridge ships its own runtime, so it is optional), and
// shared_config_dir is where that CLI keeps its sign-in, which the bridge reads
// too, so installing the adapter needs no second login.
func (s *Server) adapterInfo(name, nativeCmd, nativeLabel, version, docsURL string, installed bool) Row {
	pkg := "@agentclientprotocol/" + name
	if version != "" && version != "local" {
		pkg += "@" + version
	}
	nativePath := ""
	if p, err := exec.LookPath(nativeCmd); err == nil {
		nativePath = p
	}
	return Row{
		"adapter_package":   pkg,
		"adapter_cmd":       name,
		"adapter_installed": installed,
		"native_cmd":        nativeCmd,
		"native_label":      nativeLabel,
		"native_path":       nativeOrNull(nativePath),
		"native_required":   false,
		"shared_config_dir": nativeConfigDir(name),
		"docs_url":          docsURL,
	}
}

// nativeConfigDir points at the vendor CLI's config directory so the adapter
// card can promise a shared sign-in. Claude Code uses ~/.claude, Codex ~/.codex.
func nativeConfigDir(adapterName string) string {
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return ""
	}
	if adapterName == "claude-agent-acp" {
		return filepath.Join(home, ".claude")
	}
	return filepath.Join(home, ".codex")
}

// adapterLookup reports the installed adapter version, its package directory,
// and whether it is present at all. It reuses the same search order as
// installedVersion (global npm, then the bundled tree) so the launch gate, the
// agent list and preflight never disagree about what is installed.
func (s *Server) adapterLookup(realTool string) (version, dir string, found bool) {
	name := "codex-acp"
	if realTool == "claude" {
		name = "claude-agent-acp"
	}
	if realTool != "claude" && realTool != "codex" {
		return "", "", false
	}
	home, _ := os.UserHomeDir()
	for _, base := range []string{filepath.Join(home, "AppData", "Roaming", "npm", "node_modules"), filepath.Join(s.Root, "node_modules")} {
		pkgDir := filepath.Join(base, "@agentclientprotocol", name)
		b, err := os.ReadFile(filepath.Join(pkgDir, "package.json"))
		if err != nil {
			continue
		}
		version = jsonVersion(b)
		return version, pkgDir, true
	}
	return "", "", false
}

func nativeOrNull(p string) any {
	if p == "" {
		return nil
	}
	return p
}
