package providers

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

type Provider struct {
	ID        string            `json:"id"`
	Name      string            `json:"name"`
	Agent     string            `json:"agent"`
	URL       string            `json:"url"`
	Model     string            `json:"model"`
	Key       string            `json:"key,omitempty"`
	HasKey    bool              `json:"hasKey"`
	Source    string            `json:"source,omitempty"`
	CCCurrent bool              `json:"ccCurrent,omitempty"`
	Env       map[string]string `json:"-"`
}
type Data struct {
	Providers []Provider        `json:"providers"`
	Active    map[string]string `json:"active"`
	CCError   string            `json:"ccError,omitempty"`
}

var mu sync.Mutex

func path(root string) string { return filepath.Join(root, ".data", "api-providers.json") }
func read(root string) (Data, error) {
	d := Data{Providers: []Provider{}, Active: map[string]string{}}
	b, e := os.ReadFile(path(root))
	if os.IsNotExist(e) {
		return d, nil
	}
	if e != nil {
		return d, e
	}
	if e = json.Unmarshal(b, &d); e != nil {
		return d, e
	}
	if d.Active == nil {
		d.Active = map[string]string{}
	}
	return d, nil
}
func write(root string, d Data) error {
	if e := os.MkdirAll(filepath.Dir(path(root)), 0700); e != nil {
		return e
	}
	b, e := json.MarshalIndent(d, "", "  ")
	if e != nil {
		return e
	}
	return os.WriteFile(path(root), b, 0600)
}
func Public(root string) (Data, error) {
	mu.Lock()
	defer mu.Unlock()
	d, e := read(root)
	for i := range d.Providers {
		d.Providers[i].HasKey = d.Providers[i].Key != ""
		d.Providers[i].Key = ""
	}
	cc, ccErr := CCSwitch()
	if ccErr != nil {
		d.CCError = "无法读取 CC Switch 数据库: " + ccErr.Error()
	} else {
		d.Providers = append(d.Providers, cc...)
	}
	return d, e
}
func Save(root string, p Provider) error {
	mu.Lock()
	defer mu.Unlock()
	p.Name = strings.TrimSpace(p.Name)
	p.URL = strings.TrimRight(strings.TrimSpace(p.URL), "/")
	p.Model = strings.TrimSpace(p.Model)
	u, e := url.Parse(p.URL)
	if p.Name == "" || p.Model == "" || e != nil || u.Host == "" || (u.Scheme != "http" && u.Scheme != "https") || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (p.Agent != "claude" && p.Agent != "dsh") {
		return fmt.Errorf("请填写名称、模型和有效的 HTTP(S) 地址，并选择执行端")
	}
	d, e := read(root)
	if e != nil {
		return e
	}
	for i, old := range d.Providers {
		if old.ID == p.ID && p.ID != "" {
			if old.Agent != p.Agent {
				return fmt.Errorf("不能更改供应商所属执行端")
			}
			if p.Key == "" {
				p.Key = old.Key
			}
			p.HasKey = false
			d.Providers[i] = p
			return write(root, d)
		}
	}
	if p.ID != "" {
		return fmt.Errorf("供应商不存在")
	}
	if p.Key == "" {
		return fmt.Errorf("请输入 API 密钥")
	}
	b := make([]byte, 12)
	if _, e = rand.Read(b); e != nil {
		return e
	}
	p.ID = hex.EncodeToString(b)
	p.HasKey = false
	d.Providers = append(d.Providers, p)
	return write(root, d)
}
func Activate(root, agent, id string) error {
	mu.Lock()
	defer mu.Unlock()
	if agent != "claude" && agent != "dsh" {
		return fmt.Errorf("执行端无效")
	}
	d, e := read(root)
	if e != nil {
		return e
	}
	if id != "" {
		ok := false
		for _, p := range d.Providers {
			if p.ID == id && p.Agent == agent {
				ok = true
			}
		}
		if !ok && agent == "claude" && strings.HasPrefix(id, "ccswitch:") {
			cc, err := CCSwitch()
			if err != nil {
				return err
			}
			for _, p := range cc {
				if p.ID == id && (p.HasKey || p.CCCurrent) {
					ok = true
				}
			}
		}
		if !ok {
			return fmt.Errorf("供应商不存在")
		}
	}
	d.Active[agent] = id
	return write(root, d)
}
func Delete(root, id string) error {
	mu.Lock()
	defer mu.Unlock()
	d, e := read(root)
	if e != nil {
		return e
	}
	for i, p := range d.Providers {
		if p.ID == id {
			d.Providers = append(d.Providers[:i], d.Providers[i+1:]...)
			if d.Active[p.Agent] == id {
				delete(d.Active, p.Agent)
			}
			return write(root, d)
		}
	}
	return fmt.Errorf("供应商不存在")
}
func Active(root, agent string) (Provider, bool, error) {
	mu.Lock()
	defer mu.Unlock()
	d, e := read(root)
	if e != nil {
		return Provider{}, false, e
	}
	if agent == "claude" && (d.Active[agent] == "" || strings.HasPrefix(d.Active[agent], "ccswitch:")) {
		cc, err := CCSwitch()
		if err != nil {
			return Provider{}, false, fmt.Errorf("读取 CC Switch 失败: %w", err)
		}
		for _, p := range cc {
			if p.ID == d.Active[agent] || (d.Active[agent] == "" && p.CCCurrent) {
				if d.Active[agent] != "" && !p.HasKey && !p.CCCurrent {
					return Provider{}, false, fmt.Errorf("该 CC Switch 供应商没有 API 密钥，请先在 CC Switch 中切换到它")
				}
				return p, true, nil
			}
		}
		if d.Active[agent] != "" {
			return Provider{}, false, fmt.Errorf("所选 CC Switch 供应商不存在")
		}
	}
	for _, p := range d.Providers {
		if p.Agent == agent && p.ID == d.Active[agent] {
			return p, true, nil
		}
	}
	return Provider{}, false, nil
}
