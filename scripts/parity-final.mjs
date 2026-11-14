#!/usr/bin/env node
// 收尾批（R1–R5，28 条）双跑：instances / custom-agents 写 / 安装状态 /
// connection / mcp status / turn 库与 token usage / 任务引擎。
//
// 孪生 workspace + 孪生 codeg.db（SQLite 由各自服务进程建）；turn 数据在两侧
// 以 SQL 直插同一 schema，确保读路径对同一输入。
//
//   node scripts/parity-final.mjs [--go 28970] [--rust 28971] [--keep]

import { spawn } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';
import process from 'node:process';

const repoRoot = path.resolve(new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const args = process.argv.slice(2);
const option = (name, fallback) => {
  const at = args.indexOf(`--${name}`);
  return at >= 0 ? args[at + 1] : fallback;
};
const keep = args.includes('--keep');
const GO_PORT = Number(option('go', '28970'));
const RUST_PORT = Number(option('rust', '28971'));
const GO_EXE = path.join(repoRoot, 'server-go', 'serve-codeg.exe');
const RUST_EXE = path.join(repoRoot, 'src-tauri', 'target', 'debug', 'codepluse.exe');
if (!existsSync(GO_EXE)) die('缺少 server-go/serve-codeg.exe');
if (!existsSync(RUST_EXE)) die('缺少 src-tauri/target/debug/codepluse.exe');

function die(message) {
  console.error(`❌ ${message}`);
  process.exit(2);
}

// ── 孪生 workspace ─────────────────────────────────────────────────────────

const base = mkdtempSync(path.join(repoRoot, '.superpowers', 'sdd', 'parity-final-'));
const twin = (tag) => {
  const root = path.join(base, tag);
  mkdirSync(path.join(root, '.data'), { recursive: true });
  // 技能资产（安装状态命令读 manifest + central copy）
  const skills = path.join(root, 'codeg-assets', 'experts', 'skills', 'one');
  mkdirSync(skills, { recursive: true });
  writeFileSync(path.join(skills, 'SKILL.md'), '# one\n');
  const hash = crypto.createHash('sha256').update('# one\n').digest('hex');
  writeFileSync(
    path.join(root, 'codeg-assets', 'experts', 'manifest.json'),
    JSON.stringify([{ id: 'one', display_name: { en: 'One' }, bundled_hash: hash }]),
  );
  const science = path.join(root, 'codeg-assets', 'science', 'skills', 'two');
  mkdirSync(science, { recursive: true });
  writeFileSync(path.join(science, 'SKILL.md'), '# two\n');
  const shash = crypto.createHash('sha256').update('# two\n').digest('hex');
  writeFileSync(
    path.join(root, 'codeg-assets', 'science', 'manifest.json'),
    JSON.stringify([{ id: 'two', display_name: { en: 'Two' }, bundled_hash: shash }]),
  );
  writeFileSync(
    path.join(root, '.data', 'codeg-workspace.json'),
    JSON.stringify({
      next: 10,
      folders: [
        { id: 1, name: 'ws', path: root, kind: 'regular', open: true, sort_order: 0, color: 'default', group_id: null, alias: null },
      ],
      conversations: [
        { id: 2, folder_id: 1, title: 'Alpha', status: 'pending_review', agent_type: 'codex', kind: 'regular', message_count: 2, child_count: 0, parent_id: null, tag_ids: [] },
      ],
      settings: {},
      tabs: [],
      tabsVersion: 0,
      canvasBoards: [],
      canvasNodes: [],
      canvasRevision: 0,
      folderGroups: [],
      conversationTags: [],
      assistants: [],
      quickMessages: [],
    }),
  );
  writeFileSync(
    path.join(root, '.data', 'api-providers.json'),
    JSON.stringify({
      providers: [{
        id: 'p1', name: 'P', agent: 'codex', url: 'https://api.openai.com',
        model: 'gpt-5', key: 'sk-final', hasKey: true, api: true,
        models: [{ id: 'gpt-5' }, { id: 'gpt-4.1', enabled: false }],
        protocol: 'openai-responses',
      }],
      active: { codex: 'p1' },
    }, null, 2),
  );
  writeFileSync(
    path.join(root, '.data', 'codeg-worktasks.json'),
    JSON.stringify({
      next: 3, nextEvent: 1, nextTemplate: 1,
      tasks: [
        { id: 1, folder_id: 1, title: 'review me', status: 'review', sort_order: 0, created_at: '2026-11-13T00:00:00Z', updated_at: '2026-11-13T00:00:00Z' },
        { id: 2, folder_id: 1, title: 'queued one', status: 'queued', sort_order: 1, created_at: '2026-11-13T00:00:00Z', updated_at: '2026-11-13T00:00:00Z' },
      ],
      events: {}, templates: [], settings: {},
    }),
  );
  return root;
};
const goRoot = twin('go');
const rustRoot = twin('rust');

// ── 孪生 SQLite（turns 表，与两侧 schema 相同）────────────────────────────

const turnRows = [
  [2, 0, JSON.stringify({ id: 't1', role: 'user', blocks: [{ type: 'text', text: 'hi' }], timestamp: '2026-11-13T01:00:00Z' }), 'user', '2026-11-13T01:00:00Z'],
  [2, 1, JSON.stringify({
    id: 't2', role: 'assistant',
    blocks: [
      { type: 'text', text: 'hello from the agent' },
      { type: 'tool_result', tool_use_id: 'tu1', output_preview: '{"content":[{"text":"out"}]}' },
    ],
    timestamp: '2026-11-13T01:00:05Z', model: 'gpt-5',
    usage: { input_tokens: 100, output_tokens: 40, cache_creation_input_tokens: 10, cache_read_input_tokens: 5 },
    duration_ms: 4200,
  }), 'assistant', '2026-11-13T01:00:05Z'],
];

function seedDb(root) {
  // 用 node 的 sqlite（node 22+ 自带 node:sqlite）建库；不可用时退回 rusqlite
  // 通过 Rust 服务自身建表后再插（此处直接写 SQL 文件用 sqlite3 CLI 更稳）。
  const db = new DatabaseSync(path.join(root, '.data', 'codeg.db'));
  db.exec(`CREATE TABLE IF NOT EXISTS turns (
    conversation_id INTEGER NOT NULL, seq INTEGER NOT NULL, data TEXT NOT NULL,
    role TEXT NOT NULL, timestamp TEXT NOT NULL, hash_after TEXT NOT NULL,
    assistant_upto INTEGER NOT NULL, user_upto INTEGER NOT NULL, max_ts TEXT NOT NULL,
    PRIMARY KEY (conversation_id, seq));`);
  const fnv = (hash, role, ts) => {
    const tag = role === 'user' ? 0 : role === 'assistant' ? 1 : role === 'system' ? 2 : 3;
    hash ^= BigInt(tag);
    hash = BigInt.asUintN(64, hash * 0x100000001b3n);
    const millis = BigInt(Date.parse(ts) || 0);
    for (let i = 0; i < 8; i += 1) {
      hash ^= (millis >> BigInt(8 * i)) & 0xffn;
      hash = BigInt.asUintN(64, hash * 0x100000001b3n);
    }
    return hash;
  };
  let hash = 0xcbf29ce484222325n;
  let assistant = 0;
  let user = 0;
  let maxTs = '';
  for (const [conv, seq, data, role, ts] of turnRows) {
    hash = fnv(hash, role, ts);
    if (role === 'assistant') assistant += 1;
    if (role === 'user') user += 1;
    maxTs = ts;
    db.prepare(
      `INSERT INTO turns (conversation_id, seq, data, role, timestamp, hash_after, assistant_upto, user_upto, max_ts)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    ).run(conv, seq, data, role, ts, hash.toString(16).padStart(16, '0'), assistant, user, maxTs);
  }
  db.close();
}
seedDb(goRoot);
seedDb(rustRoot);

// ── servers ─────────────────────────────────────────────────────────────────

const children = [];
const spawnServer = (exe, cwd, env, label) => {
  const child = spawn(exe, [], { cwd, env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stderr.on('data', (chunk) => process.env.PARITY_VERBOSE && process.stderr.write(`[${label}] ${chunk}`));
  children.push(child);
  return child;
};
spawnServer(GO_EXE, goRoot, {
  HARNESS_HUB_CODEG: '1', PORT: String(GO_PORT),
  HARNESS_HUB_ALLOW_ANY_PATH: '1', HARNESS_HUB_WARM: '0',
}, 'go');
spawnServer(RUST_EXE, rustRoot, {
  HARNESS_HUB_ALLOW_ANY_PATH: '1', HARNESS_HUB_WARM: '0',
  CODEPLUSE_PROXY_ONLY: '1', CODEPLUSE_PORT: String(GO_PORT),
  CODEPLUSE_PROXY_PORT: String(RUST_PORT), CODEPLUSE_ROOT: rustRoot,
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

// ── comparison ─────────────────────────────────────────────────────────────

const TIME = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z/g;
const UID = /\b[0-9a-f]{32}\b/g;
const CHAT = /chat-[0-9a-z]+/g;
const maskText = (text) =>
  text
    .replaceAll(goRoot.replaceAll('\\', '\\\\'), '<ROOT>')
    .replaceAll(rustRoot.replaceAll('\\', '\\\\'), '<ROOT>')
    .replaceAll(goRoot, '<ROOT>')
    .replaceAll(rustRoot, '<ROOT>')
    .replace(TIME, '<TS>')
    .replace(UID, '<UID>')
    .replace(CHAT, '<CHAT>')
    .replaceAll(GO_EXE, '<BIN>')
    .replaceAll(RUST_EXE, '<BIN>');
const maskValue = (value) => {
  if (typeof value === 'string') return maskText(value);
  if (Array.isArray(value)) return value.map(maskValue);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, maskValue(v)]));
  }
  return value;
};
const firstDiff = (a, b, at, out) => {
  if (a === b) return;
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) { out.push(`${at}: 长度 ${a.length} != ${b.length}`); return; }
    for (let i = 0; i < a.length; i += 1) { const n = out.length; firstDiff(a[i], b[i], `${at}[${i}]`, out); if (out.length > n) return; }
    return;
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    for (const key of new Set([...Object.keys(a), ...Object.keys(b)])) {
      if (!(key in a)) { out.push(`${at}.${key}: 仅 Rust 有`); return; }
      if (!(key in b)) { out.push(`${at}.${key}: 仅 Go 有`); return; }
      const n = out.length;
      firstDiff(a[key], b[key], `${at}.${key}`, out);
      if (out.length > n) return;
    }
    return;
  }
  out.push(`${at}: Go=${JSON.stringify(a)} Rust=${JSON.stringify(b)}`);
};
const post = async (port, command, body) => {
  const response = await fetch(`http://127.0.0.1:${port}/api/${command}`, {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body ?? {}), signal: AbortSignal.timeout(15_000),
  });
  const text = await response.text();
  let json; let parsed = false;
  try { json = JSON.parse(text); parsed = true; } catch {}
  return { status: response.status, json, text, parsed };
};
const readFile = (root, file) => {
  const full = path.join(root, '.data', file);
  return existsSync(full) ? readFileSync(full, 'utf8') : null;
};

// ── steps ──────────────────────────────────────────────────────────────────

const steps = [
  // 安装状态（experts / science）
  { name: 'experts.statuses', command: 'experts_list_all_install_statuses', body: {} },
  { name: 'experts.status', command: 'experts_get_install_status', body: { expertId: 'one', agentType: 'codex' } },
  { name: 'experts.status.missing', command: 'science_get_install_status', body: { expertId: 'nope' } },
  { name: 'science.statuses', command: 'science_list_all_install_statuses', body: {} },
  // custom agents 写
  { name: 'custom.save', command: 'acp_save_custom_agent', body: { params: { registryId: 'demo-a', name: 'Demo', spec: { npx: { package: '@demo/a', args: [] } }, source: 'manual' } }, file: 'codeg-workspace.json' },
  { name: 'custom.save.dup', command: 'acp_save_custom_agent', body: { params: { registryId: 'codex-acp', spec: { npx: { package: 'p' } } } } },
  { name: 'custom.list', command: 'acp_list_custom_agents', body: {} },
  { name: 'custom.delete', command: 'acp_delete_custom_agent', body: { registryId: 'demo-a' }, file: 'codeg-workspace.json' },
  { name: 'custom.delete.again', command: 'acp_delete_custom_agent', body: { registryId: 'demo-a' } },
  // connection / mcp
  { name: 'conn.catalog', command: 'codepluse_connection_catalog', body: {} },
  { name: 'conn.save.bad', command: 'codepluse_connection_save', body: { agentType: 'nope', authMode: 'api' } },
  { name: 'mcp.status', command: 'get_codeg_mcp_service_status', body: {} },
  // instances
  { name: 'inst.list', command: 'codepluse_instance_list', body: {} },
  { name: 'inst.create.bad', command: 'codepluse_instance_create', body: { name: 'x', realTool: 'qoder-not-installed' } },
  { name: 'inst.update.missing', command: 'codepluse_instance_update', body: { agentId: 'agt_inst_nope', name: 'x' } },
  { name: 'inst.delete.missing', command: 'codepluse_instance_delete', body: { agentId: 'agt_inst_nope' } },
  { name: 'mainbot.save', command: 'update_main_bot_settings', body: { settings: { agent_type: 'codex' } }, file: 'codeg-workspace.json' },
  { name: 'mainbot.bad', command: 'update_main_bot_settings', body: { settings: 'nope' } },
  // turn 库
  { name: 'conv.get', command: 'get_conversation', body: { conversationId: 2 } },
  { name: 'conv.get.missing', command: 'get_folder_conversation', body: { conversationId: 99 } },
  { name: 'conv.turns', command: 'get_folder_conversation_turns', body: { conversationId: 2, beforeIndex: 2, limit: 1 } },
  { name: 'conv.turns.bad', command: 'get_folder_conversation_turns', body: { conversationId: 99 } },
  { name: 'delete.missing', command: 'delete_conversation', body: { conversationId: 99 } },
  // token usage
  { name: 'usage.report', command: 'token_usage_report', body: { filter: { tzOffsetMinutes: 480, bucket: 'day', start: '2026-11-01T00:00:00Z', end: '2026-12-01T00:00:00Z', comparePrevious: true } } },
  { name: 'usage.facets', command: 'token_usage_facets', body: {} },
  { name: 'usage.status', command: 'token_usage_status', body: {} },
  { name: 'usage.sync', command: 'token_usage_sync', body: {} },
  // 引擎
  { name: 'engine.merge.missing', command: 'work_task_engine_merge', body: { id: 999 } },
  { name: 'engine.cancel', command: 'work_task_engine_cancel_conn', body: { id: 2 } },
  { name: 'engine.discard', command: 'work_task_engine_discard_worktree', body: { id: 1 } },
];

let failures = 0;
const check = (ok, message) => {
  if (!ok) { failures += 1; console.error(`❌ ${message}`); }
};

try {
  await waitHealth(GO_PORT);
  await waitHealth(RUST_PORT);
  for (const { name, command, body, file } of steps) {
    const [go, rust] = await Promise.all([
      post(GO_PORT, command, body),
      post(RUST_PORT, command, body),
    ]);
    if (go.status !== rust.status) {
      check(false, `${name}: 状态码 ${go.status} != ${rust.status}\n   Go ${go.text.slice(0, 200)}\n   Rust ${rust.text.slice(0, 200)}`);
      continue;
    }
    const diffs = [];
    if (!go.parsed || !rust.parsed) {
      if (maskText(go.text) !== maskText(rust.text)) diffs.push(`非 JSON 响应：${go.text.slice(0, 160)} vs ${rust.text.slice(0, 160)}`);
    } else {
      firstDiff(maskValue(go.json), maskValue(rust.json), '$', diffs);
    }
    if (diffs.length) check(false, `${name}: 响应不一致\n   ${diffs.slice(0, 3).join('\n   ')}`);
    else if (go.status >= 400) console.log(`· ${name}: ${go.status}（错误文案一致）`);
    else console.log(`· ${name}: MATCH`);

    if (file) {
      const a = maskText(readFile(goRoot, file) ?? '');
      const b = maskText(readFile(rustRoot, file) ?? '');
      if (a !== b) {
        const at = [...a].findIndex((ch, i) => ch !== b[i]);
        check(false, `${name}: ${file} 落盘不一致 @${at}\n   Go   …${a.slice(Math.max(0, at - 50), at + 50)}…\n   Rust …${b.slice(Math.max(0, at - 50), at + 50)}…`);
      }
    }
  }
} finally {
  for (const child of children) { try { child.kill(); } catch {} }
  if (!keep) {
    for (let attempt = 0; attempt < 5; attempt += 1) {
      try { rmSync(base, { recursive: true, force: true }); break; } catch { await new Promise((r) => setTimeout(r, 300)); }
    }
  } else console.log(`· 保留现场：${base}`);
}

if (failures > 0) {
  console.error(`\n❌ ${failures} 处不一致`);
  process.exit(1);
}
console.log('\n✅ 双跑全部一致');
process.exit(0);
