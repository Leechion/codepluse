'use strict';
const { randomUUID } = require('node:crypto');

function json(text) {
  const trimmed = String(text).trim();
  try { return JSON.parse(trimmed); } catch (_) { /* CLI may wrap JSON in Markdown. */ }
  const blocks = [...trimmed.matchAll(/```(?:json)?\s*([\s\S]*?)```/gi)];
  const candidate = blocks.length === 1 ? blocks[0][1] :
    blocks.length === 0 ? trimmed.slice(trimmed.indexOf('{'), trimmed.lastIndexOf('}') + 1) : '';
  try { return JSON.parse(candidate); } catch (_) { throw new Error('Harness 未返回约定的 JSON，任务未被视为完成'); }
}

// Prefer the captured submit_result tool arguments; prose JSON stays the fallback.
function parseResult(result) {
  if (result && result.structured && typeof result.structured === 'object' && !Array.isArray(result.structured)) return result.structured;
  return json(result && result.output);
}

// Per-stage output schemas (the harness-hub take on codeg's --output-schema):
// submit_result arguments are validated against the stage's schema and a
// refused submission carries back what to fix, so the agent can retry inside
// the same turn. Prose JSON keeps working as the fallback, exactly like codeg.
const OUTPUT_SCHEMAS = {
  plan: { type: 'object', required: ['tasks'],
    properties: { message: { type: 'string' }, requiresPeerCommunication: { type: 'boolean' },
      tasks: { type: 'array', items: { type: 'object', required: ['id', 'agentId', 'title', 'instructions'],
        properties: { id: { type: 'string' }, agentId: { type: 'string' }, title: { type: 'string' }, instructions: { type: 'string' }, dependsOn: { type: 'array' } } } } } },
  worker: { type: 'object', required: ['output', 'complete'],
    properties: { output: { type: 'string' }, complete: { type: 'boolean' }, blocked: { type: 'boolean' },
      messages: { type: 'array' }, reads: { type: 'array' } } },
  report: { type: 'object', required: ['complete', 'report', 'taskVerdicts'],
    properties: { complete: { type: 'boolean' }, reply: { type: 'string' }, report: { type: 'string' },
      taskVerdicts: { type: 'array', items: { type: 'object', required: ['id', 'accepted', 'reason'],
        properties: { id: { type: 'string' }, accepted: { type: 'boolean' }, reason: { type: 'string' } } } } } },
  decision: { type: 'object', required: ['reply', 'action'],
    properties: { reply: { type: 'string' }, action: { type: 'string', enum: ['reply', 'execute', 'steer'] },
      objective: { type: 'string' }, models: { type: 'object' }, assignments: { type: 'array' } } },
  adjust: { type: 'object', required: ['updates', 'newTasks'],
    properties: { reply: { type: 'string' }, updates: { type: 'array' }, newTasks: { type: 'array' } } },
  briefing: { type: 'object', required: ['summary'], properties: { summary: { type: 'string' } } }
};
function schemaKeyFor(stage) {
  if (stage === '宰相拆解任务' || stage === '宰相修正计划') return 'plan';
  if (stage === '宰相汇总汇报') return 'report';
  if (stage === '宰相对话') return 'decision';
  if (stage === '宰相调整分工') return 'adjust';
  if (stage === '会话移交简报') return 'briefing';
  return 'worker';
}

