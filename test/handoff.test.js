'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { Hub } = require('../core/engine');
const { AcpRuntime } = require('../core/acp-runtime');
const { Orchestrator } = require('../core/orchestrator');
const { ChatController } = require('../core/chat-controller');
const { handoffSession } = require('../core/handoff');

async function fixture(run) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'harness-handoff-test-'));
  const hub = new Hub({ autoSeed: false });
  ['manager', 'agent', 'agent2'].forEach(id => { const a = hub.createAgent({ id, type: 'codex', name: id }); a.real = true; a.realTool = 'codex'; });
  const runtime = new AcpRuntime(hub, { stateFile: path.join(root, 'sessions.json'), timeoutMs: 10000,
    launch: () => ({ command: process.execPath, args: [path.resolve(__dirname, '../fixtures/acp-agent.cjs')], env: process.env }) });
  const orchestrator = new Orchestrator(hub, async (id, prompt, options) => {
    assert.equal(options.stage, '会话移交简报');
    return { ok: true, output: JSON.stringify({ summary: '任务过半：token42 已记录，剩余校验未做。' }) };
  });
  const chats = new ChatController(hub, orchestrator);
  const chat = chats.create({ managerId: 'manager', workerIds: ['agent', 'agent2'] });
  const agent = { id: 'agent', realTool: 'codex' };
  const agent2 = { id: 'agent2', realTool: 'codex' };
  const options = { workdir: root, permission: 'readonly', sessionKey: 'chat:worker', chatId: chat.id, structured: false };
  try { await run({ runtime, orchestrator, chats, chat, hub, agent, agent2, options }); }
  finally {
    await runtime.shutdown();
    fs.rmSync(root, { recursive: true, force: true });
  }
}

test('handoff requires another connected agent and rejects repeats', async () => fixture(async ctx => {
  const { runtime, orchestrator, chats, chat, hub, agent, agent2, options } = ctx;
  await runtime.prompt(agent, 'remember token42', options);
  const source = runtime.records[0];
  source.chatId = chat.id;
  const deps = { acpRuntime: runtime, orchestrator, hub, chats };
  await assert.rejects(handoffSession(deps, 'session_missing', 'agent2'), /不存在/);
  await assert.rejects(handoffSession(deps, source.id, 'agent'), /当前会话的执行端/);
  await assert.rejects(handoffSession(deps, source.id, 'ghost'), /未连接或已禁用/);
  const result = await handoffSession(deps, source.id, 'agent2');
  assert.equal(result.handedOffTo, 'agent2');
  assert.match(result.briefing, /token42/);
  assert.equal(source.handedOff, true);
  assert.equal(runtime.records.length, 2, 'the target gets its own native session');
  await assert.rejects(runtime.prompt(agent, 'again', options), /已移交/);
}));

test('failed handoff leaves the source usable and allows retry', async () => fixture(async ctx => {
  const { runtime, orchestrator, chats, hub, agent, options } = ctx;
  await runtime.prompt(agent, 'remember token42', options);
  const source = runtime.records[0];
  const deps = { acpRuntime: runtime, orchestrator, hub, chats };
  const prompt = runtime.prompt.bind(runtime);
  runtime.prompt = async () => ({ ok: false, error: 'adapter unavailable' });
  await assert.rejects(handoffSession(deps, source.id, 'agent2'), /adapter unavailable/);
  assert.equal(source.handedOff, undefined);
  assert.equal(source.handedOffTo, undefined);
  runtime.prompt = prompt;
  const reply = await runtime.prompt(agent, 'recall', options);
  assert.equal(reply.output, 'token42');
  const result = await handoffSession(deps, source.id, 'agent2');
  assert.equal(result.handedOffTo, 'agent2');
  assert.equal(source.handedOff, true);
}));
