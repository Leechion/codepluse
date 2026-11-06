// Harness Hub Go server — the full rewrite of server.js on the Go standard
// library only: REST /api/*, SSE /api/events, static console, automations
// tick loop, mission/chat persistence, ACP launching and mcp subcommands.
//
//	serve.exe                # http://127.0.0.1:8787
//	serve.exe mcp-result '<schema>'
//	serve.exe mcp-files <workspace>
package main

import (
	"encoding/json"
	"io"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

	"harnesshub/internal/extra"
	"harnesshub/internal/hub"
	"harnesshub/internal/orch"
)

// ----------------------------- responses -----------------------------

func setHeaders(w http.ResponseWriter) {
	h := w.Header()
	h.Set("Access-Control-Allow-Origin", "*")
	h.Set("Access-Control-Allow-Methods", "GET,POST,PATCH,DELETE,OPTIONS")
	h.Set("Access-Control-Allow-Headers", "Content-Type, Authorization, X-Harness-Id")
	h.Set("X-Harness-Hub", "2.0.0-go")
}

func send(w http.ResponseWriter, status int, body any) {
	setHeaders(w)
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(status)
	var payload []byte
	if raw, isRaw := body.(json.RawMessage); isRaw {
		payload = raw
	} else {
		payload, _ = json.Marshal(body)
	}
	w.Write(payload)
}

func ok(w http.ResponseWriter, data any) {
	send(w, 200, map[string]any{"ok": true, "data": data, "at": time.Now().UnixMilli()})
}

func fail(w http.ResponseWriter, status int, message string) {
	send(w, status, map[string]any{"ok": false, "error": message, "at": time.Now().UnixMilli()})
}

func readBody(r *http.Request) map[string]any {
	if r.Body == nil {
		return map[string]any{}
	}
	body, err := io.ReadAll(http.MaxBytesReader(nil, r.Body, 1024*1024))
	if err != nil || len(body) == 0 {
		return map[string]any{}
	}
	var m map[string]any
	if json.Unmarshal(body, &m) != nil {
		return map[string]any{}
	}
	return m
}

// raw writes pre-marshaled JSON in the standard envelope.
func (s *server) raw(w http.ResponseWriter, data any) {
	inner, err := json.Marshal(data)
	if err != nil {
		fail(w, 500, err.Error())
		return
	}
	send(w, 200, json.RawMessage(`{"ok":true,"data":`+string(inner)+`,"at":`+
		strconv.FormatInt(time.Now().UnixMilli(), 10)+`}`))
}

// ----------------------------- routing -----------------------------

