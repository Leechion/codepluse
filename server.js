#!/usr/bin/env node
/**
 * Harness Hub - coordination server
 * ---------------------------------------------------------------
 * Zero dependencies. Node >= 18.
 *   node server.js                 # http://127.0.0.1:8787
 *   PORT=9000 node server.js
 *
 * Exposes the very same core used by the browser console:
 *   - REST  /api/*   registration, dispatch, context, grants, audit
 *   - SSE   /api/events   real-time event stream for external Harnesses
 */
'use strict';

const http = require('http');
const fs = require('fs');
const path = require('path');
const Core = require('./core/engine.js');
const Real = require('./core/real-adapters.js');
const HttpHarnessAdapter = require('./core/http-adapter.js');
const Communication = require('./core/communication.js');
const { Orchestrator } = require('./core/orchestrator.js');
const { ChatController } = require('./core/chat-controller.js');
const { AcpRuntime } = require('./core/acp-runtime.js');
const { createFiles } = require('./core/readonly-mcp.js');
const { Notifier } = require('./core/notifier.js');
const { Worktrees } = require('./core/worktree.js');
const { Automations } = require('./core/automations.js');
const { Skills } = require('./core/skills.js');
const { handoffSession } = require('./core/handoff.js');

const ROOT = __dirname;
const PORT = Number(process.env.PORT || 8787);
const HOST = process.env.HOST || '127.0.0.1';
const REAL_ENABLED = process.env.HARNESS_HUB_REAL !== '0';

const realConfig = {
  permission: process.env.HARNESS_HUB_PERMISSION || 'readonly',
  workdir: path.resolve(process.env.HARNESS_HUB_WORKDIR || ROOT),
  allowedRoot: path.resolve(process.env.HARNESS_HUB_ALLOWED_ROOT || ROOT),
  timeoutMs: Number(process.env.HARNESS_HUB_TIMEOUT || 180000),
  maxConcurrent: Number(process.env.HARNESS_HUB_CONCURRENCY || 2),
  defaultTool: process.env.HARNESS_HUB_TOOL || 'auto'
};

const hub = new Core.Hub({ autoSeed: false, mode: 'real' });
hub.meta.running = true;
new Notifier(hub, { log: (level, source, text) => hub.log(level, source, text) });
const acpRuntime = new AcpRuntime(hub, { stateFile: process.env.HARNESS_HUB_STATE_FILE === ':memory:' ? ':memory:' : path.join(ROOT, '.data', 'sessions.json'), timeoutMs: realConfig.timeoutMs });
const customAcp = new Map();
acpRuntime.launch = (agent, permission) => {
  if (customAcp.has(agent.id)) return { ...customAcp.get(agent.id), env: process.env };
  const env = Real.buildEnv(agent.realTool);
  if (agent.realTool === 'codex') env.INITIAL_AGENT_MODE = permission === 'workspace' ? 'workspace-write' : 'read-only';
  if (agent.realTool === 'claude') env.CLAUDE_CODE_EXECUTABLE = Real.resolveBin('claude');
  return { command: process.execPath, args: [AcpRuntime.installed(agent.realTool)], env };
};

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.js': 'application/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.ico': 'image/x-icon',
  '.md': 'text/markdown; charset=utf-8',
  '.png': 'image/png',
  '.webmanifest': 'application/manifest+json'
};

function send(res, status, body, type) {
  const payload = typeof body === 'string' || Buffer.isBuffer(body) ? body : JSON.stringify(body);
  res.writeHead(status, {
    'Content-Type': type || 'application/json; charset=utf-8',
    'Cache-Control': 'no-store',
    'Access-Control-Allow-Origin': '*',
    'Access-Control-Allow-Methods': 'GET,POST,PATCH,DELETE,OPTIONS',
    'Access-Control-Allow-Headers': 'Content-Type, Authorization, X-Harness-Id',
    'X-Harness-Hub': '1.0.0'
  });
  res.end(payload);
}

function ok(res, data) { send(res, 200, { ok: true, data: data, at: Date.now() }); }
function fail(res, status, message) { send(res, status, { ok: false, error: message, at: Date.now() }); }

function readBody(req) {
  return new Promise((resolve, reject) => {
    let buf = '';
    req.on('data', c => {
      buf += c;
      if (buf.length > 1024 * 1024) { reject(new Error('请求体过大')); req.destroy(); }
    });
    req.on('end', () => {
      if (!buf) return resolve({});
      try { resolve(JSON.parse(buf)); } catch (e) { reject(new Error('JSON 解析失败: ' + e.message)); }
    });
    req.on('error', reject);
  });
}

/* ------------------------------ static ------------------------------ */
function serveStatic(res, pathname) {
  const rel = decodeURIComponent(pathname === '/' ? '/index.html' : pathname);
  const candidates = [
    path.join(ROOT, 'public', rel),
    path.join(ROOT, rel)
  ];
  let file = null;
  for (const p of candidates) {
    const normalized = path.normalize(p);
    if (normalized.indexOf(ROOT) !== 0) continue;
    if (fs.existsSync(normalized) && fs.statSync(normalized).isFile()) { file = normalized; break; }
  }
  if (!file) return fail(res, 404, '资源不存在: ' + rel);
  const type = MIME[path.extname(file).toLowerCase()] || 'application/octet-stream';
  fs.readFile(file, (err, buf) => {
    if (err) return fail(res, 500, err.message);
    send(res, 200, buf, type);
  });
}

