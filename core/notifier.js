'use strict';
const http = require('node:http');
const https = require('node:https');

// Terminal-mission webhook notifier, following Codeg's notification stance:
// only main-thread completions and errors are pushed, never per-worker noise.
// One silent retry; notification failures never affect the orchestration flow.
class Notifier {
  constructor(hub, { url = process.env.HARNESS_HUB_WEBHOOK_URL, log = () => {} } = {}) {
    this.url = url || '';
    this.log = log;
    if (this.url && hub) hub.on('mission', m => {
      if (!['completed', 'partial', 'failed'].includes(m.status)) return;
      this.push(this.payload(m));
    });
  }
  payload(m) {
    return { type: 'mission.finished', missionId: m.id, chatId: m.chatId || null, status: m.status,
      objective: (m.objective || '').slice(0, 2000), reply: (m.reply || m.error || '').slice(0, 8000),
      error: m.error || '', workerIds: m.workerIds || [], finishedAt: m.finishedAt || Date.now() };
  }
  push(body, attempt = 0) {
    if (!this.url) return Promise.resolve(false);
    return new Promise(resolve => {
      let target;
      try { target = new URL(this.url); } catch (_) { this.log('error', '通知', 'Webhook 地址无效'); return resolve(false); }
      const data = JSON.stringify(body);
      const req = (target.protocol === 'http:' ? http : https).request(target, {
        method: 'POST', headers: { 'Content-Type': 'application/json; charset=utf-8', 'Content-Length': Buffer.byteLength(data) },
        timeout: 10000
      }, res => {
        res.resume();
        if (res.statusCode >= 200 && res.statusCode < 300) return resolve(true);
        if (attempt === 0) return resolve(this.push(body, 1));
        this.log('warn', '通知', 'Webhook 返回 ' + res.statusCode);
        resolve(false);
      });
      req.on('timeout', () => { req.destroy(new Error('timeout')); });
      req.on('error', () => {
        if (attempt === 0) return resolve(this.push(body, 1));
        resolve(false);
      });
      req.end(data);
    });
  }
}
module.exports = { Notifier };
