'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const http = require('node:http');
const { Notifier } = require('../core/notifier');

function fakeHub() {
  const handlers = [];
  return { on: (type, fn) => { if (type === 'mission') handlers.push(fn); }, emitMission: m => handlers.forEach(fn => fn(m)) };
}
const mission = { id: 'mission_x', chatId: 'chat_1', status: 'completed', objective: '分析项目', reply: '已完成验收', workerIds: ['a', 'b'], finishedAt: 123 };

test('terminal missions are pushed to the webhook exactly once on success', async t => {
  const received = [];
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', c => body += c);
    req.on('end', () => { received.push(JSON.parse(body)); res.writeHead(200); res.end('ok'); });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  const hub = fakeHub();
  const notifier = new Notifier(hub, { url: 'http://127.0.0.1:' + server.address().port + '/hook', log: () => {} });
  const deliveries = [], push = notifier.push.bind(notifier);
  notifier.push = (...args) => { const delivery = push(...args); deliveries.push(delivery); return delivery; };
  hub.emitMission({ ...mission, status: 'running' });
  hub.emitMission(mission);
  hub.emitMission({ ...mission, id: 'mission_y', status: 'partial' });
  assert.deepEqual(await Promise.all(deliveries), [true, true]);
  assert.deepEqual(received.map(r => r.status).sort(), ['completed', 'partial'], 'only terminal states are pushed; independent requests may arrive in either order');
  const completed = received.find(r => r.missionId === 'mission_x');
  assert.equal(completed.type, 'mission.finished');
  assert.deepEqual(completed.workerIds, ['a', 'b']);
});

test('a failing webhook is retried once and never throws', async () => {
  let hits = 0;
  const server = http.createServer((req, res) => { hits++; res.writeHead(500); res.end('no'); });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const hub = fakeHub();
  const notifier = new Notifier(hub, { url: 'http://127.0.0.1:' + server.address().port + '/hook', log: () => {} });
  const delivered = await notifier.push(notifier.payload(mission));
  assert.equal(delivered, false);
  assert.equal(hits, 2, 'one silent retry, then give up');
  server.close();
});

test('no url configured means notifications stay disabled', async () => {
  const hub = fakeHub();
  const notifier = new Notifier(hub, {});
  assert.equal(await notifier.push(notifier.payload(mission)), false);
});
