/*!
 * Harness Hub - Multi-Harness Orchestration Core
 * ---------------------------------------------------------------
 * UMD module: works in Node (require) and in the browser (global).
 * Zero external dependencies.
 *
 * Layers:
 *   Adapters      - pluggable Harness drivers (registry + lifecycle)
 *   MessageBus    - canonical envelope protocol + delivery policy
 *   ContextStore  - session context with ACL, sensitivity, isolation
 *   TaskEngine    - dispatch, flow, conflict, retry, aggregation
 *   AuditChain    - hash-chained, tamper-evident access auditing
 *   Hub           - composition root, events, simulation driver
 */
(function (root, factory) {
  'use strict';
  var api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  if (root) root.HarnessHubCore = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  /* ============================================================
   * 0. Utilities & constants
   * ========================================================== */

  function mulberry32(a) {
    return function () {
      a |= 0; a = (a + 0x6D2B79F5) | 0;
      var t = Math.imul(a ^ (a >>> 15), 1 | a);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }

  var _counter = 0;
  function uid(prefix) {
    _counter += 1;
    return (prefix || 'id') + '_' + Date.now().toString(36) + '_' + _counter.toString(36);
  }

  function fnv1a(str) {
    var h = 0x811c9dc5;
    for (var i = 0; i < str.length; i++) {
      h ^= str.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return ('00000000' + h.toString(16)).slice(-8);
  }

  function clamp(v, lo, hi) { return Math.max(lo, Math.min(hi, v)); }
  function pick(rng, arr) { return arr[Math.floor(rng() * arr.length)]; }
  function chance(rng, p) { return rng() < p; }
  function round(v, n) { var p = Math.pow(10, n || 0); return Math.round(v * p) / p; }
  function ts() { return Date.now(); }
  function iso(t) { return new Date(t || Date.now()).toISOString(); }
  function clone(o) { return JSON.parse(JSON.stringify(o)); }

  var SENS = { PUBLIC: 0, INTERNAL: 1, CONFIDENTIAL: 2, SECRET: 3 };
  var SENS_LABEL = ['公开', '内部', '机密', '绝密'];
  var SENS_KEY = ['public', 'internal', 'confidential', 'secret'];

  var STATE = {
    QUEUED: 'queued', ASSIGNED: 'assigned', RUNNING: 'running',
    BLOCKED: 'blocked', RETRYING: 'retrying', SUCCEEDED: 'succeeded',
    FAILED: 'failed', CANCELLED: 'cancelled'
  };
  var STATE_LABEL = {
    queued: '排队中', assigned: '已派发', running: '执行中', blocked: '冲突阻塞',
    retrying: '重试等待', succeeded: '已完成', failed: '失败', cancelled: '已取消'
  };

  var AGENT_STATUS = { ONLINE: 'online', BUSY: 'busy', OFFLINE: 'offline', ERROR: 'error', PAUSED: 'paused' };
  var STATUS_LABEL = { online: '在线', busy: '忙碌', offline: '离线', error: '异常', paused: '已暂停' };

  var MSG_KIND = {
    ASSIGN: 'task.assign', PROGRESS: 'task.progress', RESULT: 'task.result',
    CTX_REQUEST: 'ctx.request', CTX_GRANT: 'ctx.grant', CTX_SYNC: 'ctx.sync', CTX_WRITE: 'ctx.write',
    COLLAB_ASK: 'collab.ask', COLLAB_REPLY: 'collab.reply',
    CONFLICT: 'conflict.alert', CONTROL: 'control', HEARTBEAT: 'heartbeat', ERROR: 'error'
  };

  var DECISION = { ALLOWED: 'allowed', DENIED: 'denied', PENDING: 'pending' };

  /** Built-in Harness descriptors. Any new tool can be added at runtime. */
  var HARNESS_TYPES = {
    'codex': {
      label: 'Codex', vendor: 'OpenAI', glyph: 'C', color: '#10a37f', transport: 'stdio',
      capabilities: ['code.write', 'code.review', 'shell', 'refactor'],
      maxConcurrency: 2, reliability: 0.93, clearance: SENS.CONFIDENTIAL
    },
    'claude-code': {
      label: 'Claude Code', vendor: 'Anthropic', glyph: 'A', color: '#d97757', transport: 'stdio',
      capabilities: ['code.write', 'code.review', 'analysis', 'shell', 'long-context'],
      maxConcurrency: 2, reliability: 0.95, clearance: SENS.CONFIDENTIAL
    },
    'deepseek': {
      label: 'DeepSeek Harness', vendor: 'DeepSeek', glyph: 'D', color: '#4d6bfe', transport: 'http',
      capabilities: ['reasoning', 'code.write', 'analysis', 'math'],
      maxConcurrency: 3, reliability: 0.90, clearance: SENS.INTERNAL
    },
    'workbuddy': {
      label: 'WorkBuddy', vendor: 'Tencent', glyph: 'W', color: '#7c3aed', transport: 'sse',
      capabilities: ['research', 'docs', 'web', 'files', 'orchestration', 'analysis', 'long-context'],
      maxConcurrency: 4, reliability: 0.96, clearance: SENS.SECRET
    },
    'zocde': {
      label: 'Zocde', vendor: 'Zocde', glyph: 'Z', color: '#0891b2', transport: 'websocket',
      capabilities: ['code.write', 'debug', 'test', 'deploy'],
      maxConcurrency: 2, reliability: 0.88, clearance: SENS.INTERNAL
    }
  };

  var TASK_TEMPLATES = [
    { title: '重构认证中间件并补充单元测试', caps: ['code.write', 'test'], sens: SENS.CONFIDENTIAL, priority: 'P1' },
    { title: '分析订单服务 P99 延迟回归', caps: ['analysis', 'reasoning'], sens: SENS.INTERNAL, priority: 'P0' },
    { title: '生成 v2.4 接口变更文档', caps: ['docs'], sens: SENS.PUBLIC, priority: 'P2' },
    { title: '交叉审阅支付模块 PR #418', caps: ['code.review'], sens: SENS.CONFIDENTIAL, priority: 'P1' },
    { title: '修复 CI 流水线偶发超时', caps: ['debug', 'shell'], sens: SENS.INTERNAL, priority: 'P0' },
    { title: '梳理多租户数据隔离边界', caps: ['analysis', 'long-context'], sens: SENS.SECRET, priority: 'P1' },
    { title: '实现批量导出的断点续传', caps: ['code.write'], sens: SENS.INTERNAL, priority: 'P2' },
    { title: '汇总本周各 Harness 执行报告', caps: ['research', 'docs'], sens: SENS.INTERNAL, priority: 'P3' },
    { title: '验证灰度环境回滚脚本', caps: ['test', 'deploy'], sens: SENS.CONFIDENTIAL, priority: 'P1' },
    { title: '推导缓存命中率优化上界', caps: ['math', 'reasoning'], sens: SENS.PUBLIC, priority: 'P2' },
    { title: '扫描依赖树中的高危 CVE', caps: ['web', 'code.review'], sens: SENS.INTERNAL, priority: 'P1' },
    { title: '同步跨工具会话摘要到知识库', caps: ['files', 'orchestration'], sens: SENS.CONFIDENTIAL, priority: 'P2' }
  ];

  var COLLAB_LINES = [
    '需要你提供该模块的调用链摘要，用于补全上下文。',
    '我已完成静态分析，把风险点清单同步给你。',
    '检测到同一资源上的并发写入，建议改为顺序提交。',
    '请把你那侧的测试结论回传，我做结果汇总。',
    '请求读取会话快照的最新版本（只读）。',
    '这部分涉及密钥材料，需先申请授权再共享。',
    '已完成冲突变基，合并后版本已推送。',
    '建议把该子任务交给具备 long-context 能力的实例。'
  ];

  /* ============================================================
   * 1. Event emitter (supports '*' wildcard)
   * ========================================================== */

  function Emitter() { this._h = {}; }
  Emitter.prototype.on = function (type, fn) {
    (this._h[type] || (this._h[type] = [])).push(fn);
    return function () { this.off(type, fn); }.bind(this);
  };
  Emitter.prototype.off = function (type, fn) {
    if (!this._h[type]) return;
    this._h[type] = this._h[type].filter(function (f) { return f !== fn; });
  };
  Emitter.prototype.emit = function (type, data) {
    var list = (this._h[type] || []).slice();
    for (var i = 0; i < list.length; i++) {
      try { list[i](data); } catch (e) { /* subscriber must not break the engine */ }
    }
    var all = (this._h['*'] || []).slice();
    for (var j = 0; j < all.length; j++) {
      try { all[j]({ type: type, data: data, at: ts() }); } catch (e) { /* noop */ }
    }
  };

  /* ============================================================
   * 2. Adapter layer - the extension point for new Harness tools
   * ========================================================== */

  /**
   * Contract every Harness adapter must implement:
   *   descriptor()                 -> static registration metadata
   *   heartbeat(agent)             -> { ok, load, status? }
   *   dispatch(task, contextView)  -> { ok, output } | Promise
   *   send(message)                -> { ok } | Promise      (optional)
   *   teardown()                   -> void                  (optional)
   */
  function HarnessAdapter(spec) { this.spec = spec || {}; }
  HarnessAdapter.prototype.descriptor = function () { return this.spec; };
  HarnessAdapter.prototype.heartbeat = function () { return { ok: true, load: 0 }; };
  HarnessAdapter.prototype.dispatch = function () { return { ok: true, output: '' }; };
  HarnessAdapter.prototype.send = function () { return { ok: true }; };
  HarnessAdapter.prototype.teardown = function () {};

  /** Default driver: models a real Harness without spawning external processes. */
  function SimulatedAdapter(spec, rng) {
    HarnessAdapter.call(this, spec);
    this.rng = rng || Math.random;
  }
  SimulatedAdapter.prototype = Object.create(HarnessAdapter.prototype);
  SimulatedAdapter.prototype.constructor = SimulatedAdapter;
  SimulatedAdapter.prototype.heartbeat = function (agent) {
    var r = this.rng();
    return { ok: r > 0.004, load: round(clamp(agent.load + (this.rng() - 0.5) * 0.18, 0, 1), 2) };
  };
  SimulatedAdapter.prototype.dispatch = function (task) {
    return { ok: true, output: '[' + (task.assignedTo || 'agent') + '] accepted ' + task.id };
  };

  /* ============================================================
   * 3. Message bus - canonical envelope + delivery policy
   * ========================================================== */

  function MessageBus(hub) {
    this.hub = hub;
    this.messages = [];
    this.subscriptions = {}; // agentId -> [topic]
  }

  MessageBus.prototype.subscribe = function (agentId, topics) {
    this.subscriptions[agentId] = (this.subscriptions[agentId] || []).concat(topics || []);
  };

  /** Validate + authorize + record one envelope. Returns the stored envelope. */
  MessageBus.prototype.publish = function (partial) {
    var hub = this.hub;
    var msg = {
      id: uid('msg'),
      v: '1.0',
      at: ts(),
      from: partial.from || 'hub',
      to: partial.to || 'broadcast',
      kind: partial.kind || MSG_KIND.CONTROL,
      correlationId: partial.correlationId || null,
      replyTo: partial.replyTo || null,
      scope: {
        sensitivity: partial.sensitivity == null ? SENS.INTERNAL : partial.sensitivity,
        sessionId: partial.sessionId || null,
        tags: partial.tags || []
      },
      payload: partial.payload || {},
      auth: { grantId: partial.grantId || null, decision: DECISION.ALLOWED },
      status: 'pending'
    };

    var check = this._authorize(msg);
    msg.auth.decision = check.decision;
    msg.auth.grantId = check.grantId || null;
    msg.auth.reason = check.reason || '';
    msg.status = check.decision === DECISION.ALLOWED ? 'delivered' : 'rejected';

    this.messages.push(msg);
    if (this.messages.length > 600) this.messages.splice(0, this.messages.length - 600);

    hub.audit({
      actor: msg.from, action: 'message.publish', target: msg.to,
      decision: msg.status === 'delivered' ? DECISION.ALLOWED : DECISION.DENIED,
      sensitivity: msg.scope.sensitivity,
      detail: msg.kind + (check.reason ? ' / ' + check.reason : '')
    });
    hub.emit('message', msg);
    return msg;
  };

  MessageBus.prototype._authorize = function (msg) {
    var hub = this.hub;
    var sender = hub.agents.get(msg.from);
    if (msg.from !== 'hub') {
      if (!sender) return { decision: DECISION.DENIED, reason: '未知发送端' };
      if (!sender.enabled) return { decision: DECISION.DENIED, reason: '发送端已停用' };
      if (msg.scope.sensitivity > sender.clearance) return { decision: DECISION.DENIED, reason: '发送端密级不足' };
    }
    // Control-plane reports (progress, errors, access requests) are addressed
    // to the coordinator itself, which is a valid endpoint.
    if (msg.to === 'hub') return { decision: DECISION.ALLOWED };
    if (msg.to === 'broadcast' || (typeof msg.to === 'string' && msg.to.charAt(0) === '#')) {
      if (msg.scope.sensitivity >= SENS.SECRET && !hub.policy.allowSecretBroadcast) {
        return { decision: DECISION.DENIED, reason: '绝密内容禁止广播' };
      }
      return { decision: DECISION.ALLOWED };
    }
    var target = hub.agents.get(msg.to);
    if (!target) return { decision: DECISION.DENIED, reason: '未知通信端点' };
    if (!target.enabled) return { decision: DECISION.DENIED, reason: '目标已停用' };
    if (target.status === AGENT_STATUS.OFFLINE) return { decision: DECISION.DENIED, reason: '目标离线' };
    if (msg.scope.sensitivity > target.clearance) {
      return { decision: DECISION.DENIED, reason: '目标密级不足（需 ' + SENS_LABEL[msg.scope.sensitivity] + '）' };
    }
    if (msg.scope.sessionId) {
      var session = hub.context.sessions.get(msg.scope.sessionId);
      if (!session) return { decision: DECISION.DENIED, reason: '会话不存在' };
      var sourceAuth = hub.context._authorizeContext(msg.from, session, 'read');
      var targetAuth = hub.context._authorizeContext(msg.to, session, 'read');
      if (!sourceAuth.ok || !targetAuth.ok) return { decision: DECISION.DENIED, reason: '通信双方均需持有会话读取授权' };
      return { decision: DECISION.ALLOWED, grantId: sourceAuth.grantId };
    }
    return { decision: DECISION.ALLOWED };
  };

  /* ============================================================
   * 4. Context store - ACL, sensitivity isolation, conflict control
   * ========================================================== */

  function ContextStore(hub) {
    this.hub = hub;
    this.sessions = new Map();
    this.grants = new Map();
  }

  ContextStore.prototype.createSession = function (ownerId, spec) {
    var s = {
      id: uid('sess'),
      title: spec.title || '未命名会话',
      ownerId: ownerId,
      sensitivity: spec.sensitivity == null ? SENS.INTERNAL : spec.sensitivity,
      version: 1,
      createdAt: ts(),
      updatedAt: ts(),
      entries: [],
      locks: {}
    };
    (spec.entries || []).forEach(function (e) {
      s.entries.push({
        id: uid('entry'), key: e.key, value: e.value,
        sensitivity: e.sensitivity == null ? s.sensitivity : e.sensitivity,
        author: e.author || ownerId, at: ts(), version: 1
      });
    });
    this.sessions.set(s.id, s);
    this.hub.emit('context.session', s);
    return s;
  };

  ContextStore.prototype.findGrant = function (subjectId, objectId, action) {
    var now = ts();
    var found = null;
    this.grants.forEach(function (g) {
      if (found || g.revoked) return;
      if (g.subjectId !== subjectId) return;
      if (g.objectId !== objectId && g.objectId !== '*') return;
      if (g.expiresAt && g.expiresAt < now) return;
      if (action === 'write' && g.scope === 'read') return;
      if (action === 'read' && g.scope === 'write') return;
      found = g;
    });
    return found;
  };

  ContextStore.prototype.addGrant = function (spec) {
    var g = {
      id: uid('grant'),
      subjectId: spec.subjectId,
      objectId: spec.objectId,
      scope: spec.scope || 'read',
      maxSensitivity: spec.maxSensitivity == null ? SENS.CONFIDENTIAL : spec.maxSensitivity,
      grantedBy: spec.grantedBy || 'user',
      createdAt: ts(),
      expiresAt: spec.ttlMs ? ts() + spec.ttlMs : null,
      revoked: false
    };
    this.grants.set(g.id, g);
    this.hub.audit({
      actor: g.grantedBy, action: 'grant.create', target: g.subjectId + ' -> ' + g.objectId,
      decision: DECISION.ALLOWED, sensitivity: g.maxSensitivity,
      detail: g.scope + ' / ' + SENS_LABEL[g.maxSensitivity]
    });
    this.hub.log('success', '授权中心', '已授予 ' + this.hub.nameOf(g.subjectId) + ' 对 ' + g.objectId + ' 的 ' + g.scope + ' 权限');
    this.hub.emit('grant', g);
    return g;
  };

  ContextStore.prototype.revokeGrant = function (id) {
    var g = this.grants.get(id);
    if (!g) return null;
    g.revoked = true;
    this.hub.audit({
      actor: 'user', action: 'grant.revoke', target: g.subjectId + ' -> ' + g.objectId,
      decision: DECISION.ALLOWED, sensitivity: g.maxSensitivity, detail: '用户主动撤销'
    });
    this.hub.log('warn', '授权中心', '已撤销授权 ' + g.id);
    this.hub.emit('grant', g);
    return g;
  };

  /** Raise an approval request instead of silently granting access. */
  ContextStore.prototype._requestApproval = function (actor, action, session) {
    var hub = this.hub;
    var key = actor + '|' + action + '|' + session.id;
    var existing = hub.pendingApprovals.find(function (p) { return p.key === key && p.state === 'pending'; });
    if (existing) return existing;
    // Backpressure: keep the human approval queue actionable.
    var open = hub.pendingApprovals.filter(function (p) { return p.state === 'pending'; }).length;
    if (open >= 12) {
      hub.audit({
        actor: actor, action: 'context.' + action + '.request', target: session.id,
        decision: DECISION.DENIED, sensitivity: session.sensitivity, detail: '审批队列已满，自动拒绝'
      });
      return { id: null, state: 'rejected', key: key, reason: '审批队列已满' };
    }
    var req = {
      id: uid('appr'), key: key, actor: actor, action: action,
      sessionId: session.id, sessionTitle: session.title,
      sensitivity: session.sensitivity, createdAt: ts(), state: 'pending'
    };
    hub.pendingApprovals.unshift(req);
    if (hub.pendingApprovals.length > 60) hub.pendingApprovals.pop();
    hub.audit({
      actor: actor, action: 'context.' + action + '.request', target: session.id,
      decision: DECISION.PENDING, sensitivity: session.sensitivity, detail: '等待用户授权'
    });
    hub.log('warn', '权限门禁', hub.nameOf(actor) + ' 申请' + (action === 'write' ? '写入' : '读取') + '「' + session.title + '」— 待审批');
    hub.bus.publish({
      from: actor, to: 'hub', kind: MSG_KIND.CTX_REQUEST,
      sensitivity: session.sensitivity, sessionId: session.id,
      payload: { text: '请求 ' + action + ' 权限', approvalId: req.id }
    });
    hub.emit('approval', req);
    return req;
  };

  ContextStore.prototype._authorizeContext = function (actor, session, action) {
    var hub = this.hub;
    if (actor === session.ownerId) return { ok: true, grantId: 'owner' };
    if (actor === 'hub') return { ok: true, grantId: 'system' };
    var grant = this.findGrant(actor, session.id, action);
    if (!grant) {
      this.hub.audit({
        actor: actor, action: 'context.' + action, target: session.id,
        decision: DECISION.DENIED, sensitivity: session.sensitivity, detail: '无有效授权'
      });
      var req = this._requestApproval(actor, action, session);
      return { ok: false, reason: req && req.state === 'rejected' ? '缺少授权（审批队列已满，自动拒绝）' : '缺少授权，已提交审批' };
    }
    if (session.sensitivity > grant.maxSensitivity) {
      this.hub.audit({
        actor: actor, action: 'context.' + action, target: session.id,
        decision: DECISION.DENIED, sensitivity: session.sensitivity,
        detail: '会话密级高于授权上限 ' + SENS_LABEL[grant.maxSensitivity]
      });
      this.hub.log('error', '权限门禁', hub.nameOf(actor) + ' 越权访问「' + session.title + '」被拦截');
      return { ok: false, reason: '会话密级超出授权上限' };
    }
    // Hard isolation: SECRET sessions never accept wildcard grants
    if (session.sensitivity === SENS.SECRET && grant.objectId === '*') {
      this.hub.audit({
        actor: actor, action: 'context.' + action, target: session.id,
        decision: DECISION.DENIED, sensitivity: SENS.SECRET, detail: '绝密会话不接受通配授权'
      });
      return { ok: false, reason: '绝密会话需单独授权' };
    }
    return { ok: true, grantId: grant.id };
  };

  ContextStore.prototype.read = function (actor, sessionId, key) {
    var session = this.sessions.get(sessionId);
    if (!session) return { ok: false, reason: '会话不存在' };
    var auth = this._authorizeContext(actor, session, 'read');
    if (!auth.ok) return auth;
    var agent = this.hub.agents.get(actor);
    var ceiling = agent ? agent.clearance : SENS.PUBLIC;
    var entries = session.entries.filter(function (e) {
      return e.sensitivity <= ceiling && (!key || e.key === key);
    });
    this.hub.audit({
      actor: actor, action: 'context.read', target: sessionId,
      decision: DECISION.ALLOWED, sensitivity: session.sensitivity,
      detail: '返回 ' + entries.length + '/' + session.entries.length + ' 条（已过滤高密级）', grantId: auth.grantId
    });
    return { ok: true, sessionId: sessionId, version: session.version, entries: clone(entries), redacted: session.entries.length - entries.length };
  };

  /** Optimistic-concurrency write. baseVersion mismatch => conflict record. */
  ContextStore.prototype.write = function (actor, sessionId, key, value, baseVersion) {
    var hub = this.hub;
    var session = this.sessions.get(sessionId);
    if (!session) return { ok: false, reason: '会话不存在' };
    var auth = this._authorizeContext(actor, session, 'write');
    if (!auth.ok) return auth;

    if (baseVersion != null && baseVersion !== session.version) {
      var conflict = {
        id: uid('cfl'), sessionId: sessionId, key: key, actor: actor,
        expected: baseVersion, actual: session.version,
        holder: session.locks[key] || null, at: ts(),
        strategy: 'rebase + key-level merge', resolved: false
      };
      hub.conflicts.unshift(conflict);
      if (hub.conflicts.length > 60) hub.conflicts.pop();
      hub.audit({
        actor: actor, action: 'context.conflict', target: sessionId,
        decision: DECISION.DENIED, sensitivity: session.sensitivity,
        detail: '版本 ' + baseVersion + ' -> ' + session.version + ' 冲突'
      });
      hub.log('warn', '冲突协调', '「' + session.title + '」键 ' + key + ' 检测到并发写：v' + baseVersion + ' → v' + session.version);
      hub.bus.publish({
        from: 'hub', to: actor, kind: MSG_KIND.CONFLICT, sessionId: sessionId,
        sensitivity: session.sensitivity,
        payload: { text: '检测到并发写冲突，正在变基合并', conflictId: conflict.id }
      });
      hub.emit('conflict', conflict);

      // Resolution: rebase onto current version, merge by key (no lost update)
      var existing = session.entries.filter(function (e) { return e.key === key; }).pop();
      if (existing) existing.value = value, existing.author = actor, existing.at = ts(), existing.version = session.version + 1;
      else session.entries.push({ id: uid('entry'), key: key, value: value, sensitivity: session.sensitivity, author: actor, at: ts(), version: session.version + 1 });
      session.version += 1;
      session.updatedAt = ts();
      conflict.resolved = true;
      conflict.resolvedAt = ts();
      hub.log('success', '冲突协调', '已按 ' + conflict.strategy + ' 解决，会话升至 v' + session.version);
      hub.emit('conflict', conflict);
      hub.audit({
        actor: 'hub', action: 'context.conflict.resolve', target: sessionId,
        decision: DECISION.ALLOWED, sensitivity: session.sensitivity, detail: conflict.strategy
      });
      return { ok: true, conflict: conflict, version: session.version, merged: true };
    }

    var prev = session.entries.filter(function (e) { return e.key === key; }).pop();
    if (prev) { prev.value = value; prev.author = actor; prev.at = ts(); prev.version = session.version + 1; }
    else session.entries.push({ id: uid('entry'), key: key, value: value, sensitivity: session.sensitivity, author: actor, at: ts(), version: session.version + 1 });
    session.version += 1;
    session.updatedAt = ts();
    session.locks[key] = actor;
    hub.audit({
      actor: actor, action: 'context.write', target: sessionId,
      decision: DECISION.ALLOWED, sensitivity: session.sensitivity,
      detail: key + ' @ v' + session.version, grantId: auth.grantId
    });
    hub.emit('context.write', { sessionId: sessionId, key: key, actor: actor, version: session.version });
    return { ok: true, version: session.version };
  };

  /** Cross-tool context sync with per-agent redaction. */
  ContextStore.prototype.sync = function (actor, sessionId) {
    var r = this.read(actor, sessionId);
    if (!r.ok) return r;
    var session = this.sessions.get(sessionId);
    this.hub.bus.publish({
      from: 'hub', to: actor, kind: MSG_KIND.CTX_SYNC, sessionId: sessionId,
      sensitivity: session.sensitivity,
      payload: { text: '上下文同步 v' + session.version + '，' + r.entries.length + ' 条可见条目', redacted: r.redacted }
    });
    this.hub.log('info', '上下文同步', this.hub.nameOf(actor) + ' ← 「' + session.title + '」v' + session.version + (r.redacted ? '（脱敏 ' + r.redacted + ' 条）' : ''));
    return r;
  };

  /* ============================================================
   * 5. Task engine - dispatch / flow / conflict / retry / aggregate
   * ========================================================== */

  function TaskEngine(hub) {
    this.hub = hub;
    this.tasks = [];
    this.completed = 0;
    this.failed = 0;
    this.retried = 0;
  }

  TaskEngine.prototype.get = function (id) {
    return this.tasks.find(function (t) { return t.id === id; }) || null;
  };

  TaskEngine.prototype.submit = function (spec) {
    var hub = this.hub;
    var real = !!spec.real;
    var t = {
      id: uid('task'),
      execution: {
        mode: real ? 'real' : 'sim',
        tool: real ? (spec.tool || 'auto') : null,
        permission: real ? (spec.permission || 'readonly') : null,
        workdir: real ? (spec.workdir || null) : null,
        prompt: real ? (spec.prompt || spec.title) : null,
        status: real ? 'pending' : 'not-applicable',
        output: '', error: '', exitCode: null, durationMs: null,
        costUsd: null, model: null, sessionId: null,
        startedAt: null, finishedAt: null, attempts: 0
      },
      title: spec.title || '未命名任务',
      description: spec.description || '',
      priority: spec.priority || 'P2',
      requiredCapabilities: spec.requiredCapabilities || [],
      sensitivity: spec.sensitivity == null ? SENS.INTERNAL : spec.sensitivity,
      resources: spec.resources || [],
      sessionId: spec.sessionId || null,
      collaborative: !!spec.collaborative || (spec.requiredCapabilities || []).length >= 2,
      maxRetries: spec.maxRetries == null ? 2 : spec.maxRetries,
      backoffMs: spec.backoffMs || 1600,
      assignedTo: spec.assignedTo || null,
      collaborators: [],
      fragments: [],
      status: STATE.QUEUED,
      progress: 0,
      attempts: 0,
      failovers: 0,
      createdAt: ts(), startedAt: null, finishedAt: null,
      retryAt: null, result: null, error: null, conflictId: null,
      history: []
    };
    this._push(t, STATE.QUEUED, '任务提交至统一调度队列');
    this.tasks.unshift(t);
    if (this.tasks.length > 90) this.tasks.pop();
    hub.log(real ? 'success' : 'info', '调度器',
      (real ? '[真实执行] ' : '') + '新任务 ' + t.id.slice(-6) + '「' + t.title + '」进入队列（' + t.priority + '）');
    hub.audit({
      actor: spec.requestedBy || 'user', action: real ? 'task.submit.real' : 'task.submit', target: t.id,
      decision: DECISION.ALLOWED, sensitivity: t.sensitivity,
      detail: t.title + (real ? ' | ' + t.execution.tool + ' / ' + t.execution.permission : '')
    });
    hub.emit('task', t);
    this.schedule();
    return t;
  };

  TaskEngine.prototype._push = function (t, status, note) {
    t.status = status;
    t.history.push({ at: ts(), status: status, note: note, agent: t.assignedTo });
    if (t.history.length > 30) t.history.shift();
  };

  /**
   * Capability + load + trust + clearance aware scheduling.
   * A lead only needs to cover ONE required capability; uncovered capabilities
   * are delegated to peer Harnesses through the collaboration channel. This
   * avoids deadlocking multi-skill tasks while modelling real delegation.
   */
  TaskEngine.prototype._rank = function (t, agent) {
    if (agent.communication === 'http') return -1;
    if (!agent.enabled || agent.status === AGENT_STATUS.OFFLINE || agent.status === AGENT_STATUS.ERROR) return -1;
    if (agent.clearance < t.sensitivity) return -1;
    var caps = t.requiredCapabilities || [];
    var matched = caps.filter(function (c) { return agent.capabilities.indexOf(c) >= 0; }).length;
    if (caps.length && matched === 0) return -1;
    var active = this.tasks.filter(function (x) {
      return x.assignedTo === agent.id && (x.status === STATE.RUNNING || x.status === STATE.ASSIGNED);
    }).length;
    if (active >= agent.maxConcurrency) return -1;
    var score = 40 - agent.load * 30 - active * 12 + agent.trust * 10 + agent.reliability * 15;
    score += matched * 6;
    if (caps.length && matched < caps.length) score -= (caps.length - matched) * 3;
    if (t.priority === 'P0') score += 8;
    return score;
  };

  TaskEngine.prototype.schedule = function () {
    var self = this, hub = this.hub;
    // Real tasks are driven by the server-side execution queue, never by
    // the simulation scheduler.
    var queued = this.tasks.filter(function (t) {
      return t.status === STATE.QUEUED && t.execution.mode === 'sim';
    });
    queued.sort(function (a, b) { return a.priority.localeCompare(b.priority) || a.createdAt - b.createdAt; });
    queued.forEach(function (t) {
      var candidates = [];
      hub.agents.forEach(function (a) {
        var s = self._rank(t, a);
        if (s >= 0) candidates.push({ agent: a, score: s });
      });
      candidates.sort(function (a, b) { return b.score - a.score; });
      var chosen = null;
      if (t.assignedTo) {
        var forced = hub.agents.get(t.assignedTo);
        if (forced && self._rank(t, forced) >= 0) chosen = forced;
      }
      if (!chosen && candidates.length) chosen = candidates[0].agent;
      if (!chosen) {
        if (!t._waitLogged) {
          hub.log('warn', '调度器', '任务 ' + t.id.slice(-6) + ' 暂挂：无满足能力与密级要求的空闲实例');
          t._waitLogged = true;
        }
        return;
      }
      self._assign(t, chosen, candidates.length);
    });
  };

  TaskEngine.prototype._assign = function (t, agent, candidateCount) {
    var hub = this.hub;
    t.assignedTo = agent.id;
    t.collaborators = [];
    t.fragments = [];
    this._push(t, STATE.ASSIGNED, '派发给 ' + agent.name + '（候选 ' + candidateCount + ' 个）');
    hub.bus.publish({
      from: 'hub', to: agent.id, kind: MSG_KIND.ASSIGN, sensitivity: t.sensitivity,
      sessionId: t.sessionId, correlationId: t.id,
      payload: { text: '任务派发：' + t.title, taskId: t.id, priority: t.priority }
    });
    hub.audit({
      actor: 'hub', action: 'task.assign', target: t.id, decision: DECISION.ALLOWED,
      sensitivity: t.sensitivity, detail: agent.name
    });

    // Resource conflict: same resource simultaneously touched by a running task
    var colliding = this.tasks.find(function (x) {
      if (x.id === t.id || x.status !== STATE.RUNNING) return false;
      return x.resources.some ? x.resources.some(function (r) {
        return t.resources.some(function (r2) { return r2.key === r.key; });
      }) : false;
    });
    if (colliding) {
      var c = {
        id: uid('cfl'), sessionId: t.sessionId || colliding.sessionId || '-',
        key: (t.resources[0] || {}).key || 'resource', actor: agent.id,
        expected: t.createdAt, actual: colliding.createdAt,
        holder: colliding.assignedTo, at: ts(),
        strategy: '优先级排队 + 版本变基', resolved: false
      };
      hub.conflicts.unshift(c);
      t.conflictId = c.id;
      this._push(t, STATE.BLOCKED, '资源 ' + c.key + ' 被 ' + hub.nameOf(colliding.assignedTo) + ' 占用，进入冲突协调');
      hub.log('warn', '冲突协调', '任务 ' + t.id.slice(-6) + ' 与 ' + colliding.id.slice(-6) + ' 争用 ' + c.key + '，已降序排队');
      hub.bus.publish({
        from: 'hub', to: agent.id, kind: MSG_KIND.CONFLICT, correlationId: t.id,
        sensitivity: t.sensitivity, sessionId: t.sessionId,
        payload: { text: '资源冲突，等待前一任务释放', conflictId: c.id }
      });
      hub.emit('conflict', c);
      return;
    }

    this._begin(t);
  };

  TaskEngine.prototype._begin = function (t) {
    var hub = this.hub;
    var agent = hub.agents.get(t.assignedTo);
    if (!agent) { this._push(t, STATE.QUEUED, '执行端失效，重新入队'); return; }
    t.startedAt = t.startedAt || ts();
    this._push(t, STATE.RUNNING, agent.name + ' 开始执行');
    agent.status = AGENT_STATUS.BUSY;
    agent.load = clamp(agent.load + 0.22, 0, 1);
    hub.log('info', '任务流', agent.name + ' 开始执行 ' + t.id.slice(-6) + '「' + t.title + '」');

    if (t.collaborative) this._openCollaboration(t, agent);
    hub.emit('task', t);
  };

  TaskEngine.prototype._openCollaboration = function (t, lead) {
    var hub = this.hub;
    // Capabilities the lead cannot cover are filled by peer Harnesses.
    var missing = (t.requiredCapabilities || []).filter(function (c) {
      return lead.capabilities.indexOf(c) < 0;
    });
    var pool = [];
    hub.agents.forEach(function (a) {
      if (a.communication === 'http' || a.id === lead.id || !a.enabled || a.status === AGENT_STATUS.OFFLINE || a.clearance < t.sensitivity) return;
      var covers = missing.filter(function (c) { return a.capabilities.indexOf(c) >= 0; }).length;
      pool.push({ agent: a, covers: covers, jitter: hub.rng() });
    });
    pool.sort(function (x, y) { return y.covers - x.covers || x.jitter - y.jitter; });
    var helpers = pool.slice(0, Math.min(Math.max(missing.length, 1), 2, pool.length)).map(function (p) { return p.agent; });
    helpers.forEach(function (h) {
      t.collaborators.push(h.id);
      // Collaboration messages are plain envelopes; session access is a
      // separate, explicitly authorized channel (ctx.* + ContextStore ACL).
      hub.bus.publish({
        from: lead.id, to: h.id, kind: MSG_KIND.COLLAB_ASK, correlationId: t.id,
        sensitivity: t.sensitivity,
        payload: { text: pick(hub.rng, COLLAB_LINES), taskId: t.id }
      });
      var reply = hub.bus.publish({
        from: h.id, to: lead.id, kind: MSG_KIND.COLLAB_REPLY, correlationId: t.id,
        sensitivity: Math.min(t.sensitivity, h.clearance),
        payload: { text: '已回传子结论：' + t.title.slice(0, 12) + ' 的局部分析完成', fragment: h.name + ' 产出 ' + (2 + Math.floor(hub.rng() * 5)) + ' 条结论' }
      });
      if (reply.status === 'delivered') {
        t.fragments.push({ agentId: h.id, agentName: h.name, text: reply.payload.fragment, at: ts() });
      }
    });
    if (helpers.length) hub.log('info', '协作', lead.name + ' 拉起 ' + helpers.length + ' 个协作方共同处理 ' + t.id.slice(-6));
  };

  TaskEngine.prototype._succeed = function (t) {
    var hub = this.hub;
    var agent = hub.agents.get(t.assignedTo);
    t.progress = 100;
    t.finishedAt = ts();
    this._push(t, STATE.SUCCEEDED, '执行完成，结果汇总');
    t.result = this.aggregate(t);
    if (agent) {
      agent.stats.completed += 1;
      agent.load = clamp(agent.load - 0.18, 0, 1);
      agent.status = agent.load > 0.55 ? AGENT_STATUS.BUSY : AGENT_STATUS.ONLINE;
    }
    this.completed += 1;
    hub.bus.publish({
      from: t.assignedTo || 'hub', to: t.execution.mode === 'real' ? 'hub' : 'broadcast', kind: MSG_KIND.RESULT,
      correlationId: t.id, sensitivity: Math.min(t.sensitivity, SENS.CONFIDENTIAL), sessionId: t.sessionId,
      payload: { text: '任务完成：' + t.title, summary: t.result.summary, taskId: t.id }
    });
    if (t.sessionId && hub.context.sessions.has(t.sessionId)) {
      hub.context.write(t.assignedTo || 'hub', t.sessionId, 'last_result', t.result.summary);
    }
    hub.log('success', '任务流', (t.execution.mode === 'real' ? '[真实执行] ' : '') +
      t.id.slice(-6) + ' 完成 · ' + t.result.contributors.length + ' 方结果已汇总' +
      (t.execution.mode === 'real' && t.execution.durationMs ? ' · ' + t.execution.durationMs + 'ms' : ''));
    hub.audit({
      actor: t.assignedTo || 'hub', action: 'task.complete', target: t.id,
      decision: DECISION.ALLOWED, sensitivity: t.sensitivity, detail: t.result.summary
    });
    hub.emit('task', t);
  };

  /** Merge lead output + collaborator fragments into one provenance-tracked result. */
  TaskEngine.prototype.aggregate = function (t) {
    var lead = this.hub.nameOf(t.assignedTo);
    var ex = t.execution || { mode: 'sim' };
    var isReal = ex.mode === 'real' && !!ex.output;
    var contributors = [{
      agentId: t.assignedTo, agentName: lead,
      role: isReal ? '真实执行 · ' + (ex.tool || 'CLI') : '主执行',
      text: isReal ? String(ex.output).slice(0, 20000) : '主执行结果：' + t.title + ' 已完成'
    }];
    (t.fragments || []).forEach(function (f) {
      contributors.push({ agentId: f.agentId, agentName: f.agentName, role: '协作方', text: f.text });
    });
    // Full CLI output lives in execution.output / contributors[0].text;
    // keep the summary short so messages and logs stay readable.
    var summary = isReal
      ? String(ex.output).slice(0, 600)
      : contributors.map(function (c) { return c.agentName + ': ' + c.text; }).join(' | ').slice(0, 180);
    return {
      summary: summary,
      contributors: contributors,
      mergedAt: ts(),
      real: isReal,
      source: isReal ? (ex.tool || 'CLI') : 'simulation',
      costUsd: isReal ? ex.costUsd : null,
      model: isReal ? ex.model : null,
      durationMs: isReal ? ex.durationMs : null,
      attempts: t.attempts + 1,
      confidence: isReal ? 1 : round(clamp(0.82 + contributors.length * 0.04 - t.attempts * 0.07, 0.5, 0.99), 2),
      provenance: contributors.map(function (c) { return c.agentId; })
    };
  };

  /* ---------- Real execution lifecycle (called by server.js) ---------- */

  /** Mark a real task as running on a specific Harness instance. */
  TaskEngine.prototype.startReal = function (id, agentId, meta) {
    var t = this.get(id);
    if (!t || t.execution.mode !== 'real') return null;
    var hub = this.hub;
    t.assignedTo = agentId || t.assignedTo;
    t.startedAt = ts();
    t.execution.status = 'running';
    t.execution.startedAt = ts();
    t.execution.tool = (meta && meta.tool) || t.execution.tool;
    t.execution.permission = (meta && meta.permission) || t.execution.permission;
    t.execution.attempts += 1;
    t.progress = 12;
    this._push(t, STATE.RUNNING, '真实 CLI 启动：' + (t.execution.tool || 'CLI') + ' / ' + t.execution.permission);
    hub.log('info', '真实执行', hub.nameOf(t.assignedTo) + ' 正在执行 ' + id.slice(-6) + '「' + t.title + '」');
    hub.audit({
      actor: t.assignedTo || 'hub', action: 'real.exec.start', target: id,
      decision: DECISION.ALLOWED, sensitivity: t.sensitivity,
      detail: t.execution.tool + ' / ' + t.execution.permission + ' / ' + (t.execution.workdir || '-')
    });
    hub.emit('task', t);
    return t;
  };

  /** Optional coarse progress hint from the execution queue. */
  TaskEngine.prototype.updateReal = function (id, progress, note) {
    var t = this.get(id);
    if (!t || t.execution.mode !== 'real' || t.status !== STATE.RUNNING) return null;
    t.progress = clamp(Number(progress) || t.progress, 0, 95);
    if (note) t.execution.note = note;
    this.hub.emit('task', t);
    return t;
  };

  /** Apply the CLI result: success feeds the normal aggregation pipeline. */
  TaskEngine.prototype.completeReal = function (id, result) {
    var t = this.get(id);
    if (!t || t.execution.mode !== 'real') return null;
    // Task may have been cancelled while the CLI was still running.
    if (t.status === STATE.CANCELLED) {
      this.hub.log('warn', '真实执行', id.slice(-6) + ' 已取消，忽略迟到的 CLI 回执');
      return t;
    }
    var hub = this.hub;
    var ex = t.execution;
    var meta = (result && result.meta) || {};
    ex.status = result && result.ok ? 'done' : 'failed';
    ex.finishedAt = ts();
    ex.output = (result && result.output) || '';
    ex.error = (result && result.error) || '';
    ex.exitCode = meta.exitCode == null ? null : meta.exitCode;
    ex.durationMs = meta.durationMs == null ? (ex.startedAt ? ts() - ex.startedAt : null) : meta.durationMs;
    ex.costUsd = meta.costUsd == null ? null : meta.costUsd;
    ex.model = meta.model || null;
    ex.sessionId = meta.sessionId || null;
    ex.permission = meta.permission || ex.permission;
    ex.workdir = meta.workdir || ex.workdir;
    ex.tool = meta.tool || ex.tool;
    ex.stdout = meta.stdout || '';
    ex.stderr = meta.stderr || '';

    if (result && result.ok) {
      this._push(t, STATE.RUNNING, '真实 CLI 返回 ' + ex.output.length + ' 字符，进入结果汇总');
      this._succeed(t);
      hub.audit({
        actor: t.assignedTo || 'hub', action: 'real.exec.complete', target: id,
        decision: DECISION.ALLOWED, sensitivity: t.sensitivity,
        detail: ex.tool + ' / ' + (ex.durationMs || 0) + 'ms' + (ex.costUsd != null ? ' / $' + ex.costUsd : '')
      });
    } else {
      t.finishedAt = ts();
      this._push(t, STATE.FAILED, '真实执行失败：' + ex.error);
      t.error = ex.error;
      this.failed += 1;
      var agent = hub.agents.get(t.assignedTo);
      if (agent) { agent.load = clamp(agent.load - 0.2, 0, 1); agent.stats.failed += 1; }
      hub.bus.publish({
        from: t.assignedTo || 'hub', to: 'hub', kind: MSG_KIND.ERROR, correlationId: id,
        sensitivity: t.sensitivity, sessionId: t.sessionId,
        payload: { text: '真实执行失败：' + ex.error, taskId: id }
      });
      hub.log('error', '真实执行', id.slice(-6) + ' 失败：' + ex.error);
      hub.audit({
        actor: t.assignedTo || 'hub', action: 'real.exec.fail', target: id,
        decision: DECISION.DENIED, sensitivity: t.sensitivity, detail: ex.error
      });
      hub.emit('task', t);
    }
    return t;
  };

  TaskEngine.prototype._fail = function (t, reason, agent) {
    var hub = this.hub;
    t.error = reason;
    t.attempts += 1;
    hub.audit({
      actor: agent ? agent.id : 'hub', action: 'task.fail', target: t.id,
      decision: DECISION.DENIED, sensitivity: t.sensitivity, detail: reason
    });
    hub.bus.publish({
      from: agent ? agent.id : 'hub', to: 'hub', kind: MSG_KIND.ERROR, correlationId: t.id,
      sensitivity: t.sensitivity, sessionId: t.sessionId,
      payload: { text: '执行失败：' + reason, taskId: t.id }
    });
    if (agent) agent.stats.failed += 1;

    if (t.attempts <= t.maxRetries) {
      var backoff = t.backoffMs * Math.pow(2, t.attempts - 1) + Math.floor(hub.rng() * 500);
      t.retryAt = ts() + backoff;
      t.progress = Math.max(0, t.progress - 18);
      this._push(t, STATE.RETRYING, '第 ' + t.attempts + ' 次失败，' + Math.round(backoff / 1000 * 10) / 10 + 's 后指数退避重试：' + reason);
      this.retried += 1;
      if (agent) agent.stats.retried += 1;
      hub.log('error', '失败重试', t.id.slice(-6) + ' 失败（' + reason + '），' + t.attempts + '/' + t.maxRetries + ' 次重试已排程');
      hub.emit('task.retry', t);
    } else if (t.failovers < 1) {
      // Failover: hand over to a different qualified agent
      t.failovers += 1;
      t.attempts = 0;
      t.assignedTo = null;
      t.progress = 0;
      var alt = null, best = -1;
      var self = this;
      hub.agents.forEach(function (a) {
        if (a.id === (agent || {}).id) return;
        var s = self._rank(t, a);
        if (s > best) { best = s; alt = a; }
      });
      if (alt && best >= 0) {
        this._push(t, STATE.QUEUED, '重试耗尽，故障转移至 ' + alt.name);
        hub.log('warn', '故障转移', t.id.slice(-6) + ' 切换执行端 → ' + alt.name);
        t.assignedTo = alt.id;
        this._assign(t, alt, 1);
      } else {
        this._terminalFail(t, reason);
      }
    } else {
      this._terminalFail(t, reason);
    }
    hub.emit('task', t);
  };

  TaskEngine.prototype._terminalFail = function (t, reason) {
    t.finishedAt = ts();
    this._push(t, STATE.FAILED, '重试与故障转移均耗尽：' + reason);
    this.failed += 1;
    var agent = this.hub.agents.get(t.assignedTo);
    if (agent) { agent.load = clamp(agent.load - 0.2, 0, 1); agent.status = AGENT_STATUS.ERROR; }
    this.hub.log('error', '任务流', t.id.slice(-6) + ' 终态失败：' + reason);
    setTimeout(function () {
      if (agent && agent.status === AGENT_STATUS.ERROR) agent.status = AGENT_STATUS.ONLINE;
    }, 6000);
  };

  TaskEngine.prototype.tick = function () {
    var self = this, hub = this.hub;
    this.tasks.forEach(function (t) {
      if (t.status === STATE.RUNNING) {
        if (t.execution.mode === 'real') return; // driven by real CLI callbacks
        var agent = hub.agents.get(t.assignedTo);
        if (typeof t.progress !== 'number' || isNaN(t.progress)) t.progress = 0;
        var step = 6 + hub.rng() * 20;
        t.progress = clamp(t.progress + step, 0, 100);
        if (chance(hub.rng, 0.22)) {
          hub.bus.publish({
            from: t.assignedTo || 'hub', to: 'hub', kind: MSG_KIND.PROGRESS,
            correlationId: t.id, sensitivity: t.sensitivity, sessionId: t.sessionId,
            payload: { text: t.title.slice(0, 14) + ' 进度 ' + Math.round(t.progress) + '%', progress: Math.round(t.progress) }
          });
        }
        if (t.progress >= 100) {
          var reliability = agent ? agent.reliability : 0.9;
          if (chance(hub.rng, 1 - reliability)) self._fail(t, pick(hub.rng, ['上游超时', '上下文窗口溢出', '工具调用返回非零', '临时鉴权失效']), agent);
          else self._succeed(t);
        } else if (chance(hub.rng, 0.012)) {
          self._fail(t, '执行中断（进程异常退出）', agent);
        }
      } else if (t.status === STATE.RETRYING && t.retryAt && ts() >= t.retryAt) {
        t.retryAt = null;
        t.progress = Math.max(t.progress, 8);
        self._begin(t);
      } else if (t.status === STATE.BLOCKED) {
        var stillBlocked = self.tasks.some(function (x) {
          return x.id !== t.id && x.status === STATE.RUNNING && x.resources.some(function (r) {
            return t.resources.some(function (r2) { return r2.key === r.key; });
          });
        });
        if (!stillBlocked) {
          var c = hub.conflicts.find(function (x) { return x.id === t.conflictId; });
          if (c) { c.resolved = true; c.resolvedAt = ts(); hub.emit('conflict', c); }
          hub.log('success', '冲突协调', '资源已释放，任务 ' + t.id.slice(-6) + ' 变基后恢复执行');
          self._push(t, STATE.QUEUED, '冲突已解决，重新排队');
          self._begin(t);
        }
      }
    });
    this.schedule();
    // Keep the pipeline alive: top up the queue while few tasks are open.
    var open = this.tasks.filter(function (x) {
      return [STATE.QUEUED, STATE.ASSIGNED, STATE.RUNNING, STATE.BLOCKED, STATE.RETRYING].indexOf(x.status) >= 0;
    }).length;
    if (open < 4 && chance(hub.rng, open === 0 ? 0.7 : 0.34)) this._autoSubmit();
  };

  TaskEngine.prototype._autoSubmit = function () {
    var hub = this.hub;
    var tpl = clone(pick(hub.rng, TASK_TEMPLATES));
    tpl.requestedBy = 'scheduler';
    tpl.collaborative = chance(hub.rng, 0.42);
    if (chance(hub.rng, 0.3) && hub.context.sessions.size) {
      var sessions = Array.from(hub.context.sessions.values());
      tpl.sessionId = pick(hub.rng, sessions).id;
    }
    if (chance(hub.rng, 0.42)) tpl.resources = [{ key: pick(hub.rng, ['repo/auth', 'repo/payment', 'config/gateway']), version: 1 }];
    this.submit(tpl);
  };

  /* ============================================================
   * 6. Hub - composition root
   * ========================================================== */

  function Hub(options) {
    var opts = options || {};
    this.version = '1.0.0';
    this.rng = mulberry32(opts.seed == null ? 20261105 : opts.seed);
    this.events = new Emitter();
    this.agents = new Map();
    this.adapterFactories = {};
    this.bus = new MessageBus(this);
    this.context = new ContextStore(this);
    this.tasks = new TaskEngine(this);
    this.auditLog = [];
    // Rolling window keeps memory bounded; the anchor preserves the hash of
    // the last evicted record so the retained chain remains verifiable.
    this.auditAnchor = '00000000';
    this.auditTotal = 0;
    this.logs = [];
    this.pendingApprovals = [];
    this.conflicts = [];
    this.policy = {
      defaultDeny: true,
      allowSecretBroadcast: false,
      requireApprovalForSecret: true,
      maxAutoSensitivity: SENS.CONFIDENTIAL,
      auditRetention: 2000
    };
    this.meta = { tick: 0, running: false, speed: 1, startedAt: ts(), mode: opts.mode || 'demo' };
    this._timer = null;
    this._baseInterval = 1100;
    this._stats = { messages: 0, denied: 0, conflicts: 0, syncs: 0 };

    this._registerBuiltinAdapters();
    if (opts.autoSeed !== false) this._seed();
  }

  Hub.prototype._registerBuiltinAdapters = function () {
    var self = this;
    Object.keys(HARNESS_TYPES).forEach(function (type) {
      self.registerAdapter(type, function (spec) { return new SimulatedAdapter(spec, self.rng); }, HARNESS_TYPES[type]);
    });
  };

  /** Extension API: register a new Harness type at runtime. */
  Hub.prototype.registerAdapter = function (type, factory, descriptor) {
    this.adapterFactories[type] = { factory: factory, descriptor: descriptor || {} };
    if (!HARNESS_TYPES[type]) HARNESS_TYPES[type] = Object.assign({ label: type, glyph: type.charAt(0).toUpperCase(), color: '#6366f1', transport: 'http', capabilities: [], maxConcurrency: 1, reliability: 0.9, clearance: SENS.INTERNAL }, descriptor || {});
    this.emit('adapter', { type: type, descriptor: HARNESS_TYPES[type] });
    return HARNESS_TYPES[type];
  };

  Hub.prototype.nameOf = function (id) {
    if (id === 'hub' || id === 'user' || id === 'scheduler') return id === 'hub' ? '协调中枢' : (id === 'user' ? '用户' : '调度器');
    var a = this.agents.get(id);
    return a ? a.name : (id || '未知');
  };

  Hub.prototype.emit = function (type, data) { this.events.emit(type, data); };
  Hub.prototype.on = function (type, fn) { return this.events.on(type, fn); };

  Hub.prototype.log = function (level, source, text) {
    var entry = { id: uid('log'), at: ts(), level: level, source: source, text: text };
    this.logs.push(entry);
    if (this.logs.length > 400) this.logs.splice(0, this.logs.length - 400);
    this.events.emit('log', entry);
    return entry;
  };

  /** Hash-chained append-only audit record. */
  Hub.prototype.audit = function (entry) {
    var prev = this.auditLog.length ? this.auditLog[this.auditLog.length - 1].hash : this.auditAnchor;
    this.auditTotal += 1;
    var rec = {
      id: uid('aud'), seq: this.auditTotal, at: ts(),
      actor: entry.actor || 'system', action: entry.action || '-', target: entry.target || '-',
      decision: entry.decision || DECISION.ALLOWED,
      sensitivity: entry.sensitivity == null ? SENS.INTERNAL : entry.sensitivity,
      detail: entry.detail || '', grantId: entry.grantId || null,
      prevHash: prev
    };
    rec.hash = fnv1a(prev + '|' + rec.seq + '|' + rec.at + '|' + rec.actor + '|' + rec.action + '|' + rec.target + '|' + rec.decision + '|' + rec.detail);
    this.auditLog.push(rec);
    this._trimAudit();
    if (rec.decision === DECISION.DENIED) this._stats.denied += 1;
    this.events.emit('audit', rec);
    return rec;
  };

  /** Evict oldest records while keeping the chain head anchored. */
  Hub.prototype._trimAudit = function () {
    var limit = Math.max(1, this.policy.auditRetention);
    while (this.auditLog.length > limit) {
      this.auditAnchor = this.auditLog.shift().hash;
    }
  };

  Hub.prototype.verifyAuditChain = function () {
    var firstSeq = this.auditTotal - this.auditLog.length + 1;
    for (var i = 0; i < this.auditLog.length; i++) {
      var r = this.auditLog[i];
      var expectPrev = i === 0 ? this.auditAnchor : this.auditLog[i - 1].hash;
      var expectHash = fnv1a(expectPrev + '|' + r.seq + '|' + r.at + '|' + r.actor + '|' + r.action + '|' + r.target + '|' + r.decision + '|' + r.detail);
      if (r.prevHash !== expectPrev || r.hash !== expectHash || r.seq !== firstSeq + i) {
        return { ok: false, brokenAt: r.seq };
      }
    }
    return { ok: true, length: this.auditLog.length, total: this.auditTotal, anchor: this.auditAnchor };
  };

  /* ---------- Agents ---------- */

  Hub.prototype.createAgent = function (spec) {
    var type = spec.type || 'custom';
    var desc = HARNESS_TYPES[type] || this.registerAdapter(type, function (s) { return new SimulatedAdapter(s, this.rng); }.bind(this), {
      label: spec.label || type, glyph: (spec.label || type).charAt(0).toUpperCase(),
      color: spec.color || '#6366f1', capabilities: spec.capabilities || [],
      transport: spec.transport || 'http', maxConcurrency: 1, reliability: 0.9,
      clearance: spec.clearance == null ? SENS.INTERNAL : spec.clearance
    });
    var factory = (this.adapterFactories[type] || {}).factory;
    var adapter = factory ? factory(Object.assign({}, spec, { type: type })) : new SimulatedAdapter({ type: type }, this.rng);
    var a = {
      id: spec.id || uid('agt'),
      name: spec.name || desc.label + ' #' + (this.agents.size + 1),
      type: type, label: desc.label, vendor: spec.vendor || desc.vendor || 'Custom',
      glyph: desc.glyph, color: spec.color || desc.color,
      endpoint: spec.endpoint || 'local://' + type + '/' + (this.agents.size + 1),
      transport: desc.transport,
      capabilities: (spec.capabilities || desc.capabilities || []).slice(),
      clearance: spec.clearance == null ? desc.clearance : spec.clearance,
      maxConcurrency: spec.maxConcurrency || desc.maxConcurrency || 1,
      reliability: desc.reliability == null ? 0.9 : desc.reliability,
      trust: spec.trust == null ? round(0.7 + this.rng() * 0.28, 2) : spec.trust,
      status: spec.status || AGENT_STATUS.ONLINE,
      enabled: spec.enabled !== false,
      load: spec.load == null ? round(this.rng() * 0.35, 2) : spec.load,
      registeredAt: ts(), lastHeartbeat: ts(),
      stats: { completed: 0, failed: 0, retried: 0, messages: 0 },
      adapterKind: adapter && adapter.constructor ? adapter.constructor.name : 'SimulatedAdapter'
    };
    this.agents.set(a.id, a);
    this.bus.subscribe(a.id, ['tasks', 'context', 'collab']);
    this._adapters = this._adapters || {};
    this._adapters[a.id] = adapter;
    this.audit({ actor: 'user', action: 'agent.register', target: a.id, decision: DECISION.ALLOWED, sensitivity: SENS.INTERNAL, detail: a.name + ' / ' + type });
    this.log('success', '注册中心', a.name + ' 已注册（' + desc.transport + '，能力 ' + a.capabilities.length + ' 项）');
    this.emit('agent', a);
    return a;
  };

  Hub.prototype.setAgentEnabled = function (id, enabled) {
    var a = this.agents.get(id);
    if (!a) return null;
    a.enabled = enabled;
    a.status = enabled ? AGENT_STATUS.ONLINE : AGENT_STATUS.PAUSED;
    this.audit({ actor: 'user', action: enabled ? 'agent.enable' : 'agent.disable', target: id, decision: DECISION.ALLOWED, sensitivity: SENS.INTERNAL, detail: a.name });
    this.log(enabled ? 'success' : 'warn', '注册中心', a.name + (enabled ? ' 已启用' : ' 已停用，停止接收任务与消息'));
    this.emit('agent', a);
    return a;
  };

  Hub.prototype.setPolicy = function (patch) {
    Object.assign(this.policy, patch || {});
    this.audit({ actor: 'user', action: 'policy.update', target: 'global', decision: DECISION.ALLOWED, sensitivity: SENS.CONFIDENTIAL, detail: JSON.stringify(patch) });
    this.log('info', '策略中心', '全局安全策略已更新');
    this.emit('policy', this.policy);
    return this.policy;
  };

  /* ---------- User-facing operations ---------- */

  Hub.prototype.submitTask = function (spec) { return this.tasks.submit(spec); };

  Hub.prototype.retryTask = function (id) {
    var t = this.tasks.get(id);
    if (!t || t.status !== STATE.FAILED) return null;
    t.attempts = 0; t.failovers = 0; t.progress = 0; t.error = null; t.retryAt = null; t.finishedAt = null;
    t.assignedTo = null;
    this.tasks._push(t, STATE.QUEUED, '用户手动重新派发');
    this.audit({ actor: 'user', action: 'task.retry.manual', target: id, decision: DECISION.ALLOWED, sensitivity: t.sensitivity, detail: t.title });
    this.log('info', '任务流', id.slice(-6) + ' 由用户手动重启');
    this.tasks.schedule();
    this.emit('task', t);
    return t;
  };

  Hub.prototype.cancelTask = function (id) {
    var t = this.tasks.get(id);
    if (!t || [STATE.SUCCEEDED, STATE.FAILED, STATE.CANCELLED].indexOf(t.status) >= 0) return null;
    this.tasks._push(t, STATE.CANCELLED, '用户取消');
    t.finishedAt = ts();
    this.audit({ actor: 'user', action: 'task.cancel', target: id, decision: DECISION.ALLOWED, sensitivity: t.sensitivity, detail: t.title });
    this.log('warn', '任务流', id.slice(-6) + ' 已取消');
    this.emit('task', t);
    return t;
  };

  Hub.prototype.sendMessage = function (spec) {
    var m = this.bus.publish({
      from: spec.from, to: spec.to, kind: spec.kind || MSG_KIND.COLLAB_ASK,
      sensitivity: spec.sensitivity == null ? SENS.INTERNAL : spec.sensitivity,
      sessionId: spec.sessionId || null,
      payload: { text: spec.text || '' }, correlationId: spec.correlationId || null,
      replyTo: spec.replyTo || null
    });
    this._stats.messages += 1;
    var src = this.agents.get(spec.from);
    if (src) src.stats.messages += 1;
    this.log(m.status === 'delivered' ? 'info' : 'error', '消息总线',
      this.nameOf(m.from) + ' → ' + (m.to === 'broadcast' ? '全体广播' : this.nameOf(m.to)) + '：' + (spec.text || m.kind) + (m.status === 'rejected' ? ' [已拦截: ' + m.auth.reason + ']' : ''));
    return m;
  };

  Hub.prototype.grant = function (spec) {
    return this.context.addGrant(spec);
  };

  Hub.prototype.approve = function (id) {
    var req = this.pendingApprovals.find(function (p) { return p.id === id; });
    if (!req || req.state !== 'pending') return null;
    req.state = 'approved'; req.decidedAt = ts();
    var g = this.context.addGrant({
      subjectId: req.actor, objectId: req.sessionId, scope: req.action,
      maxSensitivity: req.sensitivity, grantedBy: 'user'
    });
    this.bus.publish({
      from: 'hub', to: req.actor, kind: MSG_KIND.CTX_GRANT, sessionId: req.sessionId,
      sensitivity: req.sensitivity, payload: { text: '授权已批准：' + req.action, grantId: g.id }
    });
    this.audit({ actor: 'user', action: 'approval.grant', target: req.sessionId, decision: DECISION.ALLOWED, sensitivity: req.sensitivity, detail: req.actor + ' / ' + req.action });
    this.context.sync(req.actor, req.sessionId);
    this.emit('approval', req);
    return req;
  };

  Hub.prototype.reject = function (id) {
    var req = this.pendingApprovals.find(function (p) { return p.id === id; });
    if (!req || req.state !== 'pending') return null;
    req.state = 'rejected'; req.decidedAt = ts();
    this.bus.publish({
      from: 'hub', to: req.actor, kind: MSG_KIND.ERROR, sessionId: req.sessionId,
      sensitivity: req.sensitivity, payload: { text: '授权申请被用户拒绝' }
    });
    this.audit({ actor: 'user', action: 'approval.reject', target: req.sessionId, decision: DECISION.DENIED, sensitivity: req.sensitivity, detail: req.actor + ' / ' + req.action });
    this.log('warn', '权限门禁', '已拒绝 ' + this.nameOf(req.actor) + ' 的上下文访问申请');
    this.emit('approval', req);
    return req;
  };

  /* ---------- Simulation driver ---------- */

  Hub.prototype.start = function () {
    if (this._timer) return;
    this.meta.running = true;
    this.log('success', '系统', '编排引擎已启动 · 仿真模式 · ' + this.agents.size + ' 个 Harness 在线');
    var self = this;
    this._timer = setInterval(function () { self._tick(); }, Math.max(180, this._baseInterval / this.meta.speed));
    this.emit('meta', this.meta);
  };

  Hub.prototype.pause = function () {
    this.meta.running = false;
    if (this._timer) { clearInterval(this._timer); this._timer = null; }
    this.log('warn', '系统', '编排引擎已暂停');
    this.emit('meta', this.meta);
  };

  Hub.prototype.setSpeed = function (s) {
    this.meta.speed = clamp(Number(s) || 1, 0.25, 4);
    if (this._timer) {
      var self = this;
      clearInterval(this._timer);
      this._timer = setInterval(function () { self._tick(); }, Math.max(180, this._baseInterval / this.meta.speed));
    }
    this.emit('meta', this.meta);
  };

  Hub.prototype.stop = function () {
    this.pause();
    if (this._adapters) Object.keys(this._adapters).forEach(function (k) {
      var a = this._adapters[k];
      if (a && a.teardown) a.teardown();
    }, this);
  };

  Hub.prototype._tick = function () {
    var self = this;
    this.meta.tick += 1;
    var now = ts();

    // Expire stale approval requests so the queue stays actionable.
    this.pendingApprovals.forEach(function (p) {
      if (p.state !== 'pending' || now - p.createdAt < 90000) return;
      p.state = 'expired'; p.decidedAt = now;
      self.audit({
        actor: 'hub', action: 'approval.expire', target: p.sessionId,
        decision: DECISION.DENIED, sensitivity: p.sensitivity, detail: p.actor + ' 申请超时'
      });
    });

    // heartbeats + telemetry drift
    this.agents.forEach(function (a) {
      var adapter = (self._adapters || {})[a.id];
      var hb = adapter && adapter.heartbeat ? adapter.heartbeat(a) : { ok: true, load: a.load };
      a.lastHeartbeat = ts();
      if (!a.enabled) return;
      if (a.status === AGENT_STATUS.ERROR) return;
      a.load = round(clamp(hb.load != null ? hb.load : a.load, 0, 1), 2);
      var busy = self.tasks.tasks.some(function (t) { return t.assignedTo === a.id && t.status === STATE.RUNNING; });
      a.status = busy || a.load > 0.62 ? AGENT_STATUS.BUSY : AGENT_STATUS.ONLINE;
      if (a.communication !== 'http' && chance(self.rng, 0.35)) a.stats.messages += 1;
    });

    this.tasks.tick();

    // spontaneous cross-tool collaboration + context access attempts
    if (chance(this.rng, 0.38)) {
      var ids = Array.from(this.agents.keys()).filter(function (id) { return self.agents.get(id).enabled && self.agents.get(id).communication !== 'http'; });
      if (ids.length >= 2) {
        var from = pick(this.rng, ids), to = pick(this.rng, ids.filter(function (x) { return x !== from; }));
        var sens = pick(this.rng, [SENS.PUBLIC, SENS.INTERNAL, SENS.INTERNAL, SENS.CONFIDENTIAL, SENS.SECRET]);
        this.sendMessage({
          from: from, to: this.rng() < 0.18 ? 'broadcast' : to,
          kind: pick(this.rng, [MSG_KIND.COLLAB_ASK, MSG_KIND.COLLAB_REPLY, MSG_KIND.CTX_SYNC, MSG_KIND.CONTROL]),
          sensitivity: sens, text: pick(this.rng, COLLAB_LINES)
        });
      }
    }

    if (chance(this.rng, 0.52) && this.context.sessions.size) {
      var sessions = Array.from(this.context.sessions.values());
      var sess = pick(this.rng, sessions);
      var actors = Array.from(this.agents.keys());
      var actor = pick(this.rng, actors);
      var action = chance(this.rng, 0.72) ? 'read' : 'write';
      if (action === 'read') {
        var r = this.context.sync(actor, sess.id);
        if (r.ok) this._stats.syncs += 1;
      } else {
        // deliberately use a stale base version sometimes to trigger conflict handling
        var stale = chance(this.rng, 0.4) ? Math.max(1, sess.version - 1) : sess.version;
        this.context.write(actor, sess.id, 'sync_note', '来自 ' + this.nameOf(actor) + ' 的增量摘要 @t' + this.meta.tick, stale);
      }
    }

    this.emit('tick', { tick: this.meta.tick, at: ts() });
  };

  /* ---------- Snapshot (REST / persistence) ---------- */

  Hub.prototype.snapshot = function () {
    return {
      meta: Object.assign({}, this.meta, { uptimeMs: ts() - this.meta.startedAt }),
      policy: clone(this.policy),
      stats: Object.assign({}, this._stats, {
        agents: this.agents.size,
        online: Array.from(this.agents.values()).filter(function (a) { return a.enabled && a.status !== 'offline'; }).length,
        tasksTotal: this.tasks.tasks.length,
        tasksActive: this.tasks.tasks.filter(function (t) { return ['running', 'assigned', 'blocked', 'retrying'].indexOf(t.status) >= 0; }).length,
        completed: this.tasks.completed,
        failed: this.tasks.failed,
        retried: this.tasks.retried,
        realTasks: this.tasks.tasks.filter(function (t) { return t.execution.mode === 'real'; }).length,
        realRunning: this.tasks.tasks.filter(function (t) { return t.execution.mode === 'real' && t.status === STATE.RUNNING; }).length,
        realCompleted: this.tasks.tasks.filter(function (t) { return t.execution.mode === 'real' && t.status === STATE.SUCCEEDED; }).length,
        conflicts: this.conflicts.length,
        pendingApprovals: this.pendingApprovals.filter(function (p) { return p.state === 'pending'; }).length,
        auditEntries: this.auditTotal,
        auditRetained: this.auditLog.length,
        chainValid: this.verifyAuditChain().ok
      }),
      agents: Array.from(this.agents.values()).map(clone),
      adapterTypes: Object.keys(HARNESS_TYPES).map(function (k) { return Object.assign({ type: k }, HARNESS_TYPES[k]); }),
      tasks: this.tasks.tasks.map(clone),
      messages: this.bus.messages.slice(-200).map(clone),
      sessions: Array.from(this.context.sessions.values()).map(clone),
      grants: Array.from(this.context.grants.values()).map(clone),
      approvals: this.pendingApprovals.slice(0, 40).map(clone),
      conflicts: this.conflicts.slice(0, 40).map(clone),
      audit: this.auditLog.slice(-300).map(clone),
      logs: this.logs.slice(-200).map(clone)
    };
  };

  /* ---------- Seed data ---------- */

  Hub.prototype._seed = function () {
    var self = this;
    var defs = [
      { id: 'agt_codex_01', name: 'Codex 主工程', type: 'codex', status: AGENT_STATUS.ONLINE, trust: 0.94, load: 0.34 },
      { id: 'agt_claude_01', name: 'Claude Code 架构组', type: 'claude-code', status: AGENT_STATUS.BUSY, trust: 0.96, load: 0.61 },
      { id: 'agt_claude_02', name: 'Claude Code 审阅组', type: 'claude-code', status: AGENT_STATUS.ONLINE, trust: 0.91, load: 0.18 },
      { id: 'agt_deepseek_01', name: 'DeepSeek 推理节点', type: 'deepseek', status: AGENT_STATUS.ONLINE, trust: 0.88, load: 0.42 },
      { id: 'agt_workbuddy_01', name: 'WorkBuddy 编排台', type: 'workbuddy', status: AGENT_STATUS.ONLINE, trust: 0.98, load: 0.27 },
      { id: 'agt_zocde_01', name: 'Zocde 测试节点', type: 'zocde', status: AGENT_STATUS.ONLINE, trust: 0.85, load: 0.12 },
      { id: 'agt_zocde_02', name: 'Zocde 部署节点', type: 'zocde', status: AGENT_STATUS.OFFLINE, trust: 0.8, load: 0 }
    ];
    defs.forEach(function (d) { self.createAgent(d); });
    var offline = this.agents.get('agt_zocde_02');
    if (offline) offline.enabled = false;

    var s1 = this.context.createSession('agt_claude_01', {
      title: '认证模块重构 · 会话上下文', sensitivity: SENS.CONFIDENTIAL,
      entries: [
        { key: 'architecture', value: 'JWT → 短期令牌 + 刷新轮转', sensitivity: SENS.CONFIDENTIAL },
        { key: 'open_questions', value: '多端登出的一致性语义待定', sensitivity: SENS.INTERNAL }
      ]
    });
    var s2 = this.context.createSession('agt_codex_01', {
      title: '支付网关密钥轮换', sensitivity: SENS.SECRET,
      entries: [
        { key: 'kms_arn', value: 'arn:aws:kms:****:key/**** (已脱敏)', sensitivity: SENS.SECRET },
        { key: 'rotation_window', value: '每周日 02:00 UTC', sensitivity: SENS.CONFIDENTIAL },
        { key: 'runbook', value: '参见内部 Runbook PG-14', sensitivity: SENS.INTERNAL }
      ]
    });
    var s3 = this.context.createSession('agt_workbuddy_01', {
      title: '公开 API 变更记录', sensitivity: SENS.PUBLIC,
      entries: [{ key: 'changelog', value: 'v2.4 新增 /v2/batch 端点', sensitivity: SENS.PUBLIC }]
    });
    var s4 = this.context.createSession('agt_deepseek_01', {
      title: '性能基准与推理轨迹', sensitivity: SENS.INTERNAL,
      entries: [{ key: 'p99_baseline', value: '182ms @ 2k rps', sensitivity: SENS.INTERNAL }]
    });

    [
      { subjectId: 'agt_codex_01', objectId: s1.id, scope: 'both', maxSensitivity: SENS.CONFIDENTIAL },
      { subjectId: 'agt_claude_02', objectId: s1.id, scope: 'read', maxSensitivity: SENS.CONFIDENTIAL },
      { subjectId: 'agt_workbuddy_01', objectId: s1.id, scope: 'read', maxSensitivity: SENS.CONFIDENTIAL },
      { subjectId: 'agt_workbuddy_01', objectId: s2.id, scope: 'read', maxSensitivity: SENS.CONFIDENTIAL },
      { subjectId: 'agt_zocde_01', objectId: s3.id, scope: 'read', maxSensitivity: SENS.PUBLIC },
      { subjectId: 'agt_deepseek_01', objectId: s4.id, scope: 'both', maxSensitivity: SENS.INTERNAL },
      { subjectId: 'agt_claude_01', objectId: s4.id, scope: 'read', maxSensitivity: SENS.INTERNAL }
    ].forEach(function (g) { self.context.addGrant(g); });

    // Seed tasks across the whole lifecycle
    var seeds = [
      { title: '重构认证中间件并补充单元测试', priority: 'P1', caps: ['code.write', 'test'], sens: SENS.CONFIDENTIAL, sessionId: s1.id, status: STATE.RUNNING, progress: 62, assignedTo: 'agt_codex_01', collaborative: true, resources: [{ key: 'repo/auth', version: 1 }] },
      { title: '分析订单服务 P99 延迟回归', priority: 'P0', caps: ['analysis', 'reasoning'], sens: SENS.INTERNAL, sessionId: s4.id, status: STATE.RUNNING, progress: 38, assignedTo: 'agt_deepseek_01' },
      { title: '生成 v2.4 接口变更文档', priority: 'P2', caps: ['docs'], sens: SENS.PUBLIC, sessionId: s3.id, status: STATE.SUCCEEDED, progress: 100, assignedTo: 'agt_workbuddy_01' },
      { title: '交叉审阅支付模块 PR #418', priority: 'P1', caps: ['code.review'], sens: SENS.CONFIDENTIAL, status: STATE.RETRYING, progress: 44, assignedTo: 'agt_claude_02' },
      { title: '修复 CI 流水线偶发超时', priority: 'P0', caps: ['debug', 'shell'], sens: SENS.INTERNAL, status: STATE.QUEUED },
      { title: '梳理多租户数据隔离边界', priority: 'P1', caps: ['analysis', 'long-context'], sens: SENS.SECRET, status: STATE.QUEUED }
    ];
    seeds.forEach(function (s) {
      var t = self.tasks.submit({
        title: s.title, priority: s.priority, requiredCapabilities: s.caps,
        sensitivity: s.sens, sessionId: s.sessionId, collaborative: s.collaborative,
        resources: s.resources, assignedTo: s.assignedTo || undefined, requestedBy: 'seed'
      });
      t.status = s.status; t.progress = s.progress == null ? 0 : s.progress;
      if (s.status === STATE.RETRYING) { t.attempts = 1; t.retryAt = ts() + 5000; t.error = '上游超时'; }
      if (s.status === STATE.SUCCEEDED) { t.finishedAt = ts(); t.result = self.tasks.aggregate(t); self.tasks.completed += 1; }
    });

    // A task touching the same resource as a running one -> conflict coordinator kicks in
    self.tasks.submit({
      title: '同步认证模块接口契约文档', priority: 'P2', requiredCapabilities: ['docs'],
      sensitivity: SENS.CONFIDENTIAL, sessionId: s1.id,
      resources: [{ key: 'repo/auth', version: 1 }], requestedBy: 'seed'
    });

    // A stale-version concurrent write -> optimistic concurrency conflict
    self.context.write('agt_codex_01', s1.id, 'auth_flow', '基线：JWT 单一令牌', s1.version);
    self.context.write('agt_claude_01', s1.id, 'auth_flow', '短期令牌 + 刷新轮转（协作修订）', Math.max(1, s1.version - 1));

    // A couple of denied attempts + approval queue to make the audit trail tangible
    this.context.sync('agt_zocde_01', s2.id);
    this.context.sync('agt_claude_02', s2.id);
    this.sendMessage({ from: 'agt_zocde_01', to: 'broadcast', kind: MSG_KIND.CTX_SYNC, sensitivity: SENS.SECRET, text: '尝试广播支付网关密钥材料' });

    this.log('info', '系统', '已加载 7 个 Harness 实例、4 个会话上下文、7 条授权策略');
    this.log('success', '系统', '审计哈希链初始化完成，控制器就绪');
  };

  /* ============================================================
   * 7. Public API
   * ========================================================== */

  return {
    Hub: Hub,
    HarnessAdapter: HarnessAdapter,
    SimulatedAdapter: SimulatedAdapter,
    Emitter: Emitter,
    CONST: {
      SENS: SENS, SENS_LABEL: SENS_LABEL, SENS_KEY: SENS_KEY,
      STATE: STATE, STATE_LABEL: STATE_LABEL,
      AGENT_STATUS: AGENT_STATUS, STATUS_LABEL: STATUS_LABEL,
      MSG_KIND: MSG_KIND, DECISION: DECISION, HARNESS_TYPES: HARNESS_TYPES
    },
    utils: { uid: uid, fnv1a: fnv1a, clone: clone, iso: iso, mulberry32: mulberry32 }
  };
});
