'use strict';
const { spawn, execFile } = require('node:child_process');
const { Readable, Writable } = require('node:stream');
const { randomUUID } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const Real = require('./real-adapters');

const packages = { codex: '@agentclientprotocol/codex-acp/dist/index.js', claude: '@agentclientprotocol/claude-agent-acp/dist/index.js' };
const modes = { codex: { readonly: 'read-only', workspace: 'workspace-write', shell: 'read-only' }, claude: { readonly: 'plan', workspace: 'default', shell: 'plan' } };
// Any tool-call raw input carrying one of these keys is treated as a
// submit_result submission; ordinary tool inputs (paths, commands) never match.
const RESULT_KEYS = ['output', 'complete', 'blocked', 'messages', 'reads', 'tasks', 'reply', 'action', 'report', 'updates', 'newTasks'];
const flatten = options => (options || []).flatMap(o => o.options ? flatten(o.options) : [o]);
function extractSubmitted(tools) {
  for (let i = (tools || []).length - 1; i >= 0; i--) {
    let raw = tools[i].rawInput;
    if (typeof raw === 'string') { try { raw = JSON.parse(raw); } catch (_) { continue; } }
    if (raw && typeof raw === 'object' && !Array.isArray(raw) && RESULT_KEYS.some(k => k in raw)) return raw;
  }
  return null;
}
function readable(text, structured) {
  if (!structured) return text;
  try {
    const { json } = require('./orchestrator');
    const value = json(text);
    return value.reply || value.message || value.output || value.report || '';
  } catch (_) { return text; }
}

