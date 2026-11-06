'use strict';
const { parseResult } = require('./orchestrator');

// Codeg-style session handoff: move an ongoing native conversation from one
// agent to another. Instead of parsing each vendor's private on-disk history
// (fragile, versioned by every CLI), the hub compresses the transcript it
// already holds and opens a fresh native session on the target agent.
async function handoffSession({ acpRuntime, orchestrator, hub, chats }, sessionId, toAgentId) {
  const source = acpRuntime.records.find(r => r.id === sessionId);
  if (!source) throw new Error('原生会话不存在');
  if (source.handedOff) throw new Error('此会话已移交过，不能再移交');
  const transcript = acpRuntime.exportTranscript(sessionId);
  if (!transcript.trim()) throw new Error('源会话没有可移交的执行记录');
  const target = hub.agents.get(toAgentId);
  if (!target || !target.enabled || !target.real) throw new Error('目标执行端未连接或已禁用');
  if (toAgentId === source.agentId) throw new Error('目标执行端就是当前会话的执行端');
  const fromAgent = hub.agents.get(source.agentId);

  const manager = pickManager(source, chats, hub, target);
  const control = { id: 'handoff_' + sessionId, chatId: source.chatId || null, managerId: manager.id,
    managerModel: manager.model || '', models: {}, sensitivity: 1, workdir: source.cwd, permission: 'readonly' };
  let briefing = '';
  try {
    const res = await orchestrator.call(control, manager.id, '会话移交简报',
      '你是宰相。以下是一次 Harness 会话的执行纪要，请把它压缩为给接手执行端的移交简报。优先通过 submit_result 工具提交，字段：{"summary":"简报"}。\n' +
      '简报包含：正在进行的任务与目标、已完成的关键结论与证据、未完成事项与下一步、需要注意的约束。用中文，不超过 600 字。\n' +
      '会话纪要：' + transcript.slice(0, 60000));
    const parsed = parseResult(res);
    briefing = String(parsed.summary || parsed.reply || res.output || '').slice(0, 6000);
  } catch (_) { briefing = ''; }
  const opening = '你将接手一位同僚（' + (fromAgent?.name || source.agentId) + '）此前负责的工作。' +
    '以下是移交简报与原始执行纪要；不要重复已完成的工作，基于这些上下文继续。\n\n' +
    '## 移交简报\n' + (briefing || '（简报生成失败，请直接依据纪要判断。）') + '\n\n## 原始执行纪要\n' + transcript.slice(0, 60000) +
    '\n\n请先用一两句话确认你已理解当前状态。';
  const result = await acpRuntime.prompt(target, opening, { sessionKey: 'handoff:' + source.id,
    permission: source.permission, workdir: source.cwd, chatId: source.chatId || null, structured: false, stage: '会话移交' });
  if (!result.ok) throw new Error('会话移交失败：' + (result.error || '目标执行端未确认接手'));
  source.handedOff = true; source.handedOffTo = toAgentId; acpRuntime.save();
  return { sessionId: source.id, handedOffTo: toAgentId, briefing, newSessionKey: 'handoff:' + source.id, output: result.output };
}

function pickManager(source, chats, hub, fallback) {
  if (source.chatId) {
    try {
      const chat = chats.get(source.chatId);
      const a = hub.agents.get(chat.config.managerId);
      if (a && a.enabled) return { id: a.id, model: chat.config.managerModel };
    } catch (_) { /* fall through */ }
  }
  let pick = null;
  hub.agents.forEach(a => { if (!pick && a.enabled && a.real) pick = a; });
  return { id: (pick || fallback).id, model: '' };
}
module.exports = { handoffSession };
