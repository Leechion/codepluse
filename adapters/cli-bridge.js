#!/usr/bin/env node
'use strict';

const http = require('node:http');
const { timingSafeEqual } = require('node:crypto');
const { RealHarnessAdapter } = require('../core/real-adapters');

// Other Harnesses can reuse this server with their own async handler.
function createBridge(handler, { token = '' } = {}) {
  return http.createServer(async (req, res) => {
    const send = (code, value) => {
      res.writeHead(code, { 'Content-Type': 'application/json; charset=utf-8' });
      res.end(JSON.stringify(value));
    };
    const expected = Buffer.from('Bearer ' + token);
    const actual = Buffer.from(req.headers.authorization || '');
    if (token && (expected.length !== actual.length || !timingSafeEqual(expected, actual))) {
      return send(401, { ok: false, error: 'Unauthorized' });
    }
    if (req.url === '/health' && req.method === 'POST') return send(200, { ok: true });
    if (req.url !== '/messages' || req.method !== 'POST') return send(404, { ok: false });
    try {
      const chunks = []; let size = 0;
      for await (const chunk of req) {
        size += chunk.length;
        if (size > 1024 * 1024) { send(413, { ok: false, error: 'Message too large' }); return; }
        chunks.push(chunk);
      }
      const message = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      if (!message.id || !message.from || !message.to || typeof message.payload?.text !== 'string') {
        return send(400, { ok: false, error: 'Invalid envelope' });
      }
      const result = await handler(message);
      send(result.ok ? 200 : 502, result);
    } catch (e) { send(400, { ok: false, error: e.message }); }
  });
}

function createCliHandler(options, Adapter = RealHarnessAdapter) {
  let running = false;
  const active = new Set();
  return {
    async handle(message) {
      if (message.payload.permission && message.payload.permission !== 'readonly') return { ok: false, error: '此 CLI 桥接固定使用只读权限' };
      const model = message.payload.model || '';
      if (typeof model !== 'string' || (model && !/^[a-zA-Z0-9][\w.:/@+\-]{0,119}$/.test(model))) return { ok: false, error: '无效模型名称' };
      if (running) return { ok: false, error: 'CLI 桥接正在执行其他调用' };
      running = true;
      const adapter = new Adapter({ ...options, permission: 'readonly', model });
      active.add(adapter);
      try { return await adapter.dispatch({ id: message.id, prompt: message.payload.text, title: 'Harness message' }, null); }
      finally { active.delete(adapter); running = false; }
    },
    teardown() { active.forEach(a => a.teardown()); }
  };
}

if (require.main === module) {
  const tool = process.env.BRIDGE_TOOL || 'claude';
  if (!['claude', 'codex'].includes(tool)) throw new Error('BRIDGE_TOOL must be claude or codex');
  const cli = createCliHandler({ tool, permission: 'readonly',
    workdir: process.env.BRIDGE_WORKDIR || process.cwd(), timeoutMs: 180000 });
  const server = createBridge(cli.handle, { token: process.env.BRIDGE_TOKEN || '' });
  server.listen(Number(process.env.BRIDGE_PORT || 9001), '127.0.0.1', () => {
    console.log('Harness bridge (' + tool + ') listening on 127.0.0.1:' + server.address().port);
  });
  const stop = () => { cli.teardown(); server.close(); };
  process.on('SIGTERM', stop);
  process.on('SIGINT', stop);
}

module.exports = { createBridge, createCliHandler };
