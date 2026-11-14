package codeg

// Ports src-tauri/src/{acp/remote_registry,acp/custom_registry,
// commands/custom_agents}.rs from the sibling Codeg source.
import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strings"
	"time"

	"harnesshub/internal/hub"
)

const acpRegistryURL = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json"

type customPackageSpec struct {
	Package      string            `json:"package"`
	Command      string            `json:"cmd,omitempty"`
	Args         []string          `json:"args,omitempty"`
	Env          map[string]string `json:"env,omitempty"`
	Python       string            `json:"python,omitempty"`
	NodeRequired string            `json:"nodeRequired,omitempty"`
	UVRequired   string            `json:"uvRequired,omitempty"`
}
type customBinarySpec struct {
	Archive string            `json:"archive"`
	Command string            `json:"cmd"`
	Args    []string          `json:"args,omitempty"`
	Env     map[string]string `json:"env,omitempty"`
	SHA256  string            `json:"sha256,omitempty"`
}
type customAgentSpec struct {
	Npx    *customPackageSpec          `json:"npx,omitempty"`
	Uvx    *customPackageSpec          `json:"uvx,omitempty"`
	Binary map[string]customBinarySpec `json:"binary,omitempty"`
}
type customAgentDef struct {
	RegistryID        string          `json:"registryId"`
	Name              string          `json:"name"`
	Description       string          `json:"description"`
	Version           string          `json:"version"`
	Kind              string          `json:"distributionKind"`
	Spec              customAgentSpec `json:"spec"`
	IconURL           *string         `json:"iconUrl"`
	SkillsSharedStore bool            `json:"skillsSharedStore"`
	SkillsDir         *string         `json:"skillsDir"`
	Source            string          `json:"source"`
	VersionProbe      *string         `json:"versionProbe"`
	SupportsMCP       bool            `json:"supportsMcp"`
}
type registryEntry struct {
	ID           string          `json:"id"`
	Name         string          `json:"name"`
	Description  string          `json:"description"`
	Version      *string         `json:"version"`
	Icon         *string         `json:"icon"`
	Website      *string         `json:"website"`
	Repository   *string         `json:"repository"`
	License      *string         `json:"license"`
	Distribution customAgentSpec `json:"distribution"`
}

func currentACPPlatform() string {
	arch := map[string]string{"amd64": "x86_64", "arm64": "aarch64", "386": "x86"}[runtime.GOARCH]
	if arch == "" {
		arch = runtime.GOARCH
	}
	return runtime.GOOS + "-" + arch
}
func customKinds(spec customAgentSpec) []string {
	out := []string{}
	if len(spec.Binary) > 0 {
		out = append(out, "binary")
	}
	if spec.Npx != nil {
		out = append(out, "npx")
	}
	if spec.Uvx != nil {
		out = append(out, "uvx")
	}
	return out
}
func customKindRunsHere(spec customAgentSpec, kind string) bool {
	switch kind {
	case "binary":
		return spec.Binary[currentACPPlatform()].Archive != ""
	case "npx":
		return spec.Npx != nil && spec.Npx.Package != ""
	case "uvx":
		return spec.Uvx != nil && spec.Uvx.Package != ""
	}
	return false
}
func defaultCustomKind(spec customAgentSpec) string {
	for _, kind := range customKinds(spec) {
		if customKindRunsHere(spec, kind) {
			return kind
		}
	}
	return ""
}

var customIDPattern = regexp.MustCompile(`^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,127}$`)

func (s *Server) builtinRegistryID(id string) bool {
	// Only the three native integrations actually ported to this Go runtime.
	return id == "codex-acp" || id == "claude-acp" || id == "claude-code-acp" || id == "deepseek" || id == "dsh" || id == "workbuddy"
}
func (s *Server) customDef(wire string) (customAgentDef, bool) {
	if !strings.HasPrefix(wire, "custom:") {
		return customAgentDef{}, false
	}
	s.store.mu.Lock()
	defer s.store.mu.Unlock()
	d, ok := s.store.CustomAgents[strings.TrimPrefix(wire, "custom:")]
	return clone(d), ok
}