/* ------------------------------ Real execution ------------------------------ */
const STATE = Core.CONST.STATE;
const realAdapters = new Map();
const realQueue = [];
const realState = new Map();
let realRunning = 0, realSeq = 0;

try { fs.mkdirSync(realConfig.workdir, { recursive: true }); } catch (_) { /* noop */ }

function resolveWorkdir(input) {
  const candidate = path.resolve(input || realConfig.workdir);
  const root = realConfig.allowedRoot;
  try {
    const realRoot = fs.realpathSync(root);
    let ancestor = candidate;
    while (!fs.existsSync(ancestor) && path.dirname(ancestor) !== ancestor) ancestor = path.dirname(ancestor);
    const resolved = path.resolve(fs.realpathSync(ancestor), path.relative(ancestor, candidate));
    const relative = path.relative(realRoot, resolved);
    if (relative === '..' || relative.startsWith('..' + path.sep) || path.isAbsolute(relative)) return { ok: false, error: '工作目录必须位于 ' + root + ' 之内' };
    return { ok: true, dir: resolved };
  } catch (e) { return { ok: false, error: '无法解析工作目录：' + e.message }; }
}

function getAdapter(tool, permission, workdir, model = '') {
  const key = [tool, permission, workdir, model].join('|');
  if (realAdapters.has(key)) {
    const cached = realAdapters.get(key);
    cached.timeoutMs = realConfig.timeoutMs;
    return cached;
  }
  const a = new Real.RealHarnessAdapter({
    tool, permission, workdir, model,
    timeoutMs: realConfig.timeoutMs,
    maxConcurrent: realConfig.maxConcurrent
  });
  a.emitLog = (level, source, text) => hub.log(level, source, text);
  realAdapters.set(key, a);
  return a;
}

function toolFor(task) {
  const t = (task.execution && task.execution.tool) || realConfig.defaultTool;
  if (t === 'codex' || t === 'claude') return t;
  const s = Real.status().tools;              // auto -> first available CLI
  if (s.claude.available) return 'claude';
  if (s.codex.available) return 'codex';
  return 'claude';
}

function agentFor(tool) {
  const type = tool === 'codex' ? 'codex' : 'claude-code';
  let found = null;
  hub.agents.forEach(a => { if (!found && a.type === type && a.enabled) found = a; });
  return found;
}

function enqueueReal(taskId) {
  const s = realState.get(taskId);
  if (s === 'queued' || s === 'running') return false;
  realState.set(taskId, 'queued');
  realQueue.push({ taskId, seq: ++realSeq });
  hub.log('info', '真实执行', '任务 ' + taskId.slice(-6) + ' 已进入真实执行队列（队列长度 ' + realQueue.length + '）');
  pumpReal();
  return true;
}

function pumpReal() {
  if (!REAL_ENABLED) return;
  while (realRunning < realConfig.maxConcurrent && realQueue.length) {
    const item = realQueue.shift();
    realRunning += 1;
    realState.set(item.taskId, 'running');
    runReal(item.taskId)
      .catch(e => hub.log('error', '真实执行', '执行队列异常：' + e.message))
      .finally(() => { realRunning -= 1; pumpInvocations(); pumpReal(); });
  }
}

async function runReal(taskId) {
  const task = hub.tasks.get(taskId);
  if (!task || task.execution.mode !== 'real') { realState.set(taskId, 'skipped'); return; }
  if (task.status === STATE.CANCELLED) { realState.set(taskId, 'cancelled'); return; }

  const tool = toolFor(task);
  const permission = task.execution.permission;
  const wd = resolveWorkdir(task.execution.workdir);
  if (!wd.ok) {
    hub.tasks.completeReal(taskId, { ok: false, error: wd.error, meta: {} });
    realState.set(taskId, 'failed');
    return;
  }
  const adapter = getAdapter(tool, permission, wd.dir);
  const avail = adapter.available();
  if (!avail.ok) {
    hub.tasks.completeReal(taskId, { ok: false, error: avail.reason, meta: {} });
    realState.set(taskId, 'failed');
    return;
  }

  const agent = agentFor(tool);
  if (!agent) {
    hub.tasks.completeReal(taskId, { ok: false, error: '没有已启用的真实执行端：' + tool, meta: {} });
    realState.set(taskId, 'failed'); return;
  }
  hub.tasks.startReal(taskId, agent ? agent.id : 'hub', { tool: tool, permission: permission });

  // Context is injected only after the ACL + sensitivity check passes.
  let contextView = null;
  if (task.sessionId && agent && agent.id !== 'hub') {
    const r = hub.context.read(agent.id, task.sessionId);
    if (r.ok) {
      contextView = { entries: r.entries, version: r.version, redacted: r.redacted };
      hub.log('info', '上下文同步', '真实执行注入 ' + r.entries.length + ' 条已授权上下文' + (r.redacted ? '（脱敏 ' + r.redacted + ' 条）' : ''));
    } else {
      hub.log('warn', '权限门禁', '真实执行未注入上下文：' + r.reason);
    }
  }

  const progressTimer = setInterval(() => {
    const t = hub.tasks.get(taskId);
    if (!t || t.status !== STATE.RUNNING) { clearInterval(progressTimer); return; }
    hub.tasks.updateReal(taskId, Math.min(90, t.progress + 3), 'CLI 执行中…');
  }, 5000);

  let result;
  try {
    const prompt = (task.prompt || task.title) + (contextView ? '\n已授权会话内容：' + JSON.stringify(contextView.entries) : '');
    result = await acpRuntime.prompt(agent, prompt, { permission, workdir: wd.dir, sessionKey: 'single:' + task.id,
      structured: false, stage: task.title, timeoutMs: realConfig.timeoutMs });
  } catch (e) {
    result = { ok: false, error: '适配器异常：' + e.message, meta: {} };
  } finally {
    clearInterval(progressTimer);
  }

  hub.tasks.completeReal(taskId, result);
  realState.set(taskId, result.ok ? 'done' : 'failed');
}

