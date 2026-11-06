'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const Core = require('../core/engine');
const { ChatController } = require('../core/chat-controller');
const { Orchestrator } = require('../core/orchestrator');
const { RealHarnessAdapter } = require('../core/real-adapters');

function setup(invoke) {
  const hub = new Core.Hub({ autoSeed: false, mode: 'real' });
  ['manager', 'alpha', 'beta'].forEach(id => { const a = hub.createAgent({ id, name: id, type: 'codex' }); a.real = true; });
  const orchestrator = new Orchestrator(hub, invoke);
  const chats = new ChatController(hub, orchestrator);
  const chat = chats.create({ managerId: 'manager', workerIds: ['alpha', 'beta'], managerModel: 'manager-model', models: { alpha: 'worker-model' } });
  return { hub, orchestrator, chats, chat };
}
const plan = { message: '我会交给 alpha 核对。', tasks: [{ id: 't1', agentId: 'alpha', title: '核对', instructions: 'Read and check' }] };
const report = { complete: true, reply: '核对完成。', report: '核对完成。详细验收依据。', taskVerdicts: [{ id: 't1', accepted: true, reason: 'Checked' }] };

test('a chat turn makes a real manager decision and launches an autonomous mission', async () => {
  const calls = [];
  const { chats, chat, orchestrator } = setup(async (id, prompt, options) => {
    calls.push({ id, model: options.model, stage: options.stage });
    if (options.stage === '宰相对话') return { ok: true, output: JSON.stringify({ reply: '我来安排核对。', action: 'execute', objective: '核对项目' }) };
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify(plan) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: JSON.stringify(report) };
    return { ok: true, output: JSON.stringify({ complete: true, output: 'Verified' }) };
  });
  chats.send(chat.id, '帮我核对项目'); await chats.queues.get(chat.id);
  const m = orchestrator.missions[0]; await m.completion;
  assert.equal(m.chatId, chat.id); assert.equal(m.status, 'completed');
  assert.deepEqual(chat.messages.map(t => t.role), ['user', 'assistant', 'assistant', 'assistant']);
  assert.equal(chat.messages.at(-1).text, '核对完成。');
  assert.equal(chat.messages.at(-1).details, '核对完成。详细验收依据。');
  assert.equal(calls.find(c => c.stage === '宰相对话').model, 'manager-model');
  assert.equal(calls.find(c => c.id === 'alpha').model, 'worker-model');
  assert.equal(chat.pending, 0);
});

test('discussion stays in dialogue and retains previous context', async () => {
  let count = 0;
  const { chats, chat, orchestrator } = setup(async (id, prompt) => {
    count++;
    if (count === 2) assert.match(prompt, /先讨论/);
    return { ok: true, output: JSON.stringify({ reply: '我们先确认范围。', action: 'reply' }) };
  });
  chats.send(chat.id, '先讨论，不执行'); await chats.queues.get(chat.id);
  chats.send(chat.id, '范围是登录模块'); await chats.queues.get(chat.id);
  assert.equal(orchestrator.missions.length, 0); assert.equal(chat.messages.length, 4);
});

test('human assignment and model changes apply after the active worker finishes', async () => {
  let release, started;
  const gate = new Promise(resolve => { release = resolve; });
  const ready = new Promise(resolve => { started = resolve; });
  const calls = [];
  const { orchestrator } = setup(async (id, prompt, options) => {
    calls.push({ id, model: options.model });
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify(plan) };
    if (options.stage === '宰相调整分工') return { ok: true, output: JSON.stringify({ reply: '改由 beta 接手。', updates: [{ taskId: 't1', agentId: 'alpha', instructions: 'New check' }] }) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: JSON.stringify(report) };
    if (id === 'alpha') { started(); await gate; }
    return { ok: true, output: JSON.stringify({ complete: true, output: 'Actual result' }) };
  });
  const m = orchestrator.create({ objective: 'Check', managerId: 'manager', workerIds: ['alpha', 'beta'] });
  await ready;
  orchestrator.steer(m.id, '交给 beta，使用 gpt-6-luna', [{ taskId: 't1', agentId: 'beta', model: 'gpt-6-luna', instructions: 'Check carefully' }]);
  assert.equal(m.tasks[0].agentId, 'alpha', 'an in-flight call must not be relabeled');
  release(); await m.completion;
  assert.equal(m.status, 'completed'); assert.equal(m.tasks[0].agentId, 'beta');
  assert.equal(m.tasks[0].instructions, 'Check carefully');
  assert.equal(calls.find(c => c.id === 'beta').model, 'gpt-6-luna');
  assert.equal(m.adjustments[0].reply, '改由 beta 接手。');
});

