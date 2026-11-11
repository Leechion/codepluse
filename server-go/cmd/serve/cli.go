package main

import (
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"harnesshub/internal/hub"
	"harnesshub/internal/providers"
)

// providerVersion fingerprints what the spawned driver's environment is
// baked from, so editing or switching a provider rotates the driver instead
// of leaving stale credentials in a pooled process. A harness instance that
// pins its own provider is hashed against that pinned provider only —
// flipping the global active selection never disturbs its driver.
func providerVersion(agent *hub.Agent) string {
	tool := agent.RealTool
	if tool == "dsh" {
		return dshModelVersion()
	}
	if tool != "claude" {
		return "default"
	}
	p, ok, err := resolveProvider(agent)
	if err != nil {
		return "error"
	}
	if !ok {
		return "default"
	}
	b, _ := json.Marshal(struct {
		Provider providers.Provider
		Env      map[string]string
	}{p, p.Env})
	return fmt.Sprintf("%x", sha256.Sum256(b))
}

// resolveProvider returns the provider actually backing one agent: the
// instance's pinned entry when set (falling back to the global selection if
// the pin went stale), otherwise the global active selection the built-in
// follows. The bool mirrors providers.Active's "configured" meaning.
func resolveProvider(agent *hub.Agent) (providers.Provider, bool, error) {
	if agent.ProviderID != "" {
		if p, err := providers.Find(ROOT, agent.ProviderID); err == nil {
			return p, true, nil
		}
		fmt.Fprintf(os.Stderr, "[hub] 实例 %s 钉住的供应商已失效，回退全局供应商\n", agent.Name)
	}
	return providers.Active(ROOT, agent.RealTool)
}

// ----------------------------- CLI locating -----------------------------

func home() string { d, _ := os.UserHomeDir(); return d }

func npmGlobal() string { return filepath.Join(home(), "AppData", "Roaming", "npm", "node_modules") }