function realStatus() {
  const base = Real.status();
  return {
    enabled: REAL_ENABLED,
    config: {
      permission: realConfig.permission,
      workdir: realConfig.workdir,
      allowedRoot: realConfig.allowedRoot,
      timeoutMs: realConfig.timeoutMs,
      maxConcurrent: realConfig.maxConcurrent,
      defaultTool: realConfig.defaultTool
    },
    tools: base.tools,
    permissions: base.permissions,
    warnings: (function () {
      const w = [];
      if (!base.tools.claude.available) w.push('未找到 Claude Code CLI 可执行文件。');
      if (!base.tools.codex.available) w.push('未找到 Codex CLI 可执行文件。');
      if (process.env.HTTPS_PROXY || process.env.https_proxy) {
        w.push('检测到 HTTP 代理：Codex 已自动回退到本地网关（Responses API），首次调用约需 60s；如需直连官方后端可设置 HARNESS_HUB_CODEX_GATEWAY=0，模型可用 HARNESS_HUB_CODEX_MODEL 覆盖。');
      }
      return w;
    })(),
    queue: { pending: realQueue.length, running: realRunning, entries: realQueue.map(x => x.taskId) },
    stats: (function () {
      const all = hub.tasks.tasks;
      return {
        realTasks: all.filter(t => t.execution.mode === 'real').length,
        realRunning: all.filter(t => t.execution.mode === 'real' && t.status === STATE.RUNNING).length,
        realCompleted: all.filter(t => t.execution.mode === 'real' && t.status === STATE.SUCCEEDED).length
      };
    })()
  };
}

// Mark the two CLI-backed instances as real and give them real heartbeat providers.
if (REAL_ENABLED) {
  const st = Real.status();
  Object.keys(st.tools).forEach(tool => {
    if (!st.tools[tool].available) return;
    const a = hub.createAgent({ id: 'agt_' + tool + '_01', type: tool === 'codex' ? 'codex' : 'claude-code',
      name: tool === 'codex' ? 'Codex' : 'Claude Code', load: 0, trust: 1 });
    a.real = !!st.tools[tool].available;
    a.realTool = tool;
    a.endpoint = st.tools[tool].bin || a.endpoint;
    a.adapterKind = tool === 'codex' ? 'CodexCLIAdapter' : 'ClaudeCodeCLIAdapter';
    a.communication = 'acp'; a.endpoint = 'acp://' + tool; a.adapterKind = 'ACP';
    hub._adapters[a.id] = getAdapter(tool, realConfig.permission, realConfig.workdir);
  });
  hub.log('success', '系统', '真实执行已启用：' + Object.keys(st.tools).map(k => k + '=' + (st.tools[k].available ? '就绪' : '未找到')).join(' / '));
}

const customAcpFile = path.join(ROOT, '.data', 'acp-agents.json');
function registerAcp(spec, persist = true) {
  if (!REAL_ENABLED) throw new Error('本机 Harness 执行已关闭');
  if (typeof spec.command !== 'string' || !spec.command.trim() || /\.(cmd|bat)$/i.test(spec.command)) throw new Error('请提供可执行文件或 node 路径，不使用 shell 脚本入口');
  if (!Array.isArray(spec.args) || spec.args.length > 40 || spec.args.some(a => typeof a !== 'string')) throw new Error('args 必须是字符串数组');
  if (!['plan', 'read-only', 'readonly'].includes(spec.readOnlyMode || 'plan')) throw new Error('只读模式须为 plan、read-only 或 readonly');
  if (spec.id && hub.agents.has(spec.id)) throw new Error('实例 ID 已存在');
  const a = hub.createAgent({ ...spec, load: 0, trust: 1, capabilities: spec.capabilities || ['analysis', 'code.write'], clearance: spec.clearance ?? 2 });
  a.real = true; a.communication = 'acp'; a.endpoint = 'acp://custom/' + a.id; a.adapterKind = 'ACP';
  a.acpModes = { readonly: spec.readOnlyMode || 'plan', shell: spec.readOnlyMode || 'plan', workspace: spec.workspaceMode || 'default' };
  customAcp.set(a.id, { command: spec.command === 'node' ? process.execPath : spec.command, args: spec.args });
  if (persist && process.env.HARNESS_HUB_STATE_FILE !== ':memory:') {
    const saved = [...customAcp.entries()].map(([id, launch]) => ({ id, name: hub.agents.get(id).name, type: hub.agents.get(id).type,
      command: launch.command, args: launch.args, readOnlyMode: hub.agents.get(id).acpModes.readonly, workspaceMode: hub.agents.get(id).acpModes.workspace }));
    fs.mkdirSync(path.dirname(customAcpFile), { recursive: true }); fs.writeFileSync(customAcpFile, JSON.stringify(saved));
  }
  hub.emit('agent', a); return a;
}
if (REAL_ENABLED && process.env.HARNESS_HUB_STATE_FILE !== ':memory:' && fs.existsSync(customAcpFile)) {
  try { JSON.parse(fs.readFileSync(customAcpFile, 'utf8')).forEach(s => registerAcp(s, false)); }
  catch (e) { console.error('[hub] 读取 ACP 执行端配置失败：' + e.message); }
}

