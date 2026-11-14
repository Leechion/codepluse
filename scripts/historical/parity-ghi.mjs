#!/usr/bin/env node
// Twin-workspace dual-run for the G/H/I migration batch (terminal /
// automations / workspace-state / misc / git_commit).
//
// 同 scripts/parity-store.mjs 的骨架：Go 与 Rust 各领一个相同种子的双胞胎
// 工作区，逐条命令对比 HTTP 响应与落盘字节。差异：
//   - 终端命令的输出内容是实时流（分块边界不定），按结构化字段比较，
//     输出文本只断言包含标记；
//   - pick_directory 弹模态对话框，无法自动化，不进双跑（人工点一次即可）；
//   - automation_run_now 的运行执行经桥接走到 Go 的 acp_connect（ACP 仍归
//     Go），双胞胎里没有可用执行端 → 运行失败，对比失败路径的事件与落盘。
//
//   go build -o server-go/serve-codeg.exe ./cmd/serve        （先构建）
//   cargo build                                              （先构建）
//   node scripts/parity-ghi.mjs [--keep] [--go 8840] [--rust 8841]
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
import { execSync } from 'node:child_process';
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

const base = mkdtempSync(path.join(repoRoot, '.superpowers', 'sdd', 'parity-ghi-'));
const twin = (tag) => {
  const root = path.join(base, tag);
  mkdirSync(path.join(root, '.data'), { recursive: true });
  return root;
};
const rootA = twin('go');
const rootB = twin('rust');

// git 仓库（两边同样步骤初始化），git_commit 步骤用。b.txt 保持未跟踪，
// 由 git_commit 步骤提交。
const seedGit = (root) => {
  const run = (cmd) => execSync(cmd, { cwd: root, stdio: 'pipe' });
  run('git init -b main');
  run('git config user.email parity@example.com');
  run('git config user.name parity');
  writeFileSync(path.join(root, 'a.txt'), 'base\n');
  writeFileSync(path.join(root, 'b.txt'), 'new\n');
  run('git add -- a.txt');
  run('git commit -m baseline');
};
seedGit(rootA);
seedGit(rootB);

const seedStore = (root) =>
  JSON.stringify({
    next: 10,
    folders: [
      { id: 1, name: 'a', path: root, kind: 'regular', open: true, sort_order: 0, color: 'default', group_id: null, alias: null, last_opened_at: '2026-11-13T00:00:00Z' },
    ],
    conversations: [],
    settings: { theme: 'dark' },
    tabs: [],
    tabsVersion: 0,
    canvasBoards: [],
    canvasNodes: [],
    canvasRevision: 0,
    folderGroups: [],
    conversationTags: [],
    assistants: [],
    quickMessages: [],
    customAgents: {
      good: {
        registryId: 'good',
        name: 'Good Agent',
        description: 'd',
        version: '1.0.0',
        distributionKind: 'npx',
        spec: { npx: { package: '@x/good' } },
        skillsSharedStore: false,
        source: 'manual',
        supportsMcp: true,
      },
      badpkg: {
        registryId: 'badpkg',
        name: 'Bad',
        version: '0.1.0',
        distributionKind: 'npx',
        spec: { npx: { package: '-flag' } },
        skillsSharedStore: false,
        source: 'manual',
        supportsMcp: true,
      },
    },
  });

for (const root of [rootA, rootB]) {
  writeFileSync(path.join(root, '.data', 'codeg-workspace.json'), seedStore(root));
}

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
  // 关闭 warm 池：预热会尝试拉起 codex，往 .data/codex-native-home 写文件，
  // 让两个孪生的磁盘内容失真（workspace 扫描如实报告差异）。
  HARNESS_HUB_WARM: '0',
}, 'go');
spawnServer(RUST_EXE, rootB, {
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
  CODEPLUSE_HEADLESS: '1',
  CODEPLUSE_PORT: String(RUST_PORT),
  CODEPLUSE_ROOT: rootB,
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

const TIME = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})/g;
const UID = /\b[0-9a-f]{32}\b/g;
const PORT = /:(8840|8841|8842)\b/g;

const maskText = (text) =>
  text
    .replaceAll(rootA.replaceAll('\\', '\\\\'), '<ROOT>')
    .replaceAll(rootB.replaceAll('\\', '\\\\'), '<ROOT>')
    .replaceAll(rootA.replaceAll('\\', '/'), '<ROOT>')
    .replaceAll(rootB.replaceAll('\\', '/'), '<ROOT>')
    .replaceAll(rootA, '<ROOT>')
    .replaceAll(rootB, '<ROOT>')
    .replace(TIME, '<TS>')
    .replace(UID, '<UID>')
    .replace(PORT, ':<PORT>');

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

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// ── steps ───────────────────────────────────────────────────────────────────

