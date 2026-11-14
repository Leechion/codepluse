#!/usr/bin/env node
// Dual-run for the external provider write-back (Go `internal/providers/writeback.go`).
//
// Twin homes: each side gets its own `$HOME` with a Codex `config.toml` and a
// CC Switch SQLite database, so a write has somewhere real to land. The two
// sides then receive the same `codepluse_provider_save` call and the harness
// compares the response and every touched file.
//
//   go build -o server-go/serve-codeg.exe ./cmd/serve
//   cargo build
//   node scripts/parity-provider.mjs [--go 8860] [--rust 8861]

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
const GO_PORT = Number(option('go', '8860'));
const RUST_PORT = Number(option('rust', '8861'));
const GO_EXE = path.join(repoRoot, 'server-go', 'serve-codeg.exe');
const RUST_EXE = path.join(repoRoot, 'src-tauri', 'target', 'debug', 'codepluse.exe');

const die = (message) => {
  console.error(`❌ ${message}`);
  process.exit(2);
};
if (!existsSync(GO_EXE)) die('缺少 server-go/serve-codeg.exe');
if (!existsSync(RUST_EXE)) die('缺少 src-tauri/target/debug/codepluse.exe');

const base = mkdtempSync(path.join(repoRoot, '.superpowers', 'sdd', 'parity-provider-'));
mkdirSync(path.join(base, '.data'), { recursive: true });

// ── twin workspaces + homes ────────────────────────────────────────────────
const CODEX_CONFIG = `model_provider = "test"
model = "old-model"

[model_providers.test]
name = "Test"
base_url = "https://old.example"
model = "old-model"
wire_api = "responses"
experimental_bearer_token = "old-key"
`;

const twin = (tag) => {
  const root = path.join(base, tag);
  const home = path.join(base, `${tag}-home`);
  mkdirSync(path.join(root, '.data'), { recursive: true });
  mkdirSync(path.join(home, '.codex'), { recursive: true });
  mkdirSync(path.join(home, '.dsh', 'profiles', 'desktop'), { recursive: true });
  writeFileSync(path.join(home, '.codex', 'config.toml'), CODEX_CONFIG);
  writeFileSync(
    path.join(home, '.dsh', 'profiles', 'desktop', 'cordis.patch.yml'),
    [
      '- id: llm-pi-ai',
      '  config:',
      '    providers:',
      '      pi:',
      '        baseURL: https://old.example',
      '        api: openai-completions',
      '        models:',
      '          - id: old-model',
      '            name: Old',
      '- id: agent-default-model',
      '  config:',
      '    provider: pi',
      '    model: old-model',
      '',
    ].join('\n'),
  );
  return { root, home };
};

const goTwin = twin('go');
const rustTwin = twin('rust');