test('natural-language model choices are forwarded and invented model choices are rejected', async () => {
  const { chats, chat } = setup(async () => ({ ok: true, output: JSON.stringify({ reply: '模型已设置。', action: 'reply', models: { alpha: 'gpt-6-luna' } }) }));
  chats.send(chat.id, 'alpha 使用 gpt-6-luna'); await chats.queues.get(chat.id);
  assert.equal(chat.config.models.alpha, 'gpt-6-luna');
  chats.send(chat.id, '你好'); await chats.queues.get(chat.id);
  assert.equal(chat.messages.at(-1).role, 'error');
});

test('@ mentions delegate directly without manager planning and still report via the manager', async () => {
  const calls = [];
  const { chats, chat, orchestrator } = setup(async (id, prompt, options) => {
    calls.push({ id, stage: options.stage });
    if (options.stage === '宰相汇总汇报') return { ok: true, output: JSON.stringify(report) };
    return { ok: true, output: JSON.stringify({ complete: true, output: 'Delegated work done' }) };
  });
  chats.send(chat.id, '@alpha 检查登录模块'); await chats.queues.get(chat.id);
  assert.equal(orchestrator.missions.length, 1);
  const m = orchestrator.missions[0]; await m.completion;
  assert.equal(m.skipPlanning, true);
  assert.deepEqual(m.workerIds, ['alpha']);
  assert.equal(m.objective, '检查登录模块', 'the mention token is stripped from the objective');
  assert.equal(m.status, 'completed');
  assert.equal(calls.some(c => c.stage === '宰相拆解任务'), false, 'planning is skipped');
  assert.equal(calls.some(c => c.stage === '宰相汇总汇报'), true, 'manager still verifies');
  assert.equal(chat.messages.some(t => t.key === m.id + ':plan' && /直接委派/.test(t.text)), true);
  assert.equal(chat.messages.some(t => t.key === m.id + ':final'), true);
});

test('mentions resolve by agent name, dedupe, and unknown mentions fail loudly', async () => {
  const { chats, chat, orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相汇总汇报') return { ok: true, output: JSON.stringify(report) };
    return { ok: true, output: JSON.stringify({ complete: true, output: 'done' }) };
  });
  chats.send(chat.id, '@alpha 和 @alpha 都看一下'); await chats.queues.get(chat.id);
  assert.equal(orchestrator.missions.length, 1, 'duplicate mentions collapse');
  chats.send(chat.id, '换个说法 @alpha'); await chats.queues.get(chat.id);
  chats.send(chat.id, '@不存在的家伙 做事'); await chats.queues.get(chat.id);
  assert.equal(chat.messages.at(-1).role, 'error');
  assert.match(chat.messages.at(-1).text, /未找到执行端 @不存在的家伙/);
  await Promise.all(orchestrator.missions.map(m => m.completion));
});

test('explicit mentions in the API body delegate without @ syntax', async () => {
  const { chats, chat, orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相汇总汇报') return { ok: true, output: JSON.stringify(report) };
    return { ok: true, output: JSON.stringify({ complete: true, output: 'done' }) };
  });
  chats.send(chat.id, '检查登录模块', [], ['beta']); await chats.queues.get(chat.id);
  assert.equal(orchestrator.missions.length, 1);
  assert.deepEqual(orchestrator.missions[0].workerIds, ['beta']);
  assert.equal(orchestrator.missions[0].objective, '检查登录模块');
  await orchestrator.missions[0].completion;
});

test('both CLI adapters pass the explicit model as a distinct argument', () => {  for (const tool of ['codex', 'claude']) {
    const adapter = new RealHarnessAdapter({ tool, model: 'gpt-6-luna' });
    const args = adapter._argv('out').args;
    assert.equal(args[args.indexOf('--model') + 1], 'gpt-6-luna');
  }
});

test('the standalone CLI bridge forwards the selected model and rejects unsupported permissions', async () => {
  const { createCliHandler } = require('../adapters/cli-bridge');
  const options = [];
  class Adapter {
    constructor(spec) { options.push(spec); }
    async dispatch() { return { ok: true, output: 'done' }; }
  }
  const bridge = createCliHandler({ tool: 'codex', workdir: '.' }, Adapter);
  assert.equal((await bridge.handle({ id: 'a', payload: { text: 'Hello', model: 'gpt-6-luna' } })).ok, true);
  assert.equal(options[0].model, 'gpt-6-luna');
  assert.equal((await bridge.handle({ id: 'b', payload: { text: 'Edit', permission: 'workspace' } })).ok, false);
  assert.equal(options.length, 1);
});
