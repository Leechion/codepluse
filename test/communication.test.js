'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { once } = require('node:events');
const { spawn } = require('node:child_process');
const http = require('node:http');
const Core = require('../core/engine');
const Communication = require('../core/communication');
const HttpAdapter = require('../core/http-adapter');
const { createBridge } = require('../adapters/cli-bridge');

async function listen(server) {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return 'http://127.0.0.1:' + server.address().port;
}
function close(server) { server.closeAllConnections(); return new Promise(resolve => server.close(resolve)); }

test('HTTP delivery, authenticated bridge reply, correlation and audit', async () => {
  let calls = 0;
  const bridge = createBridge(async message => {
    calls++; return { ok: true, output: 'reply: ' + message.payload.text };
  }, { token: 'test-token' });
  const endpoint = await listen(bridge);
  try {
    const hub = new Core.Hub({ autoSeed: false });
    const from = hub.createAgent({ type: 'codex', name: 'Sender', clearance: 2 });
    const to = hub.createAgent({ type: 'claude-code', name: 'Receiver', clearance: 2 });
    const adapter = new HttpAdapter({ endpoint, token: 'test-token' });
    const comm = new Communication(hub, () => message => adapter.send(message));
    const msg = await comm.send({ from: from.id, to: to.id, text: 'hello', sensitivity: 1 });
    assert.equal(msg.delivery.status, 'delivered');
    const reply = hub.bus.messages.find(m => m.id === msg.delivery.replyId);
    assert.equal(reply.payload.text, 'reply: hello');
    assert.equal(reply.from, to.id);
    assert.equal(reply.to, from.id);
    assert.equal(reply.replyTo, msg.id);
    assert.equal(reply.correlationId, msg.correlationId);
    assert.equal(calls, 1, 'a reply must not recursively invoke another Harness');
    assert.equal(hub.verifyAuditChain().ok, true);
    to.enabled = false;
    assert.equal((await comm.send({ from: 'hub', to: to.id, text: 'blocked' })).status, 'rejected');
    assert.equal(calls, 1);
  } finally { await close(bridge); }
});

test('ACL, endpoint failures and invalid destinations do not report delivery', async () => {
  const hub = new Core.Hub({ autoSeed: false });
  const a = hub.createAgent({ type: 'codex', name: 'A', clearance: 3 });
  const b = hub.createAgent({ type: 'claude-code', name: 'B', clearance: 3 });
  let calls = 0;
  const comm = new Communication(hub, () => async () => { calls++; throw new Error('remote failure'); });
  const session = hub.context.createSession(a.id, { sensitivity: 3, entries: [] });
  hub.grant({ subjectId: b.id, objectId: '*', scope: 'read', maxSensitivity: 3 });
  const blocked = await comm.send({ from: a.id, to: b.id, text: 'secret', sensitivity: 3, sessionId: session.id });
  assert.equal(blocked.status, 'rejected');
  assert.equal(calls, 0);
  const failed = await comm.send({ from: a.id, to: b.id, text: 'hello' });
  assert.equal(failed.delivery.status, 'failed');
  assert.match(failed.delivery.error, /remote failure/);
  await assert.rejects(comm.send({ from: a.id, to: 'broadcast', text: 'no' }));
  assert.equal(calls, 1);
});

test('bridge token and response errors propagate as failures', async () => {
  let calls = 0;
  const bridge = createBridge(async () => { calls++; return { ok: false }; }, { token: 'secret' });
  const endpoint = await listen(bridge);
  try {
    await assert.rejects(new HttpAdapter({ endpoint }).send({}), /401/);
    assert.equal(calls, 0);
    await assert.rejects(new HttpAdapter({ endpoint, token: 'secret' }).send({ id: 'x', from: 'a', to: 'b', payload: { text: 'hello' } }), /502/);
    assert.equal(calls, 1);
  } finally { await close(bridge); }
});

