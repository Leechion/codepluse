// CDP client: a minimal Chrome DevTools Protocol driver over gorilla/websocket.
// Launches a local Chrome/Edge with --remote-debugging-port, talks to the
// browser endpoint, attaches flat sessions per tab and evaluates in isolated
// worlds for the browser-agent bundle.
package codeg

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/gorilla/websocket"
)

type cdpBrowser struct {
	mu          sync.Mutex
	cmd         *exec.Cmd
	wsURL       string
	conn        *websocket.Conn
	nextID      int64
	pending     map[int64]chan Row
	events      chan Row
	closed      chan struct{}
	userDataDir string
}

var browserProc *cdpBrowser
var browserMu sync.Mutex

// findBrowser locates a Chrome/Edge executable on this machine.
func findBrowser() string {
	programFiles := os.Getenv("ProgramFiles")
	programFilesX86 := os.Getenv("ProgramFiles(x86)")
	localAppData := os.Getenv("LOCALAPPDATA")
	candidates := []string{
		filepath.Join(programFiles, `Google\Chrome\Application\chrome.exe`),
		filepath.Join(programFilesX86, `Google\Chrome\Application\chrome.exe`),
		filepath.Join(localAppData, `Google\Chrome\Application\chrome.exe`),
		filepath.Join(programFilesX86, `Microsoft\Edge\Application\msedge.exe`),
		filepath.Join(programFiles, `Microsoft\Edge\Application\msedge.exe`),
	}
	for _, path := range candidates {
		if path == "" {
			continue
		}
		if _, err := os.Stat(path); err == nil {
			return path
		}
	}
	return ""
}

// ensureBrowser launches (or reuses) the local browser with CDP enabled. The
// profile lives under the project's .data (the system temp directory is not
// writable under this machine's ACLs) and the chosen port is persisted so a
// server restart re-adopts the running browser instead of spawning another.
func ensureBrowser(profileRoot string) (*cdpBrowser, error) {
	browserMu.Lock()
	defer browserMu.Unlock()
	if browserProc != nil && browserProc.alive() {
		return browserProc, nil
	}
	exe := findBrowser()
	if exe == "" {
		return nil, fmt.Errorf("未找到 Chrome 或 Edge，请安装后重试")
	}
	userDataDir := filepath.Join(profileRoot, ".data", "browser-profile")
	_ = os.MkdirAll(userDataDir, 0o755)
	portFile := filepath.Join(profileRoot, ".data", "browser-port.txt")

	// Re-adopt a browser we launched earlier.
	if raw, err := os.ReadFile(portFile); err == nil {
		if port := strings.TrimSpace(string(raw)); port != "" {
			if version, err := browserVersionPort(port); err == nil && version != nil {
				if ws, ok := version["webSocketDebuggerUrl"].(string); ok && ws != "" {
					proc := &cdpBrowser{wsURL: ws, pending: map[int64]chan Row{},
						events: make(chan Row, 256), closed: make(chan struct{}), userDataDir: userDataDir}
					if err := proc.connect(); err == nil {
						browserProc = proc
						return browserProc, nil
					}
				}
			}
		}
	}

	// Pick a free debugging port and launch.
	wsURL := ""
	var cmd *exec.Cmd
	for port := 9337; port <= 9341 && wsURL == ""; port++ {
		if _, err := browserVersionPort(fmt.Sprint(port)); err == nil {
			continue // something else owns this port
		}
		candidate := exec.Command(exe, fmt.Sprintf("--remote-debugging-port=%d", port),
			"--no-first-run", "--no-default-browser-check", "--user-data-dir="+userDataDir,
			"--window-size=1280,900", "about:blank")
		if err := candidate.Start(); err != nil {
			return nil, fmt.Errorf("启动浏览器失败：%v", err)
		}
		deadline := time.Now().Add(20 * time.Second)
		for time.Now().Before(deadline) {
			time.Sleep(500 * time.Millisecond)
			if version, err := browserVersionPort(fmt.Sprint(port)); err == nil && version != nil {
				if ws, ok := version["webSocketDebuggerUrl"].(string); ok && ws != "" {
					wsURL = ws
					cmd = candidate
					_ = os.WriteFile(portFile, []byte(fmt.Sprint(port)), 0o600)
					break
				}
			}
		}
		if wsURL == "" {
			_ = candidate.Process.Kill()
		}
	}
	if wsURL == "" {
		return nil, fmt.Errorf("浏览器调试端口未就绪，请关闭已打开的 Edge/Chrome 后重试")
	}
	proc := &cdpBrowser{cmd: cmd, wsURL: wsURL, pending: map[int64]chan Row{},
		events: make(chan Row, 256), closed: make(chan struct{}), userDataDir: userDataDir}
	if err := proc.connect(); err != nil {
		return nil, err
	}
	browserProc = proc
	return browserProc, nil
}

func (b *cdpBrowser) alive() bool {
	select {
	case <-b.closed:
		return false
	default:
		return true
	}
}

func (b *cdpBrowser) connect() error {
	conn, _, err := websocket.DefaultDialer.Dial(b.wsURL, nil)
	if err != nil {
		return fmt.Errorf("连接浏览器调试通道失败：%v", err)
	}
	conn.SetReadLimit(64 << 20)
	b.conn = conn
	go b.readLoop()
	return nil
}

