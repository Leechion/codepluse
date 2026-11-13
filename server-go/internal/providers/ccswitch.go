package providers

import (
	"database/sql"
	"encoding/json"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strings"

	_ "modernc.org/sqlite"
)

// CCSwitch reads the live database without exposing keys to the frontend.
func CCSwitch() ([]Provider, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, err
	}
	return readCCSwitch(filepath.Join(home, ".cc-switch", "cc-switch.db"))
}
func readCCSwitch(path string) ([]Provider, error) {
	if _, err := os.Stat(path); os.IsNotExist(err) {
		return []Provider{}, nil
	} else if err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", sqliteURL(path, "ro"))
	if err != nil {
		return nil, err
	}
	defer db.Close()
	return readCCSwitchRows(db)
}
func sqliteURL(path, mode string) string {
	filePath := filepath.ToSlash(path)
	if !strings.HasPrefix(filePath, "/") {
		filePath = "/" + filePath
	}
	u := url.URL{Scheme: "file", Path: filePath}
	q := u.Query()
	q.Set("mode", mode)
	u.RawQuery = q.Encode()
	return u.String()
}
func readCCSwitchRows(db *sql.DB) ([]Provider, error) {
	rows, err := db.Query(`SELECT id,name,settings_config,is_current FROM providers WHERE app_type = 'claude' ORDER BY COALESCE(sort_index, 2147483647), name`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []Provider{}
	for rows.Next() {
		var id, name, raw string
		var current bool
		if err = rows.Scan(&id, &name, &raw, &current); err != nil {
			return nil, err
		}
		var cfg struct {
			Env          map[string]any `json:"env"`
			Model        string         `json:"model"`
			ModelCatalog struct {
				Models []struct {
					Model       string `json:"model"`
					DisplayName string `json:"displayName"`
				}
			} `json:"modelCatalog"`
		}
		if err = json.Unmarshal([]byte(raw), &cfg); err != nil {
			return nil, fmt.Errorf("CC Switch 供应商配置无效: %w", err)
		}
		env := map[string]string{}
		for k, v := range cfg.Env {
			if strings.HasPrefix(k, "ANTHROPIC_") || k == "CLAUDE_CODE_SUBAGENT_MODEL" {
				if s, ok := v.(string); ok {
					env[k] = s
				}
			}
		}
		model := env["ANTHROPIC_MODEL"]
		if model == "" {
			model = cfg.Model
		}
		key := env["ANTHROPIC_AUTH_TOKEN"]
		if key == "" {
			key = env["ANTHROPIC_API_KEY"]
		}
		models := claudeModels(env, model)
		if len(cfg.ModelCatalog.Models) > 0 {
			models = []Model{}
			for _, m := range cfg.ModelCatalog.Models {
				if m.Model != "" {
					models = append(models, Model{ID: m.Model, Name: m.DisplayName})
				}
			}
		}
		out = append(out, Provider{ID: "ccswitch:" + id, Name: name, Agent: "claude", URL: env["ANTHROPIC_BASE_URL"], Model: model, Key: key, HasKey: key != "", Source: "ccswitch", CCCurrent: current, Env: env, Protocol: "anthropic", Models: models})
	}
	if err = rows.Err(); err != nil {
		return nil, err
	}
	return out, nil
}
