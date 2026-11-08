'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const readline = require('node:readline');
const path = require('node:path');

const SERVER = path.resolve(__dirname, '../core/result-mcp.js');

function rpc(child, msg) {
  return new Promise((resolve, reject) => {
    const onLine = line => {
      let data;
      try { data = JSON.parse(line); } catch (_) { return; }
      if (data.id === msg.id) { child._lines.off('line', onLine); resolve(data); }
    };
    child._lines.on('line', onLine);
    child.stdin.write(JSON.stringify(msg) + '\n');
    setTimeout(() => { child._lines.off('line', onLine); reject(new Error('result-mcp timeout')); }, 3000);
  });
}

test('result-mcp server exposes submit_result and echoes structured submissions', async () => {
  const child = spawn(process.execPath, [SERVER], { stdio: ['pipe', 'pipe', 'pipe'] });
  child._lines = readline.createInterface({ input: child.stdout });
  try {
    const init = await rpc(child, { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } });
    assert.equal(init.result.serverInfo.name, 'harness-result');
    const list = await rpc(child, { jsonrpc: '2.0', id: 2, method: 'tools/list' });
    assert.equal(list.result.tools.length, 1);
    assert.equal(list.result.tools[0].name, 'submit_result');
    const call = await rpc(child, { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'submit_result', arguments: { output: 'done', complete: true } } });
    assert.equal(call.result.isError, undefined);
    assert.deepEqual(JSON.parse(call.result.content[0].text), { received: true });
    const bad = await rpc(child, { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'submit_result', arguments: null } });
    assert.equal(bad.result.isError, true);
    const unknown = await rpc(child, { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'read_file', arguments: { path: 'x' } } });
    assert.equal(unknown.result.isError, true);
  } finally { child.kill(); }
});

test('the sink accepts any object shape and rejects non-objects', () => {
  const { createSink } = require('../core/result-mcp');
  const call = createSink();
  assert.deepEqual(call('submit_result', { message: 'plan', tasks: [] }), { received: true });
  assert.deepEqual(call('submit_result', { output: '', complete: false }), { received: true });
  assert.throws(() => call('submit_result', null), /参数对象/);
  assert.throws(() => call('submit_result', [1, 2]), /参数对象/);
  assert.throws(() => call('submit_result', {}), /参数对象/);
  assert.throws(() => call('other_tool', {}), /Unknown tool/);
});

test('a staged schema refuses bad submissions with what to fix, then stops refusing', () => {
  const { createSink, MAX_REFUSED } = require('../core/result-mcp');
  const schema = { type: 'object', required: ['output', 'complete'],
    properties: { output: { type: 'string' }, complete: { type: 'boolean' },
      messages: { type: 'array', items: { type: 'object', required: ['to', 'text'], properties: { to: { type: 'string' }, text: { type: 'string' } } } } } };
  const call = createSink(schema);
  assert.throws(() => call('submit_result', { output: 3, complete: true }), /output 必须是字符串/);
  assert.throws(() => call('submit_result', { output: 'ok' }), /缺少必填字段 complete/);
  assert.throws(() => call('submit_result', { output: 'ok', complete: true, messages: [{ to: 't1' }] }), /messages\[0\].*缺少必填字段 text/);
  assert.deepEqual(call('submit_result', { output: 'ok', complete: true, messages: [{ to: 't1', text: 'hi' }] }), { received: true });
  // A stubborn model is refused at most MAX_REFUSED times, then let through.
  const fresh = createSink(schema);
  const outcomes = [];
  for (let i = 0; i <= MAX_REFUSED; i++) {
    try { outcomes.push(['passed', fresh('submit_result', { complete: 'no' })]); }
    catch (e) { outcomes.push(['refused', e.message]); }
  }
  assert.equal(outcomes.filter(o => o[0] === 'refused').length, MAX_REFUSED, 'exactly MAX_REFUSED refusals');
  assert.deepEqual(outcomes[MAX_REFUSED], ['passed', { received: true, note: '已多次拒绝，本次放行，由服务端最终校验' }]);
});

test('an enum schema rejects values outside the set', () => {
  const { createSink } = require('../core/result-mcp');
  const call = createSink({ type: 'object', required: ['reply', 'action'], properties: { reply: { type: 'string' }, action: { type: 'string', enum: ['reply', 'execute', 'steer'] } } });
  assert.throws(() => call('submit_result', { reply: '好', action: 'dance' }), /必须是 "reply" \/ "execute" \/ "steer"/);
  assert.deepEqual(call('submit_result', { reply: '好', action: 'steer' }), { received: true });
});
