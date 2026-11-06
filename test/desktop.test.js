'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const http = require('node:http');
const { probe, ensureBackend } = require('../desktop/backend');

test('desktop reuses the Go backend and rejects an unrelated service', async t => {
  let value = { ok: true, data: { status: 'ok', sessionRuntime: 'acp-v1', version: '2.0.0-go' } };
  const server = http.createServer((_req, res) => { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(value)); });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  const port = server.address().port;
  assert.equal(await probe(port), 'hub');
  assert.deepEqual(await ensureBackend('unused-root', port), { started: false });
  value = { ok: true };
  assert.equal(await probe(port), 'other');
  await assert.rejects(ensureBackend('unused-root', port), /占用/);
  await assert.rejects(ensureBackend('unused-root', NaN), /端口无效/);
});

test('desktop reports a missing backend instead of opening an empty window', async () => {
  const server = http.createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  assert.equal(await probe(port), 'offline');
  await assert.rejects(ensureBackend(__dirname, port), /未找到 Go 后端/);
});