test('REST registration and delivery reach a separate HTTP Harness', { timeout: 15000 }, async () => {
  const received = [];
  const bridge = createBridge(async m => {
    received.push(m);
    const text = m.payload.text;
    if (text.startsWith('你是用户的宰相')) return { ok: true, output: JSON.stringify({ reply: '我来安排。', action: 'execute', objective: 'HTTP 真实链路核对' }) };
    if (text.startsWith('你是宰相，负责分解')) return { ok: true, output: JSON.stringify({ message: '交给执行端核对。', tasks: [{ id: 't1', agentId: m.to, title: 'HTTP任务', instructions: '核对' }] }) };
    if (text.startsWith('你是执行 Harness')) return { ok: true, output: JSON.stringify({ complete: true, output: 'HTTP worker result' }) };
    if (text.startsWith('你是宰相，现在验收')) return { ok: true, output: JSON.stringify({ complete: true, report: 'HTTP 汇报完成', taskVerdicts: [{ id: 't1', accepted: true, reason: 'HTTP result checked' }] }) };
    return { ok: true, output: 'external:' + text };
  }, { token: 'api-test' });
  const endpoint = await listen(bridge);
  const reservation = http.createServer();
  const base = await listen(reservation);
  const port = reservation.address().port;
  await close(reservation);
  const child = spawn(process.execPath, ['server.js'], {
    cwd: require('node:path').resolve(__dirname, '..'), windowsHide: true,
    env: { ...process.env, PORT: String(port), HOST: '127.0.0.1', HARNESS_HUB_REAL: '1', HARNESS_HUB_STATE_FILE: ':memory:' },
    stdio: ['ignore', 'pipe', 'pipe']
  });
  try {
    await new Promise((resolve, reject) => {
      child.stdout.on('data', d => { if (d.toString().includes('Console')) resolve(); });
      child.once('error', reject);
      child.once('exit', code => reject(new Error('server exited: ' + code)));
    });
    const post = async (path, value) => (await fetch(base + path, {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(value)
    })).json();
    const registered = await post('/api/agents', { type: 'test-http', name: 'External', endpoint, token: 'api-test', clearance: 2 });
    assert.equal(registered.data.adapterKind, 'HttpHarnessAdapter');
    assert.equal(JSON.stringify(registered).includes('api-test'), false);
    const sent = await post('/api/messages', { from: 'hub', to: registered.data.id, text: 'ping', deliver: true });
    assert.equal(sent.data.delivery.status, 'delivered');
    const snapshot = await (await fetch(base + '/api/snapshot')).json();
    assert.equal(snapshot.data.messages.find(m => m.id === sent.data.delivery.replyId).payload.text, 'external:ping');
    assert.equal(JSON.stringify(snapshot).includes('api-test'), false);
    const direct = await post('/api/messages', { from: 'hub', to: registered.data.id, text: 'real by default' });
    assert.equal(direct.data.delivery.status, 'delivered');
    assert.equal(snapshot.data.tasks.length, 0, 'startup must not create simulated tasks');
    const id = registered.data.id;
    const conversation = await post('/api/chats', { managerId: id, workerIds: [id], managerModel: 'manager-test', models: { [id]: 'worker-test' } });
    assert.equal(conversation.ok, true);
    const turn = await post('/api/chats/' + conversation.data.id + '/messages', { text: '请核对 HTTP 链路并汇报' });
    assert.equal(turn.data.messages[0].role, 'user');
    let completed;
    for (let i = 0; i < 100; i++) {
      const view = await (await fetch(base + '/api/chats/' + conversation.data.id)).json();
      completed = view.data;
      if (completed.missions[0]?.status === 'completed' && !completed.pending) break;
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.equal(completed.missions[0].status, 'completed');
    assert.equal(completed.messages.at(-1).text, 'HTTP 汇报完成');
    assert.equal(received.find(m => m.payload.text.startsWith('你是用户的宰相')).payload.model, 'manager-test');
    assert.equal(received.find(m => m.payload.text.startsWith('你是执行 Harness')).payload.model, 'worker-test');
    const native = await post('/api/agents', { name: 'Protocol fixture', type: 'test-acp', connection: 'acp', command: process.execPath,
      args: [require('node:path').resolve(__dirname, '../fixtures/acp-agent.cjs')], readOnlyMode: 'read-only', workspaceMode: 'workspace-write' });
    assert.equal(native.ok, true);
    const remember = await post('/api/messages', { from: 'hub', to: native.data.id, text: 'remember native-api-memory' });
    assert.equal(remember.data.delivery.status, 'delivered');
    const recall = await post('/api/messages', { from: 'hub', to: native.data.id, text: 'recall' });
    const messages = await (await fetch(base + '/api/messages')).json();
    assert.equal(messages.data.find(m => m.id === recall.data.delivery.replyId).payload.text, 'native-api-memory');
    const sessions = await (await fetch(base + '/api/sessions')).json();
    assert.equal(sessions.data.find(s => s.agentId === native.data.id).turns.length, 2);
  } finally {
    child.kill();
    await once(child, 'exit');
    await close(bridge);
  }
});
