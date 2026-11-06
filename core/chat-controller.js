'use strict';
const { randomUUID } = require('node:crypto');
const { parseResult } = require('./orchestrator');
const terminal = m => ['completed', 'partial', 'failed'].includes(m.status);

class ChatController {
  constructor(hub, orchestrator, save = () => {}) {
    this.hub = hub; this.orchestrator = orchestrator; this.save = save;
    this.chats = []; this.queues = new Map();
    hub.on('mission', m => this.onMission(m));
  }
  emit(chat) { chat.updatedAt = Date.now(); this.save(this.chats); this.hub.emit('chat', { id: chat.id, updatedAt: chat.updatedAt }); }
  agents() { return [...this.hub.agents.values()].filter(a => a.enabled && (a.real || a.communication === 'http')); }
  config(value = {}) {
    const roster = this.agents();
    const managerId = value.managerId || roster.find(a => a.realTool === 'claude')?.id || roster[0]?.id;
    if (!managerId) throw new Error('请先接入一个真实 Harness');
    this.orchestrator.eligible(managerId, 1);
    const workerIds = value.workerIds || roster.map(a => a.id);
    if (!Array.isArray(workerIds) || !workerIds.length || workerIds.length > 8) throw new Error('请选择 1–8 个执行 Harness');
    workerIds.forEach(id => this.orchestrator.eligible(id, 1));
    const models = value.models || {};
    if (!models || typeof models !== 'object' || Array.isArray(models)) throw new Error('模型配置无效');
    for (const model of [value.managerModel, ...Object.values(models)]) {
      if (model != null && (typeof model !== 'string' || (model && !/^[a-zA-Z0-9][\w.:/@+\-]{0,119}$/.test(model)))) throw new Error('模型名称无效');
    }
    if (!['readonly', 'workspace', 'shell'].includes(value.permission || 'readonly')) throw new Error('权限配置无效');
    if (value.isolate != null && !['shared', 'worktree'].includes(value.isolate)) throw new Error('隔离设置无效');
    return { managerId, managerModel: value.managerModel || '', workerIds: [...new Set(workerIds)], models,
      permission: value.permission || 'readonly', workdir: value.workdir, isolate: value.isolate || 'shared' };
  }
  create(value = {}) {
    const chat = { id: 'chat_' + randomUUID(), title: '新对话', config: this.config(value), messages: [], missionIds: [], pending: 0, createdAt: Date.now() };
    this.chats.unshift(chat); this.emit(chat); return chat;
  }
  get(id) { const chat = this.chats.find(c => c.id === id); if (!chat) throw new Error('对话不存在'); return chat; }
  active(chat) { return [...chat.missionIds].reverse().map(id => this.orchestrator.missions.find(m => m.id === id)).find(m => m && !terminal(m)); }
  view(chat) { return { ...chat, missions: chat.missionIds.map(id => this.orchestrator.missions.find(m => m.id === id)).filter(Boolean) }; }
  message(chat, role, text, extra = {}) {
    const item = { id: 'turn_' + randomUUID(), role, text, at: Date.now(), ...extra };
    chat.messages.push(item); this.emit(chat); return item;
  }
  configure(id, patch) { const chat = this.get(id); chat.config = this.config({ ...chat.config, ...patch }); this.emit(chat); return chat; }
  send(id, text, overrides = [], mentions = []) {
    const chat = this.get(id);
    if (typeof text !== 'string' || !text.trim() || text.length > 12000) throw new Error('请输入 1–12000 字符的消息');
    if (!Array.isArray(overrides) || overrides.length > 8) throw new Error('人工分工无效');
    if (!Array.isArray(mentions) || mentions.length > 4 || mentions.some(m => typeof m !== 'string' || !m.trim())) throw new Error('委派名单无效');
    // Record input immediately. Actual manager turns are serialized per chat.
    const user = this.message(chat, 'user', text.trim());
    if (chat.title === '新对话') chat.title = text.trim().slice(0, 36);
    chat.pending = (chat.pending || 0) + 1; this.emit(chat);
    const previous = this.queues.get(id) || Promise.resolve();
    const work = previous.then(() => this.decide(chat, user, overrides, mentions)).catch(e => {
      this.message(chat, 'error', e.message);
    }).finally(() => { chat.pending--; this.emit(chat); if (this.queues.get(id) === work) this.queues.delete(id); });
    this.queues.set(id, work);
    return user;
  }
  parseMentions(text) {
    const tokens = [...String(text || '').matchAll(/@([^\s@，。、,]+)/g)].map(m => m[1]);
    if (!tokens.length) return [];
    const roster = this.agents();
    return tokens.map(token => {
      const hit = roster.find(a => a.id === token || a.name === token ||
        a.id.toLowerCase() === token.toLowerCase() || a.name.toLowerCase() === token.toLowerCase());
      if (!hit) throw new Error('未找到执行端 @' + token + '，可用：' + (roster.map(a => a.name).join('、') || '（无已连接 Harness）'));
      return hit.id;
    });
  }
  delegate(chat, user, agentIds) {
    // Codeg-style @-delegation: each mention starts an independent mission that
    // skips manager planning; the manager still verifies and reports.
    const cfg = chat.config;
    const objective = (user.text.replace(/@[^\s@，。、,]+/g, '').trim() || user.text.trim()).slice(0, 12000);
    const created = [];
    for (const agentId of [...new Set(agentIds)]) {
      const m = this.orchestrator.create({ ...cfg, objective, managerId: cfg.managerId, workerIds: [agentId], skipPlanning: true, chatId: chat.id });
      chat.missionIds.push(m.id);
      created.push(m);
    }
    this.emit(chat);
    return created;
  }
  async decide(chat, user, overrides, extraMentions = []) {
    const cfg = chat.config;
    const mentioned = [...new Set([...this.parseMentions(user.text), ...extraMentions])];
    if (mentioned.length) { this.delegate(chat, user, mentioned); return; }
    const active = this.active(chat);
    const control = { id: chat.id, chatId: chat.id, managerId: cfg.managerId, managerModel: cfg.managerModel, models: cfg.models,
      sensitivity: 1, workdir: cfg.workdir, permission: 'readonly' };
    const prompt = '你是用户的宰相和主要负责人。以自然中文与用户对话，主动调用执行 Harness 完成明确的工作请求，不要仅表示能够做。' +
      '你可以自主挑选名单中的 Harness，规划器随后会根据能力和模型分工；尊重用户指定的执行端、模型和任务。\n' +
      '优先通过 submit_result 工具提交 JSON（字段作为工具参数传入），工具不可用时才在正文中只输出 JSON：{"reply":"直接对用户说的话","action":"reply 或 execute 或 steer","objective":"本次要执行或调整的具体要求","models":{},"assignments":[]}。' +
      '用户明确指定模型时，models 可填写 {"Harness ID":"用户给出的完整模型名"}；不得自行发明或改写模型名称。' +
      '用户指定正在执行的子任务由谁做时，assignments 填写 [{"taskId":"已有子任务ID","agentId":"允许的Harness ID","model":"可选模型名","instructions":"调整后要求"}]。其他情况留空。' +
      '闲聊或确实缺少必要信息才 reply；新的明确任务 execute；有执行中的任务且用户补充要求或调整分工时 steer。' +
      '不要向用户展示内部 JSON。不能声称任务已经执行或消息已发出。\n' +
      '当前设置：' + JSON.stringify(cfg) + '\n真实 Harness 名单：' + JSON.stringify(this.agents().map(a => ({ id: a.id, name: a.name, capabilities: a.capabilities }))) +
      '\n执行中的任务：' + JSON.stringify(active ? { id: active.id, objective: active.objective, tasks: active.tasks } : null) +
      '\n对话记录（当前消息及之前）：' + JSON.stringify(chat.messages.slice(0, chat.messages.indexOf(user) + 1).slice(-24)) +
      '\n人工指定分工（必须遵守）：' + JSON.stringify(overrides);
    const decision = parseResult(await this.orchestrator.call(control, cfg.managerId, '宰相对话', prompt));
    if (typeof decision.reply !== 'string' || !decision.reply.trim() || !['reply', 'execute', 'steer'].includes(decision.action)) throw new Error('宰相未返回有效的对话决策');
    if (decision.action !== 'reply' && (typeof decision.objective !== 'string' || !decision.objective.trim())) throw new Error('宰相未给出具体执行要求');
    const current = this.active(chat);
    const instruction = (user.text + '\n宰相理解：' + (decision.objective || '')).slice(0, 12000);
    const suggested = decision.assignments || [];
    if (!Array.isArray(suggested) || suggested.length > 8) throw new Error('宰相返回的人工分工格式无效');
    const changes = [...suggested, ...overrides];
    const models = decision.models || {};
    if (!models || typeof models !== 'object' || Array.isArray(models)) throw new Error('宰相返回的模型设置无效');
    for (const [id, model] of Object.entries(models)) {
      if (!cfg.workerIds.includes(id) || typeof model !== 'string' || !model || !user.text.toLowerCase().includes(model.toLowerCase())) throw new Error('请写出完整模型名称，或在模型设置中指定');
    }
    const effective = this.config({ ...cfg, models: { ...cfg.models, ...models } });
    chat.config = effective;
    if (changes.length && !current) throw new Error('原任务已经结束，请在对话中发起后续工作');
    if ((decision.action === 'steer' || changes.length || Object.keys(models).length) && current) {
      const modelChanges = current.tasks.filter(t => models[t.agentId]).map(t => ({ taskId: t.id, model: models[t.agentId] }));
      this.orchestrator.steer(current.id, instruction, [...modelChanges, ...changes]);
      this.message(chat, 'assistant', decision.reply, { agentId: cfg.managerId, model: cfg.managerModel, note: '调整将在当前调用结束后生效', missionId: current.id });
    } else if (decision.action !== 'reply') {
      if (current) {
        this.orchestrator.steer(current.id, instruction, overrides);
        this.message(chat, 'assistant', decision.reply, { agentId: cfg.managerId, missionId: current.id, note: '追加工作已进入后续调度' });
      } else {
        this.message(chat, 'assistant', decision.reply, { agentId: cfg.managerId, model: cfg.managerModel });
        const m = this.orchestrator.create({ ...effective, objective: instruction, chatId: chat.id });
        chat.missionIds.push(m.id); this.emit(chat);
      }
    } else this.message(chat, 'assistant', decision.reply, { agentId: cfg.managerId, model: cfg.managerModel });
  }
  onMission(m) {
    if (!m.chatId) return;
    const chat = this.chats.find(c => c.id === m.chatId);
    if (!chat) return;
    if (m.planMessage && !chat.messages.some(t => t.key === m.id + ':plan')) {
      this.message(chat, 'assistant', m.planMessage, { key: m.id + ':plan', missionId: m.id, agentId: m.managerId });
    }
    (m.adjustments || []).forEach((a, i) => {
      const key = m.id + ':adjust:' + i;
      if (!chat.messages.some(t => t.key === key)) this.message(chat, 'assistant', a.reply, { key, missionId: m.id, agentId: m.managerId });
    });
    if (terminal(m) && !chat.messages.some(t => t.key === m.id + ':final')) {
      this.message(chat, m.status === 'failed' ? 'error' : 'assistant', m.reply || m.report || m.error,
        { key: m.id + ':final', missionId: m.id, agentId: m.managerId, details: m.reply && m.reply !== m.report ? m.report : '', note: m.status === 'completed' ? '验收完成' : '包含未完成事项' });
    }
    this.emit(chat);
  }
}
module.exports = { ChatController };
