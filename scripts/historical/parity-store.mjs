#!/usr/bin/env node
// Twin-workspace dual-run for the store-write migration batch.
//
// Unlike scripts/parity.mjs (same file, both sides write it, ids drift), this
// harness gives Go and Rust two identical workspaces and compares, per step:
//   1. the HTTP responses (deep JSON equality), and
//   2. the on-disk `codeg-workspace.json` / `codeg-folder-commands.json`,
//      byte-for-byte after masking timestamps, random ids and absolute paths.
// A WebSocket client stays attached to the Rust front door and records the
// injected event frames so ported commands are proven to still feed the UI.
//
//   go build -o server-go/serve-codeg.exe ./cmd/serve        (first)
//   cargo build                                              (first)
//   node scripts/parity-store.mjs [--keep] [--go 8840] [--rust 8841]
//
// Exit 0 = every step matched.

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
import process from 'node:process';

const repoRoot = path.resolve(new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const args = process.argv.slice(2);
const option = (name, fallback) => {
  const at = args.indexOf(`--${name}`);
  return at >= 0 ? args[at + 1] : fallback;
};
const keep = args.includes('--keep');
const GO_PORT = Number(option('go', '8840'));
const RUST_PORT = Number(option('rust', '8841'));
const GO_EXE = path.join(repoRoot, 'server-go', 'serve-codeg.exe');
const RUST_EXE = path.join(repoRoot, 'src-tauri', 'target', 'debug', 'codepluse.exe');

const die = (message) => {
  console.error(`❌ ${message}`);
  process.exit(2);
};
if (!existsSync(GO_EXE)) die('缺少 server-go/serve-codeg.exe，先 go build -o server-go/serve-codeg.exe ./cmd/serve');
if (!existsSync(RUST_EXE)) die('缺少 src-tauri/target/debug/codepluse.exe，先 cargo build');

// ── twin workspaces ─────────────────────────────────────────────────────────

// Keep the twins on the repo drive: AppData may hide junctions that would
// make Go's EvalSymlinks and the raw temp path disagree.
const base = mkdtempSync(path.join(repoRoot, '.superpowers', 'sdd', 'parity-store-'));
const twin = (tag) => {
  const root = path.join(base, tag);
  mkdirSync(path.join(root, '.data'), { recursive: true });
  mkdirSync(path.join(root, 'fresh-1'), { recursive: true });
  mkdirSync(path.join(root, 'chat-pre'), { recursive: true });
  mkdirSync(path.join(root, 'proj2'), { recursive: true });
  writeFileSync(
    path.join(root, 'proj2', 'package.json'),
    JSON.stringify({ scripts: { zulu: 'z', alpha: 'a', build: 'b' } }),
  );
  return root;
};
const rootA = twin('go');
const rootB = twin('rust');
const homeA = twin('home-go');
const homeB = twin('home-rust');

const sha256 = (data) => crypto.createHash('sha256').update(data).digest('hex');
const skillHash = sha256('# one\n');
const scienceHash = sha256('# two\n');

const seedStore = (root) =>
  JSON.stringify({
    next: 10,
    folders: [
      { id: 1, name: 'a', path: root, kind: 'regular', open: true, sort_order: 0, color: 'default', group_id: 1, alias: null, last_opened_at: '2026-11-13T00:00:00Z' },
      { id: 2, name: 'proj2', path: path.join(root, 'proj2'), kind: 'regular', open: true, sort_order: 1, color: 'default', group_id: null, alias: null },
      { id: 3, name: 'closed', path: path.join(root, 'closed'), kind: 'regular', open: false, sort_order: 2, color: 'default', group_id: null, alias: null },
    ],
    conversations: [
      { id: 4, folder_id: 1, title: 'Alpha', title_locked: false, agent_type: 'claude', status: 'completed', kind: 'regular', model: null, git_branch: null, external_id: null, message_count: 0, child_count: 0, created_at: '2026-11-13T00:00:00Z', updated_at: '2026-11-13T00:00:00Z', pinned_at: null, parent_id: null, tag_ids: [7] },
      { id: 5, folder_id: 1, title: 'Beta', title_locked: false, agent_type: 'codex', status: 'running', kind: 'regular', model: null, git_branch: null, external_id: null, message_count: 0, child_count: 0, created_at: '2026-11-13T00:00:00Z', updated_at: '2026-11-13T00:00:00Z', pinned_at: null, parent_id: null, tag_ids: [8] },
    ],
    settings: { theme: 'dark' },
    tabs: [],
    tabsVersion: 0,
    canvasBoards: [],
    canvasNodes: [],
    canvasRevision: 0,
    folderGroups: [{ id: 1, name: 'g', color: 'default', sort_order: 0 }],
    conversationTags: [
      { id: 7, name: 't7', color: '', sort_order: 0 },
      { id: 8, name: 't8', color: '', sort_order: 1 },
    ],
    assistants: [],
    quickMessages: [],
  });

for (const root of [rootA, rootB]) {
  writeFileSync(path.join(root, '.data', 'codeg-workspace.json'), seedStore(root));
}

// codeg-assets for the office/experts batch (identical bytes on both sides).
const assets = (root) => {
  const dir = path.join(root, 'codeg-assets');
  mkdirSync(path.join(dir, 'experts', 'skills', 'one'), { recursive: true });
  mkdirSync(path.join(dir, 'science', 'skills', 'two'), { recursive: true });
  writeFileSync(path.join(dir, 'office-skill.md'), '# office\n');
  writeFileSync(path.join(dir, 'experts', 'skills', 'one', 'SKILL.md'), '# one\n');
  writeFileSync(
    path.join(dir, 'experts', 'manifest.json'),
    JSON.stringify([{ id: 'one', display_name: { en: 'One' }, bundled_hash: skillHash }]),
  );
  writeFileSync(path.join(dir, 'science', 'skills', 'two', 'SKILL.md'), '# two\n');
  writeFileSync(
    path.join(dir, 'science', 'manifest.json'),
    JSON.stringify([{ id: 'two', display_name: { en: 'Two' }, bundled_hash: scienceHash }]),
  );
};
assets(rootA);
assets(rootB);

// ── servers ─────────────────────────────────────────────────────────────────

const children = [];
const spawnServer = (exe, cwd, env, label) => {
  const child = spawn(exe, [], { cwd, env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stderr.on('data', (chunk) => process.env.PARITY_VERBOSE && process.stderr.write(`[${label}] ${chunk}`));
  child.on('exit', (code) => {
    if (code !== 0 && code !== null) die(`${label} 退出（${code}）`);
  });
  children.push(child);
  return child;
};

spawnServer(GO_EXE, rootA, {
  HARNESS_HUB_CODEG: '1',
  PORT: String(GO_PORT),
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
  USERPROFILE: homeA,
  HOME: homeA,
}, 'go');
spawnServer(RUST_EXE, rootB, {
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
  CODEPLUSE_HEADLESS: '1',
  CODEPLUSE_PORT: String(RUST_PORT),
  CODEPLUSE_ROOT: rootB,
  USERPROFILE: homeB,
  HOME: homeB,
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

// ── WS frame recorder (Rust front door) ─────────────────────────────────────

const wsFrames = [];
const connectRecorder = () =>
  new Promise((resolve) => {
    try {
      const socket = new WebSocket(`ws://127.0.0.1:${RUST_PORT}/ws/events`, 'codeg-events');
      socket.onmessage = (event) => {
        try {
          wsFrames.push(JSON.parse(event.data));
        } catch {}
      };
      socket.onopen = () => resolve(socket);
      socket.onerror = () => resolve(null);
      setTimeout(() => resolve(socket.readyState === 1 ? socket : null), 5000);
    } catch {
      resolve(null);
    }
  });

// ── comparison ──────────────────────────────────────────────────────────────

const TIME = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z/g;
const UID = /\b[0-9a-f]{32}\b/g;
const CHAT_DIR = /chat-[0-9a-z]+/g;

const maskText = (text) =>
  text
    .replaceAll(rootA.replaceAll('\\', '\\\\'), '<ROOT>')
    .replaceAll(rootB.replaceAll('\\', '\\\\'), '<ROOT>')
    .replaceAll(rootA.replaceAll('\\', '/'), '<ROOT>')
    .replaceAll(rootB.replaceAll('\\', '/'), '<ROOT>')
    .replaceAll(rootA, '<ROOT>')
    .replaceAll(rootB, '<ROOT>')
    .replaceAll(homeA.replaceAll('\\', '\\\\'), '<HOME>')
    .replaceAll(homeB.replaceAll('\\', '\\\\'), '<HOME>')
    .replaceAll(homeA, '<HOME>')
    .replaceAll(homeB, '<HOME>')
    .replace(TIME, '<TS>')
    .replace(UID, '<UID>')
    .replace(CHAT_DIR, '<CHAT>');

const maskValue = (value) => {
  if (typeof value === 'string') return maskText(value);
  if (Array.isArray(value)) return value.map(maskValue);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, maskValue(item)]));
  }
  return value;
};

const firstDiff = (a, b, at, out) => {
  if (a === b) return;
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) {
      out.push(`${at}: 长度 ${a.length} != ${b.length}`);
      return;
    }
    for (let index = 0; index < a.length; index += 1) firstDiff(a[index], b[index], `${at}[${index}]`, out);
    return;
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
    for (const key of keys) {
      if (!(key in a)) {
        out.push(`${at}.${key}: 仅 Rust 有`);
        return;
      }
      if (!(key in b)) {
        out.push(`${at}.${key}: 仅 Go 有`);
        return;
      }
      const before = out.length;
      firstDiff(a[key], b[key], `${at}.${key}`, out);
      if (out.length > before) return;
    }
    return;
  }
  out.push(`${at}: Go=${JSON.stringify(a)} Rust=${JSON.stringify(b)}`);
};