/* ------------------------------ SSE ------------------------------ */
const sseClients = new Set();

hub.on('*', evt => {
  if (!sseClients.size) return;
  const line = 'event: ' + evt.type + '\ndata: ' + JSON.stringify({ type: evt.type, data: evt.data, at: evt.at }) + '\n\n';
  sseClients.forEach(res => {
    try { res.write(line); } catch (_) { sseClients.delete(res); }
  });
});

function serveEvents(req, res) {
  res.writeHead(200, {
    'Content-Type': 'text/event-stream; charset=utf-8',
    'Cache-Control': 'no-cache, no-transform',
    Connection: 'keep-alive',
    'Access-Control-Allow-Origin': '*',
    'X-Accel-Buffering': 'no'
  });
  res.write(': connected to Harness Hub event stream\n\n');
  res.write('event: snapshot\ndata: ' + JSON.stringify({ type: 'snapshot', data: hub.snapshot(), at: Date.now() }) + '\n\n');
  sseClients.add(res);
  const keepalive = setInterval(() => { try { res.write(': ping\n\n'); } catch (_) { /* noop */ } }, 20000);
  req.on('close', () => { clearInterval(keepalive); sseClients.delete(res); });
}

/* ------------------------------ routing ------------------------------ */
const invocationQueue = [];
const activeAgents = new Map();
function invokeHarness(agentId, prompt, options = {}) {
  return new Promise((resolve, reject) => { invocationQueue.push({ agentId, prompt, options, resolve, reject }); pumpInvocations(); });
}
function pumpInvocations() {
  for (let i = 0; i < invocationQueue.length && realRunning < realConfig.maxConcurrent;) {
    const job = invocationQueue[i];
    const agent = hub.agents.get(job.agentId);
    if ((activeAgents.get(job.agentId) || 0) >= (agent?.maxConcurrency || 1)) { i++; continue; }
    invocationQueue.splice(i, 1); realRunning++;
    activeAgents.set(job.agentId, (activeAgents.get(job.agentId) || 0) + 1);
    (async () => {
      if (!agent || !agent.enabled || !(agent.real || agent.communication === 'http')) throw new Error('Harness 未连接或已禁用');
      const permission = job.options.permission || 'readonly';
      if (job.options.model && !/^[a-zA-Z0-9][\w.:/@+\-]{0,119}$/.test(job.options.model)) throw new Error('模型名称无效');
      const wd = resolveWorkdir(job.options.workdir);
      if (!wd.ok) throw new Error(wd.error);
      agent.status = 'busy'; agent.load = (activeAgents.get(agent.id) || 1) / agent.maxConcurrency; hub.emit('agent', agent);
      const adapter = hub._adapters[agent.id];
      if (adapter instanceof HttpHarnessAdapter) {
        if (!(agent.permissions || ['readonly']).includes(permission)) throw new Error('HTTP Harness 未声明支持此权限模式');
        return adapter.send({ id: 'call_' + require('crypto').randomUUID(), v: '1.0', from: 'hub', to: agent.id,
          correlationId: job.options.missionId || null, scope: { sensitivity: job.options.sensitivity ?? 1 },
          payload: { text: job.prompt, permission, workdir: wd.dir, model: job.options.model || null } });
      }
      if (!REAL_ENABLED) throw new Error('本机 CLI 执行已关闭');
      return acpRuntime.prompt(agent, job.prompt, { ...job.options, permission, workdir: wd.dir, timeoutMs: realConfig.timeoutMs });
    })().then(job.resolve, job.reject).finally(() => {
      realRunning--; activeAgents.set(job.agentId, activeAgents.get(job.agentId) - 1);
      if (agent) { agent.load = activeAgents.get(agent.id) / agent.maxConcurrency; agent.status = agent.load ? 'busy' : 'online'; hub.emit('agent', agent); }
      pumpInvocations(); pumpReal();
    });
  }
}
const worktrees = new Worktrees({ root: ROOT });
const skills = new Skills({ dir: path.join(ROOT, 'skills') });
const orchestrator = new Orchestrator(hub, invokeHarness, (mission, request) => {
  const wd = resolveWorkdir(mission.workdir);
  if (!wd.ok) throw new Error(wd.error);
  const content = createFiles(wd.dir)('read_file', request);
  content.text = content.text.slice(0, 30000);
  return content;
}, worktrees, skills);
const missionStateFile = process.env.HARNESS_HUB_STATE_FILE || path.join(ROOT, '.data', 'missions.json');
if (missionStateFile !== ':memory:') {
  try {
    if (fs.existsSync(missionStateFile)) {
      const saved = JSON.parse(fs.readFileSync(missionStateFile, 'utf8').replace(/^\uFEFF/, ''));
      if (!Array.isArray(saved)) throw new Error('任务记录格式无效');
      orchestrator.missions = saved.slice(0, 20);
      orchestrator.missions.forEach(m => {
        if (['planning', 'running', 'reporting'].includes(m.status)) {
          m.status = 'failed'; m.error = '服务重启中断了执行，请重新下达目标；不会自动重复执行。';
          m.tasks.forEach(t => { if (t.status !== 'completed') { t.status = 'failed'; t.error = m.error; } });
        }
      });
    }
  } catch (e) { console.error('[hub] 读取任务记录失败：' + e.message); }
  hub.on('mission', () => {
    try {
      fs.mkdirSync(path.dirname(missionStateFile), { recursive: true });
      fs.writeFileSync(missionStateFile + '.tmp', JSON.stringify(orchestrator.missions.slice(0, 20)));
      fs.renameSync(missionStateFile + '.tmp', missionStateFile);
    } catch (e) { console.error('[hub] 保存任务记录失败：' + e.message); }
  });
}