func (s *Server) customDefForAgent(agent *hub.Agent) (customAgentDef, bool) {
	wire := wireAgent(agent)
	if agent.BaseAgentType != "" {
		wire = agent.BaseAgentType
	}
	return s.customDef(wire)
}
func (s *Server) validateCustomDef(d customAgentDef) error {
	if !customIDPattern.MatchString(d.RegistryID) || s.builtinRegistryID(d.RegistryID) {
		return fmt.Errorf("自定义智能体 ID 无效或与内置执行端冲突")
	}
	if !customKindRunsHere(d.Spec, d.Kind) {
		return fmt.Errorf("%s 未提供适用于 %s 的 %s 发行版", d.RegistryID, currentACPPlatform(), d.Kind)
	}
	if d.Kind == "binary" {
		b := d.Spec.Binary[currentACPPlatform()]
		u, err := url.Parse(b.Archive)
		if err != nil || u.Host == "" || (u.Scheme != "https" && u.Scheme != "http") {
			return fmt.Errorf("归档下载地址必须为 HTTP(S) URL")
		}
		if _, err := safeCustomPath(".", b.Command); err != nil {
			return fmt.Errorf("归档启动路径无效: %w", err)
		}
		if b.SHA256 != "" && !regexp.MustCompile(`^[a-fA-F0-9]{64}$`).MatchString(b.SHA256) {
			return fmt.Errorf("SHA-256 校验值必须是 64 位十六进制字符串")
		}
	} else {
		p := d.Spec.Npx
		if d.Kind == "uvx" {
			p = d.Spec.Uvx
		}
		if strings.HasPrefix(p.Package, "-") || strings.ContainsAny(p.Package, "\r\n\x00") {
			return fmt.Errorf("包名称无效")
		}
	}
	return nil
}
func (s *Server) fetchRegistryEntries() ([]registryEntry, error) {
	address := s.RegistryURL
	if address == "" {
		address = acpRegistryURL
	}
	client := s.RegistryClient
	if client == nil {
		client = &http.Client{Timeout: 35 * time.Second}
	}
	req, err := http.NewRequest("GET", address, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", "codepluse/2.0-go")
	res, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("无法读取 ACP 注册表：%w", err)
	}
	defer res.Body.Close()
	if res.StatusCode != 200 {
		return nil, fmt.Errorf("无法读取 ACP 注册表：HTTP %d", res.StatusCode)
	}
	var payload struct {
		Agents []registryEntry `json:"agents"`
	}
	if err = json.NewDecoder(io.LimitReader(res.Body, 8<<20)).Decode(&payload); err != nil {
		return nil, fmt.Errorf("ACP 注册表 JSON 无效：%w", err)
	}
	if payload.Agents == nil {
		return nil, fmt.Errorf("ACP 注册表缺少 agents 列表")
	}
	return payload.Agents, nil
}
func (s *Server) registryCatalog() (any, error) {
	entries, err := s.fetchRegistryEntries()
	if err != nil {
		return nil, err
	}
	s.store.mu.Lock()
	installed := clone(s.store.CustomAgents)
	s.store.mu.Unlock()
	rows := []Row{}
	for _, e := range entries {
		_, added := installed[e.ID]
		rows = append(rows, Row{"registryId": e.ID, "name": e.Name, "description": e.Description, "version": e.Version, "iconUrl": e.Icon, "website": e.Website, "repository": e.Repository, "license": e.License, "distributionKinds": customKinds(e.Distribution), "builtin": s.builtinRegistryID(e.ID), "installed": added, "supportedOnPlatform": defaultCustomKind(e.Distribution) != "", "spec": e.Distribution})
	}
	sort.SliceStable(rows, func(i, j int) bool {
		return strings.ToLower(text(rows[i]["name"])) < strings.ToLower(text(rows[j]["name"]))
	})
	return rows, nil
}
func customHubID(id string) string { return "agt_custom_" + id }
func (s *Server) registerCustomAgent(d customAgentDef) {
	if s.Hub.Agent(customHubID(d.RegistryID)) != nil {
		s.Hub.UpdateAgent(customHubID(d.RegistryID), func(a *hub.Agent) { a.Name = d.Name; a.ACPVersion = d.Version })
		return
	}
	s.Hub.CreateAgent(&hub.Agent{ID: customHubID(d.RegistryID), Wire: "custom:" + d.RegistryID, Name: d.Name, Real: true, Communication: "acp", AdapterKind: "ACP", Endpoint: "acp://custom/" + d.RegistryID, ACPVersion: d.Version, MaxCon: 4})
}
func (s *Server) loadCustomAgents() {
	for _, d := range s.store.CustomAgents {
		s.registerCustomAgent(d)
	}
}
func (s *Server) saveCustomAgent(params Row) error {
	b, _ := json.Marshal(params)
	var d customAgentDef
	if err := json.Unmarshal(b, &d); err != nil {
		return fmt.Errorf("智能体配置无效: %w", err)
	}
	d.RegistryID = strings.TrimSpace(d.RegistryID)
	d.Name = strings.TrimSpace(d.Name)
	if d.Name == "" {
		d.Name = d.RegistryID
	}
	if d.Kind == "" {
		d.Kind = defaultCustomKind(d.Spec)
	}
	if err := s.validateCustomDef(d); err != nil {
		return err
	}
	if d.SkillsDir != nil {
		dir := strings.TrimSpace(*d.SkillsDir)
		if strings.HasPrefix(dir, "~/") || strings.HasPrefix(dir, `~\`) {
			home, _ := os.UserHomeDir()
			dir = filepath.Join(home, dir[2:])
		}
		if dir == "" {
			d.SkillsDir = nil
		} else if !filepath.IsAbs(dir) {
			return fmt.Errorf("技能目录必须为绝对路径")
		} else {
			d.SkillsDir = &dir
		}
	}
	s.store.mu.Lock()
	old, exists := s.store.CustomAgents[d.RegistryID]
	if _, ok := params["supportsMcp"]; !ok {
		d.SupportsMCP = true
		if exists {
			d.SupportsMCP = old.SupportsMCP
		}
	}
	if d.Source == "" {
		d.Source = "manual"
		if exists {
			d.Source = old.Source
		}
	}
	if d.Source != "manual" && d.Source != "registry" {
		s.store.mu.Unlock()
		return fmt.Errorf("智能体来源无效")
	}
	if s.store.CustomAgents == nil {
		s.store.CustomAgents = map[string]customAgentDef{}
	}
	s.store.CustomAgents[d.RegistryID] = d
	err := s.store.saveLocked()
	if err != nil {
		if exists {
			s.store.CustomAgents[d.RegistryID] = old
		} else {
			delete(s.store.CustomAgents, d.RegistryID)
		}
	}
	s.store.mu.Unlock()
	if err != nil {
		return err
	}
	s.registerCustomAgent(d)
	s.retireAgentConnections(customHubID(d.RegistryID))
	s.emit("app://acp-agents-updated", Row{})
	return nil
}
func (s *Server) customAgentCommand(command string, a Row) (any, error) {
	switch command {
	case "acp_current_platform":
		// 已翻转 Rust（I 组 custom_agents.rs）；Go 实现留给测试。
		return currentACPPlatform(), nil
	case "acp_fetch_registry_catalog":
		// 已翻转 Rust（B 组 acp_registry.rs）；Go 实现留给测试。
		return s.registryCatalog()
	case "acp_list_custom_agents":
		// 已翻转 Rust（I 组 custom_agents.rs）；Go 实现留给测试。
		s.store.mu.Lock()
		defs := clone(s.store.CustomAgents)
		s.store.mu.Unlock()
		rows := []Row{}
		for _, d := range defs {
			b, _ := json.Marshal(d)
			row := Row{}
			_ = json.Unmarshal(b, &row)
			row["agentType"] = "custom:" + d.RegistryID
			err := s.validateCustomDef(d)
			row["launchable"] = err == nil
			row["problem"] = nil
			if err != nil {
				row["problem"] = err.Error()
			}
			rows = append(rows, row)
		}
		sort.Slice(rows, func(i, j int) bool { return text(rows[i]["registryId"]) < text(rows[j]["registryId"]) })
		return rows, nil
	case "acp_save_custom_agent":
		params, _ := a["params"].(map[string]any)
		return nil, s.saveCustomAgent(params)
	case "acp_add_registry_agent":
		entries, err := s.fetchRegistryEntries()
		if err != nil {
			return nil, err
		}
		for _, e := range entries {
			if e.ID == text(a["registryId"]) {
				s.store.mu.Lock()
				_, exists := s.store.CustomAgents[e.ID]
				s.store.mu.Unlock()
				if exists {
					return nil, fmt.Errorf("智能体已添加：%s", e.ID)
				}
				kind := text(a["distributionKind"])
				if kind == "" {
					kind = defaultCustomKind(e.Distribution)
				}
				version := ""
				if e.Version != nil {
					version = *e.Version
				}
				return nil, s.saveCustomAgent(Row{"registryId": e.ID, "name": e.Name, "description": e.Description, "version": version, "distributionKind": kind, "spec": e.Distribution, "iconUrl": e.Icon, "source": "registry", "supportsMcp": true})
			}
		}
		return nil, fmt.Errorf("ACP 注册表中未找到 %s", text(a["registryId"]))
	case "acp_delete_custom_agent":
		id := text(a["registryId"])
		if a["deleteTranscripts"] == true {
			return nil, fmt.Errorf("Go 版暂不支持清除自定义智能体的历史记录；取消勾选后可移除定义并保留会话")
		}
		s.store.mu.Lock()
		d, ok := s.store.CustomAgents[id]
		if !ok {
			s.store.mu.Unlock()
			return nil, fmt.Errorf("自定义智能体不存在")
		}
		delete(s.store.CustomAgents, id)
		err := s.store.saveLocked()
		if err != nil {
			s.store.CustomAgents[id] = d
		}
		s.store.mu.Unlock()
		if err != nil {
			return nil, err
		}
		s.retireAgentConnections(customHubID(id))
		s.Hub.DeleteAgent(customHubID(id))
		s.emit("app://acp-agents-updated", Row{})
		return nil, nil
	}
	return nil, fmt.Errorf("未知自定义智能体命令：%s", command)
}
