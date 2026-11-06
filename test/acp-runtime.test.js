'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { Hub } = require('../core/engine');
const { AcpRuntime } = require('../core/acp-runtime');

async function fixture(run) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'harness-acp-test-'));
  fs.writeFileSync(path.join(root, 'package.json'), '{"name":"actual-test-file"}');
  const hub = new Hub({ autoSeed: false });
  const runtime = new AcpRuntime(hub, { stateFile: path.join(root, 'sessions.json'), timeoutMs: 10000,
    launch: () => ({ command: process.execPath, args: [path.resolve(__dirname, '../fixtures/acp-agent.cjs')], env: process.env }) });
  const agent = { id: 'agent', realTool: 'codex' };
  const options = { workdir: root, permission: 'readonly', sessionKey: 'chat:worker', chatId: 'chat', structured: false };
  try { await run({ runtime, agent, options, hub }); }
  finally {
    await runtime.shutdown();
    assert.equal(path.dirname(path.resolve(root)), path.resolve(os.tmpdir()));
    fs.rmSync(root, { recursive: true, force: true });
  }
}
async function until(check) { for (let i = 0; i < 150; i++) { if (check()) return; await new Promise(resolve => setTimeout(resolve, 10)); } throw new Error('Timed out'); }

test('ACP reuses a native session, preserves its context and streams real tool updates', async () => fixture(async ({ runtime, agent, options, hub }) => {
  let notifications = 0; hub.on('session', () => notifications++);
  const first = await runtime.prompt(agent, 'remember token42', options);
  const second = await runtime.prompt(agent, 'recall', options);
  assert.equal(first.meta.sessionId, second.meta.sessionId);
  assert.equal(second.output, 'token42'); assert.equal(runtime.records.length, 1);
  await runtime.prompt(agent, 'tool', options);
  const view = runtime.list('chat')[0];
  assert.equal(view.turns.at(-1).tools[0].status, 'completed');
  assert.equal(view.turns.at(-1).tools[0].title, 'Read package.json');
  assert.ok(notifications > 6);
}));

test('ACP changes the model within the same native context and exposes the catalogue', async () => fixture(async ({ runtime, agent, options }) => {
  const first = await runtime.prompt(agent, 'remember still-here', options);
  const next = await runtime.prompt(agent, 'recall', { ...options, model: 'fixture-v2' });
  assert.equal(next.meta.sessionId, first.meta.sessionId); assert.equal(next.output, 'still-here');
  assert.equal(next.meta.model, 'fixture-v2'); assert.equal(runtime.list()[0].models.availableModels.length, 2);
  await assert.rejects(runtime.prompt(agent, 'no', { ...options, model: 'not-a-model' }), /Unknown model/);
}));

test('ACP permission requests wait for a real user choice and read-only requests are refused', async () => fixture(async ({ runtime, agent, options }) => {
  const refused = await runtime.prompt(agent, 'permission', options);
  assert.equal(refused.output, 'cancelled'); assert.equal(runtime.approvals.size, 0);
  const pending = runtime.prompt(agent, 'permission', { ...options, permission: 'workspace', timeoutMs: 80 });
  await until(() => runtime.approvals.size === 1);
  await new Promise(resolve => setTimeout(resolve, 150));
  assert.equal(runtime.records.find(r => r.permission === 'workspace').state, 'requires_action', 'human approval time must not consume the model timeout');
  const id = [...runtime.approvals.keys()][0];
  assert.throws(() => runtime.respondPermission(id, 'made-up'), /无效/);
  runtime.respondPermission(id, 'yes');
  assert.equal((await pending).output, 'yes');
  assert.throws(() => runtime.respondPermission(id, 'yes'), /失效/);
}));

test('ACP cancellation stops the active turn and client file reads are real and scoped', async () => fixture(async ({ runtime, agent, options }) => {
  const read = await runtime.prompt(agent, 'read', options);
  assert.match(read.output, /actual-test-file/);
  assert.throws(() => runtime.within(options.workdir, path.resolve(options.workdir, '..')), /超出/);
  const pending = runtime.prompt(agent, 'wait', options);
  await until(() => runtime.records[0].activeTurn != null);
  // Wait until the fixture has actually received the prompt.
  await new Promise(resolve => setTimeout(resolve, 30));
  await runtime.cancel(runtime.records[0].id);
  const result = await pending; assert.equal(result.ok, false); assert.equal(result.meta.stopReason, 'cancelled');
}));

test('structured turns inject the result MCP server and capture submit_result rawInput', async () => fixture(async ({ runtime, agent, options }) => {
  const plain = await runtime.prompt(agent, 'mcp', options);
  assert.equal(plain.output, 'mcp:0', 'unstructured sessions must not receive extra MCP servers');
  const withTool = await runtime.prompt(agent, 'mcp', { ...options, sessionKey: 'chat:worker2', structured: true });
  assert.equal(withTool.output, 'mcp:1', 'structured orchestrator sessions get the submit_result sink');
  assert.equal(withTool.structured, null);
  const submitted = await runtime.prompt(agent, 'submit', { ...options, sessionKey: 'chat:worker2', structured: true });
  assert.equal(submitted.ok, true);
  assert.deepEqual(submitted.structured, { output: 'tool says done', complete: true });
  assert.equal(runtime.list('chat').length, 2);
}));

test('extractSubmitted only treats result-shaped rawInput as a submission', () => {
  const { extractSubmitted } = require('../core/acp-runtime');
  assert.equal(extractSubmitted([]), null);
  assert.equal(extractSubmitted([{ title: 'Read', rawInput: { path: 'a.js', startLine: 1 } }]), null);
  assert.equal(extractSubmitted([{ title: 'Bad', rawInput: '{not json' }]), null);
  assert.deepEqual(extractSubmitted([{ title: 'Read', rawInput: { path: 'a' } }, { title: 'submit_result', rawInput: '{"output":"x","complete":true}' }]), { output: 'x', complete: true });
  assert.deepEqual(extractSubmitted([{ title: 'submit_result', rawInput: { tasks: [] } }]), { tasks: [] });
});
