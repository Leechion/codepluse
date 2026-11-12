// list_provider_models：拉取供应商网关的模型列表。协议与响应形状对齐前端
// lib/types.ts 的 ProviderModelsRequest / ProviderModelList。
package codeg

import (
	"encoding/json"
	"fmt"
	"harnesshub/internal/modelproviders"
	"harnesshub/internal/providers"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"strings"
	"time"
)

// outboundProxy 解析出站代理：环境变量优先，其次 Windows 用户代理设置
// （很多网关在本机需要经代理访问）。
func outboundProxy() string {
	if p := strings.TrimSpace(os.Getenv("HTTPS_PROXY")); p != "" {
		return p
	}
	if p := strings.TrimSpace(os.Getenv("HTTP_PROXY")); p != "" {
		return p
	}
	k, e := exec.Command(
		"reg",
		"query",
		`HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`,
		"/v", "ProxyServer",
	).Output()
	if e != nil {
		return ""
	}
	// 输出形如 "    ProxyServer    REG_SZ    127.0.0.1:7897"。
	server := ""
	for _, line := range strings.Split(string(k), "\n") {
		if strings.Contains(line, "ProxyServer") {
			parts := strings.Fields(line)
			if len(parts) >= 3 {
				server = parts[len(parts)-1]
			}
			break
		}
	}
	if e != nil {
		return ""
	}
	server = strings.TrimSpace(server)
	if server == "" {
		return ""
	}
	if !strings.Contains(server, "=") {
		if !strings.Contains(server, "://") {
			server = "http://" + server
		}
		return server
	}
	// "http=…;https=…" 形式：优先取 https，再退 http。
	for _, part := range strings.Split(server, ";") {
		if strings.HasPrefix(strings.ToLower(part), "https=") {
			return strings.TrimPrefix(part[len("https="):], "//")
		}
	}
	for _, part := range strings.Split(server, ";") {
		if strings.HasPrefix(strings.ToLower(part), "http=") {
			return strings.TrimPrefix(part[len("http="):], "//")
		}
	}
	return ""
}

type listedModel struct {
	ID            string `json:"id"`
	DisplayName   string `json:"displayName,omitempty"`
	ContextWindow int64  `json:"contextWindow,omitempty"`
}

// resolveModelListKey 由 keySource 解出访问网关用的密钥。支持内联密钥、本地
// 模型供应商 store、执行端 store 与 CC Switch；其余来源（pi/opencode）未移植。
func (s *Server) resolveModelListKey(keySource map[string]any) (string, error) {
	switch text(keySource["kind"]) {
	case "inline":
		return text(keySource["apiKey"]), nil
	case "provider":
		id := int64(number(keySource["providerId"]))
		if id == 0 {
			return "", fmt.Errorf("providerId 无效")
		}
		key, e := modelproviders.KeyByID(s.Root, id)
		if e != nil {
			// 旧执行端 store（api-providers.json）也接受同一数字 id 语义之外的需求不大，
			// 这里直接回落 CC Switch（前端不会用数字 id 指向它）。
			return "", e
		}
		return key, nil
	case "ccswitch":
		p, e := providers.Find(s.Root, text(keySource["providerId"]))
		if e != nil {
			return "", e
		}
		key := p.Env["ANTHROPIC_AUTH_TOKEN"]
		if key == "" {
			key = p.Env["ANTHROPIC_API_KEY"]
		}
		return key, nil
	default:
		return "", fmt.Errorf("暂不支持该密钥来源: %s", text(keySource["kind"]))
	}
}

func (s *Server) listProviderModels(a Row) (any, error) {
	params, _ := a["params"].(map[string]any)
	if params == nil {
		return nil, fmt.Errorf("缺少 params")
	}
	protocol := text(params["protocol"])
	baseURL := strings.TrimRight(strings.TrimSpace(text(params["baseUrl"])), "/")
	if baseURL == "" || (!strings.HasPrefix(baseURL, "http://") && !strings.HasPrefix(baseURL, "https://")) {
		return nil, fmt.Errorf("API 地址必须是 http(s) 地址")
	}
	keySource, _ := params["keySource"].(map[string]any)
	key, e := s.resolveModelListKey(keySource)
	if e != nil {
		return nil, e
	}

	var listURL string
	headers := map[string]string{}
	switch protocol {
	case "openai":
		listURL = baseURL + "/models"
		headers["Authorization"] = "Bearer " + key
	case "anthropic":
		listURL = baseURL + "/v1/models"
		if strings.TrimSpace(key) != "" {
			headers["x-api-key"] = key
		}
		headers["anthropic-version"] = "2023-06-01"
	case "gemini":
		listURL = baseURL + "/v1beta/models"
		headers["x-goog-api-key"] = key
	default:
		return nil, fmt.Errorf("暂不支持该协议: %s", protocol)
	}
	// 非默认密钥传递方式：覆盖协议默认头。
	switch text(params["keyHeader"]) {
	case "bearer":
		delete(headers, "x-api-key")
		delete(headers, "x-goog-api-key")
		headers["Authorization"] = "Bearer " + key
	case "x_api_key":
		delete(headers, "Authorization")
		headers["x-api-key"] = key
	case "x_goog_api_key":
		headers["x-goog-api-key"] = key
	}

	client := &http.Client{Timeout: 12 * time.Second}
	if transport, ok := http.DefaultTransport.(*http.Transport); ok {
		t := transport.Clone()
		if proxy := outboundProxy(); proxy != "" {
			if u, e := url.Parse(proxy); e == nil {
				t.Proxy = func(*http.Request) (*url.URL, error) { return u, nil }
			}
		}
		client.Transport = t
	}
	req, e := http.NewRequest(http.MethodGet, listURL, nil)
	if e != nil {
		return nil, e
	}
	for k, v := range headers {
		if v != "" {
			req.Header.Set(k, v)
		}
	}
	resp, e := client.Do(req)
	if e != nil {
		return nil, fmt.Errorf("无法访问模型列表: %w", e)
	}
	defer resp.Body.Close()
	body, e := io.ReadAll(io.LimitReader(resp.Body, 4<<20))
	if e != nil {
		return nil, e
	}
	if resp.StatusCode >= 400 {
		snippet := strings.TrimSpace(string(body))
		if len(snippet) > 200 {
			snippet = snippet[:200]
		}
		return nil, fmt.Errorf("模型列表请求失败（HTTP %d）: %s", resp.StatusCode, snippet)
	}

	models := []listedModel{}
	switch protocol {
	case "gemini":
		var parsed struct {
			Models []struct {
				Name        string `json:"name"`
				DisplayName string `json:"displayName"`
			} `json:"models"`
		}
		if e := json.Unmarshal(body, &parsed); e != nil {
			return nil, fmt.Errorf("模型列表响应无效: %w", e)
		}
		for _, m := range parsed.Models {
			id := strings.TrimPrefix(m.Name, "models/")
			if id == "" {
				continue
			}
			models = append(models, listedModel{ID: id, DisplayName: m.DisplayName})
		}
	default:
		var parsed struct {
			Data []struct {
				ID            string `json:"id"`
				DisplayName   string `json:"display_name"`
				ContextWindow int64  `json:"context_window"`
			} `json:"data"`
		}
		if e := json.Unmarshal(body, &parsed); e != nil {
			return nil, fmt.Errorf("模型列表响应无效: %w", e)
		}
		for _, m := range parsed.Data {
			if m.ID == "" {
				continue
			}
			models = append(models, listedModel{ID: m.ID, DisplayName: m.DisplayName, ContextWindow: m.ContextWindow})
		}
	}
	return Row{"url": listURL, "models": models}, nil
}
