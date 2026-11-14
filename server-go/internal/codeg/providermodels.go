// list_provider_models：拉取供应商网关的模型列表。协议与响应形状对齐前端
// lib/types.ts 的 ProviderModelsRequest / ProviderModelList。
package codeg

import (
	"encoding/json"
	"fmt"
	"harnesshub/internal/modelproviders"
	"harnesshub/internal/providers"
	"io"
	"net"
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
// sameHost reports whether two URLs address the same host (and port, when
// both specify one). Scheme is intentionally ignored: a provider stored over
// http may legitimately be reached over https through a gateway.
func sameHost(a, b string) bool {
	ua, err := url.Parse(strings.TrimSpace(a))
	if err != nil || ua.Hostname() == "" {
		return false
	}
	ub, err := url.Parse(strings.TrimSpace(b))
	if err != nil || ub.Hostname() == "" {
		return false
	}
	if !strings.EqualFold(ua.Hostname(), ub.Hostname()) {
		return false
	}
	if ua.Port() != "" && ub.Port() != "" {
		return ua.Port() == ub.Port()
	}
	return true
}

// modelListProviderURL returns the destination that was stored together with
// the credential, so listProviderModels can pin its request to it.
func (s *Server) modelListProviderURL(keySource map[string]any) (string, error) {
	switch text(keySource["kind"]) {
	case "executor", "ccswitch":
		p, err := providers.Find(s.Root, text(keySource["providerId"]))
		if err != nil {
			return "", err
		}
		if strings.TrimSpace(p.URL) != "" {
			return p.URL, nil
		}
		return p.Env["ANTHROPIC_BASE_URL"], nil
	case "provider":
		id := int64(number(keySource["providerId"]))
		if id == 0 {
			return "", fmt.Errorf("providerId 无效")
		}
		list, err := modelproviders.List(s.Root)
		if err != nil {
			return "", err
		}
		for _, info := range list {
			if info.ID == id {
				return info.ApiURL, nil
			}
		}
		return "", fmt.Errorf("供应商不存在")
	}
	return "", nil
}

func (s *Server) resolveModelListKey(keySource map[string]any) (string, error) {
	switch text(keySource["kind"]) {
	case "executor":
		p, err := providers.Find(s.Root, text(keySource["providerId"]))
		if err != nil {
			return "", err
		}
		return p.Key, nil
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

// Resolve URL and credentials together from the stored provider. The frontend
// sends an ID, never a key or a replacement destination for that key.
func (s *Server) harnessProviderModels(id string) (any, error) {
	p, err := providers.Find(s.Root, id)
	if err != nil {
		return nil, err
	}
	if !providers.IsAPI(p) {
		return nil, fmt.Errorf("请选择 API 供应商，官方订阅不提供此配置")
	}
	models := []listedModel{}
	seen := map[string]bool{}
	disabled := map[string]bool{}
	for _, m := range p.Models {
		if m.Enabled != nil && !*m.Enabled {
			disabled[m.ID] = true
		}
	}
	add := func(id, name string) {
		if id != "" && !seen[id] && !disabled[id] {
			seen[id] = true
			models = append(models, listedModel{ID: id, DisplayName: name})
		}
	}
	for _, m := range p.Models {
		add(m.ID, m.Name)
	}
	add(p.Model, "")
	if p.Source == "dsh" && len(models) > 0 {
		return Row{"models": models, "source": "native"}, nil
	}
	protocol := p.Protocol
	if protocol == "openai-responses" {
		protocol = "openai"
	}
	if protocol == "" {
		protocol = "openai"
		if p.Agent == "claude" {
			protocol = "anthropic"
		}
	}
	base := strings.TrimRight(p.URL, "/")
	if protocol == "anthropic" {
		base = strings.TrimSuffix(base, "/v1")
	}
	params := Row{"protocol": protocol, "baseUrl": base, "keySource": Row{"kind": "executor", "providerId": id}}
	if p.Env["ANTHROPIC_AUTH_TOKEN"] != "" {
		params["keyHeader"] = "bearer"
	}
	raw, err := s.listProviderModels(Row{"params": params})
	warning := ""
	if err == nil {
		if row, ok := raw.(map[string]any); ok {
			if remote, ok := row["models"].([]listedModel); ok {
				for _, m := range remote {
					add(m.ID, m.DisplayName)
				}
			}
		}
	} else {
		warning = "供应商模型目录暂不可用，已显示本地配置中的模型；也可填写模型 ID。"
	}
	return Row{"models": models, "source": "provider", "warning": warning}, nil
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
	// A stored credential is only ever sent to the host it was stored with.
	// Letting the request body pick baseUrl turns this endpoint into a
	// credential-exfiltration primitive: point it at an attacker host and the
	// Authorization header follows. Only the host is compared — the caller
	// still controls the path prefix, which providers normalise differently.
	keyKind := text(keySource["kind"])
	if keyKind != "" && keyKind != "inline" {
		stored, e := s.modelListProviderURL(keySource)
		if e != nil {
			return nil, e
		}
		if !sameHost(baseURL, stored) {
			return nil, fmt.Errorf("API 地址与密钥来源的供应商不一致")
		}
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
	if keyKind == "inline" {
		// A user-typed key has no stored destination, so the request may name
		// any host; refuse private/loopback targets to keep this from becoming
		// an SSRF probe.
		client = safeFetchClient()
	} else if transport, ok := http.DefaultTransport.(*http.Transport); ok {
		t := transport.Clone()
		target, _ := url.Parse(baseURL)
		ip := net.ParseIP(target.Hostname())
		if target.Hostname() == "localhost" || (ip != nil && ip.IsLoopback()) {
			t.Proxy = nil
		} else if proxy := outboundProxy(); proxy != "" {
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