func (b *cdpBrowser) readLoop() {
	defer close(b.closed)
	for {
		_, data, err := b.conn.ReadMessage()
		if err != nil {
			b.mu.Lock()
			for _, ch := range b.pending {
				close(ch)
			}
			b.pending = map[int64]chan Row{}
			b.mu.Unlock()
			return
		}
		var message Row
		if json.Unmarshal(data, &message) != nil {
			continue
		}
		if id := number(message["id"]); id != 0 {
			b.mu.Lock()
			ch, ok := b.pending[int64(id)]
			if ok {
				delete(b.pending, int64(id))
			}
			b.mu.Unlock()
			if ok {
				ch <- message
			}
			continue
		}
		if method := text(message["method"]); method != "" {
			select {
			case b.events <- message:
			default:
			}
		}
	}
}

// call sends one CDP command and waits for its result.
func (b *cdpBrowser) call(method string, params Row) (Row, error) {
	return b.callSession("", method, params)
}

// callSession sends a command on a flat session (empty = browser level).
func (b *cdpBrowser) callSession(sessionID, method string, params Row) (Row, error) {
	b.mu.Lock()
	if b.conn == nil {
		b.mu.Unlock()
		return nil, fmt.Errorf("浏览器连接已关闭")
	}
	b.nextID++
	id := b.nextID
	ch := make(chan Row, 1)
	b.pending[id] = ch
	message := Row{"id": id, "method": method, "params": params}
	if sessionID != "" {
		message["sessionId"] = sessionID
	}
	if err := b.conn.WriteJSON(message); err != nil {
		delete(b.pending, id)
		b.mu.Unlock()
		return nil, err
	}
	b.mu.Unlock()
	select {
	case response, ok := <-ch:
		if !ok {
			return nil, fmt.Errorf("浏览器连接中断")
		}
		if e, ok := response["error"].(Row); ok {
			return nil, fmt.Errorf("%s：%s", method, text(e["message"]))
		}
		result, _ := response["result"].(Row)
		return result, nil
	case <-time.After(30 * time.Second):
		b.mu.Lock()
		delete(b.pending, id)
		b.mu.Unlock()
		return nil, fmt.Errorf("%s 超时", method)
	}
}

// tab is one attach session (a page target).
type cdpTab struct {
	SessionID string
	TargetID  string
	URL       string
	Title     string
	worldID   string
	bundleOn  bool
}

// listTabs enumerates page targets.
func (b *cdpBrowser) listTabs() ([]cdpTab, error) {
	result, err := b.call("Target.getTargets", nil)
	if err != nil {
		return nil, err
	}
	out := []cdpTab{}
	if infos, ok := result["targetInfos"].([]any); ok {
		for _, raw := range infos {
			info, ok := raw.(Row)
			if !ok || text(info["type"]) != "page" {
				continue
			}
			out = append(out, cdpTab{TargetID: text(info["targetId"]), URL: text(info["url"]), Title: text(info["title"])})
		}
	}
	return out, nil
}

// attach returns a flat session for the tab (attaching on demand).
func (b *cdpBrowser) attach(tab *cdpTab) error {
	if tab.SessionID != "" {
		return nil
	}
	result, err := b.call("Target.attachToTarget", Row{"targetId": tab.TargetID, "flatten": true})
	if err != nil {
		return err
	}
	tab.SessionID = text(result["sessionId"])
	return nil
}

// ensureAgentWorld creates the isolated world and installs the agent bundle.
func (b *cdpBrowser) ensureAgentWorld(tab *cdpTab, bundle string) error {
	if err := b.attach(tab); err != nil {
		return err
	}
	if tab.bundleOn && tab.worldID != "" {
		return nil
	}
	result, err := b.callSession(tab.SessionID, "Page.createIsolatedWorld", Row{"targetId": tab.TargetID})
	if err != nil {
		return err
	}
	tab.worldID = text(result["executionContextId"])
	context := Row{"contextId": tab.worldID}
	if _, err := b.callSession(tab.SessionID, "Runtime.evaluate", mergeRows(context, Row{"expression": bundle, "returnByValue": true})); err != nil {
		return fmt.Errorf("注入浏览器 agent 失败：%v", err)
	}
	tab.bundleOn = true
	return nil
}

// tabByRef resolves a tab ref from the frontend (id or target id).
func (b *cdpBrowser) tabByRef(ref string) (*cdpTab, error) {
	tabs, err := b.listTabs()
	if err != nil {
		return nil, err
	}
	for i := range tabs {
		if fmt.Sprint(tabs[i].TargetID) == ref || fmt.Sprint(i) == ref {
			return &tabs[i], nil
		}
	}
	return nil, fmt.Errorf("标签页不存在：%s", ref)
}

func mergeRows(base, extra Row) Row {
	out := Row{}
	for k, v := range base {
		out[k] = v
	}
	for k, v := range extra {
		out[k] = v
	}
	return out
}

// browserVersionPort fetches /json/version from the devtools endpoint.
func browserVersionPort(port string) (Row, error) {
	client := &http.Client{Timeout: 800 * time.Millisecond}
	response, err := client.Get(fmt.Sprintf("http://127.0.0.1:%s/json/version", port))
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	var version Row
	if err := json.NewDecoder(response.Body).Decode(&version); err != nil {
		return nil, err
	}
	return version, nil
}
