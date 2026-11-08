/* ============================================================
 * Harness Hub - Console controller
 * ------------------------------------------------------------
 * Dual runtime:
 *   - Standalone (file://)  -> in-page engine instance (demo)
 *   - Served by server.js   -> REST snapshots + SSE events (live)
 * Both paths render from the exact same snapshot shape.
 * ============================================================ */
(function () {
  'use strict';

  var Core = window.HarnessHubCore;
  var CONST = Core.CONST;
  var SENS = CONST.SENS, SENS_LABEL = CONST.SENS_LABEL;
  var STATE_LABEL = CONST.STATE_LABEL, STATUS_LABEL = CONST.STATUS_LABEL;
  var MSG_KIND = CONST.MSG_KIND, DECISION = CONST.DECISION;

  var CAPS = ['code.write', 'code.review', 'shell', 'refactor', 'analysis', 'long-context',
    'reasoning', 'math', 'research', 'docs', 'web', 'files', 'orchestration', 'debug', 'test', 'deploy'];

  var ui = {
    page: 'missions',
    agentSearch: '', agentFilter: 'all',
    taskFilter: 'all',
    logFilter: 'all',
    msgFilter: 'all', selectedMsg: null,
    auditFilter: 'all', auditSearch: '',
    selectedSession: null,
    real: null, taskPermission: 'readonly', taskTool: 'auto',
    envelopeSig: '', agentOptSig: '', topoSig: '',
    spark: { tasks: [], msgs: [], audit: [], done: [] }
  };

  var state = { snap: null };
  var LIVE = false, localHub = null, es = null;
  var refreshTimer = null, ready = false;

  /* ============================================================
   * Helpers
   * ========================================================== */
  function $(id) { return document.getElementById(id); }
  function esc(v) {
    return String(v == null ? '' : v).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }
  function pad(n) { return n < 10 ? '0' + n : '' + n; }
  function fmtTime(t) { var d = new Date(t); return pad(d.getHours()) + ':' + pad(d.getMinutes()) + ':' + pad(d.getSeconds()); }
  function fmtAgo(t) {
    var s = Math.max(0, Math.floor((Date.now() - t) / 1000));
    if (s < 60) return s + 's 前';
    if (s < 3600) return Math.floor(s / 60) + 'm 前';
    return Math.floor(s / 3600) + 'h 前';
  }
  function shortId(id) { return String(id || '').slice(-6); }
  function nameOf(snap, id) {
    if (id === 'hub') return '协调中枢';
    if (id === 'user') return '用户';
    if (id === 'scheduler') return '调度器';
    if (id === 'broadcast') return '全体广播';
    var a = (snap.agents || []).find(function (x) { return x.id === id; });
    return a ? a.name : (id || '未知');
  }
  function agentById(snap, id) { return (snap.agents || []).find(function (x) { return x.id === id; }) || null; }
  function sensBadge(n) { return '<span class="sens sens-' + n + '">' + SENS_LABEL[n] + '</span>'; }
  function statusBadge(s) {
    var map = { online: 'badge-ok', busy: 'badge-busy', offline: 'badge-muted', error: 'badge-err', paused: 'badge-muted' };
    return '<span class="badge ' + (map[s] || 'badge-muted') + '">' + (STATUS_LABEL[s] || s) + '</span>';
  }
  function stateBadge(s) {
    var map = {
      queued: 'badge-muted', assigned: 'badge-info', running: 'badge-purple',
      blocked: 'badge-err', retrying: 'badge-busy', succeeded: 'badge-ok',
      failed: 'badge-err', cancelled: 'badge-muted'
    };
    return '<span class="badge ' + (map[s] || 'badge-muted') + '">' + (STATE_LABEL[s] || s) + '</span>';
  }
  function decisionBadge(d) {
    if (d === DECISION.ALLOWED) return '<span class="badge badge-ok">允许</span>';
    if (d === DECISION.DENIED) return '<span class="badge badge-err">拦截</span>';
    return '<span class="badge badge-busy">待审</span>';
  }
  function avatar(a, size) {
    return '<span class="ac-avatar" style="background:' + esc(a.color) + ';width:' + (size || 38) + 'px;height:' + (size || 38) + 'px;font-size:' + ((size || 38) / 2.4) + 'px">' + esc(a.glyph) + '</span>';
  }
  function tinyDot(a) { return '<span class="tc-dot" style="background:' + esc(a.color) + '"></span>'; }
  function toast(msg, type) {
    var el = document.createElement('div');
    el.className = 'toast ' + (type || '');
    el.textContent = msg;
    $('toastStack').appendChild(el);
    setTimeout(function () { el.style.opacity = '0'; el.style.transform = 'translateY(8px)'; el.style.transition = 'all .25s'; }, 2600);
    setTimeout(function () { el.remove(); }, 2950);
  }
  function optionsHtml(list, valueFn, labelFn, selected) {
    return list.map(function (x) {
      var v = valueFn(x);
      return '<option value="' + esc(v) + '"' + (v === selected ? ' selected' : '') + '>' + esc(labelFn(x)) + '</option>';
    }).join('');
  }
  function sparkline(values, color) {
    if (!values.length) return '';
    var w = 68, h = 26, n = values.length;
    var max = Math.max.apply(null, values.concat([1])), min = Math.min.apply(null, values.concat([0]));
    var span = Math.max(1, max - min);
    var pts = values.map(function (v, i) {
      return (i / Math.max(1, n - 1) * w).toFixed(1) + ',' + (h - 2 - (v - min) / span * (h - 5)).toFixed(1);
    }).join(' ');
    return '<svg class="kpi-spark" viewBox="0 0 ' + w + ' ' + h + '"><polyline points="' + pts + '" fill="none" stroke="' + color + '" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>';
  }
  function pushSpark(key, v) {
    var arr = ui.spark[key];
    arr.push(v);
    if (arr.length > 22) arr.shift();
  }

  /* ============================================================
   * Runtime source (demo engine OR live REST + SSE)
   * ========================================================== */
  var ACTIONS = {
    'agent.create': { method: 'POST', path: function (p) { return '/api/agents'; }, demo: function (h, p) { return h.createAgent(p); } },
    'agent.toggle': { method: 'PATCH', path: function (p) { return '/api/agents/' + encodeURIComponent(p.id); }, body: function (p) { return { enabled: p.enabled }; }, demo: function (h, p) { return h.setAgentEnabled(p.id, p.enabled); } },
    'adapter.create': { method: 'POST', path: function () { return '/api/adapters'; }, demo: function (h, p) { return h.registerAdapter(p.type, function (s) { return new Core.SimulatedAdapter(s, h.rng); }, p.descriptor); } },
    'task.create': { method: 'POST', path: function () { return '/api/tasks'; }, demo: function (h, p) { return h.submitTask(p); } },
    'task.retry': { method: 'POST', path: function (p) { return '/api/tasks/' + p.id + '/retry'; }, demo: function (h, p) { return h.retryTask(p.id); } },
    'task.cancel': { method: 'POST', path: function (p) { return '/api/tasks/' + p.id + '/cancel'; }, demo: function (h, p) { return h.cancelTask(p.id); } },
    'message.send': { method: 'POST', path: function () { return '/api/messages'; }, demo: function (h, p) { return h.sendMessage(p); } },
    'grant.create': { method: 'POST', path: function () { return '/api/grants'; }, demo: function (h, p) { return h.grant(p); } },
    'grant.revoke': { method: 'DELETE', path: function (p) { return '/api/grants/' + p.id; }, demo: function (h, p) { return h.context.revokeGrant(p.id); } },
    'approval.approve': { method: 'POST', path: function (p) { return '/api/approvals/' + p.id + '/approve'; }, demo: function (h, p) { return h.approve(p.id); } },
    'approval.reject': { method: 'POST', path: function (p) { return '/api/approvals/' + p.id + '/reject'; }, demo: function (h, p) { return h.reject(p.id); } },
    'policy.patch': { method: 'PATCH', path: function () { return '/api/policy'; }, demo: function (h, p) { return h.setPolicy(p); } },
    'context.sync': { method: 'POST', path: function () { return '/api/context/sync'; }, demo: function (h, p) { return h.context.sync(p.actor, p.sessionId); } },
    'control': { method: 'POST', path: function () { return '/api/control'; }, demo: function (h, p) {
      if (p.action === 'pause') h.pause(); else if (p.action === 'start') h.start(); else if (p.action === 'speed') h.setSpeed(p.speed);
      return h.meta;
    } },
    'real.config': {
      method: 'POST', path: function () { return '/api/real/config'; },
      demo: function () { throw new Error('真实执行配置需要启动服务端'); }
    }
  };

  function act(name, payload) {
    var a = ACTIONS[name];
    if (!a) return Promise.reject(new Error('未知操作 ' + name));
    if (!LIVE) {
      try { var r = a.demo(localHub, payload || {}); refresh(); return Promise.resolve(r); }
      catch (e) { toast(e.message, 'err'); return Promise.reject(e); }
    }
    return fetch(a.path(payload || {}), {
      method: a.method,
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(a.body ? a.body(payload || {}) : (payload || {}))
    }).then(function (r) { return r.json(); }).then(function (j) {
      if (!j.ok) throw new Error(j.error || '请求失败');
      refresh();
      return j.data;
    }).catch(function (e) { toast(e.message, 'err'); });
  }

  function scheduleRefresh() {
    if (refreshTimer) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(refresh, 170);
  }

  function refresh() {
    if (LIVE) {
      fetch('/api/snapshot').then(function (r) { return r.json(); }).then(function (j) {
        if (j.ok) { state.snap = j.data; renderAll(); }
      }).catch(function () { /* offline: keep last state */ });
    } else {
      state.snap = localHub.snapshot();
      renderAll();
    }
  }

  function loadRealStatus() {
    if (!LIVE) return Promise.resolve(null);
    return fetch('/api/real/status').then(function (r) { return r.json(); }).then(function (j) {
      if (j.ok) ui.real = j.data;
      return ui.real;
    }).catch(function () { return null; });
  }

  function initSource() {
    var isHttp = /^https?:$/.test(location.protocol);
    var tryLive = function () {
      if (!isHttp) return Promise.resolve(false);
      return fetch('/api/health').then(function (r) { return r.ok; }).catch(function () { return false; });
    };
    tryLive().then(function (live) {
      LIVE = live;
      if (LIVE) {
        var types = ['tick', 'log', 'message', 'task', 'task.retry', 'agent', 'audit', 'grant',
          'approval', 'conflict', 'context.session', 'context.write', 'adapter', 'policy', 'meta', 'snapshot'];
        es = new EventSource('/api/events');
        types.forEach(function (t) { es.addEventListener(t, scheduleRefresh); });
        es.onerror = function () { /* reconnect handled by browser */ };
      } else {
        throw new Error('请先运行 npm start，再从 http://127.0.0.1:8787 打开。当前页面不会生成演示数据。');
      }
      ready = true;
      return loadRealStatus();
    }).then(function () {
      buildStaticUI();
      refresh();
      if (LIVE) setInterval(loadRealStatus, 15000);
    }).catch(function (e) { $('engineLabel').textContent = '服务未连接'; toast(e.message, 'err'); });
  }

  /* ============================================================
   * Navigation
   * ========================================================== */
  var PAGE_META = {
    missions: ['任务指挥台', '下达目标 → 宰相拆解 → Harness 协作执行 → 汇总汇报'],
    overview: ['总览', '全局运行态势与实时协作'],
    agents: ['智能体', '集中注册、能力画像与运行状态'],
    tasks: ['任务流', '统一派发、冲突协调、失败重试与结果汇总'],
    context: ['上下文与权限', '会话隔离、授权审批与密级管控'],
    messages: ['协作消息', '标准化信封协议与跨工具通信记录'],
    audit: ['访问审计', '防篡改哈希链与全量访问留痕'],
    integrate: ['接入与扩展', '适配器契约、通信协议与对外接口']
  };
  function goto(page) {
    if (page === 'missions') { document.body.classList.add('chat-mode'); return; }
    ui.page = page;
    Array.prototype.forEach.call(document.querySelectorAll('.nav-item'), function (b) {
      b.classList.toggle('active', b.dataset.page === page);
    });
    Array.prototype.forEach.call(document.querySelectorAll('.page'), function (s) {
      s.classList.toggle('active', s.id === 'page-' + page);
    });
    var m = PAGE_META[page] || ['', ''];
    $('pageTitle').textContent = m[0];
    $('pageSub').textContent = m[1];
    if (state.snap) renderAll();
  }

  /* ============================================================
   * Global chrome
   * ========================================================== */
  function renderChrome() {
    var s = state.snap;
    $('navAgentCount').textContent = s.agents.length;
    $('navTaskCount').textContent = s.stats.tasksActive;
    var pending = s.approvals.filter(function (a) { return a.state === 'pending'; }).length;
    var pe = $('navApprovalCount');
    pe.textContent = pending;
    pe.style.display = pending ? '' : 'none';
    $('engineDot').className = 'dot ' + (s.meta.running ? 'dot-ok' : 'dot-off');
    $('engineLabel').textContent = '真实任务服务已连接';
    $('engineTick').textContent = '已连接 ' + s.agents.length + ' 个真实执行端';
    $('chainMini').textContent = s.stats.chainValid ? '有效' : '异常';
    $('chainMini').style.color = s.stats.chainValid ? '#6fd3a5' : '#ff8a9b';
    var pauseBtn = $('pauseBtn');
    if (pauseBtn) pauseBtn.innerHTML = s.meta.running ? '<span class="ico-pause"></span> 暂停' : '▶ 继续';
  }

  /* ============================================================
   * Overview
   * ========================================================== */
  function renderOverview() {
    var s = state.snap;
    pushSpark('tasks', s.stats.tasksActive);
    pushSpark('msgs', s.messages.length);
    pushSpark('audit', s.stats.auditEntries);
    pushSpark('done', s.stats.completed);

    var delivered = s.messages.filter(function (m) { return m.status === 'delivered'; }).length;
    var rate = s.messages.length ? Math.round(delivered / s.messages.length * 100) : 100;
    var kpis = [
      { label: '在线 Harness', value: s.stats.online + ' / ' + s.stats.agents, foot: '适配器类型 ' + s.adapterTypes.length + ' 种', color: '#5554d6', bg: '#eef0ff', icon: '⬡', spark: null },
      { label: '进行中任务', value: s.stats.tasksActive, foot: '累计完成 ' + s.stats.completed + ' · 失败 ' + s.stats.failed, color: '#7c3aed', bg: '#f2ecff', icon: '⇄', spark: ui.spark.tasks },
      { label: '消息投递', value: s.messages.length, foot: '通过率 ' + rate + '% · 拦截 ' + s.stats.denied + ' 次', color: '#2f6bdd', bg: '#eaf1ff', icon: '✉', spark: ui.spark.msgs },
      { label: '待授权申请', value: s.stats.pendingApprovals, foot: '冲突已协调 ' + s.stats.conflicts + ' 次', color: '#c47a12', bg: '#fff3e2', icon: '⚿', spark: null },
      { label: '审计记录', value: s.stats.auditEntries, foot: s.stats.chainValid ? '哈希链完整 · 防篡改' : '哈希链异常', color: '#12996b', bg: '#e6f7f0', icon: '◈', spark: ui.spark.audit }
    ];
    $('kpiGrid').innerHTML = kpis.map(function (k) {
      return '<div class="kpi" style="--kc:' + k.color + ';--kbg:' + k.bg + '">' +
        '<div class="kpi-top"><span class="kpi-label">' + k.label + '</span><span class="kpi-icon">' + k.icon + '</span></div>' +
        '<div class="kpi-value">' + k.value + '</div>' +
        '<div class="kpi-foot">' + esc(k.foot) + '</div>' +
        (k.spark ? sparkline(k.spark, k.color) : '') + '</div>';
    }).join('');

    renderTopology();
    renderOverviewTasks();
  }

  function renderTopology() {
    var s = state.snap;
    var agents = s.agents;
    var sig = agents.map(function (a) { return a.id + a.color; }).join('|');
    if (ui.topoSig !== sig) { ui.topoSig = sig; buildTopology(agents); }

    var cutoff = Date.now() - 7000;
    var recent = s.messages.filter(function (m) { return m.at >= cutoff && m.status === 'delivered'; });
    var hot = {};
    recent.forEach(function (m) { hot[m.from] = 1; hot[m.to] = 1; });
    Array.prototype.forEach.call($('topology').querySelectorAll('.topo-g'), function (g) {
      g.classList.toggle('hot', !!hot[g.dataset.id]);
    });
    $('topoMsgCount').textContent = s.messages.length + ' 条消息 · ' + recent.length + ' 条活跃';
    $('topoLegend').innerHTML = agents.map(function (a) {
      return '<span><i style="background:' + esc(a.color) + '"></i>' + esc(a.name) + '</span>';
    }).join('') + '<span><i style="background:#c9cbe6"></i>协调中枢</span>';
  }

  function buildTopology(agents) {
    var W = 500, H = 268, cx = W / 2, cy = H / 2 - 4;
    var n = Math.max(1, agents.length);
    var rx = 178, ry = 98;
    var out = '<svg viewBox="0 0 ' + W + ' ' + H + '" preserveAspectRatio="xMidYMid meet">';
    agents.forEach(function (a, i) {
      var ang = -Math.PI / 2 + (i / n) * Math.PI * 2;
      var x = cx + Math.cos(ang) * rx, y = cy + Math.sin(ang) * ry;
      var d = 'M ' + cx + ' ' + cy + ' Q ' + ((cx + x) / 2 + (y - cy) * 0.12) + ' ' + ((cy + y) / 2 - (x - cx) * 0.12) + ' ' + x + ' ' + y;
      out += '<g class="topo-g" data-id="' + esc(a.id) + '">' +
        '<path class="topo-link" d="' + d + '"/>' +
        '<circle class="topo-packet" r="2.6"><animateMotion dur="' + (1.7 + (i % 3) * 0.35) + 's" repeatCount="indefinite" path="' + d + '"/></circle>' +
        '<circle class="topo-node-ring" cx="' + x + '" cy="' + y + '" r="22"/>' +
        '<g class="topo-node"><circle cx="' + x + '" cy="' + y + '" r="15" fill="' + esc(a.color) + '"/>' +
        '<text x="' + x + '" y="' + y + '">' + esc(a.glyph) + '</text></g>' +
        '<text class="topo-label" x="' + x + '" y="' + (y + 29) + '">' + esc(a.name.length > 12 ? a.name.slice(0, 11) + '…' : a.name) + '</text>' +
        '</g>';
    });
    out += '<g><circle class="topo-hub" cx="' + cx + '" cy="' + cy + '" r="27"/>' +
      '<text x="' + cx + '" y="' + (cy - 3) + '" style="font-size:9.5px;fill:#5554d6;font-weight:700;text-anchor:middle">HUB</text>' +
      '<text x="' + cx + '" y="' + (cy + 10) + '" style="font-size:8px;fill:#8b90ad;text-anchor:middle">协调中枢</text></g>';
    out += '</svg>';
    $('topology').innerHTML = out;
  }

  function renderLiveLog() {
    var list = state.snap.logs.slice(-70);
    if (ui.logFilter !== 'all') {
      list = list.filter(function (l) {
        return ui.logFilter === 'error' ? (l.level === 'error') : (l.level === 'warn' || l.level === 'error');
      });
    }
    $('liveLog').innerHTML = list.slice().reverse().map(function (l) {
      return '<div class="log-line lv-' + l.level + '"><span class="log-time">' + fmtTime(l.at) + '</span>' +
        '<span class="log-src">' + esc(l.source) + '</span><span class="log-text">' + esc(l.text) + '</span></div>';
    }).join('') || '<div class="empty-state">暂无匹配事件</div>';
  }

  function renderLoads() {
    var agents = state.snap.agents;
    $('loadBars').innerHTML = agents.map(function (a) {
      var pct = Math.round((a.load || 0) * 100);
      return '<div class="load-row"><span class="load-name">' + tinyDot(a) + esc(a.name) + '</span>' +
        '<span class="bar' + (pct > 70 ? ' hot' : '') + '"><i style="width:' + pct + '%"></i></span>' +
        '<span class="load-num">' + pct + '%</span></div>';
    }).join('');
  }

  function renderOverviewTasks() {
    var s = state.snap;
    var active = s.tasks.filter(function (t) {
      return ['running', 'assigned', 'blocked', 'retrying', 'queued'].indexOf(t.status) >= 0;
    }).slice(0, 8);
    $('overviewTasks').innerHTML = active.map(function (t) {
      var a = agentById(s, t.assignedTo);
      return '<div class="mini-task"><div class="mt-main"><div class="mt-title">' + esc(t.title) + '</div>' +
        '<div class="mt-meta">' + (STATE_LABEL[t.status] || t.status) + ' · ' + (a ? esc(a.name) : '待调度') +
        (t.collaborators.length ? ' · ' + t.collaborators.length + ' 个协作方' : '') + '</div></div>' +
        '<div class="mt-prog bar"><i style="width:' + t.progress + '%"></i></div>' +
        '<span class="mt-pct">' + Math.round(t.progress) + '%</span></div>';
    }).join('') || '<div class="empty-state">当前没有进行中的任务</div>';
  }

  /* ============================================================
   * Agents
   * ========================================================== */
  function renderAgents() {
    var s = state.snap;
    var q = ui.agentSearch.toLowerCase();
    var list = s.agents.filter(function (a) {
      if (ui.agentFilter !== 'all' && a.status !== ui.agentFilter) return false;
      if (!q) return true;
      return (a.name + ' ' + a.label + ' ' + a.type + ' ' + a.capabilities.join(' ')).toLowerCase().indexOf(q) >= 0;
    });
    $('agentSummary').textContent = '共 ' + s.agents.length + ' 个实例 · ' + s.adapterTypes.length + ' 类适配器 · 显示 ' + list.length + ' 个';
    $('agentGrid').innerHTML = list.map(function (a) {
      var active = s.tasks.filter(function (t) { return t.assignedTo === a.id && ['running', 'assigned'].indexOf(t.status) >= 0; }).length;
      return '<div class="agent-card' + (!a.enabled ? ' is-off' : '') + '" style="--ac:' + esc(a.color) + '">' +
        '<div class="ac-head">' + avatar(a) + '<div><div class="ac-name">' + esc(a.name) + '</div>' +
        '<div class="ac-type">' + esc(a.label) + ' · ' + esc(a.vendor) + ' · ' + esc(a.transport) + '</div></div>' + statusBadge(a.status) + '</div>' +
        '<div class="ac-meta"><div><small>当前并发</small><strong>' + active + '/' + a.maxConcurrency + '</strong></div>' +
        '<div><small>完成任务</small><strong>' + a.stats.completed + '</strong></div>' +
        '<div><small>可信度</small><strong>' + Math.round(a.trust * 100) + '%</strong></div></div>' +
        '<div class="ac-caps">' + a.capabilities.map(function (c) { return '<span class="chip chip-cap">' + esc(c) + '</span>'; }).join('') + '</div>' +
        '<div class="ac-endpoint">' + esc(a.endpoint) + '</div>' +
        '<div class="ac-foot"><span class="tag tag-real">' + (a.communication === 'http' ? 'HTTP 桥接' : '真实 CLI') + '</span>' +
        sensBadge(a.clearance) + '<span class="chip">密级上限</span><span class="spacer"></span>' +
        '<button class="btn btn-ghost btn-sm" data-act="agent-detail" data-id="' + esc(a.id) + '">详情</button>' +
        '<button class="switch' + (a.enabled ? ' on' : '') + '" data-act="agent-toggle" data-id="' + esc(a.id) + '" data-enabled="' + (a.enabled ? '1' : '0') + '" title="启用 / 停用"></button></div></div>';
    }).join('') || '<div class="empty-state">没有匹配的智能体</div>';
  }

  /* ============================================================
   * Tasks board
   * ========================================================== */
  var COLUMNS = [
    { key: 'queue', label: '排队 / 已派发', states: ['queued', 'assigned'] },
    { key: 'run', label: '执行中', states: ['running'] },
    { key: 'issue', label: '冲突 / 重试', states: ['blocked', 'retrying'] },
    { key: 'done', label: '已完成', states: ['succeeded'] },
    { key: 'end', label: '失败 / 取消', states: ['failed', 'cancelled'] }
  ];
  function renderTasks() {
    var s = state.snap;
    var all = s.tasks;
    var list = all.filter(function (t) {
      var f = ui.taskFilter;
      if (f === 'all') return true;
      if (f === 'active') return ['queued', 'assigned', 'running'].indexOf(t.status) >= 0;
      if (f === 'problem') return ['blocked', 'retrying', 'failed'].indexOf(t.status) >= 0;
      if (f === 'real') return !!(t.execution && t.execution.mode === 'real');
      return ['succeeded', 'failed', 'cancelled'].indexOf(t.status) >= 0;
    });
    $('taskSummary').textContent = '共 ' + all.length + ' 个任务 · 完成 ' + s.stats.completed + ' · 重试 ' + s.stats.retried + ' 次 · 冲突 ' + s.stats.conflicts + ' 次';
    $('taskBoard').innerHTML = COLUMNS.map(function (c) {
      var items = list.filter(function (t) { return c.states.indexOf(t.status) >= 0; });
      return '<div class="board-col"><div class="bc-head"><span>' + c.label + '</span><span class="bc-count">' + items.length + '</span></div>' +
        items.map(function (t) {
          var a = agentById(s, t.assignedTo);
          return '<div class="task-card" data-act="task-detail" data-id="' + esc(t.id) + '">' +
            '<div class="tc-top"><span class="prio prio-' + t.priority + '">' + t.priority + '</span>' + sensBadge(t.sensitivity) +
            (t.execution && t.execution.mode === 'real' ? '<span class="tag tag-real">真实</span>' : '') +
            '<span class="tc-id">#' + shortId(t.id) + '</span></div>' +
            '<div class="tc-title">' + esc(t.title) + '</div>' +
            '<div class="bar tc-bar"><i style="width:' + t.progress + '%"></i></div>' +
            '<div class="tc-foot"><span class="tc-agent">' + (a ? tinyDot(a) + esc(a.name) : '待调度') + '</span><span class="spacer"></span>' +
            (t.attempts ? '<span class="tc-tag">重试 ' + t.attempts + '</span>' : '') +
            (t.collaborators.length ? '<span class="tc-tag">协作 ' + t.collaborators.length + '</span>' : '') +
            '<span>' + Math.round(t.progress) + '%</span></div></div>';
        }).join('') + (items.length ? '' : '<div class="empty-state" style="padding:18px 6px;font-size:11px">暂无任务</div>') + '</div>';
    }).join('');
  }

  /* ============================================================
   * Context & permissions
   * ========================================================== */
  function renderContext() {
    var s = state.snap;
    renderApprovals();
    renderSessions();
    renderSessionDetail();
    renderMatrix();
    renderGrantList();
    renderPolicy();
  }

  function renderApprovals() {
    var s = state.snap;
    var pending = s.approvals.filter(function (a) { return a.state === 'pending'; });
    var card = $('approvalCard');
    card.classList.toggle('clear', pending.length === 0);
    card.querySelector('strong').textContent = pending.length ? '待审批的上下文访问申请（' + pending.length + '）' : '暂无待审批申请';
    card.querySelector('p').textContent = pending.length
      ? '外部 Harness 请求访问会话上下文，需由你逐项授权。默认拒绝所有未授权访问。'
      : '所有访问请求均已处理。未授权访问会被拦截并写入审计链。';
    $('approvalList').innerHTML = pending.slice(0, 6).map(function (a) {
      return '<div class="approval-row"><div class="ar-main"><strong>' + esc(nameOf(s, a.actor)) + '</strong> 申请' +
        (a.action === 'write' ? '写入' : '读取') + '「' + esc(a.sessionTitle) + '」 ' + sensBadge(a.sensitivity) +
        '<small>' + fmtAgo(a.createdAt) + ' · 默认拒绝，等待用户决策 · ' + shortId(a.id) + '</small></div>' +
        '<button class="btn btn-primary btn-sm" data-act="approval-approve" data-id="' + esc(a.id) + '">批准</button>' +
        '<button class="btn btn-ghost btn-sm" data-act="approval-reject" data-id="' + esc(a.id) + '">拒绝</button></div>';
    }).join('') || '';
  }

  function renderSessions() {
    var s = state.snap;
    if (!ui.selectedSession && s.sessions.length) ui.selectedSession = s.sessions[0].id;
    $('sessionCount').textContent = s.sessions.length + ' 个会话';
    $('sessionList').innerHTML = s.sessions.map(function (x) {
      return '<div class="session-row' + (ui.selectedSession === x.id ? ' sel' : '') + '" data-act="session-select" data-id="' + esc(x.id) + '">' +
        '<div class="sr-top"><span class="sr-title">' + esc(x.title) + '</span>' + sensBadge(x.sensitivity) + '</div>' +
        '<div class="sr-meta">v' + x.version + ' · ' + x.entries.length + ' 条目 · 所有者 ' + esc(nameOf(s, x.ownerId)) + ' · ' + fmtAgo(x.updatedAt) + '</div></div>';
    }).join('');
  }

  function renderSessionDetail() {
    var s = state.snap;
    var x = s.sessions.find(function (v) { return v.id === ui.selectedSession; });
    if (!x) { $('sessionDetail').innerHTML = '<div class="empty-state">← 从左侧选择会话</div>'; return; }
    $('sessionDetailSub').textContent = x.title + ' · v' + x.version;
    var html = '<div class="sd-head"><strong style="font-size:13px">' + esc(x.title) + '</strong>' + sensBadge(x.sensitivity) +
      '<span class="spacer" style="flex:1"></span><span class="chip">ID ' + shortId(x.id) + '</span></div>' +
      '<div class="sd-stats"><div><strong>' + x.version + '</strong>版本号</div><div><strong>' + x.entries.length + '</strong>上下文条目</div>' +
      '<div><strong>' + esc(nameOf(s, x.ownerId)) + '</strong>所有者</div></div>' +
      '<div style="font-size:11px;color:var(--text-3);margin-bottom:8px">上下文条目（按密级标注，读取时自动脱敏）</div>' +
      x.entries.map(function (e) {
        return '<div class="entry-row"><span class="entry-key">' + esc(e.key) + '</span>' +
          '<span class="entry-val">' + esc(e.value) + '<span class="entry-author">by ' + esc(nameOf(s, e.author)) + ' · v' + e.version + ' · ' + fmtTime(e.at) + '</span></span>' +
          sensBadge(e.sensitivity) + '</div>';
      }).join('');
    if (x.sensitivity === SENS.SECRET) {
      html += '<div class="isolation-note">硬隔离：绝密会话不接受通配授权，必须逐会话显式授予；读取时密级高于请求方上限的条目会被自动过滤。</div>';
    }
    var prevActor = $('syncActor') ? $('syncActor').value : null;
    html += '<div class="compose-row" style="margin-top:14px"><label>以该身份读取<select class="input" id="syncActor">' +
      optionsHtml(s.agents, function (a) { return a.id; }, function (a) { return a.name; }, prevActor || (s.agents[0] && s.agents[0].id)) +
      '</select></label><button class="btn btn-ghost" data-act="context-sync" data-id="' + esc(x.id) + '">测试上下文同步</button></div>';
    $('sessionDetail').innerHTML = html;
  }

  function renderMatrix() {
    var s = state.snap;
    var head = '<thead><tr><th>智能体 ＼ 会话</th>' + s.sessions.map(function (x) {
      return '<th title="' + esc(x.title) + '">' + esc(x.title.length > 9 ? x.title.slice(0, 8) + '…' : x.title) + '<br>' + sensBadge(x.sensitivity) + '</th>';
    }).join('') + '</tr></thead>';
    var rows = s.agents.map(function (a) {
      return '<tr><td><span class="row-agent">' + tinyDot(a) + esc(a.name) + '</span></td>' +
        s.sessions.map(function (x) {
          var g = s.grants.find(function (v) { return !v.revoked && v.subjectId === a.id && v.objectId === x.id; });
          if (a.id === x.ownerId) return '<td><span class="mx-btn both" style="cursor:default">所有者</span></td>';
          if (g) return '<td><button class="mx-btn ' + g.scope + '" data-act="grant-revoke" data-id="' + esc(g.id) + '" title="点击撤销">' +
            ({ read: '只读', write: '只写', both: '读写' }[g.scope] || g.scope) + '</button></td>';
          return '<td><button class="mx-btn" data-act="grant-new" data-subject="' + esc(a.id) + '" data-object="' + esc(x.id) + '" data-sens="' + x.sensitivity + '">+ 授权</button></td>';
        }).join('') + '</tr>';
    }).join('');
    $('grantMatrix').innerHTML = head + '<tbody>' + rows + '</tbody>';
  }

  function renderGrantList() {
    var s = state.snap;
    $('grantList').innerHTML = s.grants.slice().reverse().slice(0, 40).map(function (g) {
      var objName = g.objectId === '*' ? '全部会话（通配）' : (function () {
        var x = s.sessions.find(function (v) { return v.id === g.objectId; });
        return x ? x.title : shortId(g.objectId);
      })();
      return '<div class="grant-row' + (g.revoked ? ' revoked' : '') + '"><div class="gr-main"><strong>' + esc(nameOf(s, g.subjectId)) + '</strong> → ' + esc(objName) +
        '<small>' + esc(g.scope) + ' · ' + SENS_LABEL[g.maxSensitivity] + ' · 由 ' + esc(g.grantedBy) + ' 授予 · ' + fmtAgo(g.createdAt) + '</small></div>' +
        sensBadge(g.maxSensitivity) + (g.revoked ? '<span class="badge badge-muted">已撤销</span>' :
          '<button class="btn btn-ghost btn-sm" data-act="grant-revoke" data-id="' + esc(g.id) + '">撤销</button>') + '</div>';
    }).join('') || '<div class="empty-state">尚无授权记录</div>';
  }

  function renderPolicy() {
    var p = state.snap.policy;
    var items = [
      { key: 'defaultDeny', name: '默认拒绝', desc: '无显式授权的任何上下文访问一律拦截' },
      { key: 'allowSecretBroadcast', name: '允许绝密广播', desc: '关闭时，绝密级消息禁止向全体广播' },
      { key: 'requireApprovalForSecret', name: '绝密访问需审批', desc: '绝密会话访问请求进入人工审批队列' }
    ];
    $('policyPanel').innerHTML = items.map(function (i) {
      return '<div class="policy-row"><div class="pr-main"><strong>' + i.name + '</strong><small>' + i.desc + '</small></div>' +
        '<button class="switch' + (p[i.key] ? ' on' : '') + '" data-act="policy-toggle" data-key="' + i.key + '" data-val="' + (p[i.key] ? '1' : '0') + '"></button></div>';
    }).join('') + '<div class="policy-row"><div class="pr-main"><strong>自动派发密级上限</strong><small>超过该密级的任务需人工确认</small></div>' +
      sensBadge(p.maxAutoSensitivity) + '</div>' +
      '<div class="policy-row"><div class="pr-main"><strong>审计保留条数</strong><small>哈希链滚动上限</small></div><span class="mono">' + p.auditRetention + '</span></div>';
  }

  /* ============================================================
   * Messages
   * ========================================================== */
  function refreshAgentOptions() {
    var s = state.snap;
    var sig = s.agents.map(function (a) { return a.id; }).join('|');
    if (sig === ui.agentOptSig) return;
    ui.agentOptSig = sig;
    $('msgFrom').innerHTML = '<option value="hub" selected>用户 / 协调中枢</option>' + optionsHtml(s.agents, function (a) { return a.id; }, function (a) { return a.name; });
    $('msgTo').innerHTML = '<option value="broadcast">全体广播</option>' +
      optionsHtml(s.agents, function (a) { return a.id; }, function (a) { return a.name + (a.communication === 'http' ? ' · HTTP 已连接' : a.real ? ' · CLI' : ' · 仿真'); }, s.agents[1] && s.agents[1].id);
    if (!$('msgKind').options.length) {
      var kinds = Object.keys(MSG_KIND).map(function (k) { return MSG_KIND[k]; });
      $('msgKind').innerHTML = optionsHtml(kinds, function (k) { return k; }, function (k) { return k; }, MSG_KIND.COLLAB_ASK);
      $('msgSens').innerHTML = optionsHtml([0, 1, 2, 3], function (n) { return String(n); }, function (n) { return SENS_LABEL[n]; }, '1');
    }
  }

  function renderMessages() {
    var s = state.snap;
    refreshAgentOptions();
    var list = s.messages.slice().reverse();
    var f = ui.msgFilter;
    if (f === 'collab') list = list.filter(function (m) { return m.kind.indexOf('collab.') === 0 || m.kind.indexOf('ctx.') === 0; });
    else if (f === 'task') list = list.filter(function (m) { return m.kind.indexOf('task.') === 0 || m.kind === 'conflict.alert'; });
    else if (f === 'denied') list = list.filter(function (m) { return m.status === 'rejected'; });
    $('messageFeed').innerHTML = list.slice(0, 120).map(function (m) {
      var denied = m.status === 'rejected';
      return '<div class="msg-row' + (denied ? ' denied' : '') + (ui.selectedMsg === m.id ? ' sel' : '') + '" data-act="msg-select" data-id="' + esc(m.id) + '">' +
        '<div class="mr-top"><span class="mr-route">' + esc(nameOf(s, m.from)) + ' → ' + esc(nameOf(s, m.to)) + '</span>' +
        '<span class="mr-kind">' + esc(m.kind) + '</span>' + sensBadge(m.scope.sensitivity) +
        '<span class="spacer" style="flex:1"></span>' + (m.delivery ? '<span class="badge">' + esc({ sending: '投递中', delivered: 'Harness 已接收', failed: '投递失败' }[m.delivery.status] || m.delivery.status) + '</span>' : '') + (denied ? '<span class="badge badge-err">已拦截</span>' : '') + fmtAgo(m.at) + '</div>' +
        '<div class="mr-text">' + esc((m.payload && (m.payload.text || m.payload.summary)) || JSON.stringify(m.payload)) + '</div>' +
        (denied ? '<div class="mr-reason">拦截原因：' + esc(m.auth.reason) + '</div>' : '') +
        (m.delivery && m.delivery.error ? '<div class="mr-reason">' + esc(m.delivery.error) + '</div>' : '') + '</div>';
    }).join('') || '<div class="empty-state">暂无匹配消息</div>';
    renderEnvelope();
  }

  function renderEnvelope() {
    var s = state.snap;
    var m = s.messages.find(function (x) { return x.id === ui.selectedMsg; });
    if (!m) {
      $('envelopeView').innerHTML = '<div class="empty-state">点击左侧任意消息查看完整信封与鉴权结果</div>';
      return;
    }
    var pretty = JSON.stringify(m, null, 2);
    var html = esc(pretty)
      .replace(/&quot;([^&]+?)&quot;(\s*:)/g, '<span class="ev-k">&quot;$1&quot;</span>$2')
      .replace(/: &quot;([^&]*?)&quot;/g, ': <span class="ev-s">&quot;$1&quot;</span>');
    $('envelopeView').innerHTML = html;
  }

  /* ============================================================
   * Audit
   * ========================================================== */
  function renderAudit() {
    var s = state.snap;
    var chain = s.stats.chainValid;
    $('chainBanner').className = 'chain-banner' + (chain ? '' : ' bad');
    $('chainBanner').innerHTML = '<span class="dot ' + (chain ? 'dot-ok' : '') + '" style="' + (chain ? '' : 'background:#dc3d5b') + '"></span>' +
      '<strong>' + (chain ? '审计链完整' : '审计链校验失败') + '</strong>' +
      '<span>累计 ' + s.stats.auditEntries + ' 条 · 窗口保留 ' + s.stats.auditRetained + ' 条，逐条哈希串联（prevHash → hash）；滚动截断后以链头锚点续接，篡改任意一条都会导致校验失败</span>' +
      '<span class="spacer"></span><span class="mono">拦截 ' + s.stats.denied + ' 次 · 待审 ' + s.stats.pendingApprovals + ' 项</span>';

    var q = ui.auditSearch.toLowerCase();
    var rows = s.audit.slice().reverse().filter(function (r) {
      if (ui.auditFilter !== 'all' && r.decision !== ui.auditFilter) return false;
      if (!q) return true;
      return (r.actor + r.action + r.target + r.detail).toLowerCase().indexOf(q) >= 0;
    }).slice(0, 160);
    $('auditTable').innerHTML = '<thead><tr><th>#</th><th>时间</th><th>操作者</th><th>动作</th><th>对象</th><th>判定</th><th>密级</th><th>详情</th><th>哈希</th></tr></thead><tbody>' +
      rows.map(function (r) {
        return '<tr><td class="mono">' + r.seq + '</td><td class="mono">' + fmtTime(r.at) + '</td>' +
          '<td>' + esc(nameOf(s, r.actor)) + '</td><td class="mono">' + esc(r.action) + '</td>' +
          '<td class="mono">' + esc(r.target.length > 14 ? r.target.slice(0, 13) + '…' : r.target) + '</td>' +
          '<td>' + decisionBadge(r.decision) + '</td><td>' + sensBadge(r.sensitivity) + '</td>' +
          '<td>' + esc(r.detail.length > 46 ? r.detail.slice(0, 45) + '…' : r.detail) + '</td>' +
          '<td class="mono" style="color:#a2a8bd">' + esc(r.hash) + '</td></tr>';
      }).join('') + '</tbody>';
  }

  /* ============================================================
   * Integrate (mostly static)
   * ========================================================== */
  var ARCH = [
    ['01', '适配器层 Adapters', '统一 heartbeat / dispatch / send / teardown 契约，隔离各 Harness 的传输差异', 'core/engine.js · adapters/'],
    ['02', '注册中心 Registry', '实例注册、能力画像、心跳负载、可信度与密级上限管理', 'hub.createAgent()'],
    ['03', '消息总线 MessageBus', '标准信封 v1.0，发布鉴权、广播管控、拒绝原因回执', 'bus.publish()'],
    ['04', '上下文存储 ContextStore', '会话隔离、ACL 授权、读取脱敏、乐观锁冲突合并', 'context.read/write/sync()'],
    ['05', '任务引擎 TaskEngine', '能力调度、协作分派、冲突排队、指数退避重试、故障转移、结果汇总', 'tasks.submit/tick()'],
    ['06', '审计与授权 AuditChain', '哈希链留痕、审批队列、用户可撤销授权与全局策略', 'hub.audit/approve/grant()'],
    ['07', '接入面 REST + SSE', '对真实 Harness 暴露 HTTP 接口与实时事件订阅', 'server.js']
  ];
  var APIS = [
    ['GET', '/api/snapshot', '全量状态快照'],
    ['GET', '/api/events', 'SSE 实时事件流'],
    ['GET', '/api/agents', '实例清单'],
    ['POST', '/api/agents', '注册新 Harness'],
    ['PATCH', '/api/agents/:id', '启用 / 停用'],
    ['POST', '/api/adapters', '注册适配器类型'],
    ['GET', '/api/tasks', '任务列表'],
    ['POST', '/api/tasks', '派发任务（real:true 走真实 CLI）'],
    ['GET', '/api/real/status', 'CLI 可用性 / 权限模式 / 配置'],
    ['POST', '/api/real/config', '切换权限 / 工作目录 / 工具'],
    ['GET', '/api/real/queue', '真实执行队列'],
    ['POST', '/api/tasks/:id/retry', '手动重试'],
    ['POST', '/api/tasks/:id/cancel', '取消任务'],
    ['POST', '/api/messages', '发送标准信封'],
    ['GET', '/api/context/sessions', '会话清单'],
    ['POST', '/api/context/sync', '上下文同步（脱敏）'],
    ['POST', '/api/context/write', '乐观锁写入'],
    ['POST', '/api/grants', '新增授权'],
    ['DELETE', '/api/grants/:id', '撤销授权'],
    ['POST', '/api/approvals/:id/approve', '批准访问申请'],
    ['GET', '/api/audit/verify', '校验审计哈希链'],
    ['PATCH', '/api/policy', '更新全局策略'],
    ['POST', '/api/control', '启动 / 暂停 / 调速']
  ];
  function escCode(s) { return esc(s).replace(/&#39;/g, "'"); }
  function buildIntegrate() {
    $('archLayers').innerHTML = ARCH.map(function (a) {
      return '<div class="arch-layer"><span class="al-num">' + a[0] + '</span><span class="al-name">' + esc(a[1]) + '</span>' +
        '<span class="al-desc">' + esc(a[2]) + '</span><span class="al-file">' + esc(a[3]) + '</span></div>';
    }).join('');
    var envelope = [
      '{',
      '  "id": "msg_lx2k_9f",        "v": "1.0",',
      '  "at": 1762352143123,',
      '  "from": "agt_claude_01",   "to": "agt_codex_01",',
      '  "kind": "collab.ask",      // task.* | ctx.* | collab.* | conflict.alert',
      '  "correlationId": "task_lx2k_81",',
      '  "scope": {',
      '    "sensitivity": 2,        // 0 公开 1 内部 2 机密 3 绝密',
      '    "sessionId": "sess_lx2k_03",',
      '    "tags": []',
      '  },',
      '  "payload": { "text": "请回传测试结论" },',
      '  "auth": {',
      '    "grantId": "grant_lx2k_12",',
      '    "decision": "allowed",   // allowed | denied | pending',
      '    "reason": ""',
      '  },',
      '  "status": "delivered"      // delivered | rejected',
      '}'
    ].join('\n');
    $('envelopeSpec').innerHTML = envelope.split('\n').map(function (l) {
      var cls = l.indexOf('//') >= 0 ? 'c' : '';
      return '<span class="' + cls + '">' + esc(l) + '</span>';
    }).join('\n');
    var adapter = [
      'class MyAdapter extends HarnessAdapter {',
      '  descriptor() {',
      '    return { label, capabilities, transport,',
      '             maxConcurrency, reliability, clearance };',
      '  }',
      '  async heartbeat(agent) { return { ok, load }; }',
      '  async dispatch(task, contextView) {',
      '    // contextView 已按 ACL 过滤并脱敏',
      '    return { ok: true, output };',
      '  }',
      '  async send(message)  { return { ok: true }; }',
      '  teardown() {}',
      '}',
      '',
      '// 运行时注册，无需重启：',
      'hub.registerAdapter("my-tool", s => new MyAdapter(s), desc);',
      'hub.createAgent({ type: "my-tool", name: "My #1" });'
    ].join('\n');
    $('adapterSpec').innerHTML = adapter.split('\n').map(function (l) {
      var cls = l.indexOf('//') === 0 ? 'c' : '';
      return '<span class="' + cls + '">' + esc(l) + '</span>';
    }).join('\n');
    $('endpointBase').textContent = (LIVE ? location.origin : 'http://127.0.0.1:8787');
    $('apiGrid').innerHTML = APIS.map(function (a) {
      return '<div class="api-row"><span class="api-method ' + a[0].toLowerCase() + '">' + a[0] + '</span>' +
        '<span class="api-path">' + esc(a[1]) + '</span><span class="api-desc">' + esc(a[2]) + '</span></div>';
    }).join('');
    $('inlineRegister').innerHTML =
      '<label>实例名称<input class="input" id="irName" placeholder="例如：Gemini CLI Worker"></label>' +
      '<label>工具类型<input class="input" id="irType" placeholder="自定义类型标识，如 gemini-cli"></label>' +
      '<label>能力标签<input class="input" id="irCaps" placeholder="code.write,analysis"></label>' +
      '<label>接入地址<input class="input" id="irEndpoint" placeholder="http://127.0.0.1:9100"></label>' +
      '<label>密级上限<select class="input" id="irSens">' + optionsHtml([0, 1, 2, 3], function (n) { return String(n); }, function (n) { return SENS_LABEL[n]; }, '1') + '</select></label>' +
      '<button class="btn btn-primary" id="irSubmit">立即注册并接入</button>';
    $('irSubmit').addEventListener('click', function () {
      var name = $('irName').value.trim(), type = $('irType').value.trim();
      if (!name || !type) return toast('请填写名称与类型标识', 'err');
      var caps = $('irCaps').value.split(/[,，\s]+/).filter(Boolean);
      var sens = Number($('irSens').value);
      var desc = {
        label: name, vendor: 'Custom', glyph: name.charAt(0).toUpperCase(), color: '#6366f1',
        transport: 'http', capabilities: caps, maxConcurrency: 2, reliability: 0.9, clearance: sens
      };
      act('adapter.create', { type: type, descriptor: desc }).then(function () {
        return act('agent.create', {
          type: type, name: name, endpoint: $('irEndpoint').value.trim() || 'http://127.0.0.1:9100',
          capabilities: caps, clearance: sens
        });
      }).then(function () {
        $('irName').value = ''; $('irType').value = ''; $('irCaps').value = '';
        toast('已接入 ' + name + '，现在可参与统一调度', 'ok');
      });
    });
  }

  /* ============================================================
   * Modals
   * ========================================================== */
  function openModal(title, body, foot, wide) {
    $('modalTitle').textContent = title;
    $('modalBody').innerHTML = body;
    $('modalFoot').innerHTML = foot || '<button class="btn btn-ghost" data-act="modal-close">关闭</button>';
    $('modal').classList.toggle('wide', !!wide);
    $('overlay').hidden = false;
  }
  function closeModal() { $('overlay').hidden = true; }

  function capChips(selected, prefix) {
    return '<div class="check-grid">' + CAPS.map(function (c, i) {
      var on = (selected || []).indexOf(c) >= 0;
      return '<label class="check-chip' + (on ? ' on' : '') + '" data-chip="' + prefix + '"><input type="checkbox" value="' + esc(c) + '"' + (on ? ' checked' : '') + '>' + esc(c) + '</label>';
    }).join('') + '</div>';
  }
  function checkedCaps(prefix) {
    return Array.prototype.slice.call(document.querySelectorAll('.check-chip[data-chip="' + prefix + '"] input:checked'))
      .map(function (x) { return x.value; });
  }

  function modalRegister() {
    var types = state.snap.adapterTypes;
    var body = '<div class="form-grid">' +
      '<label class="form-field">实例名称<input class="input" id="fName" placeholder="例如：Claude Code 数据组"></label>' +
      '<label class="form-field">工具类型<select class="input" id="fType">' +
      optionsHtml(types, function (t) { return t.type; }, function (t) { return t.label + '（' + t.type + '）'; }, 'codex') +
      '<option value="__custom">＋ 自定义新类型…</option></select></label>' +
      '<label class="form-field">厂商<input class="input" id="fVendor" placeholder="可留空，使用适配器默认值"></label>' +
      '<label class="form-field">接入地址<small>HTTP(S) 地址会建立真实消息连接</small><input class="input" id="fEndpoint" placeholder="http://127.0.0.1:9001"></label>' +
      '<label class="form-field">桥接令牌<input class="input" id="fToken" type="password" autocomplete="off" placeholder="桥接服务的 Bearer Token，可留空"></label>' +
      '<label class="form-field">传输协议<select class="input" id="fTransport">' +
      optionsHtml(['stdio', 'http'], function (x) { return x; }, function (x) { return x; }, 'http') + '</select></label>' +
      '<label class="form-field">密级上限<select class="input" id="fSens">' +
      optionsHtml([0, 1, 2, 3], function (n) { return String(n); }, function (n) { return SENS_LABEL[n]; }, '2') + '</select></label>' +
      '<label class="form-field">最大并发<input class="input" id="fConc" type="number" min="1" max="8" value="2"></label>' +
      '<label class="form-field">驱动类<small>支持本机 CLI 或 HTTP 桥接</small><input class="input" value="真实 Harness 适配器" disabled></label>' +
      '<div class="form-field full" id="customTypeBox" style="display:none">自定义类型标识<input class="input" id="fCustomType" placeholder="例如：gemini-cli"></div>' +
      '<div class="form-field full">能力标签<small>调度器按能力 + 负载 + 可信度 + 密级上限选择执行方</small>' + capChips(['code.write', 'analysis'], 'reg') + '</div>' +
      '</div>';
    openModal('接入新 Harness 工具', body,
      '<button class="btn btn-ghost" data-act="modal-close">取消</button><button class="btn btn-primary" data-act="submit-register">注册并启用</button>');
    $('fType').addEventListener('change', function () {
      $('customTypeBox').style.display = this.value === '__custom' ? '' : 'none';
    });
  }

  function submitRegister() {
    var name = $('fName').value.trim();
    if (!name) return toast('请填写实例名称', 'err');
    var isCustom = $('fType').value === '__custom';
    var type = $('fType').value;
    if (isCustom) {
      type = $('fCustomType').value.trim();
      if (!type) return toast('请填写自定义类型标识', 'err');
    }
    var caps = checkedCaps('reg');
    var payload = {
      type: type, name: name, vendor: $('fVendor').value.trim() || undefined,
      endpoint: $('fEndpoint').value.trim() || undefined,
      token: $('fToken').value || undefined,
      capabilities: caps, clearance: Number($('fSens').value),
      maxConcurrency: Number($('fConc').value) || 1
    };
    var chain = Promise.resolve();
    if (isCustom) {
      chain = act('adapter.create', {
        type: type, descriptor: {
          label: name, vendor: payload.vendor || 'Custom', glyph: name.charAt(0).toUpperCase(),
          color: '#6366f1', transport: $('fTransport').value, capabilities: caps,
          maxConcurrency: payload.maxConcurrency, reliability: 0.9, clearance: payload.clearance
        }
      });
    }
    chain.then(function () { return act('agent.create', payload); }).then(function () {
      closeModal(); toast('已注册 ' + name + '，可在协作消息中选择接收方', 'ok');
    });
  }

  function modalTask() {
    document.body.classList.add('chat-mode');
    document.getElementById('chatInput').focus();
  }

  function submitTask() { modalTask(); }

  function modalGrant(subject, object, sens) {
    var s = state.snap;
    var body = '<div class="form-grid">' +
      '<label class="form-field">授权主体（智能体）<select class="input" id="gSubject">' +
      optionsHtml(s.agents, function (a) { return a.id; }, function (a) { return a.name; }, subject) + '</select></label>' +
      '<label class="form-field">授权对象（会话）<select class="input" id="gObject"><option value="*">全部会话（通配，绝密除外）</option>' +
      optionsHtml(s.sessions, function (x) { return x.id; }, function (x) { return x.title; }, object) + '</select></label>' +
      '<label class="form-field">权限范围<select class="input" id="gScope">' +
      optionsHtml(['read', 'write', 'both'], function (x) { return x; }, function (x) { return { read: '只读', write: '只写', both: '读写' }[x]; }, 'read') + '</select></label>' +
      '<label class="form-field">密级上限<select class="input" id="gSens">' +
      optionsHtml([0, 1, 2, 3], function (n) { return String(n); }, function (n) { return SENS_LABEL[n]; }, String(sens == null ? 2 : sens)) + '</select></label>' +
      '<label class="form-field full">有效期<select class="input" id="gTtl">' +
      '<option value="0">永不过期</option><option value="3600000">1 小时</option><option value="86400000">24 小时</option></select></label>' +
      '<div class="form-field full" style="font-weight:400;color:var(--text-3);font-size:11px">授权即时生效，可随时撤销；所有授予与撤销均写入审计哈希链。</div>' +
      '</div>';
    openModal('新增上下文授权', body,
      '<button class="btn btn-ghost" data-act="modal-close">取消</button><button class="btn btn-primary" data-act="submit-grant">确认授权</button>');
  }

  function submitGrant() {
    var ttl = Number($('gTtl').value);
    act('grant.create', {
      subjectId: $('gSubject').value, objectId: $('gObject').value,
      scope: $('gScope').value, maxSensitivity: Number($('gSens').value),
      ttlMs: ttl || null, grantedBy: 'user'
    }).then(function () { closeModal(); toast('授权已生效', 'ok'); });
  }

  function modalAgentDetail(id) {
    var s = state.snap, a = agentById(s, id);
    if (!a) return;
    var myTasks = s.tasks.filter(function (t) { return t.assignedTo === id; });
    var body = '<div style="display:flex;gap:12px;align-items:center;margin-bottom:16px">' + avatar(a, 46) +
      '<div><div style="font-size:15px;font-weight:650">' + esc(a.name) + '</div><div style="font-size:11.5px;color:var(--text-3)">' +
      esc(a.label) + ' · ' + esc(a.vendor) + ' · ' + esc(a.transport) + ' ' + statusBadge(a.status) + '</div></div></div>' +
      '<dl class="detail-grid">' +
      '<dt>实例 ID</dt><dd class="mono">' + esc(a.id) + '</dd>' +
      '<dt>接入地址</dt><dd class="mono">' + esc(a.endpoint) + '</dd>' +
      '<dt>适配器</dt><dd class="mono">' + esc(a.adapterKind) + ' / ' + esc(a.type) +
        ' <span class="tag tag-real">' + (a.communication === 'http' ? 'HTTP 桥接' : '真实 CLI 驱动') + '</span></dd>' +
      (a.real ? '<dt>可执行入口</dt><dd class="mono" style="word-break:break-all">' + esc(a.endpoint) + '</dd>' : '') +
      '<dt>密级上限</dt><dd>' + sensBadge(a.clearance) + '</dd>' +
      '<dt>可信度</dt><dd>' + Math.round(a.trust * 100) + '% · 可靠性 ' + Math.round(a.reliability * 100) + '%</dd>' +
      '<dt>并发上限</dt><dd>' + a.maxConcurrency + ' · 当前负载 ' + Math.round(a.load * 100) + '%</dd>' +
      '<dt>运行统计</dt><dd>完成 ' + a.stats.completed + ' · 失败 ' + a.stats.failed + ' · 重试 ' + a.stats.retried + ' · 消息 ' + a.stats.messages + '</dd>' +
      '<dt>能力标签</dt><dd>' + a.capabilities.map(function (c) { return '<span class="chip chip-cap">' + esc(c) + '</span>'; }).join('') + '</dd>' +
      '<dt>注册时间</dt><dd>' + fmtTime(a.registeredAt) + '</dd>' +
      '</dl><div style="font-size:11.5px;color:var(--text-3);margin-top:16px">关联任务 ' + myTasks.length + ' 个</div>' +
      '<div class="timeline">' + myTasks.slice(0, 6).map(function (t) {
        return '<div class="tl-item"><span class="tl-time">' + fmtTime(t.createdAt) + '</span><span class="tl-note">' +
          esc(t.title) + ' · ' + (STATE_LABEL[t.status] || t.status) + '</span></div>';
      }).join('') + '</div>';
    openModal('智能体详情', body,
      '<button class="btn btn-ghost" data-act="agent-toggle" data-id="' + esc(a.id) + '" data-enabled="' + (a.enabled ? '1' : '0') + '">' + (a.enabled ? '停用该实例' : '启用该实例') + '</button>' +
      '<button class="btn btn-primary" data-act="modal-close">完成</button>');
  }

  function modalTaskDetail(id) {
    var s = state.snap, t = s.tasks.find(function (x) { return x.id === id; });
    if (!t) return;
    var isReal = !!(t.execution && t.execution.mode === 'real');
    var ex = isReal ? t.execution : null;
    var body = '<div style="font-size:15px;font-weight:650;margin-bottom:6px">' + esc(t.title) +
      (isReal ? ' <span class="tag tag-real">真实执行</span>' : ' <span class="tag tag-sim">仿真</span>') + '</div>' +
      '<div style="margin-bottom:14px"><span class="prio prio-' + t.priority + '">' + t.priority + '</span> ' + stateBadge(t.status) + ' ' + sensBadge(t.sensitivity) + '</div>' +
      '<div class="bar" style="margin-bottom:14px"><i style="width:' + t.progress + '%"></i></div>' +
      '<dl class="detail-grid">' +
      '<dt>任务 ID</dt><dd class="mono">' + esc(t.id) + '</dd>' +
      '<dt>执行方式</dt><dd>' + (isReal
        ? '<span class="tag tag-real">真实 CLI</span> ' + esc(ex.tool || '-') + ' · ' + esc(ex.permission || '-') + ' · 状态 ' + esc(ex.status)
        : '<span class="tag tag-sim">仿真</span> SimulatedAdapter') + '</dd>' +
      (isReal ? '<dt>工作目录</dt><dd class="mono">' + esc(ex.workdir || '-') + '</dd>' +
        '<dt>退出码 / 耗时</dt><dd class="mono">' + (ex.exitCode == null ? '-' : ex.exitCode) + ' / ' +
        (ex.durationMs ? (ex.durationMs / 1000).toFixed(1) + 's' : '-') + '</dd>' +
        '<dt>模型 / 费用</dt><dd class="mono">' + esc(ex.model || '-') +
        (ex.costUsd != null ? ' / $' + ex.costUsd : '') + '</dd>' : '') +
      '<dt>主执行方</dt><dd>' + esc(nameOf(s, t.assignedTo)) + '</dd>' +
      '<dt>协作方</dt><dd>' + (t.collaborators.length ? t.collaborators.map(function (c) { return esc(nameOf(s, c)); }).join('、') : '无') + '</dd>' +
      '<dt>所需能力</dt><dd>' + (t.requiredCapabilities.length ? t.requiredCapabilities.map(function (c) { return '<span class="chip chip-cap">' + esc(c) + '</span>'; }).join('') : '无') + '</dd>' +
      '<dt>尝试次数</dt><dd>' + t.attempts + ' / ' + t.maxRetries + ' 次重试 · 故障转移 ' + t.failovers + ' 次</dd>' +
      '<dt>关联会话</dt><dd class="mono">' + esc(t.sessionId ? shortId(t.sessionId) : '无') + '</dd>' +
      '<dt>资源锁</dt><dd class="mono">' + esc(t.resources.length ? t.resources.map(function (r) { return r.key; }).join(', ') : '无') + '</dd>' +
      '<dt>创建时间</dt><dd>' + fmtTime(t.createdAt) + '</dd>' +
      (t.error ? '<dt>最近错误</dt><dd style="color:var(--err)">' + esc(t.error) + '</dd>' : '') +
      '</dl>' +
      (isReal ? '<div class="real-output"><div class="ro-head"><span>真实 CLI 输出</span>' +
        '<span class="chip">' + esc(ex.tool || '-') + '</span><span class="chip">' + esc(ex.permission || '-') + '</span>' +
        (ex.durationMs ? '<span class="chip">' + (ex.durationMs / 1000).toFixed(1) + 's</span>' : '') +
        (ex.costUsd != null ? '<span class="chip">$' + ex.costUsd + '</span>' : '') +
        (ex.model ? '<span class="chip">' + esc(ex.model) + '</span>' : '') + '</div>' +
        '<pre class="ro-body">' + esc(ex.output || (ex.status === 'running' ? '真实 CLI 执行中，等待输出…' : (ex.error || '(无输出)'))) + '</pre>' +
        (ex.stderr ? '<div class="ro-err">stderr: ' + esc(ex.stderr) + '</div>' : '') + '</div>' : '') +
      (t.result ? '<div class="result-box"><strong>结果汇总 · ' +
        (t.result.real ? '真实输出' : '置信度 ' + t.result.confidence) + '</strong>' + esc(t.result.summary) +
        (t.result.real ? '' : '<ul>' + t.result.contributors.map(function (c) {
          return '<li>' + esc(c.agentName) + '（' + c.role + '）：' + esc(c.text) + '</li>';
        }).join('') + '</ul>') + '</div>' : '') +
      '<div style="font-size:11.5px;color:var(--text-3);margin:16px 0 4px">状态流转记录</div>' +
      '<div class="timeline">' + t.history.slice().reverse().map(function (h) {
        return '<div class="tl-item"><span class="tl-time">' + fmtTime(h.at) + '</span><span class="tl-note">' + esc(h.note) + '</span></div>';
      }).join('') + '</div>';
    var foot = '<button class="btn btn-ghost" data-act="modal-close">关闭</button>';
    if (t.status === 'failed') foot += '<button class="btn btn-primary" data-act="task-retry" data-id="' + esc(t.id) + '">重新派发</button>';
    if (['queued', 'assigned', 'running', 'blocked', 'retrying'].indexOf(t.status) >= 0) {
      foot += '<button class="btn btn-danger" data-act="task-cancel" data-id="' + esc(t.id) + '">取消任务</button>';
    }
    openModal('任务详情', body, foot, true);
  }

  /* ============================================================
   * Real execution UI
   * ========================================================== */
  function realToolPills(r) {
    return Object.keys(r.tools || {}).map(function (k) {
      var t = r.tools[k];
      return '<span class="tool-pill' + (t.available ? '' : ' off') + '"><i></i>' +
        esc(t.label) + (t.available ? ' 就绪' : ' 未找到') + '</span>';
    }).join('');
  }

  function renderReal() {
    var banner = $('realBanner'), settings = $('realSettings');
    if (!banner || !settings) return;
    if (!LIVE || !ui.real || !ui.real.enabled) {
      banner.innerHTML = '';
      settings.innerHTML = '';
      return;
    }
    var r = ui.real, s = state.snap;
    var perm = (r.permissions || []).find(function (p) { return p.id === r.config.permission; });
    banner.innerHTML =
      '<div class="rb-icon">◈</div>' +
      '<div class="rb-main"><strong>真实 Harness 执行已启用</strong>' +
      '<small>仅手动派发并勾选「真实执行」的任务调用真实 CLI · 权限 ' + esc(perm ? perm.label : r.config.permission) +
      ' · 默认工具 ' + esc(r.config.defaultTool) + '</small></div>' +
      '<div class="rb-metrics">' +
      '<div class="rb-metric"><small>真实任务</small><strong>' + (s.stats.realTasks || 0) + '</strong></div>' +
      '<div class="rb-metric"><small>执行中</small><strong>' + (s.stats.realRunning || 0) + '</strong></div>' +
      '<div class="rb-metric"><small>已完成</small><strong>' + (s.stats.realCompleted || 0) + '</strong></div>' +
      '<div class="rb-metric"><small>队列</small><strong>' + (r.queue.pending + '/' + r.queue.running) + '</strong></div>' +
      '</div><div>' + realToolPills(r) + '</div>';

    // Rebuild the settings card only when its config actually changes,
    // otherwise live refreshes can swallow a click on a permission card.
    var settingsSig = JSON.stringify([r.config, r.tools, r.warnings]);
    if (ui.realSettingsSig === settingsSig) return;
    ui.realSettingsSig = settingsSig;
    settings.innerHTML = '<div class="card"><div class="card-head"><div>' +
      '<h3>真实 Harness 执行</h3><p>本机 CLI 可用性、权限模式与执行配置；仅由服务端模式提供</p></div>' +
      '<span class="tag tag-soft">LIVE</span></div>' +
      '<div class="rs-row"><span class="rs-label">CLI 状态</span><span>' + realToolPills(r) + '</span></div>' +
      '<div class="rs-row"><span class="rs-label">工作目录</span><span class="rs-val">' + esc(r.config.workdir) + '</span></div>' +
      '<div class="rs-row"><span class="rs-label">允许根目录</span><span class="rs-val">' + esc(r.config.allowedRoot) + '</span></div>' +
      '<div class="rs-row"><span class="rs-label">超时 / 并发</span><span class="rs-val">' +
      Math.round(r.config.timeoutMs / 1000) + 's / ' + r.config.maxConcurrent + '</span></div>' +
      '<div style="font-size:11.5px;color:var(--text-2);margin:14px 0 8px">默认权限模式（点击切换，也可在派发任务时逐任务覆盖）</div>' +
      '<div class="perm-grid">' + (r.permissions || []).map(function (p) {
        return '<div class="perm-card' + (p.id === r.config.permission ? ' on' : '') + '" data-act="perm-select" data-id="' + esc(p.id) + '">' +
          '<strong>' + esc(p.label) + '</strong><small>' + esc(p.desc) + '</small></div>';
      }).join('') + '</div>' +
      '<div style="font-size:10.8px;color:var(--text-3);margin-top:12px;line-height:1.6">' +
      '真实执行通过 stdin 传递指令，不经过 shell；每次调用都会产生模型 API 费用，退出码、耗时、模型与费用会写入任务详情和审计链。' +
      'Codex 使用隔离的 CODEX_HOME 以避开本机 MCP/插件启动阻塞，可用 <span class="mono">HARNESS_HUB_CODEX_ISOLATE=0</span> 关闭。</div></div>';
  }

  /* ============================================================
   * Top metrics / persistent rail / status bar
   * ========================================================== */
  function renderTopMetrics() {
    var el = $('topMetrics');
    if (!el) return;
    var s = state.snap;
    var chips = [
      { label: '在线实例', value: s.stats.online + '/' + s.stats.agents, cls: '' },
      { label: '活跃任务', value: s.stats.tasksActive, cls: s.stats.tasksActive > 0 ? 'hot' : '' },
      { label: '真实任务', value: s.stats.realTasks || 0, cls: 'real' },
      { label: '待审批', value: s.stats.pendingApprovals, cls: s.stats.pendingApprovals > 0 ? 'hot' : '' }
    ];
    el.innerHTML = chips.map(function (c) {
      return '<div class="metric-chip ' + c.cls + '"><small>' + c.label + '</small><strong>' + c.value + '</strong></div>';
    }).join('');
  }

  function renderRailPanels() {
    if (!$('railPanel')) return;
    var s = state.snap;
    renderLiveLog();
    renderLoads();
    var loadNote = $('railLoadNote');
    if (loadNote) loadNote.textContent = s.stats.online + ' 在线 / ' + s.stats.agents + ' 实例';
    var pending = s.approvals.filter(function (a) { return a.state === 'pending'; }).slice(0, 3);
    var badge = $('railApprovalBadge');
    if (badge) {
      var count = s.stats.pendingApprovals;
      badge.textContent = count;
      badge.style.background = count > 0 ? 'var(--busy-soft)' : '';
      badge.style.color = count > 0 ? 'var(--busy)' : '';
    }
    var box = $('railApprovals');
    if (box) {
      box.innerHTML = pending.length ? pending.map(function (a) {
        return '<div class="rail-approval"><span class="ra-main"><b>' + esc(nameOf(s, a.actor)) + '</b> ' +
          (a.action === 'write' ? '写入' : '读取') + '「' + esc(a.sessionTitle.length > 14 ? a.sessionTitle.slice(0, 13) + '…' : a.sessionTitle) + '」</span>' +
          '<button class="btn btn-approve" data-act="approval-approve" data-id="' + esc(a.id) + '">批准</button>' +
          '<button class="btn btn-deny" data-act="approval-reject" data-id="' + esc(a.id) + '">拒绝</button></div>';
      }).join('') : '<div class="rail-empty">无待审批申请</div>';
    }
  }

  function renderStatusBar() {
    var el = $('statusBar');
    if (!el) return;
    var s = state.snap;
    var real = ui.real;
    var realText = !LIVE ? '浏览器演示模式' : (real && real.enabled
      ? '真实执行已启用 · ' + Object.keys(real.tools || {}).filter(function (k) { return real.tools[k].available; }).length + ' 个 CLI 就绪'
      : '真实执行未启用');
    el.innerHTML =
      '<span class="sb-item"><span class="sb-dot' + (LIVE ? '' : ' off') + '"></span>' + (LIVE ? '<strong>LIVE 服务已连接</strong>' : '本地演示内核') + '</span>' +
      '<span class="sb-item">tick <strong>' + s.meta.tick + '</strong></span>' +
      '<span class="sb-item">实例 <strong>' + s.stats.online + '/' + s.stats.agents + '</strong></span>' +
      '<span class="sb-item">任务 <strong>' + s.stats.tasksActive + '</strong> 进行中 · <strong>' + s.stats.completed + '</strong> 已完成</span>' +
      '<span class="sb-item">审计链 <strong style="color:' + (s.stats.chainValid ? 'var(--ok)' : 'var(--err)') + '">' + (s.stats.chainValid ? '有效' : '异常') + '</strong> · ' + s.stats.auditEntries + ' 条</span>' +
      '<span class="sb-item">' + realText + '</span>' +
      '<span class="sb-spacer"></span>' +
      '<span class="sb-item">协议 v1.0 · ' + (LIVE ? 'REST + SSE' : 'in-page engine') + '</span>';
  }

  /* ============================================================
   * Render dispatcher
   * ========================================================== */
  function renderAll() {
    if (!state.snap) return;
    renderChrome();
    renderTopMetrics();
    renderRailPanels();
    renderStatusBar();
    switch (ui.page) {
      case 'overview': renderOverview(); break;
      case 'agents': renderAgents(); break;
      case 'tasks': renderTasks(); break;
      case 'context': renderContext(); break;
      case 'messages': renderMessages(); break;
      case 'audit': renderAudit(); break;
      case 'integrate': break; // static, built once
    }
    renderReal();
  }

  /* ============================================================
   * Events
   * ========================================================== */
  function handleAct(actName, d, ev) {
    var s = state.snap;
    switch (actName) {
      case 'goto': goto(d.page); break;
      case 'modal-close': closeModal(); break;
      case 'agent-detail': modalAgentDetail(d.id); break;
      case 'agent-toggle':
        act('agent.toggle', { id: d.id, enabled: d.enabled !== '1' }).then(function () {
          toast(d.enabled !== '1' ? '实例已启用' : '实例已停用', 'ok');
          if (!$('overlay').hidden) closeModal();
        });
        break;
      case 'task-detail': modalTaskDetail(d.id); break;
      case 'task-retry': act('task.retry', { id: d.id }).then(function () { closeModal(); toast('任务已重新入队', 'ok'); }); break;
      case 'task-cancel': act('task.cancel', { id: d.id }).then(function () { closeModal(); toast('任务已取消', 'ok'); }); break;
      case 'session-select': ui.selectedSession = d.id; renderContext(); break;
      case 'context-sync':
        var actor = $('syncActor') ? $('syncActor').value : (s.agents[0] || {}).id;
        act('context.sync', { actor: actor, sessionId: d.id }).then(function (r) {
          if (!r) return;
          if (r.ok) toast('同步成功：可见 ' + r.entries.length + ' 条，脱敏 ' + r.redacted + ' 条', 'ok');
          else toast('访问被拦截：' + r.reason, 'err');
        });
        break;
      case 'grant-new': modalGrant(d.subject, d.object, Number(d.sens)); break;
      case 'grant-revoke': act('grant.revoke', { id: d.id }).then(function () { toast('授权已撤销，立即生效', 'ok'); }); break;
      case 'approval-approve': act('approval.approve', { id: d.id }).then(function () { toast('已批准，授权即时生效', 'ok'); }); break;
      case 'approval-reject': act('approval.reject', { id: d.id }).then(function () { toast('已拒绝该访问申请', 'ok'); }); break;
      case 'policy-toggle':
        var patch = {}; patch[d.key] = d.val !== '1';
        act('policy.patch', patch).then(function () { toast('策略已更新', 'ok'); });
        break;
      case 'msg-select': ui.selectedMsg = d.id; renderMessages(); break;
      case 'perm-select':
        act('real.config', { permission: d.id }).then(function () {
          ui.taskPermission = d.id;
          loadRealStatus().then(function () { toast('默认权限已切换', 'ok'); renderReal(); });
        });
        break;
      case 'submit-register': submitRegister(); break;
      case 'submit-task': submitTask(); break;
      case 'submit-grant': submitGrant(); break;
      default: break;
    }
  }

  function wire() {
    $('nav').addEventListener('click', function (e) {
      var b = e.target.closest('.nav-item');
      if (b) goto(b.dataset.page);
    });
    document.addEventListener('click', function (e) {
      var chip = e.target.closest('.check-chip');
      if (chip) {
        e.preventDefault();
        var cb = chip.querySelector('input');
        cb.checked = !cb.checked;
        chip.classList.toggle('on', cb.checked);
        return;
      }
      var el = e.target.closest('[data-act]');
      if (el) { handleAct(el.dataset.act, el.dataset, e); return; }
      if (e.target.id === 'overlay') closeModal();
    });
    $('modalClose').addEventListener('click', closeModal);
    document.addEventListener('keydown', function (e) { if (e.key === 'Escape') closeModal(); });

    var pauseBtn = $('pauseBtn');
    if (pauseBtn) pauseBtn.addEventListener('click', function () {
      var running = state.snap.meta.running;
      act('control', { action: running ? 'pause' : 'start' }).then(function () {
        toast(running ? '编排引擎已暂停' : '编排引擎已恢复', 'ok');
      });
    });
    var speedSel = $('speedSel');
    if (speedSel) speedSel.addEventListener('change', function () {
      act('control', { action: 'speed', speed: Number(this.value) });
    });
    var backToChat = $('backToChat');
    if (backToChat) backToChat.addEventListener('click', function () {
      document.body.classList.add('chat-mode');
    });
    $('openRegisterBtn').addEventListener('click', modalRegister);
    $('openTaskBtn').addEventListener('click', modalTask);
    $('openGrantBtn').addEventListener('click', function () { modalGrant('', '', 2); });

    $('agentSearch').addEventListener('input', function () { ui.agentSearch = this.value; renderAgents(); });
    $('auditSearch').addEventListener('input', function () { ui.auditSearch = this.value; renderAudit(); });
    $('msgText').addEventListener('keydown', function (e) { if (e.key === 'Enter') $('sendMsgBtn').click(); });
    $('sendMsgBtn').addEventListener('click', function () {
      var text = $('msgText').value.trim();
      if (!text) return toast('请输入消息内容', 'err');
      var deliver = $('msgDeliver').checked;
      if (deliver && !LIVE) return toast('实际投递需要启动 Node 服务并进入 LIVE 模式', 'err');
      $('sendMsgBtn').disabled = true;
      act('message.send', {
        from: $('msgFrom').value, to: $('msgTo').value, kind: $('msgKind').value,
        sensitivity: Number($('msgSens').value), text: text, deliver: deliver
      }).then(function (m) {
        if (m) {
          if (m.status === 'rejected') toast('消息被拦截：' + m.auth.reason, 'err');
          else if (m.delivery && m.delivery.status === 'failed') toast('投递失败：' + m.delivery.error, 'err');
          else { $('msgText').value = ''; toast(deliver ? (m.delivery.replyId ? 'Harness 已接收，回复已记录' : 'Harness 已确认接收') : '消息已记入总线', 'ok'); }
          ui.selectedMsg = m.id;
        }
      }).finally(function () { $('sendMsgBtn').disabled = false; });
    });

    var segBindings = [
      ['agentFilter', 'status', function (v) { ui.agentFilter = v; renderAgents(); }],
      ['taskFilter', 'filter', function (v) { ui.taskFilter = v; renderTasks(); }],
      ['logFilter', 'level', function (v) { ui.logFilter = v; renderLiveLog(); }],
      ['msgFilter', 'filter', function (v) { ui.msgFilter = v; renderMessages(); }],
      ['auditFilter', 'decision', function (v) { ui.auditFilter = v; renderAudit(); }]
    ];
    segBindings.forEach(function (b) {
      var root = $(b[0]);
      if (!root) return;
      root.addEventListener('click', function (e) {
        var btn = e.target.closest('button');
        if (!btn) return;
        Array.prototype.forEach.call(root.querySelectorAll('button'), function (x) { x.classList.remove('on'); });
        btn.classList.add('on');
        b[2](btn.dataset[b[1]]);
      });
    });
  }

  /* ============================================================
   * Boot
   * ========================================================== */
  function buildStaticUI() { buildIntegrate(); }
  wire();
  initSource();
})();