const step = (name, command, body, options = {}) => ({ name, command, body, ...options });
const steps = [
  // ── G 终端：shell 枚举（响应完全确定）
  step('shell.available', 'get_available_terminal_shells', { path: '' }),
  step('shell.probe.ok', 'probe_terminal_shell_path', { path: 'cmd.exe' }),
  step('shell.probe.bad', 'probe_terminal_shell_path', { path: 'notepad.exe' }),

  // ── I：acp_current_platform / acp_list_custom_agents（种子含合法+非法定义）
  step('custom.platform', 'acp_current_platform', {}),
  step('custom.list', 'acp_list_custom_agents', {}),

  // ── I：git_commit（预置 git 仓库）
  step('git.commit', 'git_commit', { path: '.', files: ['b.txt'], message: 'parity commit', folderId: 1 }),
  step('git.commit.empty', 'git_commit', { path: '.', files: ['a.txt'], message: ' ' }),

  // ── I：workspace 状态流（时序敏感的字段用自定义比较）
  step('ws.start', 'start_workspace_state_stream', { rootPath: '.' }, { compare: 'snapshot' }),
  step('ws.get', 'get_workspace_snapshot', { rootPath: '.' }, { compare: 'snapshot', prepare: (root) => writeFileSync(path.join(root, 'ws-change.txt'), 'changed') }),
  step('ws.wait', 'probe_terminal_shell_path', { path: 'cmd.exe' }, { settle: 2200, prepare: (root) => writeFileSync(path.join(root, 'ws-change-2.txt'), 'changed') }),
  step('ws.stop', 'stop_workspace_state_stream', { rootPath: '.' }),

  // ── G 终端：真 ConPTY 会话（自定义比较：输出流只断言包含标记）
  step('term.spawn', 'terminal_spawn', { terminalId: 't1', shell: 'cmd.exe', workingDir: '.' }, { compare: 'terminal' }),
  step('term.snapshot', 'terminal_snapshot', { terminalId: 't1' }, { compare: 'terminal' }),
  step('term.resize', 'terminal_resize', { terminalId: 't1', cols: 100, rows: 28 }, { compare: 'terminal' }),
  step('term.write', 'terminal_write', { terminalId: 't1', data: 'echo PARITY_PTY> out.txt\r' }, { compare: 'terminal' }),
  step('term.output', 'terminal_snapshot', { terminalId: 't1' }, { compare: 'terminal', waitData: true }),
  step('term.write.again', 'terminal_spawn', { terminalId: 't1', shell: 'cmd.exe', workingDir: '.' }, { compare: 'terminal' }),
  step('term.kill.keep', 'terminal_kill', { terminalId: 't1', keepOutput: true }, { compare: 'terminal' }),
  step('term.snapshot.kept', 'terminal_snapshot', { terminalId: 't1' }, { compare: 'terminal', waitAlive: true }),
  step('term.kill', 'terminal_kill', { terminalId: 't1' }, { compare: 'terminal' }),
  step('term.snapshot.gone', 'terminal_snapshot', { terminalId: 't1' }, { compare: 'terminal' }),
  step('term.list', 'terminal_list', {}, { compare: 'terminal-list' }),

  // ── H 定时任务：CRUD（manual 触发，响应除时间戳外全等）
  step('auto.create', 'automation_create', { draft: {
    name: '晨报',
    trigger_kind: 'manual',
    agent_type: 'nonexistent-agent',
    isolation: 'shared_in_root',
    timezone: 'Asia/Shanghai',
    config: { prompt_blocks: [{ type: 'text', text: '早' }], display_text: 'morning', config_values: { depth: 3, flag: true }, label_snapshot: { agent: 'Codex' } },
  } }, { files: ['codeg-automations.json'] }),
  step('auto.create.schedule', 'automation_create', { draft: {
    name: '夜间',
    trigger_kind: 'schedule',
    cron: '30 2 * * *',
    agent_type: 'nonexistent-agent',
    isolation: 'shared_in_root',
    config: { prompt_blocks: [], display_text: 'night', config_values: {} },
  } }, { files: ['codeg-automations.json'] }),
  step('auto.get', 'automation_get', { id: 1 }),
  step('auto.list', 'automation_list', {}),
  step('auto.set_enabled', 'automation_set_enabled', { id: 1, enabled: false }, { files: ['codeg-automations.json'] }),
  step('auto.update', 'automation_update', { id: 1, draft: {
    name: '晨报改',
    trigger_kind: 'manual',
    agent_type: 'nonexistent-agent',
    config: { prompt_blocks: [], display_text: 'morning2', config_values: {} },
  } }, { files: ['codeg-automations.json'] }),
  step('auto.compute', 'automation_compute_next_run', { cron: '0 9 * * 1-5' }),
  step('auto.compute.bad', 'automation_compute_next_run', { cron: '99 * * * *' }),
  step('auto.runs.empty', 'automation_runs', { automationId: 1, limit: 10 }),
  step('auto.run_now', 'automation_run_now', { automationId: 1 }, { settle: 1200, files: ['codeg-automation-runs.json'] }),
  step('auto.set_enabled.back', 'automation_set_enabled', { id: 1, enabled: true }, { files: ['codeg-automations.json'] }),
  step('auto.mark_seen', 'automation_mark_seen', {}, { files: ['codeg-automations.json'] }),
  step('auto.get.missing', 'automation_get', { id: 999 }),
  step('auto.delete.missing', 'automation_delete', { id: 999 }),
  step('auto.runs.after', 'automation_runs', { automationId: 1, limit: 10 }, { settle: 800, files: ['codeg-automation-runs.json'] }),
  step('auto.cancel.missing', 'automation_cancel_run', { runId: 999 }),
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
  for (const { name, command, body, compare, settle, files, prepare, waitData, waitAlive } of steps) {
    if (prepare) {
      prepare(rootA);
      prepare(rootB);
    }
    const resolved = typeof body === 'function' ? body(seen) : body;
    let [go, rust] = await Promise.all([
      post(GO_PORT, command, resolved),
      post(RUST_PORT, command, resolved),
    ]);
    seen[name] = { status: go.status, json: go.json };
    if (waitData) {
      const deadline = Date.now() + 10_000;
      let settled = false;
      while (Date.now() < deadline) {
        const [g2, r2] = await Promise.all([post(GO_PORT, command, resolved), post(RUST_PORT, command, resolved)]);
        const has = (x) => typeof x.json?.data === 'string' && x.json.data.includes('PARITY_PTY');
        if (has(g2) && has(r2)) { seen[name] = { status: g2.status, json: g2.json }; break; }
        await sleep(200);
      }
      if (!settled && !seen[name]) seen[name] = { status: go.status, json: go.json };
      // 重新取一对用于比较
      const [g3, r3] = await Promise.all([post(GO_PORT, command, resolved), post(RUST_PORT, command, resolved)]);
      go = g3; rust = r3;
    }
    if (waitAlive) {
      const deadline = Date.now() + 10_000;
      for (;;) {
        const [g2, r2] = await Promise.all([post(GO_PORT, command, resolved), post(RUST_PORT, command, resolved)]);
        const dead = (x) => x.json && x.json.exists === true && x.json.alive === false;
        if (dead(g2) && dead(r2)) { go = g2; rust = r2; break; }
        if (Date.now() > deadline) { go = g2; rust = r2; break; }
        await sleep(200);
      }
    }
    if (settle) await sleep(settle);

    const diffs = [];
    if (compare === 'terminal' || compare === 'terminal-list') {
      compareTerminal(name, go, rust, diffs);
    } else if (compare === 'snapshot') {
      compareSnapshot(name, go, rust, diffs);
    } else if (go.status !== rust.status) {
      diffs.push(`状态码 ${go.status} != ${rust.status}\n   Go ${go.text.slice(0, 200)}\n   Rust ${rust.text.slice(0, 200)}`);
    } else if (!go.parsed || !rust.parsed) {
      if (maskText(go.text) !== maskText(rust.text)) {
        diffs.push(`非 JSON 响应体不一致：${go.text.slice(0, 120)} vs ${rust.text.slice(0, 120)}`);
      }
    } else {
      firstDiff(maskValue(go.json), maskValue(rust.json), '$', diffs);
    }
    if (diffs.length) {
      check(false, `${name}: 不一致\n   ${diffs.slice(0, 4).join('\n   ')}`);
    } else {
      console.log(`· ${name}: MATCH${go.status >= 400 ? `（${go.status}，错误文案一致）` : ''}`);
    }

    for (const file of files ?? []) {
      const a = maskText(readStore(rootA, file) ?? '');
      const b = maskText(readStore(rootB, file) ?? '');
      if (a !== b) {
        const at = [...a].findIndex((ch, index) => ch !== b[index]);
        check(false, `${name}: ${file} 落盘不一致 @${at}\n   Go   …${a.slice(Math.max(0, at - 40), at + 40)}…\n   Rust …${b.slice(Math.max(0, at - 40), at + 40)}…`);
      }
    }
  }

  // 事件注入：本批各域在 Go 里 emit 的 channel，Rust 侧必须同样出现。
  const channels = new Set(wsFrames.map((frame) => frame.channel));
  const hasChannel = (prefix) => [...channels].some((c) => c.startsWith(prefix));
  for (const expected of [
    'automation://changed',
    'folder://git-commit-succeeded',
    'folder://workspace-state-event',
    'terminal://output/',
    'terminal://exit/',
  ]) {
    check(hasChannel(expected), `WS 注入缺少 ${expected}（收到：${[...channels].join(', ') || '无'}）`);
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

// ── terminal / snapshot 的结构化比较 ────────────────────────────────────────

function compareTerminal(name, go, rust, diffs) {
  if (go.status !== rust.status) {
    diffs.push(`状态码 ${go.status} != ${rust.status}`);
    return;
  }
  const g = go.json;
  const r = rust.json;
  if (name === 'term.spawn' || name === 'term.write.again') {
    // 都返回同一 tid 字符串。
    if (g !== r) diffs.push(`tid: Go=${JSON.stringify(g)} Rust=${JSON.stringify(r)}`);
    return;
  }
  if (name === 'term.write' || name === 'term.resize' || name === 'term.kill' || name === 'term.kill.keep') {
    if (g !== r) diffs.push(`返回值: Go=${JSON.stringify(g)} Rust=${JSON.stringify(r)}`);
    return;
  }
  if (name === 'term.list') {
    // map 迭代顺序不定：按 id 排序后比较。
    const sort = (list) => [...list].sort((a, b) => (a.id > b.id ? 1 : -1));
    firstDiff(maskValue(sort(g ?? [])), maskValue(sort(r ?? [])), '$', diffs);
    return;
  }
  // snapshot：结构字段相等；data/seq 归一化（输出流是实时的）。
  const norm = (snap, loose) => {
    if (!snap || typeof snap !== 'object') return snap;
    const out = { ...snap };
    out.generation = out.generation == null ? null : '<GEN>';
    // 输出流是实时的：块边界与到达时刻不定。含标记 → 归一；kill 后的快照
    // （loose）不比 data/seq/exit_code/alive——ConPTY 的重绘与退出码采集和
    // 关闭线程之间存在竞态（Go 自身同样不确定拿不拿得到退出码）。
    out.data = loose ? '<DONTCARE>' : (typeof out.data === 'string' && out.data.includes('PARITY_PTY') ? '<HAS_OUTPUT>' : (out.data ?? ''));
    out.seq = loose ? '<DONTCARE>' : (typeof out.seq === 'number' && out.seq > 0 ? '>0' : out.seq);
    if (loose) {
      out.exit_code = out.exit_code == null ? null : '<EXIT>';
      out.alive = typeof out.alive === 'boolean' ? '<BOOL>' : out.alive;
    }
    return maskValue(out);
  };
  firstDiff(norm(g, name === 'term.snapshot.kept'), norm(r, name === 'term.snapshot.kept'), '$', diffs);
}

function compareSnapshot(name, go, rust, diffs) {
  if (go.status !== rust.status) {
    diffs.push(`状态码 ${go.status} != ${rust.status}`);
    return;
  }
  const norm = (snap) => {
    if (!snap || typeof snap !== 'object') return snap;
    const out = { ...snap };
    // seq 由各自观察器递增，只比较「非零推进」。
    out.seq = typeof out.seq === 'number' && out.seq > 0 ? '>0' : out.seq;
    // 树/事件的行序在两边独立生成，排序后再比。
    if (Array.isArray(out.tree_snapshot)) {
      out.tree_snapshot = [...out.tree_snapshot].sort((a, b) => JSON.stringify(a) > JSON.stringify(b) ? 1 : -1);
    }
    if (Array.isArray(out.git_snapshot)) {
      out.git_snapshot = [...out.git_snapshot].sort((a, b) => JSON.stringify(a) > JSON.stringify(b) ? 1 : -1);
    }
    return maskValue(out);
  };
  firstDiff(norm(go.json), norm(rust.json), '$', diffs);
}

if (failures > 0) {
  console.error(`\n❌ ${failures} 处不一致`);
  process.exit(1);
}
console.log('\n✅ 双跑全部一致');
