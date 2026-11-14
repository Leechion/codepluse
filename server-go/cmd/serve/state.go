package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/codeg"
	"harnesshub/internal/extra"
	"harnesshub/internal/hub"
	"harnesshub/internal/orch"
)

// ----------------------------- config -----------------------------

var (
	ROOT         = func() string { d, _ := os.Getwd(); return d }()
	PORT         = envInt("PORT", 8787)
	HOST         = envStr("HOST", "127.0.0.1")
	REAL_ENABLED = os.Getenv("HARNESS_HUB_REAL") != "0"
	STATE_MEMORY = os.Getenv("HARNESS_HUB_STATE_FILE") == ":memory:"
	CODEG_MODE   = os.Getenv("HARNESS_HUB_CODEG") == "1"
)

var realMu sync.Mutex

var realConfig = map[string]any{
	"permission":    envStr("HARNESS_HUB_PERMISSION", "readonly"),
	"workdir":       envStr("HARNESS_HUB_WORKDIR", ROOT),
	"allowedRoot":   envStr("HARNESS_HUB_ALLOWED_ROOT", ROOT),
	"timeoutMs":     envInt("HARNESS_HUB_TIMEOUT", 600000),
	"maxConcurrent": envInt("HARNESS_HUB_CONCURRENCY", 2),
	"defaultTool":   envStr("HARNESS_HUB_TOOL", "auto"),
}

func envStr(k, def string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return def
}

func envInt(k string, def int) int {
	if v := os.Getenv(k); v != "" {
		if n, err := strconv.Atoi(v); err == nil {
			return n
		}
	}
	return def
}

func cfgString(key string) string {
	realMu.Lock()
	defer realMu.Unlock()
	v, _ := realConfig[key].(string)
	return v
}

func cfgInt(key string) int {
	realMu.Lock()
	defer realMu.Unlock()
	v, _ := realConfig[key].(int)
	return v
}

func cfgSet(key string, v any) { realMu.Lock(); realConfig[key] = v; realMu.Unlock() }

var modelRe = regexp.MustCompile(`^[a-zA-Z0-9][\w.:/@+\-]{0,119}$`)

// ----------------------------- server state -----------------------------

type server struct {
	codeg       *codeg.Server
	hub         *hub.Hub
	rt          *acp.Runtime
	skills      *extra.Skills
	worktrees   *extra.Worktrees
	orch        *orch.Orchestrator
	chats       *orch.ChatController
	automations *orch.Automations

	mu          sync.Mutex
	customAcp   map[string]map[string]any
	agentLocks  map[string]*sync.Mutex
	realRunning int

	sseMu      sync.Mutex
	sseClients map[chan []byte]bool
}

// resolveWorkdir constrains workdirs under allowedRoot (lexical + symlink).
func resolveWorkdir(input string) (string, error) {
	resolved, err := resolveExistingPath(input)
	if err != nil {
		return "", err
	}
	root := cfgString("allowedRoot")
	realRoot, err := filepath.EvalSymlinks(root)
	if err != nil {
		realRoot = root
	}
	// 显式开关：允许把服务根之外的任意文件夹添加为工作空间。默认关闭，
	// 保持 Web 部署「只暴露服务目录」的安全边界。
	if os.Getenv("HARNESS_HUB_ALLOW_ANY_PATH") == "1" {
		return resolved, nil
	}
	rel, err := filepath.Rel(realRoot, resolved)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) || filepath.IsAbs(rel) {
		return "", fmt.Errorf("工作目录必须位于 %s 之内", root)
	}
	return resolved, nil
}

// resolveBrowseWorkdir resolves a path for the read-only directory browser:
// the same absolute-path and symlink handling as resolveWorkdir, but without
// the workspace-root restriction. It is only installed as codeg.Server's
// browse resolver and is gated on HARNESS_HUB_ALLOW_ANY_DIR_BROWSE, so it can
// never widen workdir/terminal/file access.
func resolveBrowseWorkdir(input string) (string, error) {
	return resolveExistingPath(input)
}

// resolveExistingPath turns input into an absolute path with symlinks in the
// deepest existing ancestor resolved.
func resolveExistingPath(input string) (string, error) {
	if input == "" {
		input = cfgString("workdir")
	}
	candidate, err := filepath.Abs(input)
	if err != nil {
		return "", fmt.Errorf("无法解析工作目录：%v", err)
	}
	ancestor := candidate
	for {
		if _, err := os.Stat(ancestor); err == nil {
			break
		}
		parent := filepath.Dir(ancestor)
		if parent == ancestor {
			break
		}
		ancestor = parent
	}
	resolved, err := filepath.EvalSymlinks(ancestor)
	if err != nil {
		resolved = ancestor
	}
	if resolved != ancestor {
		rel, _ := filepath.Rel(ancestor, candidate)
		resolved = filepath.Join(resolved, rel)
	}
	return resolved, nil
}

