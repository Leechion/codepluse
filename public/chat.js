(function () {
  'use strict';
  const $ = id => document.getElementById(id);
  const esc = s => String(s == null ? '' : s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const status = { planning: '规划中', running: '执行中', reporting: '验收汇报中', completed: '已完成', partial: '部分完成', failed: '失败', queued: '待执行', blocked: '受阻' };
  let agents = [], history = [], current = null, selected = localStorage.getItem('harness-chat-id'), draft = {}, busy = false, renderSig = '', selectedTask = null, networkError = false;
  let autoList = [], mention = { open: false, items: [], index: 0, from: 0, to: 0 };
  const name = id => agents.find(a => a.id === id)?.name || id || '宰相';
  const active = m => ['planning', 'running', 'reporting'].includes(m.status);
  const sessionStates = { connecting: '连接中', restoring: '恢复上下文', running: '执行中', requires_action: '等待授权', cancelling: '停止中', idle: '待命', disconnected: '待恢复', error: '连接失败' };
  const modelListId = id => 'native-models-' + id.replace(/[^\w-]/g, '_');
  let nativeSelected = null, nativeSelectSig = '', permissionSig = '', refreshTimer;
  async function api(path, body, method = 'POST') {
    const res = await fetch(path, body === undefined ? {} : { method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    const value = await res.json(); if (!value.ok) throw new Error(value.error); return value.data;
  }
  function select(id) { selected = id; current = null; renderSig = ''; localStorage.setItem('harness-chat-id', id); }
  function defaults() {
    return { managerId: agents.find(a => a.realTool === 'claude')?.id || agents[0]?.id, workerIds: agents.map(a => a.id), models: {}, permission: 'readonly', isolate: 'shared', ...draft };
  }
  async function newChat() {
    const c = await api('/api/chats', current?.config || defaults()); select(c.id); current = c; await refresh(); $('chatInput').focus();
  }
  async function refresh() {
    if (busy) return;
    busy = true;
    try {
      const [agentList, chatHistory, automations] = await Promise.all([api('/api/agents'), api('/api/chats'), api('/api/automations').catch(() => [])]);
      agents = agentList.filter(a => a.enabled && (a.real || a.communication === 'http'));
      autoList = automations;
      if (selected && !history.some(c => c.id === selected)) selected = null;
      if (!selected && history.length) select(history[0].id);
      if (selected) current = await api('/api/chats/' + encodeURIComponent(selected));
      $('chatConnection').textContent = '● 真实服务已连接'; $('chatConnection').className = 'connected';
      if (networkError) { $('chatError').textContent = ''; networkError = false; }
      $('sendChat').disabled = !agents.length;
      $('chatHistory').innerHTML = history.map(c => '<button class="history-item' + (c.id === selected ? ' selected' : '') + '" data-chat="' + esc(c.id) + '" title="' + esc(c.title) + '">' + esc(c.title) + (c.pending ? '<span> ···</span>' : '') + '</button>').join('') || '<span class="sidebar-empty">从一个目标开始</span>';
      renderTeam(); renderConversation(); renderExecPanel(); renderNative();
    } catch (e) { networkError = true; $('chatConnection').textContent = '服务未连接'; $('chatError').textContent = e.message; }
    finally { busy = false; }
  }
  function renderTeam() {
    const cfg = current?.config || defaults(), missions = current?.missions || [];
    const liveMission = missions.findLast(m => active(m));
    const visibleMissions = liveMission ? [liveMission] : missions.slice(-1);
    $('teamCount').textContent = agents.length;
    $('harnessTeam').innerHTML = agents.map(a => {
      const tasks = visibleMissions.flatMap(m => m.tasks.filter(t => t.agentId === a.id).map(t => ({ ...t, mission: m })));
      const manager = (liveMission?.managerId || cfg.managerId) === a.id;
      const sessions = (current?.sessions || []).filter(s => s.agentId === a.id);
      const latest = sessions.at(-1);
      return '<section class="team-agent"><div class="team-agent-heading"><span class="agent-letter">' + esc(a.name[0]) + '</span><div><strong>' + esc(a.name) + '</strong><small>' + esc(latest?.models?.currentModelId || cfg.models?.[a.id] || (a.communication === 'acp' ? 'ACP 原生会话' : 'HTTP 桥接')) + '</small></div><span class="team-role">' + (manager ? '宰相' : '执行端') + '</span></div>' +
        (manager ? '<div class="manager-model">负责人模型 · ' + esc((liveMission ? liveMission.managerModel : cfg.managerModel) || '默认') + '</div>' : '') +
        '<div class="agent-tasks">' + tasks.map(t => '<button class="agent-task" data-task="' + esc(t.id) + '" data-mission="' + esc(t.mission.id) + '"><span class="task-dot ' + esc(t.status) + '"></span><span>' + esc(t.title) + '<small>' + esc(status[t.status] || t.status) + (t.model ? ' · ' + esc(t.model) : '') + '</small></span><span class="task-edit">↗</span></button>').join('') + '</div>' +
        (!tasks.length ? '<div class="agent-idle">' + (manager && (current?.pending || missions.some(m => active(m) && m.managerId === a.id)) ? '正在处理对话与分工' : '等待宰相分配任务') + '</div>' : '') +
        '<div class="agent-native-sessions">' + sessions.slice(-3).map(s => '<button data-native-session="' + esc(s.id) + '"><span>◉ ' + (s.role === 'manager' ? '宰相会话' : s.taskId ? '任务 ' + esc(s.taskId) : '原生会话') + '</span><small>' + esc(sessionStates[s.state] || s.state) + ' · ' + s.turns.length + ' 轮</small></button>').join('') + '</div></section>';
    }).join('') || '<div class="sidebar-empty">尚未连接 Harness<br>从系统管理接入 CLI 或 HTTP 桥接。</div>';
  }
  function renderNative() {
    const sessions = current?.sessions || [];
    const pending = sessions.flatMap(s => (s.permissions || []).filter(p => p.state === 'pending').map(p => ({ ...p, agentId: s.agentId })));
    const nextPermissionSig = JSON.stringify(pending);
    if (permissionSig !== nextPermissionSig) {
      permissionSig = nextPermissionSig;
      $('nativePermissions').innerHTML = pending.map(p => '<section class="native-permission"><strong>' + esc(name(p.agentId)) + ' 请求权限</strong><p>' + esc(p.title) + '</p>' +
        (p.toolCall?.rawInput ? '<pre>' + esc(JSON.stringify(p.toolCall.rawInput, null, 2).slice(0, 5000)) + '</pre>' : '') +
        '<div>' + p.options.map(o => '<button type="button" data-native-permission="' + esc(p.id) + '" data-native-option="' + esc(o.optionId) + '">' + esc(o.name) + '</button>').join('') + '</div></section>').join('');
    }
    $('nativeModelLists').innerHTML = agents.map(a => {
      const catalog = sessions.filter(s => s.agentId === a.id).flatMap(s => s.models?.availableModels || []);
      const models = [...new Map(catalog.map(m => [m.modelId, m])).values()];
      return '<datalist id="' + modelListId(a.id) + '">' + models.map(m => '<option value="' + esc(m.modelId) + '">' + esc(m.name || m.modelId) + '</option>').join('') + '</datalist>';
    }).join('');
    $('chatManagerModel').setAttribute('list', modelListId($('chatManager').value || current?.config.managerId || ''));
    document.querySelectorAll('[data-worker-model]').forEach(el => el.setAttribute('list', modelListId(el.dataset.workerModel)));
    $('taskModel').setAttribute('list', modelListId($('taskHarness').value || ''));
    if (!$('nativeSessions').open) return;
    if (!sessions.some(s => s.id === nativeSelected)) nativeSelected = sessions.at(-1)?.id || null;
    const sig = sessions.map(s => s.id).join('|');
    if (nativeSelectSig !== sig) {
      nativeSelectSig = sig;
      $('nativeSessionSelect').innerHTML = sessions.map(s => '<option value="' + esc(s.id) + '">' + esc(name(s.agentId)) + ' · ' + (s.role === 'manager' ? '宰相' : s.taskId ? '子任务 ' + esc(s.taskId) : '会话') + '</option>').join('');
    }
    $('nativeSessionSelect').value = nativeSelected || '';
    const s = sessions.find(s => s.id === nativeSelected);
    if (!s) { $('nativeSessionIdentity').textContent = '发送消息后，这里会出现真实 Harness 会话。'; $('nativeSessionActivity').innerHTML = ''; $('cancelNativeTurn').disabled = true; return; }
    $('nativeSessionIdentity').innerHTML = '<div class="native-session-facts"><span>' + esc(sessionStates[s.state] || s.state) + '</span><span>模型：' + esc(s.models?.currentModelId || '默认') + '</span><span>模式：' + esc(s.modes?.currentModeId || s.permission) + '</span></div><code>' + esc(s.nativeSessionId || '等待连接') + '</code>' + (s.error ? '<p class="chat-error">' + esc(s.error) + '</p>' : '');
    $('cancelNativeTurn').disabled = !s.activeTurn || s.state === 'cancelling';
    const turns = [...s.turns, ...(s.activeTurn ? [s.activeTurn] : [])].reverse();
    const expanded = new Set([...$('nativeSessionActivity').querySelectorAll('details[open][data-detail-key]')].map(el => el.dataset.detailKey));
    $('nativeSessionActivity').innerHTML = (s.notices || []).map(n => '<details class="native-notice"><summary>' + esc(n.title) + '</summary><p>' + esc(n.description || '') + '</p></details>').join('') +
      (s.permissions || []).filter(p => p.state === 'denied').slice(-2).map(p => '<p class="chat-error">' + esc(p.reason) + '</p>').join('') +
      turns.map(t => '<section class="native-turn"><h3>' + esc(t.stage) + ' <small>' + esc(t.status === 'running' ? '执行中' : t.status === 'completed' ? '已结束' : t.status) + '</small></h3>' +
        (t.plan?.length ? '<ul>' + t.plan.map(p => '<li>' + esc(p.content) + ' · ' + esc(p.status) + '</li>').join('') + '</ul>' : '') +
        t.tools.map(tool => '<details class="native-tool" data-detail-key="' + esc(s.id + ':' + t.id + ':' + tool.toolCallId) + '"><summary>' + esc(tool.title || tool.toolCallId) + ' · ' + esc(tool.status || '') + '</summary>' +
          (tool.rawInput ? '<pre>' + esc(JSON.stringify(tool.rawInput, null, 2).slice(0, 10000)) + '</pre>' : '') +
          (tool.content || []).map(c => c.type === 'diff' ? '<p>' + esc(c.path) + '</p><div class="native-diff"><pre>' + esc((c.oldText || '').slice(0, 10000)) + '</pre><pre>' + esc((c.newText || '').slice(0, 10000)) + '</pre></div>' : c.content?.type === 'text' ? '<pre>' + esc(c.content.text.slice(0, 20000)) + '</pre>' : '').join('') + '</details>').join('') +
        (t.output ? '<pre class="native-answer">' + esc(t.output) + '</pre>' : t.status === 'running' ? '<p>正在生成结果' + (t.receivedChars ? ' · 已收到 ' + t.receivedChars + ' 字符' : '') + '…</p>' : '') + '</section>').join('');
    $('nativeSessionActivity').querySelectorAll('[data-detail-key]').forEach(el => { el.open = expanded.has(el.dataset.detailKey); });
  }
  function renderConversation() {
    const cfg = current?.config || defaults(), missions = current?.missions || [];
    const running = missions.findLast(m => active(m));
    $('conversationTitle').textContent = name(running?.managerId || cfg.managerId) || '宰相';
    const stateEl = $('conversationState');
    stateEl.textContent = current?.pending ? '宰相正在思考' : running ? status[running.status] : missions.length ? status[missions.at(-1).status] : '自主分配';
    stateEl.className = 'state-chip' + (current?.pending || running ? ' hot' : missions.length && !['failed', 'blocked'].includes(missions.at(-1).status) ? ' ok' : '');
    $('composerContext').textContent = name(cfg.managerId) + ' · ' + (cfg.managerModel || '默认模型') + ' · ' + ({ readonly: '只读', workspace: '目录内读写', shell: '只读命令' }[cfg.permission] || '只读');
    $('chatActivity').textContent = current?.pending ? '● 宰相正在处理你的消息…' : running ? '● ' + status[running.status] + ' · ' + running.tasks.filter(t => t.status === 'completed').length + '/' + running.tasks.length + ' 项完成' + (running.pendingInstructions?.length ? ' · 有待生效的调整' : '') : '';
    const signature = JSON.stringify({ messages: current?.messages, missions, pending: current?.pending });
    if (signature === renderSig) return;
    renderSig = signature;
    const feed = $('conversationMessages');
    const atBottom = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 140;
    const items = (current?.messages || []).map(m => ({ ...m, type: 'message' }));
    missions.forEach(m => {
      if (!m.planMessage && m.tasks.length) items.push({ id: m.id + '-plan', at: m.tasks[0].createdAt || m.createdAt + 1, type: 'notice', text: '已分配 ' + m.tasks.length + ' 项任务，可在侧边栏查看或调整。' });
      m.events.forEach(e => {
        if (e.kind === 'collab.ask') items.push({ ...e, type: 'peer' });
        if (e.kind === 'task.result') {
          try { const value = JSON.parse(e.text); if (value.taskId && typeof value.output === 'string') items.push({ ...e, type: 'result', text: value.output, title: m.tasks.find(t => t.id === value.taskId)?.title || value.taskId }); } catch (_) { /* final report is already in chat messages */ }
        }
      });
    });
    items.sort((a, b) => a.at - b.at);
    feed.innerHTML = items.length ? '<div class="thread-width">' + items.map(item => {
      if (item.type === 'notice') return '<div class="conversation-notice">' + esc(item.text) + '</div>';
      if (item.type === 'peer') return '<details class="peer-message"><summary><span>↔</span> ' + esc(name(item.from)) + ' → ' + esc(name(item.to)) + ' · 协作消息</summary><div>' + esc(item.text) + '</div></details>';
      if (item.type === 'result') return '<details class="worker-result"><summary><span class="worker-result-mark">↳</span><strong>' + esc(name(item.from)) + '</strong>回传 · ' + esc(item.title) + '</summary><div>' + esc(item.text) + '</div></details>';
      return '<article class="conversation-turn ' + esc(item.role) + '"><div class="turn-heading">' + (item.role === 'user' ? '你' : item.role === 'error' ? '执行提示' : '宰相 · ' + esc(name(item.agentId || cfg.managerId))) + (item.model ? '<span>' + esc(item.model) + '</span>' : '') + '</div><div class="turn-text">' + esc(item.text) + '</div>' + (item.details ? '<details class="worker-result"><summary>查看验收依据</summary><div>' + esc(item.details) + '</div></details>' : '') + (item.note ? '<small class="turn-note">' + esc(item.note) + '</small>' : '') + '</article>';
    }).join('') + '</div>' : '<div class="chat-welcome"><div class="welcome-mark">H</div><span class="eyebrow">一个目标，一支真实执行的团队</span><h2>把目标交给宰相。</h2><p>与你对话、拆解工作、召集不同 Harness。<br>你可以随时介入，指定谁来做、用什么模型。</p><div class="welcome-examples"><button data-example="分析这个项目的架构与主要问题，自主安排不同 Harness 分工，互相核对后向我汇报。">分析项目，交叉核对结论 ↗</button><button data-example="先和我讨论实现方案，确认需求后再分配给团队执行。">先聊方案，再开始执行 ↗</button></div></div>';
    if (atBottom) feed.scrollTop = feed.scrollHeight;
  }
  /* ---------- worktree 审查（Codeg To-dos 模式） ---------- */
  let wtDiff = null, wtArm = null;
  const armFresh = (id, action) => wtArm && wtArm.id === id && wtArm.action === action && Date.now() < wtArm.expires;
  function wtReview(m) {
    if (!m || (!m.worktree && !m.isolationError)) return '';
    const short = m.worktree ? m.worktree.split(/[\\/]/).pop() : '';
    let html = '<div class="wt-review" data-wt-mission="' + esc(m.id) + '">' +
      '<div class="wt-head"><b>◍ 独立工作树</b><small title="' + esc(m.worktree || '') + '">' + esc(short) + '</small></div>';
    if (m.isolationError) html += '<p class="wt-warn">' + esc(m.isolationError) + '</p>';
    if (!m.worktree) return html + '</div>';
    if (m.worktreeState === 'merge') return html + '<p class="wt-done">已合并到主分支并清理工作树。</p></div>';
    if (m.worktreeState === 'discard') return html + '<p class="wt-done">工作树已丢弃。</p></div>';
    if (!['completed', 'partial', 'failed'].includes(m.status)) return html + '<p class="wt-note">任务在隔离副本中执行，结束后在这里审查并合并。</p></div>';
    html += '<div class="wt-actions">' +
      '<button type="button" class="btn btn-ghost btn-sm" data-wt-diff="' + esc(m.id) + '">查看变更</button>' +
      '<button type="button" class="btn btn-ghost btn-sm' + (armFresh(m.id, 'merge') ? ' armed' : '') + '" data-wt-merge="' + esc(m.id) + '">' + (armFresh(m.id, 'merge') ? '确认合并？' : '合并到主分支') + '</button>' +
      '<button type="button" class="btn btn-ghost btn-sm' + (armFresh(m.id, 'discard') ? ' armed' : '') + '" data-wt-discard="' + esc(m.id) + '">' + (armFresh(m.id, 'discard') ? '确认丢弃？' : '丢弃') + '</button></div>';
    if (wtDiff && wtDiff.id === m.id) {
      html += '<details class="wt-diff" open><summary>变更摘要</summary><pre>' + esc(wtDiff.text) + '</pre></details>';
    }
    return html + '</div>';
  }
  $('execPanel').addEventListener('click', async e => {
    const missionOf = el => el?.dataset.wtDiff || el?.dataset.wtMerge || el?.dataset.wtDiscard;
    const diffBtn = e.target.closest('[data-wt-diff]');
    if (diffBtn) {
      const id = diffBtn.dataset.wtDiff;
      diffBtn.disabled = true;
      try {
        const data = await api('/api/missions/' + encodeURIComponent(id) + '/diff');
        wtDiff = { id, text: data.hasDiff ? (data.truncated ? data.text + '\n…（变更过大，已截断）' : data.text) : '（没有检测到文件变更）' };
      } catch (err) { wtDiff = { id, text: '读取变更失败：' + err.message }; }
      renderExecPanel(); return;
    }
    const mergeBtn = e.target.closest('[data-wt-merge]');
    const discardBtn = e.target.closest('[data-wt-discard]');
    if (mergeBtn || discardBtn) {
      const action = mergeBtn ? 'merge' : 'discard', id = missionOf(mergeBtn || discardBtn);
      if (!armFresh(id, action)) { wtArm = { id, action, expires: Date.now() + 6000 }; renderExecPanel(); return; }
      (mergeBtn || discardBtn).disabled = true;
      try {
        await api('/api/missions/' + encodeURIComponent(id) + '/' + action, action === 'merge' ? { message: '从对话执行面板审查合并' } : {});
        wtDiff = null; wtArm = null; await refresh();
      } catch (err) { wtDiff = null; wtArm = null; $('chatError').textContent = err.message; renderExecPanel(); }
    }
  });
  function renderExecPanel() {
    const stateEl = $('execState'), tasksEl = $('execTasks'), metricsEl = $('execMetrics');
    if (!stateEl || !tasksEl || !metricsEl) return;
    const missions = current?.missions || [];
    const running = missions.findLast(m => active(m));
    const shown = running || missions.at(-1);
    if (current?.pending) { stateEl.textContent = '思考中'; stateEl.className = 'state-chip hot'; }
    else if (shown) {
      stateEl.textContent = status[shown.status] || shown.status;
      stateEl.className = 'state-chip ' + (shown.status === 'failed' || shown.status === 'blocked' ? 'err' : ['completed', 'partial'].includes(shown.status) ? 'ok' : active(shown) ? 'hot' : '');
    } else { stateEl.textContent = '待命'; stateEl.className = 'state-chip'; }

    if (!shown || !shown.tasks.length) {
      tasksEl.innerHTML = '<div class="ep-empty">下达目标后，宰相的分工与各 Harness 的执行进度会实时显示在这里。</div>' + wtReview(shown);
    } else {
      const bar = t => t.status === 'completed' ? '<span class="bar ep-bar"><i style="width:100%;background:var(--ok)"></i></span>'
        : ['failed', 'blocked'].includes(t.status) ? '<span class="bar ep-bar"><i style="width:100%;background:var(--err)"></i></span>'
        : ['running', 'reporting'].includes(t.status) ? '<span class="bar ep-bar hot"><i style="width:62%"></i></span>'
        : '<span class="bar ep-bar"><i style="width:6%"></i></span>';
      tasksEl.innerHTML = shown.tasks.map(t => {
        const agent = agents.find(a => a.id === t.agentId);
        return '<div class="ep-task" data-task="' + esc(t.id) + '" data-mission="' + esc(shown.id) + '">' +
          '<div class="ep-task-top"><b>' + esc(t.title) + '</b>' +
          '<span class="state-chip ' + (t.status === 'completed' ? 'ok' : ['failed', 'blocked'].includes(t.status) ? 'err' : 'hot') + '">' + esc(status[t.status] || t.status) + '</span></div>' +
          '<div class="ep-task-agent"><i style="background:' + (agent ? 'linear-gradient(140deg,#6d5cf0,#4a5ce0)' : '#99a0b5') + '">' + esc((agent?.name || '?')[0]) + '</i>' + esc(agent?.name || t.agentId) + (t.model ? ' · ' + esc(t.model) : '') + '</div>' +
          bar(t) +
          '<div class="ep-task-meta"><span>已调用 ' + t.turns + ' 次</span>' + (t.dependsOn?.length ? '<span>依赖 ' + t.dependsOn.length + ' 项</span>' : '') + '</div>' +
          '</div>';
      }).join('') + wtReview(shown);
    }
    const totalTasks = missions.reduce((n, m) => n + m.tasks.length, 0);
    const doneTasks = missions.reduce((n, m) => n + m.tasks.filter(t => t.status === 'completed').length, 0);
    const rounds = missions.reduce((n, m) => n + m.round, 0);
    metricsEl.innerHTML = [
      ['协作轮次', rounds], ['任务总数', totalTasks], ['已完成', doneTasks], ['团队成员', agents.length]
    ].map(([k, v]) => '<div class="ep-metric"><small>' + k + '</small><strong>' + v + '</strong></div>').join('');
  }
  $('chatComposer').addEventListener('submit', async e => {
    e.preventDefault(); const text = $('chatInput').value.trim(); if (!text) return;
    $('sendChat').disabled = true; $('chatError').textContent = '';
    try {
      if (!selected) { const c = await api('/api/chats', defaults()); select(c.id); current = c; }
      await api('/api/chats/' + selected + '/messages', { text });
      $('chatInput').value = ''; await refresh(); $('conversationMessages').scrollTop = $('conversationMessages').scrollHeight;
    } catch (err) { $('chatError').textContent = err.message; }
    finally { $('sendChat').disabled = false; $('chatInput').focus(); }
  });
  /* ---------- @ 直接委派补全 ---------- */
  const popup = $('mentionPopup');
  function closeMention() { mention.open = false; popup.hidden = true; popup.innerHTML = ''; }
  function openMention() {
    const el = $('chatInput'), caret = el.selectionStart;
    const match = el.value.slice(0, caret).match(/(^|\s)@([^\s@，。、,]*)$/);
    if (!match || !agents.length) return closeMention();
    const from = caret - match[2].length - 1;
    const query = match[2].toLowerCase();
    const items = agents.filter(a => !query || a.name.toLowerCase().includes(query) || a.id.toLowerCase().includes(query)).slice(0, 6);
    if (!items.length) return closeMention();
    mention = { open: true, items, index: 0, from, to: caret };
    popup.hidden = false;
    popup.innerHTML = '<div class="mention-title">委派给 · 完成后仍由宰相验收汇报</div>' + items.map((a, i) =>
      '<button type="button" class="mention-item' + (i === mention.index ? ' on' : '') + '" data-mention="' + esc(a.id) + '"><b>@' + esc(a.name) + '</b><small>' + esc(a.id) + '</small></button>').join('');
  }
  function pickMention(i) {
    const a = mention.items[i == null ? mention.index : i];
    closeMention();
    if (!a) return;
    const el = $('chatInput');
    el.value = el.value.slice(0, mention.from) + '@' + a.name + ' ' + el.value.slice(mention.to);
    el.focus();
    const caret2 = mention.from + a.name.length + 2;
    el.setSelectionRange(caret2, caret2);
  }
  $('chatInput').addEventListener('input', openMention);
  $('chatInput').addEventListener('blur', () => setTimeout(closeMention, 120));
  popup.addEventListener('mousedown', e => {
    const b = e.target.closest('[data-mention]');
    if (b) { e.preventDefault(); pickMention(mention.items.findIndex(a => a.id === b.dataset.mention)); }
  });
  $('chatInput').addEventListener('keydown', e => {
    if (mention.open) {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        mention.index = (mention.index + (e.key === 'ArrowDown' ? 1 : mention.items.length - 1)) % mention.items.length;
        popup.querySelectorAll('.mention-item').forEach((el, i) => el.classList.toggle('on', i === mention.index));
        return;
      }
      if (e.key === 'Enter' || e.key === 'Tab') { e.preventDefault(); pickMention(); return; }
      if (e.key === 'Escape') { e.preventDefault(); closeMention(); return; }
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); $('chatComposer').requestSubmit(); }
  });
  $('newChat').addEventListener('click', () => newChat().catch(e => { $('chatError').textContent = e.message; }));
  $('chatHistory').addEventListener('click', e => { const el = e.target.closest('[data-chat]'); if (el) { select(el.dataset.chat); refresh(); } });
  $('conversationMessages').addEventListener('click', e => { const el = e.target.closest('[data-example]'); if (el) { $('chatInput').value = el.dataset.example; $('chatInput').focus(); } });
  $('openChatSettings').addEventListener('click', () => {
    const cfg = current?.config || defaults();
    $('chatManager').innerHTML = agents.map(a => '<option value="' + esc(a.id) + '">' + esc(a.name) + '</option>').join(''); $('chatManager').value = cfg.managerId;
    $('chatManagerModel').value = cfg.managerModel || ''; $('chatPermission').value = cfg.permission || 'readonly'; $('chatWorkdir').value = cfg.workdir || '';
    $('chatIsolate').value = cfg.isolate === 'worktree' ? 'worktree' : 'shared';
    $('workerSettings').innerHTML = agents.map(a => '<div class="worker-setting"><label><input type="checkbox" data-worker="' + esc(a.id) + '"' + (cfg.workerIds.includes(a.id) ? ' checked' : '') + '>' + esc(a.name) + '</label><input data-worker-model="' + esc(a.id) + '" aria-label="' + esc(a.name) + ' 模型" placeholder="默认模型，或填写模型名称" value="' + esc(cfg.models?.[a.id] || '') + '"></div>').join('');
    $('settingsError').textContent = ''; $('chatSettings').showModal();
  });
  $('chatSettingsForm').addEventListener('submit', async e => {
    e.preventDefault();
    const models = {}; document.querySelectorAll('[data-worker-model]').forEach(el => { models[el.dataset.workerModel] = el.value.trim(); });
    const cfg = { managerId: $('chatManager').value, managerModel: $('chatManagerModel').value.trim(), workerIds: Array.from(document.querySelectorAll('[data-worker]:checked')).map(el => el.dataset.worker), models, permission: $('chatPermission').value, workdir: $('chatWorkdir').value.trim(), isolate: $('chatIsolate').value };
    try {
      if (!cfg.workerIds.length) throw new Error('至少允许一个执行 Harness');
      if (selected) current = await api('/api/chats/' + selected + '/config', cfg, 'PATCH'); else draft = cfg;
      renderSig = ''; renderTeam(); renderConversation(); $('chatSettings').close();
    } catch (err) { $('settingsError').textContent = err.message; }
  });
  $('harnessTeam').addEventListener('click', e => {
    const native = e.target.closest('[data-native-session]');
    if (native) { nativeSelected = native.dataset.nativeSession; $('nativeSessions').showModal(); renderNative(); return; }
    const el = e.target.closest('[data-task]'); if (!el) return;
    const m = current.missions.find(m => m.id === el.dataset.mission), task = m.tasks.find(t => t.id === el.dataset.task); selectedTask = { m, task };
    $('selectedTaskTitle').textContent = task.title; $('selectedTaskInfo').textContent = (status[task.status] || task.status) + ' · 已执行 ' + task.turns + ' 次';
    $('taskHarness').innerHTML = m.workerIds.map(id => '<option value="' + esc(id) + '">' + esc(name(id)) + '</option>').join(''); $('taskHarness').value = task.agentId;
    $('taskModel').value = task.model || ''; $('taskInstructions').value = task.instructions;
    $('selectedTaskOutput').textContent = task.error || task.output || '尚未回传成果';
    $('applyTaskSettings').disabled = !active(m); $('taskSettingsError').textContent = active(m) ? '' : '本轮已结束，可在对话中要求宰相继续工作。'; $('taskSettings').showModal();
  });
  $('openNativeSessions').addEventListener('click', () => { $('nativeSessions').showModal(); renderNative(); });
  $('nativeSessionSelect').addEventListener('change', () => { nativeSelected = $('nativeSessionSelect').value; renderNative(); });
  $('chatManager').addEventListener('change', renderNative);
  $('taskHarness').addEventListener('change', renderNative);
  $('cancelNativeTurn').addEventListener('click', async () => {
    try { await api('/api/sessions/' + nativeSelected + '/cancel', {}); await refresh(); }
    catch (e) { $('nativeSessionError').textContent = e.message; }
  });
  $('nativePermissions').addEventListener('click', async e => {
    const button = e.target.closest('[data-native-permission]'); if (!button) return;
    button.disabled = true;
    try { await api('/api/session-permissions/' + button.dataset.nativePermission, { optionId: button.dataset.nativeOption }); await refresh(); }
    catch (err) { $('chatError').textContent = err.message; button.disabled = false; }
  });
  function openAcpAgent() { $('nativeSessions').close(); $('acpAgentError').textContent = ''; $('acpAgentSettings').showModal(); }
  $('openAcpAgent').addEventListener('click', openAcpAgent);
  $('openAcpFromSessions').addEventListener('click', openAcpAgent);
  $('acpAgentForm').addEventListener('submit', async e => {
    e.preventDefault();
    try {
      await api('/api/agents', { name: $('acpAgentName').value.trim(), type: $('acpAgentType').value.trim(), connection: 'acp', command: $('acpAgentCommand').value.trim(),
        args: JSON.parse($('acpAgentArgs').value), readOnlyMode: $('acpReadMode').value, workspaceMode: $('acpWriteMode').value.trim() });
      $('acpAgentSettings').close(); await refresh();
    } catch (err) { $('acpAgentError').textContent = err.message; }
  });
  $('taskSettingsForm').addEventListener('submit', async e => {
    e.preventDefault();
    const change = { taskId: selectedTask.task.id, agentId: $('taskHarness').value, model: $('taskModel').value.trim(), instructions: $('taskInstructions').value.trim() };
    try {
      await api('/api/chats/' + selected + '/messages', { text: '调整任务「' + selectedTask.task.title + '」：交给 ' + name(change.agentId) + '，模型 ' + (change.model || '使用默认') + '。要求：' + change.instructions, overrides: [change] });
      $('taskSettings').close(); await refresh();
    } catch (err) { $('taskSettingsError').textContent = err.message; }
  });
  /* ---------- 定时任务面板 ---------- */
  const autoStatus = { sent: '已触发', busy: '跳过（对话忙碌）', 'missing-chat': '对话已删除' };
  function cronHint(cron) {
    const parts = String(cron || '').trim().split(/\s+/);
    if (parts.length !== 5) return '需要 5 个字段：分 时 日 月 周';
    const [mi, h, dom, mon, dow] = parts;
    if (mi === '*' || h === '*') return '按分钟周期触发';
    const time = String(h).padStart(2, '0') + ':' + String(mi).padStart(2, '0');
    const dayMap = { 0: '日', 1: '一', 2: '二', 3: '三', 4: '四', 5: '五', 6: '六', 7: '日' };
    if (dom !== '*') return '每月 ' + dom + ' 日 ' + time;
    if (dow !== '*') {
      if (/^\d+-\d+$/.test(dow)) return '每' + (dayMap[dow.split('-')[0]] || '') + '到周' + (dayMap[dow.split('-')[1]] || '') + ' ' + time;
      const days = dow.split(',').map(d => '周' + (dayMap[d] || d));
      return days.join('、') + ' ' + time;
    }
    return '每天 ' + time + (mon !== '*' ? '（' + mon + ' 月）' : '');
  }
  const clock = ts => new Date(ts).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false });
  function renderAutomations() {
    const chatTitle = id => history.find(c => c.id === id)?.title || '（对话已删除）';
    $('automationList').innerHTML = autoList.length ? autoList.map(a => '<div class="automation-item' + (a.enabled ? '' : ' off') + '">' +
      '<div class="automation-main"><strong>' + esc(a.prompt.length > 90 ? a.prompt.slice(0, 90) + '…' : a.prompt) + '</strong>' +
      '<small>' + esc(chatTitle(a.chatId)) + ' · <code>' + esc(a.cron) + '</code> · ' + esc(cronHint(a.cron)) + '</small>' +
      '<small>' + (a.enabled ? (a.nextRunAt ? '下次 ' + esc(clock(a.nextRunAt)) : '未排期') : '已停用') +
      (a.lastStatus === 'sent' && a.lastRunAt ? ' · 上次触发 ' + esc(clock(a.lastRunAt)) : a.lastStatus && a.lastStatus !== 'sent' ? ' · 上次：' + esc(autoStatus[a.lastStatus] || a.lastStatus) : '') + '</small></div>' +
      '<div class="automation-actions"><button type="button" class="btn btn-ghost btn-sm" data-auto-toggle="' + esc(a.id) + '">' + (a.enabled ? '停用' : '启用') + '</button>' +
      '<button type="button" class="btn btn-ghost btn-sm" data-auto-delete="' + esc(a.id) + '">删除</button></div></div>').join('') : '<div class="ep-empty">还没有定时任务。创建一个，到点自动向对话发一条消息。</div>';
    $('automationChat').innerHTML = history.map(c => '<option value="' + esc(c.id) + '">' + esc(c.title) + '</option>').join('');
    if (selected && history.some(c => c.id === selected)) $('automationChat').value = selected;
  }
  $('openAutomations').addEventListener('click', async () => {
    try { autoList = await api('/api/automations'); } catch (err) { $('automationError').textContent = err.message; }
    renderAutomations(); $('automationsPanel').showModal();
  });
  $('automationList').addEventListener('click', async e => {
    const toggle = e.target.closest('[data-auto-toggle]');
    if (toggle) {
      try { await api('/api/automations/' + encodeURIComponent(toggle.dataset.autoToggle), {}, 'PATCH'); autoList = await api('/api/automations'); renderAutomations(); }
      catch (err) { $('automationError').textContent = err.message; }
      return;
    }
    const del = e.target.closest('[data-auto-delete]');
    if (del) {
      if (del.dataset.armed !== '1') { del.dataset.armed = '1'; del.textContent = '确认删除？'; return; }
      try { await api('/api/automations/' + encodeURIComponent(del.dataset.autoDelete), {}, 'DELETE'); autoList = await api('/api/automations'); renderAutomations(); }
      catch (err) { $('automationError').textContent = err.message; }
    }
  });
  $('automationForm').addEventListener('submit', async e => {
    e.preventDefault(); $('automationError').textContent = '';
    try {
      await api('/api/automations', { chatId: $('automationChat').value, prompt: $('automationPrompt').value, cron: $('automationCron').value.trim() });
      $('automationPrompt').value = ''; $('automationCron').value = '';
      autoList = await api('/api/automations'); renderAutomations();
    } catch (err) { $('automationError').textContent = err.message; }
  });
  document.querySelectorAll('[data-close-dialog]').forEach(el => el.addEventListener('click', () => $(el.dataset.closeDialog).close()));
  $('openSystem').addEventListener('click', () => { document.body.classList.remove('chat-mode'); document.querySelector('[data-page="integrate"]').click(); });
  // 「← 返回对话」按钮为静态元素，绑定由 app.js 完成（#backToChat）。
  if (/^https?:$/.test(location.protocol)) {
    refresh(); setInterval(refresh, 1800);
    const nativeEvents = new EventSource('/api/events');
    ['session', 'chat'].forEach(type => nativeEvents.addEventListener(type, () => { if (!refreshTimer) refreshTimer = setTimeout(() => { refreshTimer = null; refresh(); }, 120); }));
  }
  else { $('chatError').textContent = '请启动 npm start，并从 http://127.0.0.1:8787 打开。'; $('sendChat').disabled = true; }
})();