const communication = new Communication(hub, agent => {
  return message => invokeHarness(agent.id, message.payload.text, { sessionKey: 'direct:' + (message.scope.sessionId || agent.id), structured: false });
});

const chatStateFile = path.join(ROOT, '.data', 'chats.json');
const chats = new ChatController(hub, orchestrator, records => {
  if (process.env.HARNESS_HUB_STATE_FILE === ':memory:') return;
  try {
    fs.mkdirSync(path.dirname(chatStateFile), { recursive: true });
    fs.writeFileSync(chatStateFile + '.tmp', JSON.stringify(records.slice(0, 20)));
    fs.renameSync(chatStateFile + '.tmp', chatStateFile);
  } catch (e) { console.error('[hub] 保存对话失败：' + e.message); }
});
if (process.env.HARNESS_HUB_STATE_FILE !== ':memory:' && fs.existsSync(chatStateFile)) {
  try {
    const saved = JSON.parse(fs.readFileSync(chatStateFile, 'utf8').replace(/^\uFEFF/, ''));
    if (!Array.isArray(saved)) throw new Error('对话记录无效');
    chats.chats = saved.slice(0, 20).map(c => ({ ...c, pending: 0 }));
  } catch (e) { console.error('[hub] 读取对话失败：' + e.message); }
}

const automations = new Automations(chats, {
  file: process.env.HARNESS_HUB_STATE_FILE === ':memory:' ? ':memory:' : path.join(ROOT, '.data', 'automations.json'),
  log: (level, source, text) => hub.log(level, source, text)
});
automations.load();
setInterval(() => { try { automations.tick(); } catch (_) { /* tick must never crash the server */ } }, 30000).unref();

