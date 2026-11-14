#!/usr/bin/env node
// Dual-run for the `mcp-codeg` companion (the MCP stdio server the agents spawn).
//
// Both binaries are fed the same JSON-RPC lines on stdin and their stdout is
// compared frame by frame. `tools/call` needs a live ACP caller token, so the
// comparison covers the protocol frames the transport owns: initialize,
// tools/list, ping and the unsupported-method error.
//
//   node scripts/parity-mcp.mjs

import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';
import process from 'node:process';

const repoRoot = path.resolve(new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const GO_EXE = path.join(repoRoot, 'server-go', 'serve-codeg.exe');
const RUST_EXE = path.join(repoRoot, 'src-tauri', 'target', 'debug', 'codepluse.exe');

const die = (message) => {
  console.error(`❌ ${message}`);
  process.exit(2);
};
if (!existsSync(GO_EXE)) die('缺少 server-go/serve-codeg.exe');
if (!existsSync(RUST_EXE)) die('缺少 src-tauri/target/debug/codepluse.exe');

const REQUESTS = [
  { jsonrpc: '2.0', id: 1, method: 'initialize', params: {} },
  { jsonrpc: '2.0', id: 2, method: 'ping', params: {} },
  { jsonrpc: '2.0', id: 3, method: 'tools/list', params: {} },
  { jsonrpc: '2.0', id: 4, method: 'nope/nope', params: {} },
];

const run = (exe, args) =>
  new Promise((resolve) => {
    const child = spawn(exe, args, { stdio: ['pipe', 'pipe', 'pipe'] });
    let out = '';
    let err = '';
    child.stdout.on('data', (chunk) => (out += chunk.toString()));
    child.stderr.on('data', (chunk) => (err += chunk.toString()));
    child.on('exit', (code) => resolve({ code, out, err }));
    for (const request of REQUESTS) {
      child.stdin.write(`${JSON.stringify(request)}\n`);
    }
    child.stdin.end();
  });

/// Go names itself `codeg-mcp-go`; the Rust port is `codeg-mcp-rust`. The name
/// is cosmetic (no MCP client checks it), so it is masked in the comparison.
const normalize = (text) =>
  text
    .split('\n')
    .filter((line) => line.trim().length > 0)
    .map((line) => line.replace(/"codeg-mcp-(go|rust)"/g, '"codeg-mcp"'))
    .join('\n');

const go = await run(GO_EXE, ['mcp-codeg', 'http://127.0.0.1:1/api/codeg_mcp', 'dummy-token']);
const rust = await run(RUST_EXE, ['mcp-codeg', 'http://127.0.0.1:1/api/codeg_mcp', 'dummy-token']);

let failures = 0;
if (go.code !== rust.code) {
  failures += 1;
  console.error(`❌ 退出码 ${go.code} != ${rust.code}\n   Go   ${go.err.trim()}\n   Rust ${rust.err.trim()}`);
}
const goFrames = normalize(go.out);
const rustFrames = normalize(rust.out);
const goLines = goFrames.split('\n');
const rustLines = rustFrames.split('\n');
if (goLines.length !== rustLines.length) {
  failures += 1;
  console.error(`❌ 帧数 ${goLines.length} != ${rustLines.length}`);
} else {
  for (let index = 0; index < goLines.length; index += 1) {
    if (goLines[index] === rustLines[index]) {
      const parsed = JSON.parse(goLines[index]);
      const method = REQUESTS[index]?.method ?? '?';
      console.log(`· ${method}: 一致`);
      continue;
    }
    failures += 1;
    console.error(`❌ 第 ${index + 1} 帧不一致\n   Go   ${goLines[index]}\n   Rust ${rustLines[index]}`);
  }
}

if (failures > 0) {
  console.error(`\n❌ ${failures} 处不一致`);
  process.exit(1);
}
console.log('\n✅ MCP companion 双跑一致');
