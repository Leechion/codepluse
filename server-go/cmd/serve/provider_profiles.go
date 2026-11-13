package main

import (
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"github.com/pelletier/go-toml/v2"
	"gopkg.in/yaml.v3"
	"harnesshub/internal/hub"
	"os"
	"path/filepath"
	"strings"
)

func applyCodexProvider(agent *hub.Agent, env []string) ([]string, error) {
	p, _, err := resolveProvider(agent)
	if err != nil {
		return nil, err
	}
	dir := filepath.Join(ROOT, ".data", "codex-homes", agent.Wire)
	if agent.Wire == "" {
		dir = filepath.Join(ROOT, ".data", "codex-native-home")
	}
	// LaunchOverlay may already have written an explicit per-agent config. Read
	// the isolated copy, preserve unrelated options, then bind only this provider.
	prepared := false
	for _, v := range env {
		if strings.HasPrefix(v, "CODEX_HOME=") && strings.TrimPrefix(v, "CODEX_HOME=") == dir {
			prepared = true
		}
	}
	if !prepared {
		dir, err = codexHome(agent.Wire)
		if err != nil {
			return nil, err
		}
	}
	file := filepath.Join(dir, "config.toml")
	cfg := map[string]any{}
	if data, e := os.ReadFile(file); e == nil {
		if e = toml.Unmarshal(data, &cfg); e != nil {
			return nil, fmt.Errorf("Codex 配置格式无效")
		}
	}
	settings := map[string]any{}
	for k, v := range p.Native {
		settings[k] = v
	}
	settings["name"] = p.Name
	settings["base_url"] = p.URL
	settings["env_key"] = "CODEPLUSE_CODEX_API_KEY"
	settings["requires_openai_auth"] = false
	if settings["wire_api"] == nil {
		settings["wire_api"] = "responses"
	}
	cfg["model_provider"] = "codepluse"
	model := agent.DefaultModel
	if model == "" {
		model = p.Model
	}
	cfg["model"] = model
	configured, _ := cfg["model_providers"].(map[string]any)
	if configured == nil {
		configured = map[string]any{}
	}
	configured["codepluse"] = settings
	cfg["model_providers"] = configured
	data, err := toml.Marshal(cfg)
	if err != nil {
		return nil, err
	}
	if err = os.WriteFile(file, data, 0600); err != nil {
		return nil, err
	}
	// API profiles must not inherit the native ChatGPT login copied by codexHome.
	auth, _ := json.Marshal(map[string]string{"auth_mode": "apikey", "OPENAI_API_KEY": p.Key})
	if err = os.WriteFile(filepath.Join(dir, "auth.json"), auth, 0600); err != nil {
		return nil, err
	}
	env = append(dropEnvKey(env, "CODEX_HOME"), "CODEX_HOME="+dir)
	return append(dropEnvKey(env, "CODEPLUSE_CODEX_API_KEY"), "CODEPLUSE_CODEX_API_KEY="+p.Key), nil
}

func dshProviderPatch(agent *hub.Agent, env []string) (string, []string, error) {
	p, _, err := resolveProvider(agent)
	if err != nil {
		return "", nil, err
	}
	cfg := map[string]any{}
	for k, v := range p.Native {
		cfg[k] = v
	}
	cfg["baseURL"] = p.URL
	cfg["apiKeyEnv"] = "CODEPLUSE_DSH_API_KEY"
	delete(cfg, "apiKey")
	if cfg["api"] == nil {
		cfg["api"] = "openai-completions"
		if p.Protocol == "openai-responses" {
			cfg["api"] = "openai-responses"
		}
		if p.Protocol == "anthropic" {
			cfg["api"] = "anthropic-messages"
		}
	}
	model := agent.DefaultModel
	if model == "" {
		model = p.Model
	}
	if _, ok := cfg["models"]; !ok {
		cfg["models"] = []map[string]string{{"id": model, "name": model}}
	}
	name := "codepluse"
	if strings.HasPrefix(p.ID, "native:dsh:") {
		parts := strings.Split(p.ID, ":")
		name = parts[len(parts)-1]
	}
	config := []map[string]any{
		{"id": "llm-pi-ai", "config": map[string]any{"providers": map[string]any{name: cfg}}},
		{"id": "agent-default-model", "config": map[string]string{"provider": name, "model": model}},
		{"id": "acp", "config": map[string]string{"provider": name, "model": model}},
	}
	data, err := yaml.Marshal(config)
	if err != nil {
		return "", nil, err
	}
	hash := sha256.Sum256(data)
	dir := filepath.Join(ROOT, ".data", "dsh-model-patches")
	if err = os.MkdirAll(dir, 0700); err != nil {
		return "", nil, err
	}
	file := filepath.Join(dir, fmt.Sprintf("profile-%x.yml", hash[:12]))
	if err = os.WriteFile(file, data, 0600); err != nil {
		return "", nil, err
	}
	return file, append(dropEnvKey(env, "CODEPLUSE_DSH_API_KEY"), "CODEPLUSE_DSH_API_KEY="+p.Key), nil
}