async function route(req, res, url) {
  const p = url.pathname;
  const method = req.method.toUpperCase();
  const body = ['POST', 'PATCH', 'PUT', 'DELETE'].includes(method) ? await readBody(req) : {};
  const seg = p.split('/').filter(Boolean); // ['api','agents','<id>',...]

  if (p === '/api/chats' && method === 'GET') return ok(res, chats.chats.map(c => ({ id: c.id, title: c.title, updatedAt: c.updatedAt, pending: c.pending })));
  if (p === '/api/chats' && method === 'POST') {
    const wd = resolveWorkdir(body.workdir);
    if (!wd.ok) return fail(res, 400, wd.error);
    try { return ok(res, chats.view(chats.create({ ...body, workdir: wd.dir }))); }
    catch (e) { return fail(res, 400, e.message); }
  }
  if (seg[1] === 'chats' && seg[2]) {
    try {
      if (seg.length === 3 && method === 'GET') return ok(res, { ...chats.view(chats.get(seg[2])), sessions: acpRuntime.list(seg[2]) });
      if (seg[3] === 'config' && method === 'PATCH') {
        if (body.workdir !== undefined) {
          const wd = resolveWorkdir(body.workdir);
          if (!wd.ok) return fail(res, 400, wd.error);
          body.workdir = wd.dir;
        }
        return ok(res, chats.view(chats.configure(seg[2], body)));
      }
      if (seg[3] === 'messages' && method === 'POST') {
        chats.send(seg[2], body.text, body.overrides || [], body.mentions || []);
        return ok(res, chats.view(chats.get(seg[2])));
      }
    } catch (e) { return fail(res, 400, e.message); }
  }

  if (p === '/api/sessions' && method === 'GET') return ok(res, acpRuntime.list(url.searchParams.get('chatId')));
  if (seg[1] === 'sessions' && seg[2] && seg[3] === 'cancel' && method === 'POST') {
    try { await acpRuntime.cancel(seg[2]); return ok(res, { cancelled: true }); }
    catch (e) { return fail(res, 409, e.message); }
  }
  if (seg[1] === 'sessions' && seg[2] && seg[3] === 'handoff' && method === 'POST') {
    try {
      const result = await handoffSession({ acpRuntime, orchestrator, hub, chats }, seg[2], body.toAgentId);
      hub.audit({ actor: 'user', action: 'session.handoff', target: seg[2], decision: Core.CONST.DECISION.ALLOWED, sensitivity: 2, detail: JSON.stringify({ toAgentId: body.toAgentId }) });
      return ok(res, result);
    } catch (e) { return fail(res, 400, e.message); }
  }
  if (seg[1] === 'session-permissions' && seg[2] && method === 'POST') {
    try { acpRuntime.respondPermission(seg[2], body.optionId ?? null); return ok(res, { answered: true }); }
    catch (e) { return fail(res, 409, e.message); }
  }

  if (p === '/api/automations' && method === 'GET') return ok(res, automations.records);
  if (p === '/api/automations' && method === 'POST') {
    try { return ok(res, automations.create(body)); }
    catch (e) { return fail(res, 400, e.message); }
  }
  if (seg[1] === 'automations' && seg[2] && method === 'PATCH') {
    try { return ok(res, automations.update(seg[2], body)); }
    catch (e) { return fail(res, 404, e.message); }
  }
  if (seg[1] === 'automations' && seg[2] && method === 'DELETE') {
    try { automations.remove(seg[2]); return ok(res, { removed: true }); }
    catch (e) { return fail(res, 404, e.message); }
  }

  if (p === '/api/missions' && method === 'GET') return ok(res, orchestrator.missions);
  if (p === '/api/missions' && method === 'POST') {
    const wd = resolveWorkdir(body.workdir);
    if (!wd.ok) return fail(res, 400, wd.error);
    try { return ok(res, orchestrator.create({ ...body, workdir: wd.dir })); }
    catch (e) { return fail(res, 400, e.message); }
  }
  if (seg[1] === 'missions' && seg[2] && method === 'GET') {
    const mission = orchestrator.missions.find(m => m.id === seg[2]);
    return mission ? ok(res, mission) : fail(res, 404, '任务不存在');
  }
  if (seg[1] === 'missions' && seg[2] && seg[3] === 'diff' && method === 'GET') {
    const mission = orchestrator.missions.find(m => m.id === seg[2]);
    if (!mission) return fail(res, 404, '任务不存在');
    if (!mission.worktree) return fail(res, 409, '该任务未使用工作树隔离');
    try { return ok(res, await worktrees.diff(mission.id, mission.baseWorkdir || mission.workdir)); }
    catch (e) { return fail(res, 409, e.message); }
  }
  if (seg[1] === 'missions' && seg[2] && ['merge', 'discard'].includes(seg[3]) && method === 'POST') {
    const mission = orchestrator.missions.find(m => m.id === seg[2]);
    if (!mission) return fail(res, 404, '任务不存在');
    if (!mission.worktree) return fail(res, 409, '该任务未使用工作树隔离');
    if (!['completed', 'partial', 'failed'].includes(mission.status)) return fail(res, 409, '任务尚未结束，不能' + (seg[3] === 'merge' ? '合并' : '丢弃'));
    try {
      const result = seg[3] === 'merge'
        ? await worktrees.merge(mission.id, mission.baseWorkdir || mission.workdir, body.message)
        : await worktrees.discard(mission.id, mission.baseWorkdir || mission.workdir);
      mission.worktree = seg[3] === 'merge' ? mission.worktree : null;
      mission.worktreeState = seg[3];
      hub.audit({ actor: 'user', action: 'mission.worktree.' + seg[3], target: mission.id, decision: Core.CONST.DECISION.ALLOWED, sensitivity: 2, detail: JSON.stringify({ branch: worktrees.branch(mission.id), message: body.message || '' }) });
      hub.emit('mission', mission);
      return ok(res, { ...result, missionId: mission.id });
    } catch (e) { return fail(res, 409, e.message); }
  }
  if (seg[1] === 'missions' && seg[2] && seg[3] === 'fork' && method === 'POST') {
    try { return ok(res, orchestrator.fork(seg[2], body.taskId)); }
    catch (e) { return fail(res, 400, e.message); }
  }

  if (p === '/api/health') return ok(res, { status: 'ok', version: hub.version, sessionRuntime: 'acp-v1', uptime: Date.now() - hub.meta.startedAt, clients: sseClients.size, realEnabled: REAL_ENABLED });

  if (p === '/api/real/status' && method === 'GET') return ok(res, realStatus());
  if (p === '/api/real/queue' && method === 'GET') {
    return ok(res, { pending: realQueue.length, running: realRunning, entries: realQueue.map(x => x.taskId) });
  }
  if (p === '/api/real/config' && method === 'POST') {
    if (body.permission && !Real.PERMISSIONS[body.permission]) return fail(res, 400, '未知权限模式：' + body.permission);
    if (body.workdir) {
      const wd = resolveWorkdir(body.workdir);
      if (!wd.ok) return fail(res, 400, wd.error);
      body.workdir = wd.dir;
      try { fs.mkdirSync(wd.dir, { recursive: true }); } catch (_) { /* noop */ }
    }
    const allowed = ['permission', 'workdir', 'timeoutMs', 'maxConcurrent', 'defaultTool'];
    allowed.forEach(k => { if (body[k] !== undefined) realConfig[k] = body[k]; });
    realConfig.timeoutMs = Math.max(10000, Math.min(600000, Number(realConfig.timeoutMs) || 180000));
    realConfig.maxConcurrent = Math.max(1, Math.min(8, Number(realConfig.maxConcurrent) || 2));
    hub.audit({ actor: 'user', action: 'real.config.update', target: 'global', decision: Core.CONST.DECISION.ALLOWED, sensitivity: 2, detail: JSON.stringify({ permission: realConfig.permission, workdir: realConfig.workdir, tool: realConfig.defaultTool }) });
    hub.log('info', '策略中心', '真实执行配置已更新：' + realConfig.permission + ' / ' + realConfig.defaultTool);
    return ok(res, realStatus());
  }
  if (p === '/api/snapshot') return ok(res, hub.snapshot());
  if (p === '/api/events' && method === 'GET') return serveEvents(req, res);

  if (p === '/api/agents' && method === 'GET') return ok(res, Array.from(hub.agents.values()));
  if (p === '/api/skills' && method === 'GET') return ok(res, skills.list());
  if (p === '/api/agents' && method === 'POST') {
    if (!body.name || !body.type) return fail(res, 400, 'name 与 type 必填');
    if (body.id && hub.agents.has(body.id)) return fail(res, 409, '实例 ID 已存在');
    if (body.skills != null) { try { body.skills = skills.assign(body.skills); } catch (e) { return fail(res, 400, e.message); } }
    if (body.connection === 'acp') {
      try { return ok(res, registerAcp(body)); }
      catch (e) { return fail(res, 400, e.message); }
    }
    let httpAdapter;
    if (body.connection === 'http' || /^https?:\/\//.test(body.endpoint || '')) {
      try { httpAdapter = new HttpHarnessAdapter(body); }
      catch (e) { return fail(res, 400, e.message); }
    }
    const localTool = body.type === 'codex' ? 'codex' : body.type === 'claude-code' ? 'claude' : null;
    if (!httpAdapter && (!REAL_ENABLED || !localTool || !Real.status().tools[localTool].available)) {
      return fail(res, 400, '请提供 HTTP 桥接地址，或选择本机已安装的 Codex / Claude Code');
    }
    const agent = hub.createAgent(body);
    if (httpAdapter) {
      hub._adapters[agent.id] = httpAdapter;
      agent.communication = 'http';
      agent.transport = 'http';
      agent.adapterKind = 'HttpHarnessAdapter';
      agent.permissions = (body.permissions || ['readonly']).filter(p => ['readonly', 'workspace', 'shell'].includes(p));
      hub.emit('agent', agent);
    } else {
      agent.real = true; agent.realTool = localTool; agent.load = 0; agent.trust = 1;
      agent.communication = 'acp';
      hub._adapters[agent.id] = getAdapter(localTool, realConfig.permission, realConfig.workdir);
      agent.adapterKind = 'ACP'; hub.emit('agent', agent);
    }
    return ok(res, agent);
  }
  if (seg[1] === 'agents' && seg[2] && method === 'PATCH') {
    const a = hub.agents.get(seg[2]);
    if (!a) return fail(res, 404, '智能体不存在');
    if (body.skills !== undefined) {
      try { a.skills = skills.assign(body.skills); skills.dropCache(); hub.emit('agent', a); }
      catch (e) { return fail(res, 400, e.message); }
    }
    if (body.enabled !== undefined) {
      const changed = hub.setAgentEnabled(seg[2], !!body.enabled);
      return ok(res, changed);
    }
    return ok(res, a);
  }

  if (p === '/api/adapters' && method === 'GET') return ok(res, hub.snapshot().adapterTypes);
  if (p === '/api/adapters' && method === 'POST') {
    if (!body.type) return fail(res, 400, 'type 必填');
    // Instance endpoint determines the actual HTTP connection; types alone do
    // not turn simulation traffic into external calls.
    return ok(res, hub.registerAdapter(body.type, spec => new Core.SimulatedAdapter(spec, hub.rng), body.descriptor || {}));
  }

  if (p === '/api/tasks' && method === 'GET') return ok(res, hub.tasks.tasks);
  if (p === '/api/tasks' && method === 'POST') {
    if (!body.title) return fail(res, 400, 'title 必填');
    body.real = true;
    if (body.real && !REAL_ENABLED) return fail(res, 400, '真实执行未启用（服务端设置了 HARNESS_HUB_REAL=0）');
    if (body.real) {
      body.permission = body.permission || realConfig.permission;
      body.tool = body.tool || realConfig.defaultTool;
      body.workdir = body.workdir || realConfig.workdir;
    }
    const task = hub.submitTask(body);
    if (body.real) enqueueReal(task.id);
    return ok(res, task);
  }
  if (seg[1] === 'tasks' && seg[2] && seg[3] === 'retry' && method === 'POST') {
    const existing = hub.tasks.get(seg[2]);
    if (existing?.missionId) return fail(res, 409, '这是编排任务的内部调用，请在指挥台重新提交目标');
    const t = hub.retryTask(seg[2]);
    if (!t) return fail(res, 409, '仅失败任务可重试');
    if (existing && existing.execution.mode === 'real') {
      t.execution.status = 'pending';
      t.execution.output = '';
      t.execution.error = '';
      realState.delete(seg[2]);
      enqueueReal(seg[2]);
    }
    return ok(res, t);
  }
  if (seg[1] === 'tasks' && seg[2] && seg[3] === 'cancel' && method === 'POST') {
    const t = hub.cancelTask(seg[2]);
    return t ? ok(res, t) : fail(res, 409, '任务已处于终态');
  }

  if (p === '/api/messages' && method === 'GET') return ok(res, hub.bus.messages.slice(-200));
  if (p === '/api/messages' && method === 'POST') {
    if (!body.from || !body.to) return fail(res, 400, 'from 与 to 必填');
    try { return ok(res, await communication.send(body)); }
    catch (e) { return fail(res, 400, e.message); }
  }

  if (p === '/api/context/sessions' && method === 'GET') return ok(res, Array.from(hub.context.sessions.values()));
  if (seg[1] === 'context' && seg[2] === 'sessions' && seg[3] && method === 'GET') {
    const s = hub.context.sessions.get(seg[3]);
    return s ? ok(res, s) : fail(res, 404, '会话不存在');
  }
  if (p === '/api/context/read' && method === 'POST') {
    if (!body.actor || !body.sessionId) return fail(res, 400, 'actor 与 sessionId 必填');
    return ok(res, hub.context.read(body.actor, body.sessionId, body.key));
  }
  if (p === '/api/context/sync' && method === 'POST') {
    if (!body.actor || !body.sessionId) return fail(res, 400, 'actor 与 sessionId 必填');
    return ok(res, hub.context.sync(body.actor, body.sessionId));
  }
  if (p === '/api/context/write' && method === 'POST') {
    if (!body.actor || !body.sessionId || !body.key) return fail(res, 400, 'actor / sessionId / key 必填');
    return ok(res, hub.context.write(body.actor, body.sessionId, body.key, body.value, body.baseVersion));
  }

  if (p === '/api/grants' && method === 'GET') return ok(res, Array.from(hub.context.grants.values()));
  if (p === '/api/grants' && method === 'POST') {
    if (!body.subjectId || !body.objectId) return fail(res, 400, 'subjectId 与 objectId 必填');
    return ok(res, hub.grant(body));
  }
  if (seg[1] === 'grants' && seg[2] && method === 'DELETE') {
    const g = hub.context.revokeGrant(seg[2]);
    return g ? ok(res, g) : fail(res, 404, '授权不存在');
  }

  if (p === '/api/approvals' && method === 'GET') return ok(res, hub.pendingApprovals);
  if (seg[1] === 'approvals' && seg[2] && seg[3] === 'approve' && method === 'POST') {
    const a = hub.approve(seg[2]);
    return a ? ok(res, a) : fail(res, 409, '申请已处理或不存在');
  }
  if (seg[1] === 'approvals' && seg[2] && seg[3] === 'reject' && method === 'POST') {
    const a = hub.reject(seg[2]);
    return a ? ok(res, a) : fail(res, 409, '申请已处理或不存在');
  }

  if (p === '/api/audit' && method === 'GET') return ok(res, hub.auditLog.slice(-Number(url.searchParams.get('limit') || 300)));
  if (p === '/api/audit/verify' && method === 'GET') return ok(res, hub.verifyAuditChain());
  if (p === '/api/logs' && method === 'GET') return ok(res, hub.logs.slice(-Number(url.searchParams.get('limit') || 200)));
  if (p === '/api/conflicts' && method === 'GET') return ok(res, hub.conflicts);

  if (p === '/api/policy' && method === 'GET') return ok(res, hub.policy);
  if (p === '/api/policy' && method === 'PATCH') return ok(res, hub.setPolicy(body));

  if (p === '/api/control' && method === 'POST') {
    return fail(res, 400, '已关闭仿真控制，请在任务指挥台提交真实任务');
  }

  return fail(res, 404, '未知接口: ' + method + ' ' + p);
}