// invoke runs one ACP turn with per-agent serialisation.
func (s *server) invoke(agent *hub.Agent, prompt string, o acp.Options) acp.Result {
	s.mu.Lock()
	lock := s.agentLocks[agent.ID]
	if lock == nil {
		lock = &sync.Mutex{}
		s.agentLocks[agent.ID] = lock
	}
	s.mu.Unlock()
	lock.Lock()
	defer lock.Unlock()
	s.realRunning++
	defer func() { s.realRunning-- }()
	if o.Model != "" && !modelRe.MatchString(o.Model) {
		return acp.Result{OK: false, Error: "模型名称无效", Meta: map[string]any{}}
	}
	if o.TimeoutMs == 0 {
		o.TimeoutMs = cfgInt("timeoutMs")
	}
	return s.rt.Prompt(agent, prompt, o)
}

func (s *server) log(level, source, text string) {
	s.hub.Log(level, source, text)
}

func (s *server) snapshotData() map[string]any {
	return s.hub.Snapshot()
}

// ----------------------------- static -----------------------------

var mime = map[string]string{
	".html": "text/html; charset=utf-8", ".css": "text/css; charset=utf-8",
	".js": "application/javascript; charset=utf-8", ".json": "application/json; charset=utf-8",
	".svg": "image/svg+xml", ".ico": "image/x-icon", ".md": "text/markdown; charset=utf-8",
	".png": "image/png", ".webmanifest": "application/manifest+json",
}

func (s *server) staticDir() string {
	if v := os.Getenv("HARNESS_HUB_STATIC"); v != "" {
		return v
	}
	for _, c := range []string{filepath.Join(ROOT, "public"), filepath.Join(filepath.Dir(ROOT), "public"), "public"} {
		if st, err := os.Stat(c); err == nil && st.IsDir() {
			return c
		}
	}
	return ""
}

func (s *server) serveStatic(w http.ResponseWriter, pathname string) {
	rel := pathname
	if rel == "/" {
		rel = "/index.html"
	}
	rel, err := url.PathUnescape(rel)
	if err != nil {
		fail(w, 404, "资源不存在")
		return
	}
	dir := s.staticDir()
	if dir == "" {
		fail(w, 404, "静态目录不存在")
		return
	}
	file := filepath.Join(dir, filepath.FromSlash(rel))
	if !strings.HasPrefix(file, dir) {
		fail(w, 404, "资源不存在")
		return
	}
	data, err := os.ReadFile(file)
	if err != nil {
		fail(w, 404, "资源不存在: "+rel)
		return
	}
	setHeaders(w)
	w.Header().Set("Content-Type", mime[strings.ToLower(filepath.Ext(file))])
	w.Header().Set("Cache-Control", "no-store")
	w.Write(data)
}

// ----------------------------- SSE -----------------------------

func (s *server) serveEvents(w http.ResponseWriter, r *http.Request) {
	setHeaders(w)
	h := w.Header()
	h.Set("Content-Type", "text/event-stream; charset=utf-8")
	h.Set("Cache-Control", "no-cache, no-transform")
	h.Set("Connection", "keep-alive")
	h.Set("X-Accel-Buffering", "no")
	flusher, _ := w.(http.Flusher)
	w.Write([]byte(": connected to Harness Hub event stream\n\n"))
	snap := map[string]any{"type": "snapshot", "data": s.snapshotData(), "at": time.Now().UnixMilli()}
	if b, err := json.Marshal(snap); err == nil {
		w.Write([]byte("event: snapshot\ndata: " + string(b) + "\n\n"))
	}
	if flusher != nil {
		flusher.Flush()
	}
	ch := make(chan []byte, 256)
	s.sseMu.Lock()
	s.sseClients[ch] = true
	s.sseMu.Unlock()
	defer func() {
		s.sseMu.Lock()
		delete(s.sseClients, ch)
		s.sseMu.Unlock()
	}()
	keepalive := time.NewTicker(20 * time.Second)
	defer keepalive.Stop()
	for {
		select {
		case <-r.Context().Done():
			return
		case <-keepalive.C:
			if _, err := w.Write([]byte(": ping\n\n")); err != nil {
				return
			}
			if flusher != nil {
				flusher.Flush()
			}
		case line := <-ch:
			if _, err := w.Write(line); err != nil {
				return
			}
			if flusher != nil {
				flusher.Flush()
			}
		}
	}
}

func (s *server) sseFanOut() {
	_, events := s.hub.Subscribe()
	for evt := range events {
		payload, err := json.Marshal(map[string]any{"type": evt.Type, "data": evt.Data, "at": evt.At.UnixMilli()})
		if err != nil {
			continue
		}
		line := []byte("event: " + evt.Type + "\ndata: " + string(payload) + "\n\n")
		s.sseMu.Lock()
		for ch := range s.sseClients {
			select {
			case ch <- line:
			default: // slow client: drop the frame
			}
		}
		s.sseMu.Unlock()
	}
}