// ── servers ────────────────────────────────────────────────────────────────
const children = [];
const spawnServer = (exe, cwd, env, label) => {
  const child = spawn(exe, [], { cwd, env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stderr.on('data', (chunk) => process.env.PARITY_VERBOSE && process.stderr.write(`[${label}] ${chunk}`));
  child.on('exit', (code) => {
    if (code !== 0 && code !== null) die(`${label} 退出（${code}）`);
  });
  children.push(child);
};

spawnServer(GO_EXE, goTwin.root, {
  HARNESS_HUB_CODEG: '1',
  PORT: String(GO_PORT),
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
  USERPROFILE: goTwin.home,
  HOME: goTwin.home,
}, 'go');
spawnServer(RUST_EXE, rustTwin.root, {
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
  CODEPLUSE_HEADLESS: '1',
  CODEPLUSE_PORT: String(RUST_PORT),
  CODEPLUSE_ROOT: rustTwin.root,
  USERPROFILE: rustTwin.home,
  HOME: rustTwin.home,
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

const post = async (port, command, body) => {
  const response = await fetch(`http://127.0.0.1:${port}/api/${command}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body ?? {}),
  });
  const text = await response.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {}
  return { status: response.status, json, text };
};

const TIME = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z/g;

const maskText = (text, home) =>
  text
    .replaceAll(home.replaceAll('\\', '\\\\'), '<HOME>')
    .replaceAll(home.replaceAll('\\', '/'), '<HOME>')
    .replaceAll(home, '<HOME>')
    .replace(TIME, '<TS>');

/// 去掉空白并统一引号风格，用于比较 TOML / YAML 的语义。
/// TOML 还要丢掉「父表头」：Go 的编码器会为 `[model_providers]` 这类父表
/// 单独写一行，而 Rust 的 toml crate 只写子表。
const normalizeFormat = (text, format) => {
  let out = text;
  if (format === 'toml') {
    const headers = out.split(/\r?\n/).filter((line) => /^\s*\[/.test(line));
    const parents = new Set(
      headers.filter((header) =>
        headers.some(
          (other) =>
            other !== header && other.trim().startsWith(`${header.trim().replace(/\]$/, '')}.`)
        )
      )
    );
    out = out
      .split(/\r?\n/)
      .filter((line) => {
        const trimmed = line.trim();
        if (!/^\[/.test(trimmed)) return true;
        return ![...parents].some((parent) => parent.trim() === trimmed);
      })
      .join('\n');
  }
  return out
    .replace(/'/g, '"')
    .replace(/\s+/g, '');
};

const steps = [
  {
    name: 'codex',
    command: 'codepluse_provider_save',
    body: {
      id: 'native:codex:provider:test',
      name: 'Test Renamed',
      agent: 'codex',
      url: 'https://new.example',
      model: 'new-model',
      key: 'new-key',
      protocol: 'openai-responses',
      models: [{ id: 'new-model', name: 'New' }],
    },
    files: [
      { label: 'codex config.toml', base: 'home', rel: path.join('.codex', 'config.toml'), format: 'toml' },
      { label: 'provider-metadata.json', base: 'root', rel: path.join('.data', 'provider-metadata.json') },
    ],
  },
  {
    name: 'dsh',
    command: 'codepluse_provider_save',
    body: {
      id: 'native:dsh:desktop:pi',
      name: 'DSH Renamed',
      agent: 'dsh',
      url: 'https://dsh.example',
      model: 'dsh-model',
      key: 'dsh-key',
      protocol: 'openai',
      models: [{ id: 'dsh-model', name: 'Dsh' }],
    },
    files: [
      { label: 'dsh cordis.patch.yml', base: 'home', rel: path.join('.dsh', 'profiles', 'desktop', 'cordis.patch.yml'), format: 'yaml' },
      { label: 'provider-metadata.json', base: 'root', rel: path.join('.data', 'provider-metadata.json') },
    ],
  },
  {
    name: 'unknown-source',
    command: 'codepluse_provider_save',
    body: {
      id: 'native:codex:provider:missing',
      name: 'X',
      agent: 'codex',
      url: 'https://x.example',
      model: 'm',
      key: 'k',
      protocol: 'openai-responses',
    },
    files: [],
  },
];

let failures = 0;
const check = (ok, message) => {
  if (!ok) {
    failures += 1;
    console.error(`❌ ${message}`);
  }
};

try {
  await waitHealth(GO_PORT);
  await waitHealth(RUST_PORT);

  for (const step of steps) {
    const [go, rust] = await Promise.all([
      post(GO_PORT, step.command, step.body),
      post(RUST_PORT, step.command, step.body),
    ]);
    if (go.status !== rust.status) {
      check(false, `${step.name}: 状态码 ${go.status} != ${rust.status}\n   Go ${go.text.slice(0, 200)}\n   Rust ${rust.text.slice(0, 200)}`);
      continue;
    }
    const goText = maskText(go.text, goTwin.home);
    const rustText = maskText(rust.text, rustTwin.home);
    if (goText !== rustText) {
      check(false, `${step.name}: 响应不一致\n   Go   ${goText.slice(0, 300)}\n   Rust ${rustText.slice(0, 300)}`);
    } else if (go.status >= 400) {
      console.log(`· ${step.name}: ${go.status}（错误文案一致）`);
    } else {
      console.log(`· ${step.name}: MATCH`);
    }

    for (const file of step.files ?? []) {
      const goFile = file.base === 'home'
        ? path.join(goTwin.home, file.rel)
        : path.join(goTwin.root, file.rel);
      const rustFile = file.base === 'home'
        ? path.join(rustTwin.home, file.rel)
        : path.join(rustTwin.root, file.rel);
      const a = existsSync(goFile) ? maskText(readFileSync(goFile, 'utf8'), goTwin.home) : '<缺失>';
      const b = existsSync(rustFile) ? maskText(readFileSync(rustFile, 'utf8'), rustTwin.home) : '<缺失>';
      if (a === b) {
        console.log(`· ${step.name}: ${file.label} 字节一致`);
        continue;
      }
      // TOML / YAML 的引号与缩进风格由各自的序列化库决定（Go: pelletier /
      // yaml.v3，Rust: toml / serde_yaml），语义一致即视为通过——字节差异
      // 记在文档的「已知偏差」里。
      if (file.format && normalizeFormat(a, file.format) === normalizeFormat(b, file.format)) {
        console.log(`⚠ ${step.name}: ${file.label} 语义一致，字节有格式差异（已知偏差）`);
        continue;
      }
      check(false, `${step.name}: ${file.label} 不一致\n   Go   ${a.slice(0, 400)}\n   Rust ${b.slice(0, 400)}`);
    }
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
console.log('\n✅ 外部写回双跑一致');
