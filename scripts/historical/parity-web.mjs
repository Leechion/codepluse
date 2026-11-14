#!/usr/bin/env node
// Dual-run for the surfaces the Rust front door took over from the sidecar
// (Phase 3): static assets, /api/health, the attachment upload, the
// companion endpoint, the method/content-type contract and the local-request
// guard.
//
// Twin roots carry a small `codeg-ui/out` export so the static lookup order
// (Rsc .txt → .html → <dir>/index.html) can be compared.
//
//   node scripts/parity-web.mjs [--go 8880] [--rust 8881]

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import process from 'node:process';

const repoRoot = path.resolve(new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const args = process.argv.slice(2);
const option = (name, fallback) => {
  const at = args.indexOf(`--${name}`);
  return at >= 0 ? args[at + 1] : fallback;
};
const GO_PORT = Number(option('go', '8880'));
const RUST_PORT = Number(option('rust', '8881'));
const GO_EXE = path.join(repoRoot, 'server-go', 'serve-codeg.exe');
const RUST_EXE = path.join(repoRoot, 'src-tauri', 'target', 'debug', 'codepluse.exe');

const die = (message) => {
  console.error(`❌ ${message}`);
  process.exit(2);
};
if (!existsSync(GO_EXE)) die('缺少 server-go/serve-codeg.exe');
if (!existsSync(RUST_EXE)) die('缺少 src-tauri/target/debug/codepluse.exe');

const base = mkdtempSync(path.join(repoRoot, '.superpowers', 'sdd', 'parity-web-'));

const twin = (tag) => {
  const root = path.join(base, tag);
  const out = path.join(root, 'codeg-ui', 'out');
  mkdirSync(path.join(out, 'assets'), { recursive: true });
  mkdirSync(path.join(root, '.data'), { recursive: true });
  writeFileSync(path.join(out, 'index.html'), '<html>index</html>');
  writeFileSync(path.join(out, 'workspace.html'), '<html>workspace</html>');
  writeFileSync(path.join(out, 'app.txt'), 'rsc-payload');
  writeFileSync(path.join(out, 'assets', 'app.css'), 'body{color:red}');
  return root;
};

const goRoot = twin('go');
const rustRoot = twin('rust');

const children = [];
const spawnServer = (exe, cwd, env, label) => {
  const child = spawn(exe, [], { cwd, env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stderr.on('data', (chunk) => process.env.PARITY_VERBOSE && process.stderr.write(`[${label}] ${chunk}`));
  child.on('exit', (code) => {
    if (code !== 0 && code !== null) die(`${label} 退出（${code}）`);
  });
  children.push(child);
};

spawnServer(GO_EXE, goRoot, {
  HARNESS_HUB_CODEG: '1',
  PORT: String(GO_PORT),
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
}, 'go');
spawnServer(RUST_EXE, rustRoot, {
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
  CODEPLUSE_HEADLESS: '1',
  CODEPLUSE_PORT: String(RUST_PORT),
  CODEPLUSE_ROOT: rustRoot,
}, 'rust');

const waitHealth = async (port) => {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/api/health`);
      if (response.ok) return;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  die(`端口 ${port} 的服务 30s 内未就绪`);
};

let failures = 0;
const check = (ok, message) => {
  if (!ok) {
    failures += 1;
    console.error(`❌ ${message}`);
  }
};

const get = async (port, url, headers = {}) => {
  const response = await fetch(`http://127.0.0.1:${port}${url}`, { headers });
  return {
    status: response.status,
    type: response.headers.get('content-type') || '',
    etag: response.headers.get('etag') || '',
    body: await response.text(),
  };
};

const post = async (port, url, body, headers = {}) => {
  const response = await fetch(`http://127.0.0.1:${port}${url}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...headers },
    body,
  });
  return { status: response.status, body: await response.text() };
};

const multipart = (filename, content) =>
  [
    '--X1',
    `Content-Disposition: form-data; name="file"; filename="${filename}"`,
    'Content-Type: text/plain',
    '',
    content,
    '--X1--',
    '',
  ].join('\r\n');

try {
  await waitHealth(GO_PORT);
  await waitHealth(RUST_PORT);

  // ── 静态资源 ────────────────────────────────────────────────────────────
  for (const url of ['/', '/workspace', '/assets/app.css', '/missing.txt', '/app.txt']) {
    const [go, rust] = [await get(GO_PORT, url), await get(RUST_PORT, url)];
    const same = go.status === rust.status && go.body === rust.body && go.type === rust.type;
    check(same, `静态 ${url}: 状态 ${go.status}/${rust.status} 类型 ${go.type}/${rust.type}\n   Go   ${go.body.slice(0, 120)}\n   Rust ${rust.body.slice(0, 120)}`);
    if (same) console.log(`· 静态 ${url}: MATCH`);
  }
  // RSC 请求：先试 <file>.txt
  {
    const [go, rust] = [
      await get(GO_PORT, '/app.txt', { rsc: '1' }),
      await get(RUST_PORT, '/app.txt', { rsc: '1' }),
    ];
    const same = go.status === rust.status && go.body === rust.body && go.type === rust.type;
    check(same, `RSC /app.txt: ${go.status}/${rust.status} ${go.type}/${rust.type}`);
    if (same) console.log(`· RSC /app.txt: MATCH`);
  }

  // ── health ──────────────────────────────────────────────────────────────
  {
    const [go, rust] = [await get(GO_PORT, '/api/health'), await get(RUST_PORT, '/api/health')];
    const goJson = JSON.parse(go.body);
    const rustJson = JSON.parse(rust.body);
    const keys = new Set([...Object.keys(goJson), ...Object.keys(rustJson)]);
    for (const key of keys) {
      if (key === 'backend') continue; // "go" → "rust"
      if (key === 'data') {
        const goData = goJson.data || {};
        const rustData = rustJson.data || {};
        const dataKeys = new Set([...Object.keys(goData), ...Object.keys(rustData)]);
        for (const dataKey of dataKeys) {
          if (dataKey === 'version') continue; // "2.0.0-go" → "2.0.0-rust"
          check(
            JSON.stringify(goData[dataKey]) === JSON.stringify(rustData[dataKey]),
            `health.data.${dataKey}: ${JSON.stringify(goData[dataKey])} != ${JSON.stringify(rustData[dataKey])}`
          );
        }
        continue;
      }
      check(
        JSON.stringify(goJson[key]) === JSON.stringify(rustJson[key]),
        `health.${key}: ${JSON.stringify(goJson[key])} != ${JSON.stringify(rustJson[key])}`
      );
    }
    check(rustJson.backend === 'rust', `health.backend 应为 rust，实为 ${rustJson.backend}`);
    console.log('· health: 字段一致（backend/version 已按预期切换）');
  }

  // ── 附件上传 ────────────────────────────────────────────────────────────
  {
    const body = multipart('note.txt', 'hello');
    const headers = { 'content-type': 'multipart/form-data; boundary=X1' };
    const [go, rust] = [
      await post(GO_PORT, '/api/upload_attachment', body, headers),
      await post(RUST_PORT, '/api/upload_attachment', body, headers),
    ];
    const normalize = (text, root) =>
      text
        .replaceAll(root.replaceAll('\\', '\\\\'), '<ROOT>')
        .replaceAll(root.replaceAll('\\', '/'), '<ROOT>')
        .replaceAll(root, '<ROOT>')
        // The upload directory is a random 32-hex id.
        .replace(/[0-9a-f]{32}/g, '<ID>');
    check(
      go.status === rust.status && normalize(go.body, goRoot) === normalize(rust.body, rustRoot),
      `上传: 状态 ${go.status}/${rust.status}\n   Go   ${go.body.slice(0, 200)}\n   Rust ${rust.body.slice(0, 200)}`
    );
    if (go.status === rust.status) console.log(`· 上传: MATCH (${go.status})`);
  }
  // 空文件名 → 400
  {
    const body = multipart('..', 'x');
    const headers = { 'content-type': 'multipart/form-data; boundary=X1' };
    const [go, rust] = [
      await post(GO_PORT, '/api/upload_attachment', body, headers),
      await post(RUST_PORT, '/api/upload_attachment', body, headers),
    ];
    check(
      go.status === rust.status && go.body === rust.body,
      `上传坏文件名: ${go.status}/${rust.status}\n   Go   ${go.body.slice(0, 160)}\n   Rust ${rust.body.slice(0, 160)}`
    );
    if (go.status === rust.status) console.log(`· 上传坏文件名: MATCH (${go.status})`);
  }

  // ── companion 端点 ──────────────────────────────────────────────────────
  {
    const [go, rust] = [await get(GO_PORT, '/api/codeg_mcp'), await get(RUST_PORT, '/api/codeg_mcp')];
    check(
      go.status === rust.status && go.body === rust.body,
      `companion 非 POST: ${go.status}/${rust.status}\n   Go   ${go.body.slice(0, 160)}\n   Rust ${rust.body.slice(0, 160)}`
    );
    if (go.status === rust.status) console.log(`· companion 非 POST: MATCH (${go.status})`);
  }

  // ── 非 POST 的命令 → 405；非 JSON 的 body → 415 ──────────────────────────
  {
    const [go, rust] = [await get(GO_PORT, '/api/list_folders'), await get(RUST_PORT, '/api/list_folders')];
    check(
      go.status === rust.status && go.body === rust.body,
      `命令非 POST: ${go.status}/${rust.status}\n   Go   ${go.body.slice(0, 160)}\n   Rust ${rust.body.slice(0, 160)}`
    );
    if (go.status === rust.status) console.log(`· 命令非 POST: MATCH (${go.status})`);
  }
  {
    const headers = { 'content-type': 'text/plain' };
    const [go, rust] = [
      await post(GO_PORT, '/api/list_folders', '{}', headers),
      await post(RUST_PORT, '/api/list_folders', '{}', headers),
    ];
    check(
      go.status === rust.status && go.body === rust.body,
      `命令非 JSON: ${go.status}/${rust.status}\n   Go   ${go.body.slice(0, 160)}\n   Rust ${rust.body.slice(0, 160)}`
    );
    if (go.status === rust.status) console.log(`· 命令非 JSON: MATCH (${go.status})`);
  }

  // ── 跨站请求 → 403 ──────────────────────────────────────────────────────
  {
    const headers = { 'sec-fetch-site': 'cross-site' };
    const [go, rust] = [
      await post(GO_PORT, '/api/list_folders', '{}', headers),
      await post(RUST_PORT, '/api/list_folders', '{}', headers),
    ];
    check(
      go.status === rust.status && go.body === rust.body,
      `跨站请求: ${go.status}/${rust.status}\n   Go   ${go.body.slice(0, 160)}\n   Rust ${rust.body.slice(0, 160)}`
    );
    if (go.status === rust.status) console.log(`· 跨站请求: MATCH (${go.status})`);
  }
  {
    const headers = { origin: 'http://evil.example' };
    const [go, rust] = [
      await post(GO_PORT, '/api/list_folders', '{}', headers),
      await post(RUST_PORT, '/api/list_folders', '{}', headers),
    ];
    check(
      go.status === rust.status && go.body === rust.body,
      `外来 Origin: ${go.status}/${rust.status}\n   Go   ${go.body.slice(0, 160)}\n   Rust ${rust.body.slice(0, 160)}`
    );
    if (go.status === rust.status) console.log(`· 外来 Origin: MATCH (${go.status})`);
  }
} finally {
  for (const child of children) {
    try {
      child.kill();
    } catch {}
  }
  for (let attempt = 0; attempt < 5; attempt += 1) {
    try {
      rmSync(base, { recursive: true, force: true });
      break;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 300));
    }
  }
}

if (failures > 0) {
  console.error(`\n❌ ${failures} 处不一致`);
  process.exit(1);
}
console.log('\n✅ Phase 3 服务面双跑一致');
