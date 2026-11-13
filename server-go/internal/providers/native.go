package providers

import (
	"encoding/json"
	"fmt"
	"github.com/pelletier/go-toml/v2"
	"gopkg.in/yaml.v3"
	"harnesshub/internal/modelproviders"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

func ModelProviders(root string) ([]Provider, error) {
	rows, err := modelproviders.List(root)
	if err != nil {
		return nil, err
	}
	out := []Provider{}
	for _, row := range rows {
		tool := row.AgentType
		switch tool {
		case "claude_code":
			tool = "claude"
		case "deepseek":
			tool = "dsh"
		}
		if tool != "claude" && tool != "codex" && tool != "dsh" {
			continue
		}
		key, err := modelproviders.KeyByID(root, row.ID)
		if err != nil {
			return nil, err
		}
		model := ""
		if row.Model != nil {
			model = *row.Model
		}
		protocol := "openai"
		if tool == "claude" {
			protocol = "anthropic"
		}
		out = append(out, Provider{ID: "store:" + strconv.FormatInt(row.ID, 10), Name: row.Name, Agent: tool, URL: row.ApiURL, Key: key, HasKey: key != "", Source: "codepluse", Model: model, Protocol: protocol})
	}
	return out, nil
}

func stringValue(v any) string    { s, _ := v.(string); return s }
func object(v any) map[string]any { m, _ := v.(map[string]any); return m }
func userDir(env, suffix string) string {
	if v := os.Getenv(env); v != "" {
		return v
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, suffix)
}

// Native reads API configurations only. Subscription/login tokens are never
// treated as provider keys, and no source file is modified.
func Native() ([]Provider, error) {
	out, dshErr := readDSH(userDir("DSH_HOME", ".dsh"))
	codex, codexErr := readCodex(userDir("CODEX_HOME", ".codex"))
	out = append(out, codex...)
	claude, claudeErr := readClaude(userDir("CLAUDE_CONFIG_DIR", ".claude"))
	out = append(out, claude...)
	var errors []string
	for _, err := range []error{dshErr, codexErr, claudeErr} {
		if err != nil {
			errors = append(errors, err.Error())
		}
	}
	if len(errors) > 0 {
		return out, fmt.Errorf("%s", strings.Join(errors, "；"))
	}
	return out, nil
}

func readDSH(home string) ([]Provider, error) {
	var credentials struct {
		Refs map[string]string `yaml:"refs"`
	}
	if data, err := os.ReadFile(filepath.Join(home, ".credentials.yaml")); err == nil {
		_ = yaml.Unmarshal(data, &credentials)
	}
	out := []Provider{}
	for _, profile := range []string{"desktop", "acp"} {
		data, err := os.ReadFile(filepath.Join(home, "profiles", profile, "cordis.patch.yml"))
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			return out, fmt.Errorf("读取 DSH 配置失败")
		}
		var plugins []struct {
			ID     string         `yaml:"id"`
			Config map[string]any `yaml:"config"`
		}
		if yaml.Unmarshal(data, &plugins) != nil {
			return out, fmt.Errorf("DSH 配置格式无效")
		}
		selectedProvider, selectedModel := "", ""
		for _, p := range plugins {
			if p.ID == "agent-default-model" || p.ID == "acp" {
				selectedProvider = stringValue(p.Config["provider"])
				selectedModel = stringValue(p.Config["model"])
			}
		}
		for _, plugin := range plugins {
			if plugin.ID != "llm-pi-ai" {
				continue
			}
			configs := object(plugin.Config["providers"])
			ids := []string{}
			for id := range configs {
				ids = append(ids, id)
			}
			sort.Strings(ids)
			for _, id := range ids {
				cfg := object(configs[id])
				base := stringValue(cfg["baseURL"])
				if base == "" {
					continue
				}
				key := stringValue(cfg["apiKey"])
				ref := stringValue(cfg["apiKeyEnv"])
				if ref != "" {
					key = os.Getenv(ref)
					if key == "" {
						key = credentials.Refs[ref]
					}
				}
				models := []Model{}
				if raw, ok := cfg["models"].([]any); ok {
					for _, v := range raw {
						m := object(v)
						mid := stringValue(m["id"])
						if mid != "" {
							models = append(models, Model{ID: mid, Name: stringValue(m["name"])})
						}
					}
				}
				model := ""
				if id == selectedProvider {
					model = selectedModel
				}
				if model == "" && len(models) > 0 {
					model = models[0].ID
				}
				protocol := "openai"
				if strings.HasPrefix(stringValue(cfg["api"]), "anthropic") {
					protocol = "anthropic"
				}
				out = append(out, Provider{ID: "native:dsh:" + profile + ":" + id, Name: id + " · DSH " + profile, Agent: "dsh", URL: base, Key: key, HasKey: key != "", Source: "dsh", Model: model, Models: models, Protocol: protocol, Native: cfg})
			}
		}
	}
	return out, nil
}

