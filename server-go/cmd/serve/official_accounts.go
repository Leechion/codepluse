package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/pelletier/go-toml/v2"
	"harnesshub/internal/hub"
	"harnesshub/internal/providers"
)

// Explicit subscription profiles bypass both the global active gateway and
// API credentials in native settings. The vendor's account files stay intact.
func applyOfficialAccount(agent *hub.Agent, env []string) ([]string, error) {
	env = providers.ClearAPIEnvironment(env)
	switch agent.RealTool {
	case "codex":
		source := providers.NativeHome("codex")
		data, err := os.ReadFile(filepath.Join(source, "auth.json"))
		if err != nil {
			return nil, fmt.Errorf("未读取到 Codex 官网登录，请先在 Codex 中执行 codex login")
		}
		var auth map[string]any
		if json.Unmarshal(data, &auth) != nil {
			return nil, fmt.Errorf("Codex 登录文件格式无效")
		}
		tokens, _ := auth["tokens"].(map[string]any)
		if tokens == nil || tokens["access_token"] == nil || tokens["access_token"] == "" {
			return nil, fmt.Errorf("Codex 当前没有官网账号登录凭据；API Key 不会作为官网订阅使用")
		}
		delete(auth, "OPENAI_API_KEY")
		auth["auth_mode"] = "chatgpt"
		dir, err := codexHome(agent.Wire)
		if err != nil {
			return nil, err
		}
		// Keep a token refreshed by an earlier isolated session when it is newer.
		copied := map[string]any{}
		if b, e := os.ReadFile(filepath.Join(dir, "auth.json")); e == nil && json.Unmarshal(b, &copied) == nil {
			if copyTokens, ok := copied["tokens"].(map[string]any); ok && copyTokens["access_token"] != nil && copyTokens["access_token"] != "" {
				auth = copied
				delete(auth, "OPENAI_API_KEY")
				auth["auth_mode"] = "chatgpt"
			}
		}
		b, _ := json.Marshal(auth)
		if err = os.WriteFile(filepath.Join(dir, "auth.json"), b, 0600); err != nil {
			return nil, err
		}
		cfg := map[string]any{}
		if b, e := os.ReadFile(filepath.Join(dir, "config.toml")); e == nil {
			if err = toml.Unmarshal(b, &cfg); err != nil {
				return nil, err
			}
		}
		cfg["model_provider"] = "openai"
		delete(cfg, "profile")
		delete(cfg, "forced_login_method")
		delete(cfg, "openai_base_url")
		delete(cfg, "model_catalog_json")
		if ps, ok := cfg["model_providers"].(map[string]any); ok {
			delete(ps, "openai")
		}
		if agent.DefaultModel != "" {
			cfg["model"] = agent.DefaultModel
		} else {
			delete(cfg, "model")
		}
		b, err = toml.Marshal(cfg)
		if err != nil {
			return nil, err
		}
		if err = os.WriteFile(filepath.Join(dir, "config.toml"), b, 0600); err != nil {
			return nil, err
		}
		env = append(dropEnvKey(env, "CODEX_HOME"), "CODEX_HOME="+dir)
	case "claude":
		source := providers.NativeHome("claude")
		if !providers.Account("claude").Configured {
			return nil, fmt.Errorf("未读取到 Claude 官网登录，请先在 Claude Code 中执行 claude auth login")
		}
		dir := filepath.Join(ROOT, ".data", "claude-homes", agent.Wire)
		if err := os.MkdirAll(dir, 0700); err != nil {
			return nil, err
		}
		for _, name := range []string{".credentials.json", "settings.json"} {
			data, err := os.ReadFile(filepath.Join(source, name))
			if os.IsNotExist(err) {
				continue
			}
			if err != nil {
				return nil, err
			}
			if name == ".credentials.json" {
				src, _ := os.Stat(filepath.Join(source, name))
				dst, e := os.Stat(filepath.Join(dir, name))
				if e == nil && src != nil && !src.ModTime().After(dst.ModTime()) {
					continue
				}
			}
			if name == "settings.json" {
				cfg := map[string]any{}
				if json.Unmarshal(data, &cfg) != nil {
					return nil, fmt.Errorf("Claude 设置格式无效")
				}
				if settingsEnv, ok := cfg["env"].(map[string]any); ok {
					for key := range settingsEnv {
						if len(providers.ClearAPIEnvironment([]string{key + "=value"})) == 0 {
							delete(settingsEnv, key)
						}
					}
				}
				delete(cfg, "model")
				delete(cfg, "apiKeyHelper")
				delete(cfg, "forceLoginMethod")
				data, _ = json.Marshal(cfg)
			}
			if err = os.WriteFile(filepath.Join(dir, name), data, 0600); err != nil {
				return nil, err
			}
		}
		env = append(dropEnvKey(env, "CLAUDE_CONFIG_DIR"), "CLAUDE_CONFIG_DIR="+dir)
	}
	return env, nil
}