const post = async (port, command, body) => {
  const response = await fetch(`http://127.0.0.1:${port}/api/${command}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body ?? {}),
  });
  const text = await response.text();
  // `null` is a valid JSON body (Go's respond(nil)); only a real parse
  // failure marks the response as non-JSON.
  let json;
  let parsed = false;
  try {
    json = JSON.parse(text);
    parsed = true;
  } catch {}
  return { status: response.status, json, text, parsed };
};

const readStore = (root, file) => {
  const full = path.join(root, '.data', file);
  if (!existsSync(full)) return null;
  return readFileSync(full, 'utf8');
};

// ── steps ───────────────────────────────────────────────────────────────────

const step = (name, command, body, file = 'codeg-workspace.json') => ({ name, command, body, file });
const steps = [
  // office / experts（待验收批）
  step('office.detect', 'officecli_detect', {}, null),
  step('office.skills', 'officecli_list_skills', {}, null),
  step('office.sync', 'officecli_sync_skills', {}, null),
  step('office.read', 'officecli_skill_read_content', { skillId: 'office-skill-office-skill' }, null),
  step('office.read.bad', 'officecli_skill_read_content', { skillId: 'office-skill-../x' }, null),
  step('office.statuses', 'officecli_skill_list_all_install_statuses', {}, null),
  step('office.install', 'officecli_install', {}, null),
  step('office.render', 'officecli_render_html', {}, null),
  step('experts.list', 'experts_list', {}, null),
  step('experts.read', 'experts_read_content', { expertId: 'one' }, null),
  step('experts.read.missing', 'experts_read_content', { expertId: 'missing' }, null),
  step('science.list', 'science_list', {}, null),
  step('experts.link', 'experts_link_to_agent', { expertId: 'one', agentType: 'claude' }, null),
  step('experts.link.again', 'experts_link_to_agent', { expertId: 'one', agentType: 'claude' }, null),
  step('experts.apply', 'experts_apply_links', { ops: [{ expertId: 'one', agentType: 'codex', enable: true }, { expertId: 'missing', agentType: 'claude', enable: true }] }, null),
  step('experts.unlink', 'experts_unlink_from_agent', { expertId: 'one', agentType: 'claude' }, null),
  step('experts.unlink.codex', 'experts_unlink_from_agent', { expertId: 'one', agentType: 'codex' }, null),

  // sidebar 写
  step('group.create', 'create_folder_group', { name: 'g<b>', color: 'red' }),
  step('group.create2', 'create_folder_group', { name: 'g2' }),
  step('group.update', 'update_folder_group', { groupId: 10, color: 'blue' }),
  step('group.update.missing', 'update_folder_group', { groupId: 999 }),
  step('group.delete', 'delete_folder_group', { groupId: 11 }),
  step('group.set', 'set_folder_group', { folderId: 1, groupId: 10 }),
  step('group.set.null', 'set_folder_group', { folderId: 1, groupId: null }),
  step('layout.apply', 'apply_sidebar_layout', { entries: [{ kind: 'group', id: 10 }, { kind: 'folder', id: 1, groupId: null }, { kind: 'folder', id: 2, groupId: 10 }] }),
  step('tag.create', 'create_conversation_tag', { folderId: 1, name: 't1', color: 'c' }),
  step('tag.update', 'update_conversation_tag', { tagId: 12, color: 'x' }),
  step('tag.reorder', 'reorder_conversation_tags', { tagIds: [12, 8, 7] }),
  step('branch.update', 'update_conversation_branch_tag', { enabled: true, color: 'teal' }),
  step('conv.tags', 'update_conversation_tags', { conversationId: 4, add: [12] }),
  step('conv.tags.remove', 'update_conversation_tags', { conversationId: 4, remove: [7], add: [8] }),
  step('tag.delete', 'delete_conversation_tag', { tagId: 12 }),

  // assistants & quick messages
  step('assistant.create', 'assistant_create', { draft: { name: 'A', description: 'd', enabled: true, instructions: 'i', avatar: 'a', agent_config: { agent_type: 'claude', agent_options: { k: 1 }, label_snapshot: 'L' } } }),
  step('assistant.update', 'assistant_update', { id: 13, patch: { enabled: false, name: 'B' } }),
  step('assistant.reorder', 'assistant_reorder', { ids: [13] }),
  step('assistant.delete', 'assistant_delete', { id: 13 }),
  step('quick.create', 'quick_messages_create', { title: 'q', content: 'c' }),
  step('quick.update', 'quick_messages_update', { id: 14, content: 'c2' }),
  step('quick.update.null', 'quick_messages_update', { id: 14, title: null }),
  step('quick.reorder', 'quick_messages_reorder', { ids: [14] }),
  step('quick.delete', 'quick_messages_delete', { id: 14 }),

  // workspace 写
  step('conv.create', 'create_conversation', { folderId: 1, agentType: 'claude', title: 't' }),
  step('conv.title', 'update_conversation_title', { conversationId: 15, title: 'renamed' }),
  step('conv.status', 'update_conversation_status', { conversationId: 15, status: 'completed' }),
  step('conv.pinned', 'update_conversation_pinned', { conversationId: 15, pinned: true }),
  step('conv.pinned.off', 'update_conversation_pinned', { conversationId: 15, pinned: false }),
  step('tabs.save', 'save_opened_tabs', { expectedVersion: 0, items: [{ conversationId: 15, id: 'x' }] }),
  step('tabs.reject', 'save_opened_tabs', { expectedVersion: 99, items: [] }),
  step('settings.update', 'update_system_settings', { k: 'v', nested: { a: 1, z: 2 } }),
  step('settings.save', 'save_system_settings', { k2: 'v2' }),
  step('commit.set', 'set_commit_message_settings', { folderId: 1, settings: { agent: null, instructions: 'x', auto_commit: true } }),
  step('commit.delete', 'delete_commit_message_settings', { folderId: 1 }),
  step('folder.color', 'update_folder_color', { folderId: 1, color: 'red' }),
  step('folder.alias', 'update_folder_alias', { folderId: 1, alias: 'al' }),
  step('folder.agent', 'update_folder_default_agent', { folderId: 1, defaultAgentType: 'codex' }),
  step('folder.remove', 'remove_folder_from_workspace', { folderId: 3 }),
  step('folder.open', 'open_folder', { path: 'fresh-1' }),
  step('chat.dir', 'create_chat_dir', {}),
  step('chat.conv', 'create_chat_conversation', { agentType: 'claude', title: 'chat' }),
  step('chat.conv.existing', 'create_chat_conversation', { agentType: 'codex', title: 'pre', existingDir: 'chat-pre' }),

  // canvas — ids are captured from previous responses: the exact allocation
  // order is an implementation detail the harness must not bake in.
  step('canvas.board', 'canvas_create_board', { input: { name: 'B1', color: 'red' } }),
  step('canvas.node.note', 'canvas_create_node', (c) => ({ input: { boardId: c['canvas.board'].json.id, kind: 'note', title: 'n', content: 'c', x: 10, y: 20.5, width: 300, height: 200 } })),
  step('canvas.node.conv', 'canvas_create_node', (c) => ({ input: { boardId: c['canvas.board'].json.id, kind: 'conversation', conversationId: 15, x: 0, y: 0, width: 320, height: 220 } })),
  step('canvas.node.terminal', 'canvas_create_node', (c) => ({ input: { boardId: c['canvas.board'].json.id, kind: 'terminal', path: 'gone/deeper/none.txt', x: 1, y: 1, width: 320, height: 220 } })),
  step('canvas.node.bad', 'canvas_create_node', { input: { boardId: 9999, kind: 'note', x: 1, y: 1, width: 100, height: 100 } }),
  step('canvas.node.badkind', 'canvas_create_node', (c) => ({ input: { boardId: c['canvas.board'].json.id, kind: 'widget', x: 1, y: 1, width: 100, height: 100 } })),
  step('canvas.node.missing-geo', 'canvas_create_node', (c) => ({ input: { boardId: c['canvas.board'].json.id, kind: 'note', x: 1, y: 1, width: 100 } })),
  step('canvas.update', 'canvas_update_node', (c) => ({ nodeId: c['canvas.node.note'].json.value.id, patch: { x: 50, y: 60, title: 't2', collapsed: true } })),
  step('canvas.update.bad-collapsed', 'canvas_update_node', (c) => ({ nodeId: c['canvas.node.note'].json.value.id, patch: { collapsed: 'yes' } })),
  step('canvas.move', 'canvas_move_nodes', (c) => ({ moves: [{ id: c['canvas.node.note'].json.value.id, x: 1, y: 2 }, { id: 9999, x: 0, y: 0 }] })),
  step('canvas.grid.bad', 'canvas_update_node', (c) => ({ nodeId: c['canvas.node.note'].json.value.id, patch: { gridColumns: 3 } })),
  step('canvas.group', 'canvas_group_into_region', (c) => ({ input: { boardId: c['canvas.board'].json.id, memberIds: [15], consumeNodeIds: [c['canvas.node.conv'].json.value.id], title: 'R', gridColumns: 4, gridRows: 2, x: 0, y: 0, width: 800, height: 600 } })),
  step('canvas.detach', 'canvas_detach_member', (c) => ({ regionId: c['canvas.group'].json.value.node.id, conversationId: 15, x: 5, y: 6 })),
  step('canvas.boards', 'canvas_list_boards', {}),
  step('canvas.nodes', 'canvas_list_nodes', (c) => ({ boardId: c['canvas.board'].json.id })),
  step('canvas.delete.node', 'canvas_delete_node', (c) => ({ nodeId: c['canvas.detach'].json.value.id })),
  step('canvas.delete.nodes', 'canvas_delete_nodes', (c) => ({ nodeIds: [c['canvas.group'].json.value.node.id] })),
  step('canvas.delete.board', 'canvas_delete_board', (c) => ({ boardId: c['canvas.board'].json.id })),

  // folder commands（独立文件，自己的 id 计数从 1 起）
  step('fc.list', 'list_folder_commands', { folderId: 1 }),
  step('fc.create', 'create_folder_command', { folderId: 1, name: 'build', command: 'pnpm build' }, 'codeg-folder-commands.json'),
  step('fc.create2', 'create_folder_command', { folderId: 1, name: 'test', command: 'pnpm test', runLocal: true }, 'codeg-folder-commands.json'),
  step('fc.create.empty', 'create_folder_command', { folderId: 1, name: ' ', command: 'x' }, 'codeg-folder-commands.json'),
  step('fc.update', 'update_folder_command', { id: 1, name: 'build all' }, 'codeg-folder-commands.json'),
  step('fc.reorder', 'reorder_folder_commands', { folderId: 1, ids: [2, 1] }, 'codeg-folder-commands.json'),
  step('fc.bootstrap', 'bootstrap_folder_commands_from_package_json', { folderId: 2 }, 'codeg-folder-commands.json'),
  step('fc.bootstrap.again', 'bootstrap_folder_commands_from_package_json', { folderId: 2 }, 'codeg-folder-commands.json'),
  step('fc.delete', 'delete_folder_command', { id: 1 }, 'codeg-folder-commands.json'),
  step('fc.delete.again', 'delete_folder_command', { id: 1 }, 'codeg-folder-commands.json'),
  step('fc.list.after', 'list_folder_commands', { folderId: 1 }),
];

// ── run ─────────────────────────────────────────────────────────────────────

let failures = 0;
const check = (ok, message) => {
  if (!ok) {
    failures += 1;
    console.error(`❌ ${message}`);
  }
};

let wsSocket = null;
try {
  await waitHealth(GO_PORT);
  await waitHealth(RUST_PORT);
  wsSocket = await connectRecorder();

  const seen = {};
  for (const { name, command, body, file } of steps) {
    const resolved = typeof body === 'function' ? body(seen) : body;
    const [go, rust] = await Promise.all([
      post(GO_PORT, command, resolved),
      post(RUST_PORT, command, resolved),
    ]);
    seen[name] = { status: go.status, json: go.json };

    if (go.status !== rust.status) {
      check(false, `${name}: 状态码 ${go.status} != ${rust.status}\n   Go ${go.text.slice(0, 200)}\n   Rust ${rust.text.slice(0, 200)}`);
      continue;
    }
    const diffs = [];
    if (!go.parsed || !rust.parsed) {
      if (maskText(go.text) !== maskText(rust.text)) {
        diffs.push(`非 JSON 响应体不一致：${go.text.slice(0, 120)} vs ${rust.text.slice(0, 120)}`);
      }
    } else {
      firstDiff(maskValue(go.json), maskValue(rust.json), '$', diffs);
    }
    if (diffs.length) {
      check(false, `${name}: 响应不一致\n   ${diffs.slice(0, 4).join('\n   ')}`);
    } else if (go.status >= 400) {
      console.log(`· ${name}: ${go.status}（错误文案一致）`);
    } else {
      console.log(`· ${name}: MATCH`);
    }

    if (file) {
      const a = maskText(readStore(rootA, file) ?? '');
      const b = maskText(readStore(rootB, file) ?? '');
      if (a !== b) {
        const at = [...a].findIndex((ch, index) => ch !== b[index]);
        check(false, `${name}: ${file} 落盘不一致 @${at}\n   Go   …${a.slice(Math.max(0, at - 40), at + 40)}…\n   Rust …${b.slice(Math.max(0, at - 40), at + 40)}…`);
      }
    }
  }

  // Event injection: every channel the ported commands used to emit in Go.
  const channels = new Set(wsFrames.map((frame) => frame.channel));
  for (const expected of ['conversation://changed', 'canvas://changed', 'canvas-board://changed', 'audit://folder_open']) {
    check(channels.has(expected), `WS 注入缺少 ${expected}（收到：${[...channels].join(', ') || '无'}）`);
  }
  console.log(`· WS 注入帧：${wsFrames.length} 帧，channel：${[...channels].join(', ')}`);
} finally {
  if (wsSocket) {
    try {
      wsSocket.close();
    } catch {}
  }
  for (const child of children) {
    try {
      child.kill();
    } catch {}
  }
  if (!keep) {
    for (let attempt = 0; attempt < 5; attempt += 1) {
      try {
        rmSync(base, { recursive: true, force: true });
        break;
      } catch {
        await new Promise((resolve) => setTimeout(resolve, 300));
      }
    }
  } else {
    console.log(`· 保留现场：${base}`);
  }
}

if (failures > 0) {
  console.error(`\n❌ ${failures} 处不一致`);
  process.exit(1);
}
console.log('\n✅ 双跑全部一致');
