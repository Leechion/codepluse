package main

import (
	"crypto/sha256"
	"fmt"
	"gopkg.in/yaml.v3"
	"os"
	"path/filepath"
)

func dshOriginalHome() string {
	if v := os.Getenv("DSH_HOME"); v != "" {
		return v
	}
	return filepath.Join(home(), ".dsh")
}
func dshModelVersion() string {
	h := sha256.New()
	for _, p := range []string{filepath.Join(dshOriginalHome(), "profiles", "desktop", "cordis.patch.yml"), filepath.Join(dshOriginalHome(), ".credentials.yaml")} {
		b, e := os.ReadFile(p)
		if e == nil {
			h.Write(b)
		} else {
			h.Write([]byte(e.Error()))
		}
	}
	return fmt.Sprintf("%x", h.Sum(nil))
}
func dshDesktopPatch() (string, error) {
	original := dshOriginalHome()
	file := filepath.Join(original, "profiles", "desktop", "cordis.patch.yml")
	b, err := os.ReadFile(file)
	if os.IsNotExist(err) {
		return "", nil
	}
	if err != nil {
		return "", fmt.Errorf("读取 DeepSeek Harness 模型配置失败: %w", err)
	}
	var doc yaml.Node
	if err = yaml.Unmarshal(b, &doc); err != nil {
		return "", fmt.Errorf("解析 DeepSeek Harness 模型配置失败: %w", err)
	}
	if len(doc.Content) == 0 || doc.Content[0].Kind != yaml.SequenceNode {
		return "", fmt.Errorf("DeepSeek Harness 配置格式无效")
	}
	var selected []*yaml.Node
	var selection struct {
		Provider string `yaml:"provider"`
		Model    string `yaml:"model"`
	}
	for _, item := range doc.Content[0].Content {
		if item.Kind != yaml.MappingNode {
			continue
		}
		var id string
		var config *yaml.Node
		for i := 0; i+1 < len(item.Content); i += 2 {
			switch item.Content[i].Value {
			case "id":
				id = item.Content[i+1].Value
			case "config":
				config = item.Content[i+1]
			}
		}
		if (id == "llm-pi-ai" || id == "llm-deepseek" || id == "agent-default-model") && config != nil {
			selected = append(selected, item)
		}
		if id == "agent-default-model" && config != nil {
			if err = config.Decode(&selection); err != nil {
				return "", fmt.Errorf("解析 DeepSeek 模型选择失败: %w", err)
			}
		}
	}
	if selection.Provider == "" || selection.Model == "" {
		return "", nil
	}
	var acp yaml.Node
	if err = acp.Encode(map[string]any{"id": "acp", "config": map[string]string{"provider": selection.Provider, "model": selection.Model}}); err != nil {
		return "", err
	}
	selected = append(selected, &acp)
	out, err := yaml.Marshal(selected)
	if err != nil {
		return "", err
	}
	dir := filepath.Join(ROOT, ".data", "dsh-model-patches")
	if err = os.MkdirAll(dir, 0700); err != nil {
		return "", err
	}
	hash := sha256.Sum256(out)
	dest := filepath.Join(dir, fmt.Sprintf("desktop-%x.yml", hash[:8]))
	if err = os.WriteFile(dest, out, 0600); err != nil {
		return "", err
	}
	// The isolated ACP profile needs the same managed credentials as the desktop.
	local := filepath.Join(ROOT, ".data", "dsh-home")
	if os.Getenv("DSH_HOME") == "" {
		if info, e := os.Stat(local); e == nil && info.IsDir() {
			source := filepath.Join(original, ".credentials.yaml")
			secret, e := os.ReadFile(source)
			if e == nil {
				if e = os.WriteFile(filepath.Join(local, ".credentials.yaml"), secret, 0600); e != nil {
					return "", fmt.Errorf("同步 DeepSeek Harness 凭据失败: %w", e)
				}
			} else if !os.IsNotExist(e) {
				return "", fmt.Errorf("读取 DeepSeek Harness 凭据失败: %w", e)
			}
		}
	}
	return dest, nil
}
