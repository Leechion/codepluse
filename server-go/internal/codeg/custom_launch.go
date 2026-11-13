package codeg

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strings"
	"sync"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
	"harnesshub/internal/providers"
)

type customLaunch struct {
	Command string   `json:"command"`
	Args    []string `json:"args"`
	Version string   `json:"version"`
}

var customInstallLocks sync.Map
var regexpNPMPackage = regexp.MustCompile(`^(?:@[a-zA-Z0-9_.-]+/)?[a-zA-Z0-9_][a-zA-Z0-9_.-]*$`)

func (s *Server) customCacheDir(d customAgentDef) string {
	data, _ := json.Marshal(Row{"spec": d.Spec, "kind": d.Kind, "version": d.Version, "platform": currentACPPlatform()})
	return filepath.Join(s.Root, ".data", "acp-custom", d.RegistryID, fmt.Sprintf("%x", sha256.Sum256(data))[:16])
}
func (s *Server) cachedCustomLaunch(d customAgentDef) (customLaunch, bool) {
	var launch customLaunch
	data, err := os.ReadFile(filepath.Join(s.customCacheDir(d), "launch.json"))
	if err != nil || json.Unmarshal(data, &launch) != nil || launch.Command == "" {
		return launch, false
	}
	if st, err := os.Stat(launch.Command); err != nil || st.IsDir() {
		return launch, false
	}
	for _, arg := range launch.Args {
		if filepath.IsAbs(arg) {
			if _, err := os.Stat(arg); err != nil {
				return launch, false
			}
		}
	}
	return launch, true
}
func (s *Server) customInstalledVersion(d customAgentDef) any {
	if l, ok := s.cachedCustomLaunch(d); ok {
		if l.Version != "" {
			return l.Version
		}
		return "local"
	}
	return nil
}
func npmCLI() (string, string, error) {
	node, err := exec.LookPath("node")
	if err != nil {
		return "", "", fmt.Errorf("未找到 Node.js，请先安装 Node.js 与 npm")
	}
	paths := []string{filepath.Join(filepath.Dir(node), "node_modules", "npm", "bin", "npm-cli.js")}
	for _, name := range []string{"npm", "npm.cmd"} {
		if found, err := exec.LookPath(name); err == nil {
			if real, err := filepath.EvalSymlinks(found); err == nil && strings.HasSuffix(real, "npm-cli.js") {
				paths = append(paths, real)
			}
			paths = append(paths, filepath.Join(filepath.Dir(found), "node_modules", "npm", "bin", "npm-cli.js"))
		}
	}
	for _, p := range paths {
		if st, err := os.Stat(p); err == nil && !st.IsDir() {
			return node, p, nil
		}
	}
	return "", "", fmt.Errorf("未找到 npm-cli.js，请修复 Node.js/npm 安装")
}
func npmPackageName(spec string) string {
	name := spec
	if at := strings.LastIndex(name, "@"); at > 0 {
		name = name[:at]
	}
	return name
}
func pythonCommandName(spec string) string {
	parts := strings.FieldsFunc(spec, func(r rune) bool { return strings.ContainsRune("[=<>!~@ ", r) })
	if len(parts) == 0 {
		return ""
	}
	return strings.TrimSpace(parts[0])
}
func customEnv(base []string, extra map[string]string) []string {
	values := map[string]string{}
	for _, item := range base {
		if key, val, ok := strings.Cut(item, "="); ok {
			values[key] = val
		}
	}
	for key, val := range extra {
		values[key] = val
	}
	keys := []string{}
	for key := range values {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	out := []string{}
	for _, key := range keys {
		out = append(out, key+"="+values[key])
	}
	return out
}
func runCustomInstaller(command string, args []string, env []string, dir string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	cmd := exec.CommandContext(ctx, command, args...)
	cmd.Dir = dir
	cmd.Env = env
	hideCommand(cmd)
	out, err := cmd.CombinedOutput()
	if err != nil {
		message := strings.TrimSpace(string(out))
		if len(message) > 6000 {
			message = message[len(message)-6000:]
		}
		return fmt.Errorf("安装自定义 ACP 智能体失败：%w\n%s", err, message)
	}
	return nil
}

// customNotInstalled 复刻原版 AcpError::SdkNotInstalled 的渲染方式。
//
// 前端 acp-connections-context.tsx:7733 用 `message.includes("is not installed")`
// 作为稳定标识，把这类失败路由成「打开智能体设置」的安装引导（detail 会被丢弃，
// 只留本地化标题）。原版在 connection.rs:2358-2361 明确把该英文子串标为 INVARIANT，
// 所以这里必须原样带上它，中文只作为附加说明供日志与通用分支显示。
func customNotInstalled(name string, detail error) error {
	if detail == nil {
		return fmt.Errorf("%s is not installed. Please install it in Agent Settings.（该自定义智能体尚未安装或启动入口缺失，请在「设置 → 智能体」中重新安装）", name)
	}
	return fmt.Errorf("%s is not installed. Please install it in Agent Settings.（该自定义智能体尚未安装或启动入口缺失，请在「设置 → 智能体」中重新安装：%v）", name, detail)
}

func (s *Server) prepareCustom(d customAgentDef) (customLaunch, error) {
	if err := s.validateCustomDef(d); err != nil {
		return customLaunch{}, err
	}
	dir := s.customCacheDir(d)
	lock, _ := customInstallLocks.LoadOrStore(dir, &sync.Mutex{})
	lock.(*sync.Mutex).Lock()
	defer lock.(*sync.Mutex).Unlock()
	if launch, ok := s.cachedCustomLaunch(d); ok {
		return launch, nil
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		return customLaunch{}, err
	}
	launch := customLaunch{Version: d.Version, Args: []string{}}
	switch d.Kind {
	case "binary":
		b := d.Spec.Binary[currentACPPlatform()]
		command, err := s.installCustomArchive(dir, b)
		if err != nil {
			return launch, err
		}
		launch.Command = command
	case "npx":
		p := d.Spec.Npx
		node, npm, err := npmCLI()
		if err != nil {
			return launch, err
		}
		packageName := npmPackageName(p.Package)
		if !regexpNPMPackage.MatchString(packageName) {
			return launch, fmt.Errorf("npm 包名称无效：%s", packageName)
		}
		if err = runCustomInstaller(node, []string{npm, "install", "--prefix", dir, "--no-audit", "--no-fund", "--omit=dev", "--", p.Package}, os.Environ(), dir); err != nil {
			return launch, err
		}
		packageDir := filepath.Join(dir, "node_modules", filepath.FromSlash(packageName))
		data, err := os.ReadFile(filepath.Join(packageDir, "package.json"))
		if err != nil {
			// 包没装上（或目录被清掉）：按原版归入 SdkNotInstalled，让前端弹安装引导。
			return launch, customNotInstalled(d.Name, err)
		}
		var pkg struct {
			Version string          `json:"version"`
			Bin     json.RawMessage `json:"bin"`
		}
		if err = json.Unmarshal(data, &pkg); err != nil {
			return launch, err
		}
		var bin string
		_ = json.Unmarshal(pkg.Bin, &bin)
		if bin == "" {
			bins := map[string]string{}
			_ = json.Unmarshal(pkg.Bin, &bins)
			if p.Command != "" {
				bin = bins[p.Command]
			} else if len(bins) == 1 {
				for _, v := range bins {
					bin = v
				}
			} else {
				bin = bins[filepath.Base(packageName)]
			}
		}
		if bin == "" {
			return launch, fmt.Errorf("%s 没有唯一的启动入口，请在手动配置的 cmd 中指定 package.json 中的 bin 名称", p.Package)
		}
		entry, err := safeCustomPath(packageDir, bin)
		if err != nil {
			return launch, err
		}
		if _, err = os.Stat(entry); err != nil {
			return launch, customNotInstalled(d.Name, err)
		}
		launch.Version = pkg.Version
		prefix := make([]byte, 128)
		f, readErr := os.Open(entry)
		if readErr != nil {
			return launch, readErr
		}
		n, _ := f.Read(prefix)
		_ = f.Close()
		firstLine := strings.SplitN(string(prefix[:n]), "\n", 2)[0]
		if strings.HasSuffix(entry, ".js") || strings.HasSuffix(entry, ".cjs") || strings.HasSuffix(entry, ".mjs") || (strings.HasPrefix(firstLine, "#!") && strings.Contains(firstLine, "node")) {
			launch.Command = node
			launch.Args = []string{entry}
		} else {
			launch.Command = entry
		}
	case "uvx":
		p := d.Spec.Uvx
		uv, err := exec.LookPath("uv")
		if err != nil {
			return launch, fmt.Errorf("未找到 uv，请先安装 uv/uvx 运行环境")
		}
		args := []string{"tool", "install", "--force"}
		if p.Python != "" {
			args = append(args, "--python", p.Python)
		}
		args = append(args, "--", p.Package)
		env := customEnv(os.Environ(), map[string]string{"UV_TOOL_DIR": filepath.Join(dir, "tools"), "UV_TOOL_BIN_DIR": filepath.Join(dir, "bin")})
		if err = runCustomInstaller(uv, args, env, dir); err != nil {
			return launch, err
		}
		name := p.Command
		if name == "" {
			name = pythonCommandName(p.Package)
		}
		if runtime.GOOS == "windows" && !strings.HasSuffix(name, ".exe") {
			name += ".exe"
		}
		launch.Command, err = safeCustomPath(filepath.Join(dir, "bin"), name)
		if err != nil {
			return launch, err
		}
		if _, err = os.Stat(launch.Command); err != nil {
			return launch, customNotInstalled(d.Name, fmt.Errorf("找不到 uv 工具入口 %s，请在手动配置中填写 cmd", name))
		}
	}
	data, _ := json.Marshal(launch)
	if err := os.WriteFile(filepath.Join(dir, "launch.json"), data, 0600); err != nil {
		return launch, err
	}
	return launch, nil
}
func (s *Server) CustomAgentAvailable(agent *hub.Agent) (bool, bool) {
	d, ok := s.customDefForAgent(agent)
	if !ok {
		return false, false
	}
	if err := s.validateCustomDef(d); err != nil {
		return false, true
	}
	if _, ok := s.cachedCustomLaunch(d); ok {
		return true, true
	}
	switch d.Kind {
	case "npx":
		_, _, err := npmCLI()
		return err == nil, true
	case "uvx":
		_, err := exec.LookPath("uv")
		return err == nil, true
	default:
		return true, true
	}
}
func (s *Server) CustomAgentLaunch(agent *hub.Agent) (*acp.Launch, bool, error) {
	wire := wireAgent(agent)
	if agent.BaseAgentType != "" {
		wire = agent.BaseAgentType
	}
	d, ok := s.customDef(wire)
	if !ok {
		// 判定"注册丢失"只看 hub 记录自己的 Wire：registerCustomAgent 一定显式写
		// `custom:<registryId>`，而 wireAgent 的兜底会把 Wire 为空的 harness 实例
		// 也算成 `custom:<ID>`（server.go:139）。用兜底值判断会让
		// codepluse_instance_create 起来的实例再也走不到 customAcp 那条路径。
		if strings.HasPrefix(agent.Wire, "custom:") {
			return nil, true, fmt.Errorf("The custom agent %q is no longer registered. Re-add it in Settings → Agents.（该自定义智能体已不在注册表中，请在「设置 → 智能体」重新添加）", strings.TrimPrefix(agent.Wire, "custom:"))
		}
		return nil, false, nil
	}
	launch, err := s.prepareCustom(d)
	if err != nil {
		return nil, true, err
	}
	var args []string
	var env map[string]string
	switch d.Kind {
	case "binary":
		b := d.Spec.Binary[currentACPPlatform()]
		args = b.Args
		env = b.Env
	case "npx":
		args = d.Spec.Npx.Args
		env = d.Spec.Npx.Env
	case "uvx":
		args = d.Spec.Uvx.Args
		env = d.Spec.Uvx.Env
	}
	processEnv := customEnv(os.Environ(), env)
	overlay, _, _ := s.LaunchOverlay(agent)
	processEnv = customEnv(processEnv, overlay)
	if agent.AuthMode == "subscription" {
		processEnv = providers.ClearAPIEnvironment(processEnv)
	}
	return &acp.Launch{Command: launch.Command, Args: append(append([]string{}, launch.Args...), args...), Env: processEnv}, true, nil
}

func (s *Server) customInstallCommand(command string, a Row, d customAgentDef) (result any, err error) {
	taskID := text(a["taskId"])
	s.emit("app://agent-install", Row{"task_id": taskID, "agent_type": "custom:" + d.RegistryID, "kind": "started", "payload": "正在准备 " + d.Name})
	defer func() {
		kind, message := "completed", "已完成"
		if err != nil {
			kind, message = "failed", err.Error()
		}
		s.emit("app://agent-install", Row{"task_id": taskID, "agent_type": "custom:" + d.RegistryID, "kind": kind, "payload": message})
		s.emit("app://acp-agents-updated", Row{})
	}()
	if version := text(a["version"]); version != "" && version != d.Version {
		return nil, fmt.Errorf("自定义智能体的版本由发行版 spec 固定，请先编辑配置中的包版本或下载地址")
	}
	if command == "acp_uninstall_agent" || command == "acp_clear_binary_cache" || a["cleanFirst"] == true {
		s.mu.Lock()
		cids := []string{}
		busy := false
		for _, c := range s.connections {
			if c.AgentType == "custom:"+d.RegistryID {
				if c.Busy {
					busy = true
				}
				cids = append(cids, c.ID)
			}
		}
		s.mu.Unlock()
		if busy {
			return nil, fmt.Errorf("此智能体仍在执行任务，暂不能清除安装目录")
		}
		for _, cid := range cids {
			if err = s.disconnect(cid); err != nil {
				return nil, err
			}
		}
		if err = s.validateCustomDef(d); err != nil {
			return nil, err
		}
		if err = os.RemoveAll(s.customCacheDir(d)); err != nil {
			return nil, err
		}
		if command == "acp_uninstall_agent" || command == "acp_clear_binary_cache" {
			return nil, nil
		}
	}
	launch, err := s.prepareCustom(d)
	if err != nil {
		return nil, err
	}
	if command == "acp_prepare_npx_agent" {
		if launch.Version != "" {
			return launch.Version, nil
		}
		return "local", nil
	}
	return nil, nil
}
