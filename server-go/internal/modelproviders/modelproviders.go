// Package modelproviders 管理设置页「模型供应商」分区（list/create/update/
// delete_model_provider 四个命令）的持久化。数据存 .data/model-providers.json，
// 与执行端供应商（providers 包，api-providers.json）互不通用。
package modelproviders

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

// Info 对应前端 lib/types.ts 的 ModelProviderInfo（snake_case 字段直传）。
// ApiKey 仅在创建/更新的返回值里带回明文，列表只给掩码。
type Info struct {
	ID           int64  `json:"id"`
	Name         string `json:"name"`
	ApiURL       string `json:"api_url"`
	ApiKey       string `json:"api_key,omitempty"`
	ApiKeyMasked string `json:"api_key_masked"`
	AgentType    string `json:"agent_type"`
	Model        *string `json:"model"`
	CreatedAt    string `json:"created_at"`
	UpdatedAt    string `json:"updated_at"`
}

// entry 是落盘形态：明文密钥只存本地文件，不出网。
type entry struct {
	Info
	Key string `json:"key"`
}

type store struct {
	NextID   int64   `json:"nextId"`
	Entries  []entry `json:"entries"`
}

var mu sync.Mutex

func path(root string) string { return filepath.Join(root, ".data", "model-providers.json") }

func read(root string) (store, error) {
	s := store{NextID: 1}
	b, e := os.ReadFile(path(root))
	if os.IsNotExist(e) {
		return s, nil
	}
	if e != nil {
		return s, e
	}
	if e = json.Unmarshal(b, &s); e != nil {
		return s, e
	}
	if s.NextID < 1 {
		s.NextID = 1
	}
	return s, nil
}

func write(root string, s store) error {
	if e := os.MkdirAll(filepath.Dir(path(root)), 0700); e != nil {
		return e
	}
	b, e := json.MarshalIndent(s, "", "  ")
	if e != nil {
		return e
	}
	return os.WriteFile(path(root), b, 0600)
}

// mask 掩码展示：保留前 4 后 4，过短全打码。
func mask(key string) string {
	if key == "" {
		return ""
	}
	if len(key) <= 8 {
		return strings.Repeat("•", len(key))
	}
	return key[:4] + "…" + key[len(key)-4:]
}

func public(e entry) Info {
	info := e.Info
	info.ApiKey = ""
	info.ApiKeyMasked = mask(e.Key)
	return info
}

func validate(name, apiURL, agentType string) error {
	name = strings.TrimSpace(name)
	if name == "" {
		return fmt.Errorf("供应商名称不能为空")
	}
	u := strings.TrimSpace(apiURL)
	if !strings.HasPrefix(u, "http://") && !strings.HasPrefix(u, "https://") {
		return fmt.Errorf("API 地址必须是 http(s) 地址")
	}
	if strings.TrimSpace(agentType) == "" {
		return fmt.Errorf("请选择智能体类型")
	}
	return nil
}

// List 返回全部供应商（密钥掩码）。
func List(root string) ([]Info, error) {
	mu.Lock()
	defer mu.Unlock()
	s, e := read(root)
	if e != nil {
		return nil, e
	}
	out := make([]Info, 0, len(s.Entries))
	for _, en := range s.Entries {
		out = append(out, public(en))
	}
	return out, nil
}

// Create 新建供应商并返回带明文密钥的完整信息（创建表单需要回显 hasKey 态，
// 前端随即重新拉列表，明文不出本响应之外的任何缓存）。
func Create(root, name, apiURL, apiKey, agentType string, model *string) (Info, error) {
	mu.Lock()
	defer mu.Unlock()
	if e := validate(name, apiURL, agentType); e != nil {
		return Info{}, e
	}
	if strings.TrimSpace(apiKey) == "" {
		return Info{}, fmt.Errorf("API 密钥不能为空")
	}
	s, e := read(root)
	if e != nil {
		return Info{}, e
	}
	now := time.Now().UTC().Format(time.RFC3339)
	en := entry{
		Info: Info{
			ID:        s.NextID,
			Name:      strings.TrimSpace(name),
			ApiURL:    strings.TrimRight(strings.TrimSpace(apiURL), "/"),
			AgentType: strings.TrimSpace(agentType),
			Model:     model,
			CreatedAt: now,
			UpdatedAt: now,
		},
		Key: strings.TrimSpace(apiKey),
	}
	s.NextID++
	s.Entries = append(s.Entries, en)
	if e := write(root, s); e != nil {
		return Info{}, e
	}
	return en.Info, nil
}

// Update 部分更新：nil 字段保持原值；apiKey 传空串表示清除密钥。
// affectedRunningSessions 恒为 0（Go 移植暂无会话级凭据级联）。
func Update(root string, id int64, name, apiURL, apiKey, agentType, model *string) (Info, int, error) {
	mu.Lock()
	defer mu.Unlock()
	s, e := read(root)
	if e != nil {
		return Info{}, 0, e
	}
	for i := range s.Entries {
		if s.Entries[i].ID != id {
			continue
		}
		en := &s.Entries[i]
		if name != nil {
			en.Name = strings.TrimSpace(*name)
		}
		if apiURL != nil {
			en.ApiURL = strings.TrimRight(strings.TrimSpace(*apiURL), "/")
		}
		if agentType != nil {
			en.AgentType = strings.TrimSpace(*agentType)
		}
		if model != nil {
			m := strings.TrimSpace(*model)
			en.Model = &m
		}
		if apiKey != nil {
			en.Key = strings.TrimSpace(*apiKey)
		}
		if e := validate(en.Name, en.ApiURL, en.AgentType); e != nil {
			return Info{}, 0, e
		}
		en.UpdatedAt = time.Now().UTC().Format(time.RFC3339)
		if e := write(root, s); e != nil {
			return Info{}, 0, e
		}
		return en.Info, 0, nil
	}
	return Info{}, 0, fmt.Errorf("供应商不存在")
}

// Delete 删除供应商；不存在时报错。
func Delete(root string, id int64) error {
	mu.Lock()
	defer mu.Unlock()
	s, e := read(root)
	if e != nil {
		return e
	}
	for i := range s.Entries {
		if s.Entries[i].ID == id {
			s.Entries = append(s.Entries[:i], s.Entries[i+1:]...)
			return write(root, s)
		}
	}
	return fmt.Errorf("供应商不存在")
}

// KeyByID 取回供应商的明文密钥（模型列表等需要真实密钥的本地调用使用）。
func KeyByID(root string, id int64) (string, error) {
	mu.Lock()
	defer mu.Unlock()
	s, e := read(root)
	if e != nil {
		return "", e
	}
	for _, en := range s.Entries {
		if en.ID == id {
			return en.Key, nil
		}
	}
	return "", fmt.Errorf("供应商不存在")
}