const server = http.createServer((req, res) => {
  if (req.method === 'OPTIONS') return send(res, 204, '');
  const url = new URL(req.url, 'http://' + (req.headers.host || 'localhost'));
  if (!url.pathname.startsWith('/api/')) {
    if (req.method !== 'GET') return fail(res, 405, '仅支持 GET');
    return serveStatic(res, url.pathname);
  }
  route(req, res, url).catch(err => {
    console.error('[hub] request error:', err);
    if (!res.headersSent) fail(res, 500, err.message);
  });
});

server.listen(PORT, HOST, () => {
  console.log('');
  console.log('  Harness Hub  v' + hub.version);
  console.log('  ---------------------------------------------');
  console.log('  Console   http://' + HOST + ':' + PORT);
  console.log('  REST      http://' + HOST + ':' + PORT + '/api/snapshot');
  console.log('  SSE       http://' + HOST + ':' + PORT + '/api/events');
  console.log('  Agents    ' + hub.agents.size + ' registered / ' + Object.keys(Core.CONST.HARNESS_TYPES).length + ' adapter types');
  const rs = Real.status().tools;
  console.log('  Real CLI  ' + (REAL_ENABLED
    ? Object.keys(rs).map(k => k + '=' + (rs[k].available ? 'ready' : 'missing')).join('  ')
    : 'disabled'));
  console.log('  Mode      ' + realConfig.permission + ' / workdir ' + realConfig.workdir);
  console.log('');
});

async function shutdown() {
  const forceExit = setTimeout(() => process.exit(0), 5000); forceExit.unref();
  await acpRuntime.shutdown();
  realAdapters.forEach(a => { try { a.teardown(); } catch (_) { /* noop */ } });
  hub.stop();
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 1500).unref();
}
process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
