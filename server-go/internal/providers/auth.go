package providers

import (
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

type AccountInfo struct {
	Supported    bool    `json:"supported"`
	Configured   bool    `json:"configured"`
	Managed      bool    `json:"managed,omitempty"`
	Source       string  `json:"source"`
	LoginCommand string  `json:"loginCommand,omitempty"`
	Models       []Model `json:"models"`
}

func NativeHome(tool string) string {
	switch tool {
	case "claude":
		return userDir("CLAUDE_CONFIG_DIR", ".claude")
	case "codex":
		return userDir("CODEX_HOME", ".codex")
	case "dsh":
		return userDir("DSH_HOME", ".dsh")
	case "workbuddy":
		return userDir("CODEBUDDY_CONFIG_DIR", ".workbuddy")
	case "custom:qoder", "qoder":
		if dir := os.Getenv("QODER_CONFIG_DIR"); dir != "" {
			return dir
		}
		base := os.Getenv("QODER_CLI_HOME")
		if base == "" {
			base, _ = os.UserHomeDir()
		}
		name := os.Getenv("QODER_CONFIG_DIR_NAME")
		if name == "" {
			name = ".qoder"
		}
		return filepath.Join(base, name)
	}
	return ""
}

func SupportsSubscription(tool string) bool {
	return tool == "claude" || tool == "codex" || tool == "workbuddy" || tool == "qoder" || tool == "custom:qoder"
}

// Authentication metadata contains no token, API key, account ID or email.
// A credential file being present is deliberately not an online login check.
func Account(tool string) AccountInfo {
	info := AccountInfo{Supported: SupportsSubscription(tool), Source: NativeHome(tool), Models: []Model{}}
	switch tool {
	case "codex":
		info.LoginCommand = "codex login"
		var auth map[string]any
		if b, err := os.ReadFile(filepath.Join(info.Source, "auth.json")); err == nil && json.Unmarshal(b, &auth) == nil {
			info.Configured = stringValue(object(auth["tokens"])["access_token"]) != ""
		}
		var cache struct {
			Models []struct {
				Slug string `json:"slug"`
				Name string `json:"display_name"`
			} `json:"models"`
		}
		if b, err := os.ReadFile(filepath.Join(info.Source, "models_cache.json")); err == nil && json.Unmarshal(b, &cache) == nil {
			for _, m := range cache.Models {
				if m.Slug != "" {
					info.Models = append(info.Models, Model{ID: m.Slug, Name: m.Name})
				}
			}
		}
	case "claude":
		info.LoginCommand = "claude auth login"
		var auth map[string]any
		if b, err := os.ReadFile(filepath.Join(info.Source, ".credentials.json")); err == nil && json.Unmarshal(b, &auth) == nil {
			info.Configured = stringValue(object(auth["claudeAiOauth"])["accessToken"]) != ""
		}
		info.Configured = info.Configured || os.Getenv("CLAUDE_CODE_OAUTH_TOKEN") != ""
	case "workbuddy", "qoder", "custom:qoder":
		info.Managed = true
	}
	return info
}

func AccountVersion(tool string) string {
	name := "auth.json"
	if tool == "claude" {
		name = ".credentials.json"
	}
	b, _ := os.ReadFile(filepath.Join(NativeHome(tool), name))
	if tool == "claude" {
		b = append(b, []byte(os.Getenv("CLAUDE_CODE_OAUTH_TOKEN"))...)
	}
	return fmt.Sprintf("%x", sha256.Sum256(b))
}

func ClearAPIEnvironment(env []string) []string {
	out := []string{}
	for _, entry := range env {
		key, _, _ := strings.Cut(entry, "=")
		key = strings.ToUpper(key)
		if strings.HasPrefix(key, "ANTHROPIC_") || key == "CLAUDE_CODE_SUBAGENT_MODEL" || key == "OPENAI_API_KEY" || key == "OPENAI_BASE_URL" || key == "CODEX_API_KEY" || key == "CODEPLUSE_CODEX_API_KEY" || key == "CODEPLUSE_DSH_API_KEY" || key == "CODEBUDDY_API_KEY" || key == "CODEBUDDY_BASE_URL" {
			continue
		}
		out = append(out, entry)
	}
	return out
}
