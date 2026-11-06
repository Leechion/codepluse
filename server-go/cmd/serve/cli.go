package main

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

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
	return ""
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

// gatewayConfig reuses a configured Claude gateway for Codex (Responses API).
func gatewayConfig() map[string]string {
	data, err := os.ReadFile(filepath.Join(home(), ".claude", "settings.json"))
	if err != nil {
		return nil
	}
	var s struct {
		Env map[string]string `json:"env"`
	}
	if json.Unmarshal(data, &s) != nil {
		return nil
	}
	base, token := s.Env["ANTHROPIC_BASE_URL"], s.Env["ANTHROPIC_AUTH_TOKEN"]
	if base == "" || token == "" {
		return nil
	}
	model := os.Getenv("HARNESS_HUB_CODEX_MODEL")
	if model == "" {
		model = s.Env["ANTHROPIC_MODEL"]
	}
	if model == "" {
		model = "qwen3.8-flash"
	}
	return map[string]string{"baseUrl": strings.TrimRight(base, "/"), "token": token, "model": model}
}

// codexHome builds an isolated CODEX_HOME carrying only auth.json (+ gateway).
func codexHome() string {
	dir := filepath.Join(os.TempDir(), "harness-hub-codex-home")
	_ = os.MkdirAll(dir, 0o755)
	src := filepath.Join(home(), ".codex", "auth.json")
	if data, err := os.ReadFile(src); err == nil {
		_ = os.WriteFile(filepath.Join(dir, "auth.json"), data, 0o600)
	}
	if os.Getenv("HARNESS_HUB_CODEX_GATEWAY") != "0" {
		if gw := gatewayConfig(); gw != nil {
			toml := "model = \"" + strings.ReplaceAll(gw["model"], "\"", "\\\"") + "\"\n" +
				"model_provider = \"gateway\"\nmodel_reasoning_effort = \"low\"\n\n" +
				"[model_providers.gateway]\nname = \"Local Gateway\"\n" +
				"base_url = \"" + gw["baseUrl"] + "/v1\"\nwire_api = \"responses\"\n" +
				"env_key = \"HH_GATEWAY_KEY\"\nrequires_openai_auth = false\n"
			_ = os.WriteFile(filepath.Join(dir, "config.toml"), []byte(toml), 0o600)
		}
	}
	return dir
}

func buildEnv(tool string) []string {
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
		set("CODEX_HOME", codexHome())
		if os.Getenv("HARNESS_HUB_CODEX_GATEWAY") != "0" {
			if gw := gatewayConfig(); gw != nil {
				set("HH_GATEWAY_KEY", gw["token"])
				if os.Getenv("ALL_PROXY") == "" && os.Getenv("HTTPS_PROXY") != "" {
					set("ALL_PROXY", os.Getenv("HTTPS_PROXY"))
				}
				if os.Getenv("all_proxy") == "" && os.Getenv("https_proxy") != "" {
					set("all_proxy", os.Getenv("https_proxy"))
				}
			}
		}
	}
	return env
}
