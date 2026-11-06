'use strict';
const fs = require('node:fs');
const path = require('node:path');

// Codeg-style Automations: a saved chat prompt that runs on a cron schedule.
// An automation is nothing more than "send this message to that chat as if the
// user typed it", so orchestration, permissions, audit and SSE all come for
// free through the existing ChatController. Cron is a self-contained 5-field
// parser (minute hour day-of-month month day-of-week) — no runtime deps.
const FIELD_RANGES = [[0, 59], [0, 23], [1, 31], [1, 12], [0, 6]];
const MAX_SCAN_MINUTES = 366 * 24 * 60;

function parseField(spec, index) {
  const [min, max] = FIELD_RANGES[index];
  const values = new Set();
  for (const part of String(spec).split(',')) {
    if (!part) throw new Error('cron 字段无效：' + spec);
    const [rangeRaw, stepRaw] = part.split('/');
    const step = stepRaw == null ? 1 : Number(stepRaw);
    if (!Number.isInteger(step) || step < 1) throw new Error('cron 步长无效：' + part);
    let lo, hi;
    if (rangeRaw === '*') { lo = min; hi = max; }
    else if (rangeRaw.includes('-')) {
      const bounds = rangeRaw.split('-').map(Number);
      lo = bounds[0]; hi = bounds[1];
    } else { lo = hi = Number(rangeRaw); }
    if (!Number.isInteger(lo) || !Number.isInteger(hi) || lo < min || hi > max || lo > hi) {
      throw new Error('cron 取值无效：' + part + '（字段范围 ' + min + '-' + max + '）');
    }
    for (let v = lo; v <= hi; v += step) values.add(v);
  }
  return values;
}

function parseCron(cron) {
  const fields = String(cron || '').trim().split(/\s+/);
  if (fields.length !== 5) throw new Error('cron 需要 5 个字段：分 时 日 月 周');
  const parsed = fields.map(parseField);
  if (parsed[4].has(7)) parsed[4].add(0); // 7 means Sunday, like the crontab convention
  return parsed;
}

function cronMatches(cron, date) {
  return cron[0].has(date.getMinutes()) && cron[1].has(date.getHours()) &&
    cron[2].has(date.getDate()) && cron[3].has(date.getMonth() + 1) && cron[4].has(date.getDay());
}

function nextRun(cron, from = new Date()) {
  const cursor = new Date(from);
  cursor.setSeconds(0, 0);
  for (let i = 0; i < MAX_SCAN_MINUTES; i++) {
    cursor.setMinutes(cursor.getMinutes() + 1);
    if (cronMatches(cron, cursor)) return cursor.getTime();
  }
  throw new Error('cron 在一年内没有任何匹配时间');
}

class Automations {
  constructor(chats, { file = path.join(__dirname, '../.data/automations.json'), log = () => {} } = {}) {
    this.chats = chats;
    this.file = file;
    this.log = log;
    this.records = [];
  }
  load() {
    if (this.file === ':memory:') return;
    try {
      if (fs.existsSync(this.file)) {
        const saved = JSON.parse(fs.readFileSync(this.file, 'utf8').replace(/^\uFEFF/, ''));
        if (!Array.isArray(saved)) throw new Error('格式无效');
        this.records = saved;
      }
    } catch (e) { this.log('error', '定时任务', '读取配置失败：' + e.message); }
  }
  save() {
    if (this.file === ':memory:') return;
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true });
      fs.writeFileSync(this.file + '.tmp', JSON.stringify(this.records));
      fs.renameSync(this.file + '.tmp', this.file);
    } catch (e) { this.log('error', '定时任务', '保存配置失败：' + e.message); }
  }
  validateCron(cron) {
    const parsed = parseCron(cron);
    return { parsed, nextAt: nextRun(parsed) };
  }
  create(spec = {}) {
    if (typeof spec.prompt !== 'string' || !spec.prompt.trim() || spec.prompt.length > 12000) throw new Error('请填写 1–12000 字符的定时提示');
    const chat = this.chats.get(spec.chatId); // throws when the chat does not exist
    const { parsed, nextAt } = this.validateCron(spec.cron);
    const record = { id: 'auto_' + Math.random().toString(36).slice(2, 10), chatId: chat.id,
      prompt: spec.prompt.trim(), cron: String(spec.cron).trim(), enabled: true,
      createdAt: Date.now(), lastRunAt: 0, lastStatus: '', nextRunAt: nextAt };
    this.records.push(record); this.save();
    return record;
  }
  update(id, patch = {}) {
    const record = this.records.find(r => r.id === id);
    if (!record) throw new Error('定时任务不存在');
    if (patch.prompt != null) {
      if (typeof patch.prompt !== 'string' || !patch.prompt.trim() || patch.prompt.length > 12000) throw new Error('请填写 1–12000 字符的定时提示');
      record.prompt = patch.prompt.trim();
    }
    if (patch.cron != null) {
      const { nextAt } = this.validateCron(patch.cron);
      record.cron = String(patch.cron).trim();
      record.nextRunAt = nextAt;
    }
    if (patch.enabled != null) record.enabled = !!patch.enabled;
    this.save();
    return record;
  }
  remove(id) {
    const before = this.records.length;
    this.records = this.records.filter(r => r.id !== id);
    if (this.records.length === before) throw new Error('定时任务不存在');
    this.save();
  }
  tick(now = new Date()) {
    const minuteKey = Math.floor(now.getTime() / 60000);
    for (const record of this.records) {
      if (!record.enabled) continue;
      try {
        if (!record.nextRunAt) record.nextRunAt = nextRun(parseCron(record.cron), now);
        if (now.getTime() < record.nextRunAt) continue;
        record.nextRunAt = nextRun(parseCron(record.cron), now);
        if (record.lastMinuteKey === minuteKey) continue;
        record.lastMinuteKey = minuteKey;
        const chat = this.chats.chats.find(c => c.id === record.chatId);
        if (!chat) { record.lastStatus = 'missing-chat'; continue; }
        if (this.chats.active(chat)) { record.lastStatus = 'busy'; this.save(); continue; }
        record.lastRunAt = now.getTime();
        record.lastStatus = 'sent';
        this.chats.send(record.chatId, record.prompt);
        this.log('info', '定时任务', '已按计划触发：' + record.id);
      } catch (e) { record.lastStatus = 'error: ' + e.message; }
      this.save();
    }
  }
}
module.exports = { Automations, parseCron, cronMatches, nextRun };