var acpPackages = map[string][]string{
	"codex": {
		filepath.Join(npmGlobal(), "@agentclientprotocol", "codex-acp", "dist", "index.js"),
		filepath.Join(ROOT, "node_modules", "@agentclientprotocol", "codex-acp", "dist", "index.js"),
	},
	"claude": {
		filepath.Join(npmGlobal(), "@agentclientprotocol", "claude-agent-acp", "dist", "index.js"),
		filepath.Join(ROOT, "node_modules", "@agentclientprotocol", "claude-agent-acp", "dist", "index.js"),
	},
	"dsh": {
		filepath.Join(npmGlobal(), "@deepseek-ai", "dsh", "lib", "bin.js"),
		filepath.Join(ROOT, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js"),
	},
}

// WorkBuddy ships its own CodeBuddy execution engine. Prefer that engine over
// a separately installed CLI so it uses the same vendor distribution.
func workBuddyEntry() string {
	if p := configuredHarnessPath("workbuddy"); p != "" {
		return p
	}
	if p := os.Getenv("HARNESS_HUB_WORKBUDDY_BIN"); p != "" {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	roots := []string{os.Getenv("HARNESS_HUB_WORKBUDDY_HOME"), filepath.Join(os.Getenv("LOCALAPPDATA"), "Programs", "WorkBuddy"), filepath.Join(os.Getenv("LOCALAPPDATA"), "WorkBuddy")}
	if p, err := exec.LookPath("WorkBuddy.exe"); err == nil {
		roots = append(roots, filepath.Dir(p))
	}
	// Windows App Paths supports installations on a custom drive.
	if out, err := exec.Command("reg", "query", `HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\WorkBuddy.exe`, "/ve").Output(); err == nil {
		for _, line := range strings.Split(string(out), "\n") {
			if _, value, ok := strings.Cut(line, "REG_SZ"); ok {
				roots = append(roots, filepath.Dir(strings.Trim(strings.TrimSpace(value), `"`)))
			}
		}
	}
	for _, root := range roots {
		if root == "" {
			continue
		}
		p := filepath.Join(root, "resources", "app.asar.unpacked", "cli", "bin", "codebuddy")
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	return ""
}

func configuredHarnessPath(tool string) string {
	data, err := os.ReadFile(dataFile("harness-paths.json"))
	if err != nil {
		return ""
	}
	var paths map[string]string
	if json.Unmarshal(data, &paths) != nil {
		return ""
	}
	p := paths[tool]
	if p == "" {
		return ""
	}
	if _, err := os.Stat(p); err != nil {
		return ""
	}
	return p
}

func adapterVersion(entry string) string {
	for dir, i := filepath.Dir(entry), 0; dir != "." && i < 4; dir, i = filepath.Dir(dir), i+1 {
		data, err := os.ReadFile(filepath.Join(dir, "package.json"))
		if err != nil {
			continue
		}
		var pkg struct {
			Version string `json:"version"`
		}
		if json.Unmarshal(data, &pkg) == nil && pkg.Version != "" {
			return pkg.Version
		}
	}
	return "local"
}

var binCandidates = map[string][]string{
	"codex": {
		filepath.Join(npmGlobal(), "@openai", "codex", "bin", "codex.js"),
		filepath.Join(home(), ".npm-global", "lib", "node_modules", "@openai", "codex", "bin", "codex.js"),
		"/usr/local/lib/node_modules/@openai/codex/bin/codex.js",
	},
	"claude": {
		filepath.Join(npmGlobal(), "@anthropic-ai", "claude-code", "bin", "claude.exe"),
		filepath.Join(npmGlobal(), "@anthropic-ai", "claude-code", "bin", "claude"),
		filepath.Join(home(), ".npm-global", "lib", "node_modules", "@anthropic-ai", "claude-code", "bin", "claude"),
		"/usr/local/lib/node_modules/@anthropic-ai/claude-code/bin/claude",
	},
}

var binEnv = map[string]string{"codex": "HARNESS_HUB_CODEX_BIN", "claude": "HARNESS_HUB_CLAUDE_BIN"}

var binLabels = map[string]string{"codex": "Codex CLI", "claude": "Claude Code CLI"}

func resolveBin(tool string) string {
	if p := os.Getenv(binEnv[tool]); p != "" {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	for _, p := range binCandidates[tool] {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	return runtimeBin(tool)
}

// runtimeBin covers npm installs whose global prefix is not %APPDATA%\npm
// (custom prefixes such as a user-selected node_global directory): npm drops
// its shim (claude.cmd / codex.cmd) in the prefix root, and the package with
// the native CLI sits under <prefix>\node_modules.
func runtimeBin(tool string) string {
	shim, err := exec.LookPath(tool + ".cmd")
	if err != nil {
		return ""
	}
	prefix := filepath.Dir(shim)
	var pkg string
	switch tool {
	case "codex":
		pkg = filepath.Join("@openai", "codex", "bin", "codex.js")
	case "claude":
		pkg = filepath.Join("@anthropic-ai", "claude-code", "bin", "claude.exe")
		if _, err := os.Stat(filepath.Join(prefix, "node_modules", pkg)); err != nil {
			pkg = filepath.Join("@anthropic-ai", "claude-code", "bin", "claude")
		}
	default:
		return ""
	}
	p := filepath.Join(prefix, "node_modules", pkg)
	if _, err := os.Stat(p); err != nil {
		return ""
	}
	return p
}

func acpInstalled(tool string) string {
	if tool == "workbuddy" {
		return workBuddyEntry()
	}
	if tool == "dsh" {
		if p := configuredHarnessPath(tool); p != "" {
			return p
		}
		if p := os.Getenv("HARNESS_HUB_DSH_BIN"); p != "" {
			if _, err := os.Stat(p); err == nil {
				return p
			}
		}
	}
	for _, p := range acpPackages[tool] {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	return ""
}

var permissions = []map[string]any{
	{"id": "readonly", "label": "只读分析", "desc": "不写文件、不执行命令；仅分析与回答", "allowWrites": false, "allowShell": false},
	{"id": "workspace", "label": "限定目录内可读写", "desc": "仅允许在指定工作目录内读写文件", "allowWrites": true, "allowShell": false},
	{"id": "shell", "label": "只读 + 允许 shell", "desc": "允许执行命令，但不授予写权限", "allowWrites": false, "allowShell": true},
}

// codexHome uses Codex's own settings and credentials in a writable workspace copy.
// auth.json is copied only when the local copy is missing or the source was
// updated by a fresh `codex login`: Codex refreshes tokens inside CODEX_HOME,
// and overwriting that copy on every launch discarded the refreshed credentials
// and forced a re-authentication on the next connection. config.toml stays
// synced on every launch so user configuration edits still take effect.
// An instance passes its own name so each Codex instance logs in separately.
func codexHome(name ...string) (string, error) {
	dir := filepath.Join(ROOT, ".data", "codex-native-home")
	if len(name) > 0 && name[0] != "" {
		dir = filepath.Join(ROOT, ".data", "codex-homes", name[0])
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		return "", fmt.Errorf("无法创建 Codex 配置目录: %w", err)
	}
	for _, name := range []string{"auth.json", "config.toml"} {
		src := filepath.Join(home(), ".codex", name)
		data, err := os.ReadFile(src)
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			return "", fmt.Errorf("读取 Codex %s 失败: %w", name, err)
		}
		dst := filepath.Join(dir, name)
		if name == "auth.json" {
			if info, statErr := os.Stat(dst); statErr == nil {
				srcInfo, srcErr := os.Stat(src)
				// A source newer than the copy means the user logged in again
				// with the real CLI. Otherwise keep the copy: it may hold a
				// token Codex refreshed inside this isolated CODEX_HOME.
				if srcErr == nil && !srcInfo.ModTime().After(info.ModTime()) {
					continue
				}
			}
		}
		if err = os.WriteFile(dst, data, 0600); err != nil {
			return "", fmt.Errorf("写入 Codex %s 失败: %w", name, err)
		}
	}
	return dir, nil
}

func buildEnv(agent *hub.Agent) ([]string, error) {
	tool := agent.RealTool
	env := os.Environ()
	set := func(k, v string) { env = append(env, k+"="+v) }
	drop := func(k string) {
		var out []string
		for _, e := range env {
			if !strings.HasPrefix(e, k+"=") {
				out = append(out, e)
			}
		}
		env = out
	}
	drop("NODE_EXTRA_CA_CERTS")
	set("NO_COLOR", "1")
	set("CI", "1")
	if tool == "dsh" && os.Getenv("DSH_HOME") == "" {
		localHome := filepath.Join(ROOT, ".data", "dsh-home")
		if info, err := os.Stat(localHome); err == nil && info.IsDir() {
			set("DSH_HOME", localHome)
		}
	}
	if tool == "workbuddy" {
		set("CODEBUDDY_FORCE_LITE_WB_BUNDLE", "1")
		if os.Getenv("CODEBUDDY_CONFIG_DIR") == "" {
			set("CODEBUDDY_CONFIG_DIR", filepath.Join(home(), ".workbuddy"))
		}
	}
	if tool == "claude" {
		if data, err := os.ReadFile(filepath.Join(home(), ".claude", "settings.json")); err == nil {
			var s struct {
				Env map[string]string `json:"env"`
			}
			if json.Unmarshal(data, &s) == nil {
				for k, v := range s.Env {
					if strings.HasPrefix(k, "ANTHROPIC_") && os.Getenv(k) == "" {
						set(k, v)
					}
				}
			}
		}
		set("CLAUDE_CODE_EXECUTABLE", resolveBin("claude"))
	}
	if tool == "codex" && os.Getenv("HARNESS_HUB_CODEX_ISOLATE") != "0" {
		dir, err := codexHome(agent.Wire)
		if err != nil {
			return nil, err
		}
		set("CODEX_HOME", dir)

	}
	return env, nil
}