class Orchestrator {
  constructor(hub, invoke, readFile, worktrees, skills) {
    this.hub = hub;
    this.invoke = invoke;
    this.readFile = readFile;
    this.worktrees = worktrees;
    this.skills = skills;
    this.missions = [];
  }
  emit(m) { m.updatedAt = Date.now(); this.hub.emit('mission', m); }
  eligible(id, sensitivity) {
    const a = this.hub.agents.get(id);
    if (!a || !a.enabled || !(a.real || a.communication === 'http') || a.clearance < sensitivity) {
      throw new Error('执行端未连接、已禁用或密级不足：' + id);
    }
    return a;
  }
  create(spec) {
    if (typeof spec.objective !== 'string' || !spec.objective.trim() || spec.objective.length > 12000) throw new Error('请填写 1–12000 字符的任务目标');
    const sensitivity = spec.sensitivity == null ? 1 : spec.sensitivity;
    if (!Number.isInteger(sensitivity) || sensitivity < 0 || sensitivity > 3) throw new Error('无效密级');
    const manager = this.eligible(spec.managerId, sensitivity);
    const workers = [...new Set(spec.workerIds || [])];
    if (!workers.length || workers.length > 8) throw new Error('请选择 1–8 个执行 Harness');
    workers.forEach(id => this.eligible(id, sensitivity));
    if (!['readonly', 'workspace', 'shell'].includes(spec.permission || 'readonly')) throw new Error('未知权限模式');
    if (spec.isolate != null && !['shared', 'worktree'].includes(spec.isolate)) throw new Error('隔离设置无效，仅支持 shared 或 worktree');
    const m = { id: 'mission_' + randomUUID(), objective: spec.objective.trim(), managerId: manager.id,
      workerIds: workers, sensitivity, permission: spec.permission || 'readonly', workdir: spec.workdir,
      isolate: spec.isolate === 'worktree' ? 'worktree' : 'shared',
      status: 'planning', tasks: [], events: [], report: '', error: '', round: 0, createdAt: Date.now() };
    m.managerModel = spec.managerModel || '';
    m.models = spec.models || {};
    m.chatId = spec.chatId || null;
    m.skipPlanning = spec.skipPlanning === true;
    m.pendingInstructions = [];
    m.pinnedTasks = {};
    this.missions.unshift(m); this.emit(m);
    // The HTTP request returns the mission before model calls start.
    m.completion = this.run(m).catch(e => { m.status = 'failed'; m.error = e.message; this.emit(m); });
    // Keep the asynchronous handle private and out of snapshots.
    Object.defineProperty(m, 'completion', { enumerable: false });
    return m;
  }
  event(m, from, to, text, kind = 'collab.ask') {
    const msg = this.hub.sendMessage({ from, to, text, kind, sensitivity: m.sensitivity, correlationId: m.id });
    if (msg.status === 'rejected') throw new Error(msg.auth.reason);
    m.events.push({ id: msg.id, from, to, text, kind, at: Date.now() });
    this.emit(m);
  }
  async call(m, agentId, stage, prompt, permission = 'readonly', modelOverride, taskId) {
    this.eligible(agentId, m.sensitivity);
    const schemaKey = schemaKeyFor(stage);
    const task = this.hub.submitTask({ title: stage, prompt, real: true, sensitivity: m.sensitivity,
      assignedTo: agentId, permission, workdir: m.workdir, requestedBy: m.managerId });
    task.missionId = m.id;
    this.hub.tasks.startReal(task.id, agentId, { tool: this.hub.agents.get(agentId).type, permission });
    let result;
    const managerStage = ['宰相对话', '宰相拆解任务', '宰相修正计划', '宰相调整分工', '宰相汇总汇报'].includes(stage);
    const model = modelOverride !== undefined ? modelOverride : (managerStage ? m.managerModel : (m.models || {})[agentId]) || '';
    task.requestedModel = model;
    const skillText = this.skills ? this.skills.promptFor(this.hub.agents.get(agentId)) : '';
    try { result = await this.invoke(agentId, skillText ? prompt + skillText : prompt, { permission, model, workdir: m.workdir, missionId: m.id, chatId: m.chatId || null,
      taskId: taskId || null, role: managerStage ? 'manager' : 'worker', structured: true,
      sessionKey: (m.chatId || m.id) + ':' + (managerStage ? 'manager:' + schemaKey : taskId || stage), sensitivity: m.sensitivity, stage,
      outputSchema: OUTPUT_SCHEMAS[schemaKey] }); }
    catch (e) { result = { ok: false, error: e.message }; }
    this.hub.tasks.completeReal(task.id, result);
    task.runtimeSessionId = result?.meta?.runtimeSessionId || null;
    if (task.status === 'cancelled') throw new Error('此调用已取消，忽略迟到结果');
    if (!result?.ok || !result.output?.trim()) throw new Error(result?.error || 'Harness 未返回结果');
    return { output: result.output, structured: result.structured || null };
  }
  validatePlan(m, plan) {
    if (!Array.isArray(plan.tasks) || !plan.tasks.length || plan.tasks.length > 8) throw new Error('宰相须拆分为 1–8 个子任务');
    const ids = new Set();
    for (const t of plan.tasks) {
      if (typeof t.id !== 'string' || !/^[a-zA-Z0-9_-]{1,40}$/.test(t.id) || ids.has(t.id)) throw new Error('子任务 ID 无效或重复');
      if (!m.workerIds.includes(t.agentId) || typeof t.title !== 'string' || !t.title.trim() || typeof t.instructions !== 'string' || !t.instructions.trim()) throw new Error('子任务执行端或指令无效');
      ids.add(t.id);
      if (t.dependsOn != null && (!Array.isArray(t.dependsOn) || t.dependsOn.some(id => typeof id !== 'string'))) throw new Error('依赖列表无效');
    }
    const resolved = new Set();
    for (let i = 0; i < plan.tasks.length; i++) {
      plan.tasks.forEach(t => { if ((t.dependsOn || []).every(id => resolved.has(id))) resolved.add(t.id); });
    }
    if (resolved.size !== ids.size) throw new Error('计划包含循环依赖或不存在的依赖');
    return plan.tasks.map(t => ({ id: t.id, agentId: t.agentId, title: t.title, instructions: t.instructions,
      dependsOn: t.dependsOn || [], model: (m.models || {})[t.agentId] || '', status: 'queued', output: '', inbox: [], files: [], turns: 0, error: '' }));
  }
  steer(id, text, overrides = []) {
    const m = this.missions.find(x => x.id === id);
    if (!m || !['planning', 'running', 'reporting'].includes(m.status)) throw new Error('此轮已结束，请通过对话发起后续工作');
    for (const change of overrides) {
      if (!m.tasks.some(t => t.id === change.taskId)) throw new Error('子任务不存在');
      if (change.agentId && !m.workerIds.includes(change.agentId)) throw new Error('请选择此轮允许使用的 Harness');
      if (change.model && !/^[a-zA-Z0-9][\w.:/@+\-]{0,119}$/.test(change.model)) throw new Error('模型名称无效');
    }
    m.pendingInstructions = m.pendingInstructions || [];
    m.pendingInstructions.push({ text, overrides, at: Date.now() });
    this.emit(m);
  }
  async adjust(m) {
    const pending = m.pendingInstructions.splice(0);
    if (!pending.length) return;
    const raw = parseResult(await this.call(m, m.managerId, '宰相调整分工',
      '用户正在干预当前任务。请基于已有成果调整后续分工，只输出 JSON：{"reply":"向用户解释调整","updates":[{"taskId":"已有任务ID","agentId":"允许的Harness ID","instructions":"新指令"}],"newTasks":[]}。' +
      'newTasks 如需使用，格式与原 tasks 相同；已有执行结果不能删除。人工指定的执行端与模型必须遵守。\n允许执行端：' + JSON.stringify(m.workerIds) +
      '\n原始目标：' + m.objective + '\n用户要求：' + JSON.stringify(pending) + '\n当前任务：' + JSON.stringify(m.tasks)));
    if (!Array.isArray(raw.updates || []) || !Array.isArray(raw.newTasks || [])) throw new Error('分工调整格式无效');
    const definitions = m.tasks.map(t => ({ ...t }));
    for (const update of raw.updates || []) {
      const t = definitions.find(x => x.id === update.taskId);
      if (!t) throw new Error('调整引用了不存在的子任务');
      if (update.agentId) t.agentId = update.agentId;
      if (update.instructions) t.instructions = update.instructions;
    }
    const manual = pending.flatMap(p => p.overrides);
    const pins = { ...(m.pinnedTasks || {}) };
    for (const change of manual) pins[change.taskId] = { ...(pins[change.taskId] || {}), ...change };
    definitions.forEach(t => {
      const pin = pins[t.id];
      if (pin) { if (pin.agentId) t.agentId = pin.agentId; if (pin.instructions) t.instructions = pin.instructions; }
    });
    definitions.push(...(raw.newTasks || []));
    this.validatePlan(m, { tasks: definitions });
    m.tasks = definitions.map(t => {
      const old = m.tasks.find(x => x.id === t.id);
      const changed = !old || t.agentId !== old.agentId || t.instructions !== old.instructions || manual.some(c => c.taskId === t.id);
      return { ...t, status: changed ? 'queued' : t.status, output: t.output || '', inbox: t.inbox || [], files: t.files || [],
        turns: t.turns || 0, error: changed ? '' : t.error, model: pins[t.id]?.model ?? (changed ? (m.models || {})[t.agentId] || '' : t.model) };
    });
    m.pinnedTasks = pins;
    m.directiveHistory = [...(m.directiveHistory || []), ...pending];
    m.adjustments = m.adjustments || [];
    m.adjustments.push({ reply: raw.reply || '已更新后续分工。', at: Date.now() });
    m.report = ''; m.status = 'running'; this.emit(m);
  }
  async run(m) {
    if (m.isolate === 'worktree' && m.workdir) {
      m.baseWorkdir = m.workdir;
      try {
        if (!this.worktrees) throw new Error('服务未启用工作树隔离');
        m.worktree = await this.worktrees.for(m.id, m.baseWorkdir);
        m.workdir = m.worktree;
      } catch (e) {
        // Isolation is best-effort: non-git directories fall back to shared work.
        m.isolationError = '无法创建隔离工作树，已回退共享目录：' + e.message;
      }
      this.emit(m);
    }
    this.event(m, 'hub', m.managerId, m.objective, 'task.assign');
    if (m.skipPlanning) {
      // Direct @-delegation: no manager planning call, the delegate executes the
      // objective verbatim and the manager still owns verification and reporting.
      const agentId = m.workerIds[0];
      m.tasks = this.validatePlan(m, { tasks: [{ id: 't1', agentId, title: '直接委派', instructions: m.objective }] });
      const name = this.hub.agents.get(agentId)?.name || agentId;
      m.planMessage = '已直接委派给 ' + name + '（不经拆解），完成后由宰相验收汇报。';
      m.status = 'running'; this.emit(m);
      m.tasks.forEach(t => this.event(m, m.managerId, t.agentId, JSON.stringify({ taskId: t.id, title: t.title, instructions: t.instructions }), 'task.assign'));
      return this.work(m);
    }
    const roster = m.workerIds.map(id => { const a = this.hub.agents.get(id); return { id, name: a.name, model: (m.models || {})[id] || 'CLI 默认模型', capabilities: a.capabilities }; });
    let rawPlan = await this.call(m, m.managerId, '宰相拆解任务',
      '你是宰相，负责分解用户目标并交给真实 Harness 执行。优先通过 submit_result 工具提交 JSON（字段作为工具参数传入），工具不可用时才在正文中只输出 JSON，不要执行子任务。\n' +
      '格式：{"message":"向用户解释你决定如何分工","requiresPeerCommunication":false,"tasks":[{"id":"t1","agentId":"名单中的ID","title":"标题","instructions":"可执行指令与验收要求","dependsOn":[]}]}。' +
      '用户要求执行端相互发送消息或双向通信时 requiresPeerCommunication=true，并在指令中明确要求实际 messages 请求。任务板输出不能替代消息。你负责最终汇报，不必额外拆一个汇报子任务。' +
      '拆成 1–8 项，尽可能让不同执行端分工，明确依赖；禁止编造执行端或已完成结果。\n' +
      '只读不等于禁止读取命令：Codex 使用只读命令读取文件。若用户未禁止，不要禁止文件读取所需的命令。需要互相询问的任务应避免把回答者设置成提问者的前置依赖。\n' +
      '用户目标：' + m.objective + '\n执行端（agentId 必须逐字符复制 id，禁止省略数字）：' + JSON.stringify(roster));
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        const plan = parseResult(rawPlan); m.tasks = this.validatePlan(m, plan); m.planMessage = plan.message || '';
        m.requiresPeerCommunication = plan.requiresPeerCommunication === true || /双向|相互.{0,12}(消息|通信|发送)|互相.{0,12}(消息|通信|发送)|互发/.test(m.objective);
        break;
      }
      catch (error) {
        if (attempt) throw error;
        rawPlan = await this.call(m, m.managerId, '宰相修正计划',
          '上一次计划验证失败：' + error.message + '。只修正无效字段，返回同一 JSON tasks 格式（优先用 submit_result 工具提交），agentId 必须逐字符复制名单中的 id。\n' +
          '执行端名单：' + JSON.stringify(roster) + '\n用户目标：' + m.objective + '\n原计划：' + rawPlan.output);
      }
    }
    m.status = 'running'; this.emit(m);
    m.tasks.forEach(t => this.event(m, m.managerId, t.agentId, JSON.stringify({ taskId: t.id, title: t.title, instructions: t.instructions }), 'task.assign'));
    return this.work(m);
  }
  async work(m) {
    for (let round = 1; round <= 6; round++) {
      await this.adjust(m);
      m.round = round;
      const runnable = m.tasks.filter(t => (t.status === 'queued' || t.inbox.length) &&
        t.dependsOn.every(id => m.tasks.find(x => x.id === id).status === 'completed'));
      if (!runnable.length) break;
      // Each wave receives the same board. Deliver messages after the wave so
      // concurrent responses never consume or overwrite an unread inbox.
      const board = JSON.stringify(m.tasks.map(({ id, agentId, title, dependsOn, status, output }) => ({ id, agentId, title, dependsOn, status, output })));
      const outcomes = [];
      // Writes share a workspace, so run them sequentially; read-only work can overlap.
      const execute = async t => {
        const inbox = t.inbox.splice(0); t.status = 'running'; t.turns++; this.emit(m);
        try {
          const response = parseResult(await this.call(m, t.agentId, t.title + ' · 第 ' + t.turns + ' 轮',
            '你是执行 Harness。实际完成指派工作，可读取共享分工和同僚消息。不要声称未执行的工作已完成。\n' +
            '优先通过 submit_result 工具提交结果，把 JSON 字段作为工具参数传入；工具不可用时才在正文中只输出 JSON：{"output":"实际成果、文件路径和验证依据，或明确阻塞原因","complete":true,"blocked":false,"reads":[{"path":"相对工作目录的文件路径","startLine":1,"lineCount":200}],"messages":[{"to":"子任务ID","text":"给同僚的消息或问题"}]}。' +
            '你运行在真实的 Harness 会话中，可以使用自己的已授权原生工具。读取文件工具不可用时，可用 reads 数组请求服务端读取，在下一轮 files 中返回内容，此时 complete=false、blocked=false。已有 files 内容可直接使用，不要重复请求。\n' +
            (m.permission === 'readonly' ? '当前只读：可以读取和检索文件，不能修改文件或执行写入命令；权限请求会受当前只读模式限制。\n' : '可使用 Harness 原生工具完成授权范围内的实际修改或命令执行，需授权时等待用户选择；工具不可用时如实返回 blocked=true。\n') +
            '需要同僚回答时 complete=false。messages 是实际投递请求，只能发给当前任务板中的其他子任务；不要重复发送相同消息。\n' +
            '如果任务要求相互通信或互相核对，必须真实返回 messages，不能只看任务板后宣称已经交流。\n' +
            'complete 表示工作实际达成，不是本次回复结束。受阻、无法读取文件、未验证目标时必须 complete=false；环境或权限导致无法继续时同时 blocked=true，避免重复调用。不要在 output 声称已发出没有放入 messages 的消息。\n' +
            '当前 inbox 是最新已投递消息；全体任务板的 output 是历史产出，若矛盾请以 inbox 为准。\n' +
            '用户目标：' + m.objective + '\n追加要求：' + JSON.stringify(m.directiveHistory || []) + '\n你的子任务：' + JSON.stringify({ ...t, inbox }) + '\n全体任务板：' + board, m.permission, t.model, t.id));
          if (typeof response.output !== 'string' || !response.output.trim() || typeof response.complete !== 'boolean' ||
              (response.messages != null && !Array.isArray(response.messages))) throw new Error('执行结果格式无效');
          const messages = response.messages || [];
          const reads = response.reads || [];
          if (!Array.isArray(reads) || reads.length > 4 || reads.some(r => !r || typeof r.path !== 'string' || !r.path)) throw new Error('文件读取请求无效');
          if (messages.length > 8 || messages.some(x => !x || x.to === t.id || !m.tasks.some(v => v.id === x.to) || typeof x.text !== 'string' || !x.text.trim() || x.text.length > 6000)) throw new Error('同僚消息目标或内容无效');
          t.output = response.output; t.status = response.blocked === true ? 'blocked' : response.complete ? 'completed' : 'queued';
          for (const read of reads) {
            let content;
            try {
              if (!this.readFile) throw new Error('当前执行服务未配置文件读取');
              content = await this.readFile(m, read);
            } catch (e) { content = { path: read.path, error: e.message }; }
            t.files = t.files.filter(f => f.path !== content.path);
            t.files.push(content);
            if (t.files.length > 12) t.files.shift();
            this.event(m, 'hub', t.agentId, JSON.stringify({ path: content.path, read: !content.error, error: content.error }), 'ctx.sync');
            t.status = 'queued';
          }
          outcomes.push({ task: t, messages });
          this.event(m, t.agentId, m.managerId, JSON.stringify({ taskId: t.id, complete: response.complete, output: t.output }).slice(0, 20000), 'task.result');
        } catch (e) { t.status = 'failed'; t.error = e.message; }
        this.emit(m);
      };
      if (m.permission === 'readonly') await Promise.all(runnable.map(execute));
      else for (const t of runnable) await execute(t);
      for (const { task, messages } of outcomes) for (const msg of messages) {
        const target = m.tasks.find(t => t.id === msg.to);
        if (m.events.some(e => e.taskFrom === task.id && e.taskTo === target.id && e.text === msg.text)) continue;
        this.event(m, task.agentId, target.agentId, msg.text);
        Object.assign(m.events[m.events.length - 1], { taskFrom: task.id, taskTo: target.id });
        target.inbox.push({ from: task.id, text: msg.text });
        if (target.status === 'completed') target.status = 'queued';
      }
      this.emit(m);
      if (m.requiresPeerCommunication && !m.communicationRequested && m.tasks.every(t => t.status === 'completed') && !this.peerConfirmed(m)) {
        const first = m.tasks[0], second = m.tasks.find(t => t.agentId !== first.agentId);
        if (second) {
          for (const [task, peer] of [[first, second], [second, first]]) {
            task.inbox.push({ from: '宰相', text: '用户要求实际双向通信。请通过 messages 向子任务 ' + peer.id + ' 发送你的结论，并处理其回信；不能只引用任务板作为通信证据。' });
            task.status = 'queued';
          }
          m.communicationRequested = true; this.emit(m);
        }
      }
      if (m.tasks.every(t => t.status === 'completed' && !t.inbox.length) && !m.pendingInstructions?.length) break;
    }
    if (m.pendingInstructions?.length) { await this.adjust(m); return this.work(m); }
    const complete = m.tasks.every(t => t.status === 'completed' && !t.inbox.length) && (!m.requiresPeerCommunication || this.peerConfirmed(m));
    m.status = 'reporting'; this.emit(m);
    const verdict = parseResult(await this.call(m, m.managerId, '宰相汇总汇报',
      '你是宰相，现在验收真实成果并向用户汇报。优先通过 submit_result 工具提交 JSON（字段作为工具参数传入），工具不可用时才在正文中只输出 JSON：{"complete":true,"reply":"直接给用户的自然语言汇报","report":"详细验收依据","taskVerdicts":[{"id":"子任务ID","accepted":true,"reason":"验收依据或未完成原因"}]}。' +
      'reply 必须遵守用户要求的格式和长度，默认用2-4句直接汇报成果和未完成项，不要堆砌内部ID或日志；详细证明放在 report。' +
      '逐项验收所有子任务，检查 output 中是否有受阻、未读取或未验证。complete 只有当用户目标实际全部达成时为 true。报告包括分工、成果、证据、失败和未解决事项。' +
      '不得把子任务声明当作独立验证；通信事实只能依据下面的实际消息记录，禁止编造问答。用户明确要求消息交换时，任务板不能作为等效替代；调度器判定未全部完成时必须如实报告未完成。\n' +
      '调度器是否全部完成：' + complete + '\n用户目标：' + m.objective + '\n追加要求：' + JSON.stringify(m.directiveHistory || []) + '\n任务板：' + JSON.stringify(m.tasks) +
      '\n实际消息记录：' + JSON.stringify(m.events.filter(e => e.kind === 'collab.ask'))));
    if (m.pendingInstructions?.length) { await this.adjust(m); return this.work(m); }
    if (typeof verdict.complete !== 'boolean' || typeof verdict.report !== 'string' || !verdict.report.trim() ||
        !Array.isArray(verdict.taskVerdicts) || verdict.taskVerdicts.length !== m.tasks.length ||
        new Set(verdict.taskVerdicts.map(v => v.id)).size !== m.tasks.length ||
        verdict.taskVerdicts.some(v => !m.tasks.some(t => t.id === v.id) || typeof v.accepted !== 'boolean' || typeof v.reason !== 'string')) throw new Error('宰相未返回完整的逐项验收结论');
    m.report = verdict.report;
    m.reply = typeof verdict.reply === 'string' && verdict.reply.trim() ? verdict.reply : verdict.report;
    m.verdicts = verdict.taskVerdicts;
    m.verdicts.forEach(v => { if (!v.accepted) { const t = m.tasks.find(t => t.id === v.id); t.status = 'failed'; t.error = v.reason; } });
    m.status = complete && verdict.complete && m.verdicts.every(v => v.accepted) ? 'completed' : 'partial'; m.finishedAt = Date.now();
    if (m.status === 'partial' && m.requiresPeerCommunication && !this.peerConfirmed(m)) {
      m.reply = '本轮部分完成：尚无可核验的双向通信记录。\n\n' + m.reply;
    }
    this.event(m, m.managerId, 'hub', m.report.slice(0, 20000), 'task.result');
    this.emit(m);
  }
  // Saturating walk of the forkedFrom chain (codeg's delegation depth rule):
  // a cap bounds the walk so a long or corrupted chain cannot loop unbounded.
  forkDepth(m, cap = 5) {
    let current = m, depth = 0;
    while (depth < cap && current?.forkedFrom) {
      current = this.missions.find(x => x.id === current.forkedFrom);
      if (!current) break;
      depth++;
    }
    return depth;
  }
  // Codeg-style fork: branch a finished board into a fresh mission. Outputs and
  // files carry over as context; every task re-queues and dependencies persist.
  fork(id, taskId) {
    const m = this.missions.find(x => x.id === id);
    if (!m) throw new Error('任务不存在');
    if (!['completed', 'partial', 'failed'].includes(m.status)) throw new Error('任务尚未结束，不能分叉');
    if (this.forkDepth(m) >= 5) throw new Error('分叉嵌套已达上限（5 层），请基于更早的任务重新规划');
    const keep = new Set();
    const add = tid => { if (keep.has(tid)) return; keep.add(tid); const t = m.tasks.find(x => x.id === tid); (t?.dependsOn || []).forEach(add); };
    if (taskId) {
      if (!m.tasks.some(t => t.id === taskId)) throw new Error('子任务不存在');
      add(taskId);
    } else m.tasks.forEach(t => keep.add(t.id));
    const tasks = m.tasks.filter(t => keep.has(t.id)).map(t => ({ ...t, status: 'queued', output: t.output || '', inbox: [], files: t.files || [], turns: 0, error: '' }));
    const f = { id: 'mission_' + randomUUID(), objective: m.objective, managerId: m.managerId,
      workerIds: [...m.workerIds], sensitivity: m.sensitivity, permission: m.permission,
      workdir: m.baseWorkdir || m.workdir, isolate: m.isolate, skipPlanning: true, chatId: m.chatId || null,
      status: 'running', tasks, events: [], report: '', error: '', round: 0, createdAt: Date.now() };
    f.managerModel = m.managerModel; f.models = { ...(m.models || {}) };
    f.pendingInstructions = []; f.pinnedTasks = {}; f.forkedFrom = m.id;
    this.missions.unshift(f); this.emit(f);
    f.completion = this.work(f).catch(e => { f.status = 'failed'; f.error = e.message; this.emit(f); });
    Object.defineProperty(f, 'completion', { enumerable: false });
    return f;
  }
  peerConfirmed(m) {
    const messages = m.events.filter(e => e.kind === 'collab.ask' && e.from !== e.to);
    return messages.some(e => messages.some(reply => reply.from === e.to && reply.to === e.from));
  }
}
module.exports = { Orchestrator, json, parseResult, OUTPUT_SCHEMAS, schemaKeyFor };
