'use strict';
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');

function probe(port) {
  return new Promise(resolve => {
    const req = http.get(`http://127.0.0.1:${port}/api/health`, { timeout: 1200 }, res => {
      let body = '';
      res.on('data', chunk => { if (body.length < 16384) body += chunk; });
      res.on('end', () => {
        let value;
        try { value = JSON.parse(body); } catch (_) { /* occupied by another app */ }
        resolve(res.statusCode === 200 && value?.ok && value.data?.status === 'ok' &&
          value.data?.sessionRuntime === 'acp-v1' && value.data?.version?.endsWith('-go') ? 'hub' : 'other');
      });
      res.on('error', () => resolve('other'));
    });
    req.on('timeout', () => req.destroy());
    req.on('error', error => resolve(error.code === 'ECONNREFUSED' ? 'offline' : 'other'));
  });
}

async function ensureBackend(root, port) {
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('服务端口无效。');
  const state = await probe(port);
  if (state === 'hub') return { started: false };
  if (state !== 'offline') throw new Error(`端口 ${port} 已被其他服务占用，请修改 PORT 后启动。`);
  const executable = path.join(root, 'server-go', 'serve.exe');
  if (!fs.existsSync(executable)) throw new Error('未找到 Go 后端，请先在 server-go 目录编译 serve.exe。');
  const dataDir = path.join(root, '.data');
  fs.mkdirSync(dataDir, { recursive: true });
  const log = fs.openSync(path.join(dataDir, 'desktop-backend.log'), 'a');
  let child;
  try {
    child = spawn(executable, [], { cwd: root, detached: true, windowsHide: true,
      stdio: ['ignore', log, log], env: { ...process.env, HOST: '127.0.0.1', PORT: String(port) } });
  } finally { fs.closeSync(log); }
  let failure;
  child.on('error', error => { failure = error; });
  child.on('exit', code => { failure = new Error(`Go 服务退出（${code}），请查看 .data/desktop-backend.log。`); });
  child.unref();
  // Backend runs independently so closing the desktop never interrupts work.
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) {
    if (failure) throw failure;
    if (await probe(port) === 'hub') return { started: true, pid: child.pid };
    await new Promise(resolve => setTimeout(resolve, 200));
  }
  throw new Error('Go 服务在 20 秒内未就绪，请查看 .data/desktop-backend.log。');
}
module.exports = { probe, ensureBackend };
