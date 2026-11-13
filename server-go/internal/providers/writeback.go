package providers

import (
	"database/sql"
	"encoding/json"
	"fmt"
	"github.com/pelletier/go-toml/v2"
	"gopkg.in/yaml.v3"
	"harnesshub/internal/modelproviders"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

// Native formats do not all support model visibility or provider enablement.
// Keep only those UI attributes keyed by the original ID; never duplicate a provider or its key.
type providerMetadata struct {
	Models      []Model `json:"models"`
	Enabled     *bool   `json:"enabled,omitempty"`
	DisplayName string  `json:"displayName,omitempty"`
}

func externalProvider(id string) bool {
	return strings.HasPrefix(id, "native:") || strings.HasPrefix(id, "ccswitch:") || strings.HasPrefix(id, "store:")
}
func metadata(root string) (map[string]providerMetadata, error) {
	out := map[string]providerMetadata{}
	b, err := os.ReadFile(filepath.Join(root, ".data", "provider-metadata.json"))
	if os.IsNotExist(err) {
		return out, nil
	}
	if err != nil {
		return nil, err
	}
	err = json.Unmarshal(b, &out)
	return out, err
}
func applyMetadata(root string, p *Provider) error {
	if !externalProvider(p.ID) {
		return nil
	}
	data, err := metadata(root)
	if err != nil {
		return err
	}
	if m, ok := data[p.ID]; ok {
		if m.Models != nil {
			p.Models = m.Models
		}
		p.Enabled = m.Enabled
		if m.DisplayName != "" {
			p.Name = m.DisplayName
		}
	}
	return nil
}
func replaceFile(file string, data []byte) error {
	f, err := os.CreateTemp(filepath.Dir(file), ".codepluse-save-*")
	if err != nil {
		return err
	}
	name := f.Name()
	defer os.Remove(name)
	if err = f.Chmod(0600); err == nil {
		_, err = f.Write(data)
	}
	closeErr := f.Close()
	if err != nil {
		return err
	}
	if closeErr != nil {
		return closeErr
	}
	return os.Rename(name, file)
}
func saveExternal(root string, p Provider) (Provider, error) {
	old, err := Find(root, p.ID)
	if err != nil {
		return p, err
	}
	if old.Agent != p.Agent {
		return p, fmt.Errorf("不能更改供应商所属执行端")
	}
	keyChanged := p.Key != ""
	if !keyChanged {
		p.Key = old.Key
	}
	if p.Key == "" {
		return p, fmt.Errorf("请输入 API 密钥")
	}
	if p.Models == nil {
		p.Models = old.Models
	}
	if p.Enabled == nil {
		p.Enabled = old.Enabled
	}
	p.Source = old.Source
	p.CCCurrent = old.CCCurrent
	mu.Lock()
	defer mu.Unlock()
	meta, err := metadata(root)
	if err != nil {
		return p, err
	}
	switch {
	case strings.HasPrefix(p.ID, "ccswitch:"):
		if p.Agent != "claude" {
			return p, fmt.Errorf("只有 Claude Code 使用 CC Switch")
		}
		err = writeCCSwitch(p, keyChanged)
	case strings.HasPrefix(p.ID, "native:codex:"):
		err = writeCodex(p, keyChanged)
	case strings.HasPrefix(p.ID, "native:dsh:"):
		err = writeDSH(p, keyChanged)
	case strings.HasPrefix(p.ID, "store:"):
		id, e := strconv.ParseInt(strings.TrimPrefix(p.ID, "store:"), 10, 64)
		if e != nil {
			return p, e
		}
		_, _, err = modelproviders.Update(root, id, &p.Name, &p.URL, &p.Key, nil, &p.Model)
	default:
		err = fmt.Errorf("该来源暂不支持直接修改")
	}
	if err != nil {
		return p, fmt.Errorf("保存供应商修改失败: %w", err)
	}
	m := providerMetadata{Models: p.Models, Enabled: p.Enabled}
	if strings.HasPrefix(p.ID, "native:dsh:") {
		m.DisplayName = p.Name
	}
	meta[p.ID] = m
	b, err := json.MarshalIndent(meta, "", "  ")
	if err != nil {
		return p, err
	}
	dir := filepath.Join(root, ".data")
	if err = os.MkdirAll(dir, 0700); err != nil {
		return p, err
	}
	err = replaceFile(filepath.Join(dir, "provider-metadata.json"), b)
	return p, err
}
func writeCCSwitch(p Provider, keyChanged bool) error {
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	file := filepath.Join(home, ".cc-switch", "cc-switch.db")
	// mode=rw refuses to create a replacement database if the source disappeared.
	db, err := sql.Open("sqlite", sqliteURL(file, "rw"))
	if err != nil {
		return err
	}
	defer db.Close()
	if _, err = db.Exec("PRAGMA busy_timeout=5000"); err != nil {
		return err
	}
	tx, err := db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	id := strings.TrimPrefix(p.ID, "ccswitch:")
	var raw string
	if err = tx.QueryRow("SELECT settings_config FROM providers WHERE id=? AND app_type='claude'", id).Scan(&raw); err != nil {
		return err
	}
	cfg := map[string]any{}
	if err = json.Unmarshal([]byte(raw), &cfg); err != nil {
		return fmt.Errorf("CC Switch 配置格式无效")
	}
	env := object(cfg["env"])
	if env == nil {
		env = map[string]any{}
		cfg["env"] = env
	}
	env["ANTHROPIC_BASE_URL"] = p.URL
	env["ANTHROPIC_MODEL"] = p.Model
	if _, ok := cfg["model"]; ok {
		cfg["model"] = p.Model
	}
	if keyChanged {
		if _, ok := env["ANTHROPIC_AUTH_TOKEN"]; ok || env["ANTHROPIC_API_KEY"] == nil {
			env["ANTHROPIC_AUTH_TOKEN"] = p.Key
		}
		if _, ok := env["ANTHROPIC_API_KEY"]; ok {
			env["ANTHROPIC_API_KEY"] = p.Key
		}
	}
	catalog := object(cfg["modelCatalog"])
	if catalog == nil {
		catalog = map[string]any{}
		cfg["modelCatalog"] = catalog
	}
	existingModels := map[string]map[string]any{}
	if rows, ok := catalog["models"].([]any); ok {
		for _, v := range rows {
			m := object(v)
			existingModels[stringValue(m["model"])] = m
		}
	}
	models := []map[string]any{}
	for _, m := range p.Models {
		row := existingModels[m.ID]
		if row == nil {
			row = map[string]any{}
		}
		row["model"] = m.ID
		row["displayName"] = m.Name
		models = append(models, row)
	}
	catalog["models"] = models
	b, err := json.Marshal(cfg)
	if err != nil {
		return err
	}
	if _, err = tx.Exec("UPDATE providers SET name=?, settings_config=? WHERE id=? AND app_type='claude'", p.Name, string(b), id); err != nil {
		return err
	}
	return tx.Commit()
}
func writeCodex(p Provider, keyChanged bool) error {
	file := filepath.Join(NativeHome("codex"), "config.toml")
	b, err := os.ReadFile(file)
	if err != nil {
		return err
	}
	cfg := map[string]any{}
	if err = toml.Unmarshal(b, &cfg); err != nil {
		return err
	}
	name := strings.TrimPrefix(p.ID, "native:codex:provider:")
	if p.ID == "native:codex:config" {
		name = stringValue(cfg["model_provider"])
	}
	entry := object(object(cfg["model_providers"])[name])
	if entry == nil {
		return fmt.Errorf("Codex 供应商已不存在")
	}
	entry["name"] = p.Name
	entry["base_url"] = p.URL
	entry["model"] = p.Model
	entry["wire_api"] = "responses"
	if stringValue(cfg["model_provider"]) == name {
		cfg["model"] = p.Model
	}
	if keyChanged {
		delete(entry, "env_key")
		entry["experimental_bearer_token"] = p.Key
	}
	b, err = toml.Marshal(cfg)
	if err != nil {
		return err
	}
	return replaceFile(file, b)
}
func writeDSH(p Provider, keyChanged bool) error {
	parts := strings.SplitN(strings.TrimPrefix(p.ID, "native:dsh:"), ":", 2)
	if len(parts) != 2 || (parts[0] != "desktop" && parts[0] != "acp") {
		return fmt.Errorf("DSH 供应商标识无效")
	}
	file := filepath.Join(NativeHome("dsh"), "profiles", parts[0], "cordis.patch.yml")
	b, err := os.ReadFile(file)
	if err != nil {
		return err
	}
	var plugins []map[string]any
	if err = yaml.Unmarshal(b, &plugins); err != nil {
		return err
	}
	found := false
	for _, plugin := range plugins {
		cfg := object(plugin["config"])
		switch stringValue(plugin["id"]) {
		case "llm-pi-ai":
			entry := object(object(cfg["providers"])[parts[1]])
			if entry == nil {
				continue
			}
			found = true
			entry["baseURL"] = p.URL
			api := map[string]string{"openai": "openai-completions", "openai-responses": "openai-responses", "anthropic": "anthropic-messages"}
			entry["api"] = api[p.Protocol]
			if keyChanged {
				delete(entry, "apiKeyEnv")
				entry["apiKey"] = p.Key
			}
			existing := map[string]map[string]any{}
			if rows, ok := entry["models"].([]any); ok {
				for _, v := range rows {
					m := object(v)
					existing[stringValue(m["id"])] = m
				}
			}
			models := []map[string]any{}
			for _, m := range p.Models {
				row := existing[m.ID]
				if row == nil {
					row = map[string]any{}
				}
				row["id"] = m.ID
				row["name"] = m.Name
				models = append(models, row)
			}
			entry["models"] = models
		case "agent-default-model", "acp":
			if stringValue(cfg["provider"]) == parts[1] {
				cfg["model"] = p.Model
			}
		}
	}
	if !found {
		return fmt.Errorf("DSH 供应商已不存在")
	}
	b, err = yaml.Marshal(plugins)
	if err != nil {
		return err
	}
	return replaceFile(file, b)
}
