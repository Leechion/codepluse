'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { Automations, parseCron, cronMatches, nextRun } = require('../core/automations');
const Core = require('../core/engine');
const { Orchestrator } = require('../core/orchestrator');
const { ChatController } = require('../core/chat-controller');

function chatsWith(invoke) {
  const hub = new Core.Hub({ autoSeed: false, mode: 'real' });
  ['manager', 'alpha'].forEach(id => { const a = hub.createAgent({ id, type: 'codex', name: id }); a.real = true; });
  const orchestrator = new Orchestrator(hub, invoke);
  const chats = new ChatController(hub, orchestrator);
  const chat = chats.create({ managerId: 'manager', workerIds: ['alpha'] });
  return { hub, chats, chat };
}

test('cron parsing accepts standard syntax and rejects bad fields', () => {
  const cron = parseCron('0 9 * * 1-5');
  assert.equal(cronMatches(cron, new Date(2026, 10, 2, 9, 0)), true);   // Monday
  assert.equal(cronMatches(cron, new Date(2026, 10, 1, 9, 0)), false);  // Sunday
  assert.equal(cronMatches(parseCron('*/15 8-18 * * *'), new Date(2026, 0, 1, 8, 45)), true);
  assert.equal(cronMatches(parseCron('30 2 29 2 *'), new Date(2028, 1, 29, 2, 30)), true);
  assert.throws(() => parseCron('0 9 * *'), /5 个字段/);
  assert.throws(() => parseCron('60 9 * * *'), /取值无效/);
  assert.throws(() => parseCron('a 9 * * *'), /取值无效/);
});

test('nextRun finds the next matching minute within a year', () => {
  const from = new Date(2026, 10, 6, 15, 30); // Friday
  const next = new Date(nextRun(parseCron('0 9 * * 1-5'), from));
  assert.equal(next.getDay(), 1); assert.equal(next.getHours(), 9);
  assert.deepEqual([next.getDate(), next.getMonth()], [9, 10], 'skips the weekend to Monday');
});

test('automations create, disable and trigger through the chat queue', async () => {
  const { chats, chat } = chatsWith(async () => ({ ok: true, output: JSON.stringify({ reply: '收到', action: 'reply' }) }));
  const automations = new Automations(chats, { file: ':memory:' });
  assert.throws(() => automations.create({ chatId: chat.id, prompt: 'x', cron: 'bad cron' }), /5 个字段/);
  assert.throws(() => automations.create({ chatId: 'chat_missing', prompt: 'x', cron: '* * * * *' }), /对话不存在/);
  const record = automations.create({ chatId: chat.id, prompt: '每日站会前汇总', cron: '30 8 * * 1-5' });
  assert.equal(record.enabled, true);
  assert.equal(record.nextRunAt > Date.now(), true);

  // Simulate the scheduled minute arriving.
  const due = new Date(nextRun(parseCron(record.cron)));
  automations.tick(due);
  assert.equal(record.lastStatus, 'sent');
  assert.equal(chat.messages.at(-1).text, '每日站会前汇总');
  assert.equal(record.lastRunAt, due.getTime());

  // Busy chats skip instead of queueing a second orchestration.
  chat.pending = 1;
  chats.queues.set(chat.id, Promise.resolve());
  chats.active = () => ({ status: 'running' });
  record.nextRunAt = due.getTime(); record.lastMinuteKey = null;
  automations.tick(due);
  assert.equal(record.lastStatus, 'busy');

  automations.update(record.id, { enabled: false });
  assert.equal(automations.records[0].enabled, false);
  automations.remove(record.id);
  assert.equal(automations.records.length, 0);
  assert.throws(() => automations.remove(record.id), /不存在/);
});