func readClaude(home string) ([]Provider, error) {
	data, err := os.ReadFile(filepath.Join(home, "settings.json"))
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("读取 Claude 配置失败")
	}
	var cfg struct {
		Env   map[string]string `json:"env"`
		Model string            `json:"model"`
	}
	if json.Unmarshal(data, &cfg) != nil {
		return nil, fmt.Errorf("Claude 配置格式无效")
	}
	base := cfg.Env["ANTHROPIC_BASE_URL"]
	if base == "" && cfg.Env["ANTHROPIC_API_KEY"] != "" {
		base = "https://api.anthropic.com"
	}
	key := cfg.Env["ANTHROPIC_AUTH_TOKEN"]
	if key == "" {
		key = cfg.Env["ANTHROPIC_API_KEY"]
	}
	if base == "" || key == "" {
		return nil, nil
	}
	model := cfg.Env["ANTHROPIC_MODEL"]
	if model == "" {
		model = cfg.Model
	}
	return []Provider{{ID: "native:claude:settings", Name: "Claude 原生 API 配置", Agent: "claude", URL: base, Model: model, Key: key, HasKey: true, Source: "claude", Protocol: "anthropic", Env: cfg.Env, Models: claudeModels(cfg.Env, model)}}, nil
}

func claudeModels(env map[string]string, model string) []Model {
	out := []Model{}
	seen := map[string]bool{}
	for _, id := range []string{model, env["ANTHROPIC_DEFAULT_OPUS_MODEL"], env["ANTHROPIC_DEFAULT_SONNET_MODEL"], env["ANTHROPIC_DEFAULT_HAIKU_MODEL"]} {
		if id != "" && !seen[id] {
			seen[id] = true
			out = append(out, Model{ID: id})
		}
	}
	return out
}

func CodexProvider(id, name, source, config string, auth map[string]any, current bool) (Provider, bool) {
	var cfg map[string]any
	if toml.Unmarshal([]byte(config), &cfg) != nil {
		return Provider{}, false
	}
	selected := stringValue(cfg["model_provider"])
	settings := object(object(cfg["model_providers"])[selected])
	base := stringValue(settings["base_url"])
	key := os.Getenv(stringValue(settings["env_key"]))
	if key == "" {
		key = stringValue(auth["OPENAI_API_KEY"])
	}
	if base == "" || key == "" {
		return Provider{}, false
	}
	model := stringValue(cfg["model"])
	return Provider{ID: id, Name: name, Source: source, Agent: "codex", URL: base, Model: model, Key: key, HasKey: true, CCCurrent: current, Protocol: "openai", Models: []Model{{ID: model}}, Native: settings}, true
}

func readCodex(home string) ([]Provider, error) {
	data, err := os.ReadFile(filepath.Join(home, "config.toml"))
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("读取 Codex 配置失败")
	}
	auth := map[string]any{}
	if b, e := os.ReadFile(filepath.Join(home, "auth.json")); e == nil {
		_ = json.Unmarshal(b, &auth)
	}
	if p, ok := CodexProvider("native:codex:config", "Codex 原生 API 配置", "codex", string(data), auth, false); ok {
		return []Provider{p}, nil
	}
	return nil, nil
}
