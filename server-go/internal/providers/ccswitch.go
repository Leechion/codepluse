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

// CCSwitch reads the live database without modifying CC Switch or copying its keys.
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
	filePath := filepath.ToSlash(path)
	if !strings.HasPrefix(filePath, "/") {
		filePath = "/" + filePath
	}
	u := url.URL{Scheme: "file", Path: filePath}
	q := u.Query()
	q.Set("mode", "ro")
	u.RawQuery = q.Encode()
	db, err := sql.Open("sqlite", u.String())
	if err != nil {
		return nil, err
	}
	defer db.Close()
	rows, err := db.Query(`SELECT id,name,settings_config,is_current FROM providers WHERE app_type='claude' ORDER BY COALESCE(sort_index, 2147483647), name`)
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
			Env   map[string]any `json:"env"`
			Model string         `json:"model"`
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
		out = append(out, Provider{ID: "ccswitch:" + id, Name: name, Agent: "claude", URL: env["ANTHROPIC_BASE_URL"], Model: model, HasKey: key != "", Source: "ccswitch", CCCurrent: current, Env: env})
	}
	if err = rows.Err(); err != nil {
		return nil, err
	}
	return out, nil
}