class AcpRuntime {
  constructor(hub, { stateFile = path.join(__dirname, '../.data/sessions.json'), timeoutMs = 180000, launch } = {}) {
    this.hub = hub; this.stateFile = stateFile; this.timeoutMs = timeoutMs; this.launch = launch;
    this.records = []; this.drivers = new Map(); this.bindings = new Map(); this.approvals = new Map(); this.turnQueues = new Map();
    this.children = new Set();
    this.timeouts = new Map();
    if (stateFile !== ':memory:' && fs.existsSync(stateFile)) {
      this.records = JSON.parse(fs.readFileSync(stateFile, 'utf8').replace(/^\uFEFF/, '')).map(r => ({ ...r, state: 'disconnected', activeTurn: null, permissions: [] }));
    }
  }
  static installed(tool) { try { return require.resolve(packages[tool]); } catch (_) { return null; } }
  view(r) {
    return { ...r, activeTurn: r.activeTurn ? { ...r.activeTurn, receivedChars: r.activeTurn.output.length, output: readable(r.activeTurn.output, r.activeTurn.structured) } : null,
      turns: (r.turns || []).map(t => ({ ...t, output: readable(t.output, t.structured) })) };
  }
  list(chatId) { return this.records.filter(r => !chatId || r.chatId === chatId).map(r => this.view(r)); }
  changed(r) {
    r.updatedAt = Date.now();
    this.hub.emit('session', { id: r.id, chatId: r.chatId, agentId: r.agentId, state: r.state });
    // Stream updates stay in memory; persist on lifecycle boundaries.
  }
  save() {
    if (this.stateFile === ':memory:') return;
    fs.mkdirSync(path.dirname(this.stateFile), { recursive: true });
    fs.writeFileSync(this.stateFile + '.tmp', JSON.stringify(this.records.slice(-100)));
    fs.renameSync(this.stateFile + '.tmp', this.stateFile);
  }
  within(root, candidate) {
    const resolved = fs.realpathSync(candidate);
    const rel = path.relative(fs.realpathSync(root), resolved);
    if (rel === '..' || rel.startsWith('..' + path.sep) || path.isAbsolute(rel)) throw new Error('文件路径超出此会话工作目录');
    return resolved;
  }
  configuration(r, result) {
    if (result.modes) r.modes = result.modes;
    if (result.models) r.models = result.models;
    if (result.configOptions) {
      r.configOptions = result.configOptions.filter(o => ['model', 'mode', 'thought_level'].includes(o.category) || ['model', 'mode', 'reasoning_effort'].includes(o.id));
      const model = r.configOptions.find(o => o.category === 'model' || o.id === 'model');
      if (model) r.models = { currentModelId: model.currentValue, availableModels: flatten(model.options).map(o => ({ modelId: o.value, name: o.name || o.value })) };
    }
  }
  async driver(agent, permission, cwd) {
    const key = [agent.id, permission, cwd].join('|');
    if (this.drivers.has(key)) return this.drivers.get(key);
    const pending = this.startDriver(agent, permission, cwd);
    this.drivers.set(key, pending);
    try { return await pending; } catch (e) { this.drivers.delete(key); throw e; }
  }
  async startDriver(agent, permission, cwd) {
    const acp = await import('@agentclientprotocol/sdk');
    const tool = agent.realTool;
    const launch = this.launch ? this.launch(agent, permission, cwd) : {
      command: process.execPath, args: [AcpRuntime.installed(tool)], env: Real.buildEnv(tool)
    };
    if (!launch.command || !Array.isArray(launch.args) || launch.args.some(arg => typeof arg !== 'string')) throw new Error('尚未安装此 Harness 的 ACP 适配器');
    if (tool === 'claude' && !this.launch) launch.env.CLAUDE_CODE_EXECUTABLE = Real.resolveBin('claude');
    if (tool === 'codex' && !this.launch) launch.env.INITIAL_AGENT_MODE = modes.codex[permission];
    const child = spawn(launch.command, launch.args, { cwd, env: launch.env || process.env, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    let stderr = '';
    child.stderr.on('data', chunk => { stderr = (stderr + chunk.toString()).slice(-8000); });
    const d = { child, sessions: new Set(), acp, permission, cwd, agentId: agent.id, connection: null, alive: true };
    d.closed = new Promise(resolve => child.once('close', resolve));
    this.children.add(d);
    const app = acp.client({ name: 'harness-hub', version: '2.0.0' })
      .onNotification(acp.methods.client.session.update, ctx => this.update(d, ctx.params))
      .onRequest(acp.methods.client.session.requestPermission, ctx => this.requestPermission(d, ctx.params))
      .onRequest(acp.methods.client.fs.readTextFile, ctx => {
        const p = ctx.params;
        const file = this.within(cwd, path.resolve(cwd, p.path));
        if (fs.statSync(file).size > 1024 * 1024) throw new Error('文件超过 1 MiB，请使用分段读取');
        const lines = fs.readFileSync(file, 'utf8').split(/\r?\n/);
        return { content: lines.slice(Math.max(0, (p.line || 1) - 1), p.limit ? (p.line || 1) - 1 + p.limit : undefined).join('\n') };
      })
      .onRequest(acp.methods.client.fs.writeTextFile, ctx => {
        if (permission !== 'workspace') throw new Error('此会话未授予文件写入权限');
        const p = ctx.params, candidate = path.resolve(cwd, p.path);
        this.within(cwd, fs.existsSync(candidate) ? candidate : path.dirname(candidate));
        if (typeof p.content !== 'string' || Buffer.byteLength(p.content) > 1024 * 1024) throw new Error('写入内容超过 1 MiB');
        fs.writeFileSync(candidate, p.content, 'utf8');
        return {};
      });
    d.connection = app.connect(acp.ndJsonStream(Writable.toWeb(child.stdin), Readable.toWeb(child.stdout)));
    child.on('error', e => d.connection.close(e));
    child.on('close', () => {
      this.children.delete(d);
      d.alive = false; d.connection.close(new Error('ACP 进程已退出：' + stderr.slice(-500)));
      for (const r of d.sessions) { this.cancelPermissions(r.id); r.state = 'disconnected'; r.activeTurn = null; this.bindings.delete(r.id); this.changed(r); }
      this.save();
    });
    try {
      d.info = await d.connection.agent.request('initialize', {
        protocolVersion: acp.PROTOCOL_VERSION, clientInfo: { name: 'harness-hub', version: '2.0.0' },
        clientCapabilities: { fs: { readTextFile: true, writeTextFile: permission === 'workspace' }, terminal: false, session: { notices: {} } }
      }, { cancellationSignal: AbortSignal.timeout(30000), stopWaitingOnCancel: true });
      if (d.info.protocolVersion !== acp.PROTOCOL_VERSION) throw new Error('ACP 协议版本不兼容');
    } catch (e) { this.kill(d); d.connection.close(e); throw new Error('ACP 初始化失败：' + e.message + (stderr ? ' · ' + stderr.slice(-400) : '')); }
    return d;
  }
  update(d, params) {
    const r = [...d.sessions].find(s => s.nativeSessionId === params.sessionId);
    if (!r) return;
    const u = params.update, t = r.activeTurn;
    if (u.sessionUpdate === 'notice') { r.notices = [...(r.notices || []), u].slice(-5); }
    if (u.sessionUpdate === 'config_option_update') this.configuration(r, { configOptions: u.configOptions });
    if (u.sessionUpdate === 'current_mode_update' && r.modes) r.modes.currentModeId = u.currentModeId;
    if (t && u.sessionUpdate === 'agent_message_chunk' && u.content?.type === 'text') t.output = (t.output + u.content.text).slice(0, 200000);
    if (t && ['tool_call', 'tool_call_update'].includes(u.sessionUpdate)) {
      const old = t.tools.find(c => c.toolCallId === u.toolCallId);
      const update = JSON.parse(JSON.stringify(u));
      // Keep useful native diffs/output without unbounded transcripts in snapshots.
      if (JSON.stringify(update).length > 40000) update.content = [{ type: 'content', content: { type: 'text', text: '此工具输出较大，请查看 Harness 原生记录。' } }];
      if (old) Object.assign(old, update); else t.tools.push(update);
      t.tools = t.tools.slice(-40);
    }
    if (t && u.sessionUpdate === 'plan') t.plan = u.entries;
    this.changed(r);
  }
  requestPermission(d, params) {
    const r = [...d.sessions].find(s => s.nativeSessionId === params.sessionId);
    if (!r || !r.activeTurn) return { outcome: { outcome: 'cancelled' } };
    const id = 'permission_' + randomUUID();
    const item = { id, sessionId: r.id, nativeSessionId: params.sessionId, title: params.toolCall?.title || params.title || 'Harness 请求权限',
      toolCall: params.toolCall, options: params.options, state: 'pending', at: Date.now() };
    if (r.permission !== 'workspace') {
      item.state = 'denied'; item.reason = '当前为只读会话，权限提升已拒绝。需要写入时请选择目录内读写权限启动新任务。';
      r.permissions.push(item); this.changed(r);
      return { outcome: { outcome: 'cancelled' } };
    }
    r.permissions.push(item); r.state = 'requires_action'; this.changed(r); this.save();
    const clock = this.timeouts.get(r.id);
    if (clock && !clock.paused) { clearTimeout(clock.timer); clearTimeout(clock.hard); clock.remaining -= Date.now() - clock.started; clock.paused = true; }
    return new Promise(resolve => { this.approvals.set(id, { item, r, resolve }); });
  }
  respondPermission(id, optionId) {
    const p = this.approvals.get(id);
    if (!p || p.item.state !== 'pending') throw new Error('该权限请求已失效');
    if (optionId != null && !p.item.options.some(o => o.optionId === optionId)) throw new Error('无效的权限选项');
    // Durable allow-always choices are deliberately not synthesized or selected automatically.
    p.item.state = optionId == null ? 'cancelled' : 'answered'; p.item.selected = optionId;
    this.approvals.delete(id);
    p.resolve({ outcome: optionId == null ? { outcome: 'cancelled' } : { outcome: 'selected', optionId } });
    if (p.r.state !== 'cancelling') p.r.state = 'running';
    const clock = this.timeouts.get(p.r.id);
    if (clock?.paused && p.r.state !== 'cancelling' && ![...this.approvals.values()].some(a => a.r.id === p.r.id)) this.armTimeout(p.r, this.bindings.get(p.r.id), clock);
    this.changed(p.r); this.save();
  }
  cancelPermissions(sessionId) { for (const [id, p] of this.approvals) if (p.r.id === sessionId) this.respondPermission(id, null); }
  armTimeout(r, d, clock) {
    clock.paused = false; clock.started = Date.now();
    clock.timer = setTimeout(() => this.cancel(r.id).catch(() => {}), Math.max(1, clock.remaining));
    clock.hard = setTimeout(() => { clock.timedOut = true; d.connection.close(new Error('ACP 会话超时')); this.kill(d); }, Math.max(1, clock.remaining) + 10000);
  }
  rpc(d, method, params) { return d.connection.agent.request(method, params, { cancellationSignal: AbortSignal.timeout(30000), stopWaitingOnCancel: true }); }
  exportTranscript(id) {
    const r = this.records.find(s => s.id === id);
    if (!r) return '';
    return (r.turns || []).map(t => '【' + (t.stage || '回合') + '】\n' + (t.output || '')).join('\n\n').slice(0, 100000);
  }
  mcpServers(agent, options) {
    // Structured orchestrator turns get the submit_result sink carrying the
    // stage's output schema; only the bundled adapters are assumed to accept
    // the standard `mcpServers` field.
    if (!options.structured || !['codex', 'claude'].includes(agent.realTool)) return [];
    const args = [path.join(__dirname, 'result-mcp.js')];
    if (options.outputSchema) args.push(JSON.stringify(options.outputSchema));
    return [{ name: 'harness-result', command: process.execPath, args, env: [] }];
  }
  async ensure(agent, options) {
    const permission = options.permission || 'readonly', cwd = options.workdir;
    const key = [options.sessionKey || 'direct', agent.id, permission, cwd].join('|');
    let r = this.records.find(s => s.key === key);
    if (!r) {
      r = { id: 'session_' + randomUUID(), key, agentId: agent.id, chatId: options.chatId || null, missionId: options.missionId || null,
        taskId: options.taskId || null, role: options.role || 'direct', permission, cwd, state: 'connecting', nativeSessionId: null, turns: [], permissions: [], createdAt: Date.now() };
      this.records.push(r); this.changed(r);
    }
    if (this.bindings.has(r.id)) return r;
    let d = await this.driver(agent, permission, cwd);
    if (!d.alive) { this.drivers.delete([agent.id, permission, cwd].join('|')); d = await this.driver(agent, permission, cwd); }
    r.state = r.nativeSessionId ? 'restoring' : 'connecting'; this.changed(r);
    r.agentInfo = d.info.agentInfo; r.protocolVersion = d.info.protocolVersion;
    d.sessions.add(r);
    let result;
    try {
      const mcpServers = this.mcpServers(agent, options);
      if (r.nativeSessionId) {
        if (!d.info.agentCapabilities?.loadSession) throw new Error('该 Harness 不支持恢复旧会话，请新建对话');
        result = await this.rpc(d, 'session/load', { sessionId: r.nativeSessionId, cwd, mcpServers });
      } else {
        result = await this.rpc(d, 'session/new', { cwd, mcpServers });
        r.nativeSessionId = result.sessionId;
      }
      this.configuration(r, result);
      r.defaultModel = r.defaultModel || r.models?.currentModelId || '';
      const wantedMode = modes[agent.realTool]?.[permission] || agent.acpModes?.[permission];
      if (!wantedMode || !r.modes?.availableModes?.some(m => m.id === wantedMode)) throw new Error('该 Harness 未提供所需权限模式：' + permission);
      await this.rpc(d, 'session/set_mode', { sessionId: r.nativeSessionId, modeId: wantedMode });
      if (r.modes) r.modes.currentModeId = wantedMode;
      this.bindings.set(r.id, d); r.state = 'idle'; this.changed(r); this.save(); return r;
    } catch (e) { r.state = 'error'; r.error = e.message; this.changed(r); throw e; }
  }
  prompt(agent, text, options) {
    const key = [options.sessionKey || 'direct', agent.id, options.permission, options.workdir].join('|');
    const previous = this.turnQueues.get(key) || Promise.resolve();
    const work = previous.catch(() => {}).then(() => this.turn(agent, text, options));
    this.turnQueues.set(key, work);
    work.finally(() => { if (this.turnQueues.get(key) === work) this.turnQueues.delete(key); }).catch(() => {});
    return work;
  }
  async turn(agent, text, options) {
    const key = [options.sessionKey || 'direct', agent.id, options.permission, options.workdir].join('|');
    const existing = this.records.find(s => s.key === key);
    if (existing?.handedOff) throw new Error('此会话已移交其他执行端，请在对话中发起新任务');
    const r = await this.ensure(agent, options), d = this.bindings.get(r.id);
    const model = options.model || r.defaultModel;
    if (model && model !== r.models?.currentModelId) {
      const configured = await this.rpc(d, 'session/set_config_option', { sessionId: r.nativeSessionId, configId: 'model', value: model });
      this.configuration(r, configured);
    }
    const turn = { id: randomUUID(), stage: options.stage || '对话', structured: options.structured !== false, output: '', tools: [], startedAt: Date.now(), status: 'running' };
    r.activeTurn = turn; r.state = 'running'; r.error = ''; this.changed(r);
    const clock = { remaining: options.timeoutMs || this.timeoutMs };
    this.timeouts.set(r.id, clock); this.armTimeout(r, d, clock);
    try {
      const result = await d.connection.agent.request('session/prompt', { sessionId: r.nativeSessionId, prompt: [{ type: 'text', text }] });
      turn.stopReason = result.stopReason;
      turn.status = result.stopReason === 'cancelled' ? 'cancelled' : 'completed';
      const ok = !['cancelled', 'refusal'].includes(result.stopReason) && !!turn.output.trim();
      const submitted = extractSubmitted(turn.tools);
      return { ok, output: turn.output, structured: submitted, error: ok ? '' : '会话已停止或未返回有效结果', meta: { sessionId: r.nativeSessionId, runtimeSessionId: r.id, model: r.models?.currentModelId || model, permission: r.permission, tool: agent.realTool || agent.type, durationMs: Date.now() - turn.startedAt, stopReason: result.stopReason } };
    } catch (e) { turn.status = 'failed'; turn.error = clock.timedOut ? 'ACP 会话超时' : e.message; throw e; }
    finally {
      clearTimeout(clock.timer); clearTimeout(clock.hard); this.timeouts.delete(r.id); this.cancelPermissions(r.id);
      turn.finishedAt = Date.now(); r.turns.push(turn); r.turns = r.turns.slice(-20); r.activeTurn = null;
      r.state = d.alive && !d.connection.signal.aborted ? 'idle' : 'disconnected'; this.changed(r); this.save();
    }
  }
  async cancel(id) {
    const r = this.records.find(s => s.id === id), d = this.bindings.get(id);
    if (!r || !r.activeTurn || !d) throw new Error('此会话没有正在执行的调用');
    r.state = 'cancelling'; this.cancelPermissions(id); this.changed(r);
    await d.connection.agent.notify('session/cancel', { sessionId: r.nativeSessionId });
  }
  kill(d) {
    if (!d.child.pid || !d.alive) return Promise.resolve();
    if (process.platform === 'win32') return new Promise(resolve => execFile('taskkill.exe', ['/PID', String(d.child.pid), '/T', '/F'], { windowsHide: true }, () => resolve()));
    d.child.kill(); return Promise.resolve();
  }
  async shutdown() { await Promise.all([...this.children].map(async d => { for (const r of d.sessions) this.cancelPermissions(r.id); d.connection.close(); await this.kill(d); await d.closed; })); this.save(); }
}
module.exports = { AcpRuntime, extractSubmitted };