func (s *server) handle(w http.ResponseWriter, r *http.Request) {
	if s.codeg != nil {
		s.codeg.ServeHTTP(w, r)
		return
	}
	if r.Method == "OPTIONS" {
		setHeaders(w)
		w.WriteHeader(204)
		return
	}
	p := r.URL.Path
	method := r.Method
	seg := strings.Split(strings.Trim(p, "/"), "/")
	body := map[string]any{}
	switch method {
	case "POST", "PATCH", "PUT", "DELETE":
		body = readBody(r)
	}

	// ---- chats ----
	if p == "/api/chats" && method == "GET" {
		type row struct {
			ID        string `json:"id"`
			Title     string `json:"title"`
			UpdatedAt int64  `json:"updatedAt"`
			Pending   int    `json:"pending"`
		}
		rows := []row{}
		for _, c := range s.chats.Chats() {
			rows = append(rows, row{ID: c.ID, Title: c.Title, UpdatedAt: c.UpdatedAt, Pending: c.Pending})
		}
		ok(w, rows)
		return
	}
	if p == "/api/chats" && method == "POST" {
		wd, err := resolveWorkdir(str(body["workdir"]))
		if err != nil {
			fail(w, 400, err.Error())
			return
		}
		body["workdir"] = wd
		chat, err := s.chats.Create(body)
		if err != nil {
			fail(w, 400, err.Error())
			return
		}
		s.raw(w, json.RawMessage(s.chats.MarshalChat(chat)))
		return
	}
	if len(seg) == 3 && seg[1] == "chats" && seg[2] != "" && method == "GET" {
		chat, err := s.chats.Get(seg[2])
		if err != nil {
			fail(w, 400, err.Error())
			return
		}
		var out map[string]any
		if json.Unmarshal(s.chats.MarshalChat(chat), &out) != nil {
			fail(w, 500, "快照失败")
			return
		}
		out["sessions"] = json.RawMessage(s.rt.ListJSON(seg[2]))
		s.raw(w, out)
		return
	}
	if len(seg) == 4 && seg[1] == "chats" {
		switch {
		case seg[3] == "config" && method == "PATCH":
			if raw, exists := body["workdir"]; exists {
				wd, err := resolveWorkdir(str(raw))
				if err != nil {
					fail(w, 400, err.Error())
					return
				}
				body["workdir"] = wd
			}
			chat, err := s.chats.Configure(seg[2], body)
			if err != nil {
				fail(w, 400, err.Error())
				return
			}
			s.raw(w, json.RawMessage(s.chats.MarshalChat(chat)))
			return
		case seg[3] == "messages" && method == "POST":
			text, _ := body["text"].(string)
			overrides := toMapSlice(body["overrides"])
			mentions := toStrSlice(body["mentions"])
			if _, err := s.chats.Send(seg[2], text, overrides, mentions); err != nil {
				fail(w, 400, err.Error())
				return
			}
			chat, _ := s.chats.Get(seg[2])
			s.raw(w, json.RawMessage(s.chats.MarshalChat(chat)))
			return
		}
	}

	// ---- sessions ----
	if p == "/api/sessions" && method == "GET" {
		s.raw(w, json.RawMessage(s.rt.ListJSON(r.URL.Query().Get("chatId"))))
		return
	}
	if len(seg) == 4 && seg[1] == "sessions" && seg[3] == "cancel" && method == "POST" {
		if err := s.rt.Cancel(seg[2]); err != nil {
			fail(w, 409, err.Error())
			return
		}
		ok(w, map[string]any{"cancelled": true})
		return
	}
	if len(seg) == 4 && seg[1] == "sessions" && seg[3] == "handoff" && method == "POST" {
		toAgent, _ := body["toAgentId"].(string)
		result, err := extra.HandoffSession(s.rt, s.orch, s.hub, s.chats, seg[2], toAgent)
		if err != nil {
			fail(w, 400, err.Error())
			return
		}
		s.hub.AddAudit("user", "session.handoff", seg[2], "allowed", 2, `{"toAgentId":"`+toAgent+`"}`)
		ok(w, result)
		return
	}
	if len(seg) == 3 && seg[1] == "session-permissions" && method == "POST" {
		var optionID *string
		if v, okp := body["optionId"].(string); okp {
			optionID = &v
		}
		if err := s.rt.RespondPermission(seg[2], optionID); err != nil {
			fail(w, 409, err.Error())
			return
		}
		ok(w, map[string]any{"answered": true})
		return
	}

	// ---- automations ----
	if p == "/api/automations" && method == "GET" {
		s.raw(w, json.RawMessage(mustJSON(s.automations.Records())))
		return
	}
	if p == "/api/automations" && method == "POST" {
		rec, err := s.automations.Create(body)
		if err != nil {
			fail(w, 400, err.Error())
			return
		}
		ok(w, rec)
		return
	}
	if len(seg) == 3 && seg[1] == "automations" {
		if method == "PATCH" {
			rec, err := s.automations.Update(seg[2], body)
			if err != nil {
				fail(w, 400, err.Error())
				return
			}
			ok(w, rec)
			return
		}
		if method == "DELETE" {
			if err := s.automations.Remove(seg[2]); err != nil {
				fail(w, 400, err.Error())
				return
			}
			ok(w, map[string]any{"removed": true})
			return
		}
	}

	// ---- missions ----
	if p == "/api/missions" && method == "GET" {
		s.raw(w, s.orch.MarshalAll())
		return
	}
	if p == "/api/missions" && method == "POST" {
		wd, err := resolveWorkdir(str(body["workdir"]))
		if err != nil {
			fail(w, 400, err.Error())
			return
		}
		body["workdir"] = wd
		raw, _ := json.Marshal(body)
		var spec orch.Spec
		if json.Unmarshal(raw, &spec) != nil {
			fail(w, 400, "请求体无效")
			return
		}
		m, err := s.orch.Create(spec)
		if err != nil {
			fail(w, 400, err.Error())
			return
		}
		s.raw(w, s.orch.Marshal(m))
		return
	}
	if len(seg) == 3 && seg[1] == "missions" && method == "GET" {
		m := s.orch.Get(seg[2])
		if m == nil {
			fail(w, 404, "任务不存在")
			return
		}
		s.raw(w, s.orch.Marshal(m))
		return
	}
	if len(seg) == 4 && seg[1] == "missions" && seg[3] == "diff" && method == "GET" {
		m := s.orch.Get(seg[2])
		if m == nil {
			fail(w, 404, "任务不存在")
			return
		}
		result, err := s.worktrees.Diff(m.ID, s.orch.BaseDir(m))
		if err != nil {
			fail(w, 400, err.Error())
			return
		}
		ok(w, result)
		return
	}
	if len(seg) == 4 && seg[1] == "missions" && (seg[3] == "merge" || seg[3] == "discard") && method == "POST" {
		m := s.orch.Get(seg[2])
		if m == nil {
			fail(w, 404, "任务不存在")
			return
		}
		var result map[string]any
		var err error
		if seg[3] == "merge" {
			message, _ := body["message"].(string)
			result, err = s.worktrees.Merge(m.ID, s.orch.BaseDir(m), message)
		} else {
			result, err = s.worktrees.Discard(m.ID, s.orch.BaseDir(m))
		}
		if err != nil {
			fail(w, 400, err.Error())
			return
		}
		s.hub.AddAudit("user", "worktree."+seg[3], m.ID, "allowed", 2, "")
		ok(w, result)
		return
	}
	if len(seg) == 4 && seg[1] == "missions" && seg[3] == "fork" && method == "POST" {
		taskID, _ := body["taskId"].(string)
		f, err := s.orch.Fork(seg[2], taskID)
		if err != nil {
			fail(w, 400, err.Error())
			return
		}
		s.raw(w, s.orch.Marshal(f))
		return
	}

	// ---- health / real ----
	if p == "/api/health" && method == "GET" {
		started, _ := s.hub.Meta["startedAt"].(int64)
		ok(w, map[string]any{"status": "ok", "version": s.hub.Version, "sessionRuntime": "acp-v1",
			"uptime": time.Now().UnixMilli() - started, "realEnabled": REAL_ENABLED})
		return
	}
	if p == "/api/real/status" && method == "GET" {
		ok(w, s.realStatus())
		return
	}
	if p == "/api/real/queue" && method == "GET" {
		ok(w, map[string]any{"pending": 0, "running": s.realRunning, "entries": []string{}})
		return
	}
	if p == "/api/real/config" && method == "POST" {
		if perm, okp := body["permission"].(string); okp && perm != "" &&
			perm != "readonly" && perm != "workspace" && perm != "shell" {
			fail(w, 400, "未知权限模式："+perm)
			return
		}
		if wd, okw := body["workdir"].(string); okw && wd != "" {
			resolved, err := resolveWorkdir(wd)
			if err != nil {
				fail(w, 400, err.Error())
				return
			}
			_ = os.MkdirAll(resolved, 0o755)
			body["workdir"] = resolved
		}
		for _, k := range []string{"permission", "workdir", "timeoutMs", "maxConcurrent", "defaultTool"} {
			if v, exists := body[k]; exists {
				cfgSet(k, v)
			}
		}
		if cfgInt("timeoutMs") < 10000 {
			cfgSet("timeoutMs", 10000)
		}
		if cfgInt("timeoutMs") > 600000 {
			cfgSet("timeoutMs", 600000)
		}
		if cfgInt("maxConcurrent") < 1 {
			cfgSet("maxConcurrent", 1)
		}
		if cfgInt("maxConcurrent") > 8 {
			cfgSet("maxConcurrent", 8)
		}
		s.hub.AddAudit("user", "real.config.update", "global", "allowed", 2,
			`{"permission":"`+cfgString("permission")+`","workdir":"`+cfgString("workdir")+`","tool":"`+cfgString("defaultTool")+`"}`)
		s.log("info", "策略中心", "真实执行配置已更新："+cfgString("permission")+" / "+cfgString("defaultTool"))
		ok(w, s.realStatus())
		return
	}

	if p == "/api/snapshot" && method == "GET" {
		ok(w, s.snapshotData())
		return
	}
	if p == "/api/events" && method == "GET" {
		s.serveEvents(w, r)
		return
	}

	// ---- agents & skills ----
	if p == "/api/agents" && method == "GET" {
		s.raw(w, json.RawMessage(mustJSON(s.hub.Agents())))
		return
	}
	if p == "/api/skills" && method == "GET" {
		ok(w, s.skills.List())
		return
	}
	if p == "/api/agents" && method == "POST" {
		name, _ := body["name"].(string)
		typ, _ := body["type"].(string)
		if name == "" || typ == "" {
			fail(w, 400, "name 与 type 必填")
			return
		}
		if id, _ := body["id"].(string); id != "" && s.hub.Agent(id) != nil {
			fail(w, 409, "实例 ID 已存在")
			return
		}
		if rawSkills, exists := body["skills"]; exists {
			names, err := s.skills.Assign(toStrSlice(rawSkills))
			if err != nil {
				fail(w, 400, err.Error())
				return
			}
			body["skills"] = names
		}
		connection, _ := body["connection"].(string)
		if connection == "acp" {
			agent, err := s.registerAcp(body)
			if err != nil {
				fail(w, 400, err.Error())
				return
			}
			ok(w, agent)
			return
		}
		localTool := ""
		if typ == "codex" {
			localTool = "codex"
		} else if typ == "claude-code" {
			localTool = "claude"
		}
		if connection == "http" || strings.HasPrefix(str(body["endpoint"]), "http") {
			fail(w, 400, "Go 版暂不支持 HTTP 桥接执行端，请使用 ACP 或本机 CLI")
			return
		}
		if !REAL_ENABLED || localTool == "" || resolveBin(localTool) == "" {
			fail(w, 400, "请选择本机已安装的 Codex / Claude Code")
			return
		}
		raw, _ := json.Marshal(body)
		var agent hub.Agent
		if json.Unmarshal(raw, &agent) != nil {
			fail(w, 400, "请求体无效")
			return
		}
		created := s.hub.CreateAgent(&agent)
		created.Real = true
		created.RealTool = localTool
		created.Load = 0
		created.Trust = 1
		created.Communication = "acp"
		created.Endpoint = "acp://" + localTool
		created.AdapterKind = "ACP"
		ok(w, created)
		return
	}
	if len(seg) == 3 && seg[1] == "agents" && method == "PATCH" {
		a := s.hub.Agent(seg[2])
		if a == nil {
			fail(w, 404, "智能体不存在")
			return
		}
		if rawSkills, exists := body["skills"]; exists {
			names, err := s.skills.Assign(toStrSlice(rawSkills))
			if err != nil {
				fail(w, 400, err.Error())
				return
			}
			a.Skills = names
			s.skills.DropCache()
			s.hub.Emit("agent", a)
		}
		if rawEnabled, exists := body["enabled"]; exists {
			enabled, _ := rawEnabled.(bool)
			ok(w, s.hub.SetAgentEnabled(seg[2], enabled))
			return
		}
		ok(w, a)
		return
	}

	if p == "/api/adapters" && method == "GET" {
		tools := map[string]any{}
		for _, t := range []string{"codex", "claude"} {
			bin := resolveBin(t)
			tools[t] = map[string]any{"available": bin != "", "bin": bin, "label": binLabels[t],
				"acp": acpInstalled(t) != ""}
		}
		ok(w, map[string]any{"tools": tools, "permissions": permissions})
		return
	}
	if p == "/api/adapters" && method == "POST" {
		fail(w, 400, "Go 版请通过 agents 接口注册 ACP 执行端")
		return
	}

	// ---- tasks ----
	if p == "/api/tasks" && method == "GET" {
		s.raw(w, json.RawMessage(mustJSON(s.hub.Tasks)))
		return
	}
	if p == "/api/tasks" && method == "POST" {
		title, _ := body["title"].(string)
		if title == "" {
			fail(w, 400, "title 必填")
			return
		}
		if !REAL_ENABLED {
			fail(w, 400, "真实执行未启用（服务端设置了 HARNESS_HUB_REAL=0）")
			return
		}
		if _, exists := body["permission"]; !exists {
			body["permission"] = cfgString("permission")
		}
		if _, exists := body["workdir"]; !exists {
			body["workdir"] = cfgString("workdir")
		}
		raw, _ := json.Marshal(body)
		var task hub.Task
		if json.Unmarshal(raw, &task) != nil {
			fail(w, 400, "请求体无效")
			return
		}
		task.Real = true
		created := s.hub.SubmitTask(&task)
		go s.runSingleTask(created)
		ok(w, created)
		return
	}
	if len(seg) == 4 && seg[1] == "tasks" && seg[3] == "cancel" && method == "POST" {
		t := s.hub.CancelTask(seg[2])
		if t == nil {
			fail(w, 409, "任务不存在")
			return
		}
		ok(w, t)
		return
	}
	if len(seg) == 4 && seg[1] == "tasks" && seg[3] == "retry" && method == "POST" {
		t := s.hub.Task(seg[2])
		if t == nil || t.MissionID != "" {
			fail(w, 409, "这是编排任务的内部调用，请在指挥台重新提交目标")
			return
		}
		if t.Status != "failed" {
			fail(w, 409, "仅失败任务可重试")
			return
		}
		go s.runSingleTask(t)
		ok(w, t)
		return
	}

	// ---- messages ----
	if p == "/api/messages" && method == "GET" {
		msgs := s.hub.Bus
		if len(msgs) > 200 {
			msgs = msgs[len(msgs)-200:]
		}
		s.raw(w, json.RawMessage(mustJSON(msgs)))
		return
	}
	if p == "/api/messages" && method == "POST" {
		from, _ := body["from"].(string)
		to, _ := body["to"].(string)
		if from == "" || to == "" {
			fail(w, 400, "from 与 to 必填")
			return
		}
		text, _ := body["text"].(string)
		kind, _ := body["kind"].(string)
		sensitivity := 1
		if v, okp := body["sensitivity"].(float64); okp {
			sensitivity = int(v)
		}
		ok(w, s.hub.SendMessage(from, to, text, kind, sensitivity, ""))
		return
	}

	// ---- approvals ----
	if p == "/api/approvals" && method == "GET" {
		ok(w, s.pendingApprovals())
		return
	}

	// ---- audit / logs ----
	if p == "/api/audit" && method == "GET" {
		limit := atoiDefault(r.URL.Query().Get("limit"), 300)
		entries := s.hub.Audit
		if len(entries) > limit {
			entries = entries[len(entries)-limit:]
		}
		s.raw(w, json.RawMessage(mustJSON(entries)))
		return
	}
	if p == "/api/audit/verify" && method == "GET" {
		ok(w, s.hub.VerifyAuditChain())
		return
	}
	if p == "/api/logs" && method == "GET" {
		limit := atoiDefault(r.URL.Query().Get("limit"), 200)
		logs := s.hub.Logs
		if len(logs) > limit {
			logs = logs[len(logs)-limit:]
		}
		s.raw(w, json.RawMessage(mustJSON(logs)))
		return
	}

	// ---- demo-layer compatibility (context / grants / policy / control) ----
	if p == "/api/context/sessions" && method == "GET" {
		ok(w, []any{})
		return
	}
	if p == "/api/context/sync" && method == "POST" {
		ok(w, map[string]any{"synced": 0})
		return
	}
	if p == "/api/context/write" && method == "POST" {
		ok(w, map[string]any{"written": false, "note": "Go 版上下文共享暂未开放"})
		return
	}
	if p == "/api/grants" && method == "GET" {
		ok(w, []any{})
		return
	}
	if p == "/api/policy" && method == "GET" {
		ok(w, map[string]any{"maxSensitivity": 3, "defaultSensitivity": 1, "peerMessaging": true})
		return
	}
	if p == "/api/policy" && method == "PATCH" {
		ok(w, map[string]any{"maxSensitivity": 3, "defaultSensitivity": 1, "peerMessaging": true})
		return
	}
	if p == "/api/control" && method == "POST" {
		action, _ := body["action"].(string)
		running := action != "stop"
		s.hub.Meta["running"] = running
		s.hub.Emit("meta", s.hub.Meta)
		ok(w, map[string]any{"running": running})
		return
	}

	// ---- static ----
	if !strings.HasPrefix(p, "/api/") {
		s.serveStatic(w, p)
		return
	}
	fail(w, 404, "接口不存在: "+p)
}
