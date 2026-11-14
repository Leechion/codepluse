#!/usr/bin/env node
// Twin-workspace dual-run for the D/E/F migration batch (work task /
// browser / computer), 同 scripts/parity-ghi.mjs 的骨架.
//
// 与 ghi 的差异：
//   - work task 的执行引擎仍在 Go sidecar（work_task_engine_* 触发桥），
//     所以 Rust 前门需要**自己的** sidecar：三个进程 —— go-twin(rootA)、
//     rust-twin-sidecar(rootB)、rust-front-door(rootB, upstream=sidecarB)。
//     触发命令必须落在 rootB 的 sidecar 上才能读到 rootB 的任务文件。
//   - 浏览器的 CDP 调试端口（9337-9341）是全局资源，两个后端并行会互相
//     接管对方的浏览器。browser 步骤单独跑两轮（先 Go 后 Rust），轮间
//     按「调试端口 + user-data-dir 在系统临时目录」杀掉本轮浏览器，
//     两轮各自从全新浏览器出发，记录后对比。
//   - 缩略图/截图是实时像素：只断言 PNG 魔数与非空；窗口枚举比形状不比
//     内容（两次枚举之间桌面可能变化）。
//   - work task 的引擎路径（start → 连接失败 → failed）两边跑的是同一份
//     Go 引擎代码，最终落盘一致；孪生仓库的 commit SHA 不同（时间戳），
//     40 位十六进制一律归一。
//
//   go build -o server-go/serve-codeg.exe ./cmd/serve        （先构建）
//   cargo build                                              （先构建）
//   node scripts/parity-def.mjs [--keep] [--go 18840] [--rust 18841]
//
// Exit 0 = every step matched.

import { spawn, execSync } from 'node:child_process';
import {
  existsSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import process from 'node:process';

const repoRoot = path.resolve(new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const args = process.argv.slice(2);
const option = (name, fallback) => {
  const at = args.indexOf(`--${name}`);
  return at >= 0 ? args[at + 1] : fallback;
};
const keep = args.includes('--keep');
const GO_PORT = Number(option('go', '18840'));
const GO_PORT_B = Number(option('go-b', '18842'));
const RUST_PORT = Number(option('rust', '18841'));
const GO_EXE = path.join(repoRoot, 'server-go', 'serve-codeg.exe');
// Rust 二进制默认取主仓库 target；也可以 --rust-exe 指到 worktree 里构建的
// 隔离副本（避免与主仓库的 cargo 互锁）。
const RUST_EXE = option('rust-exe', path.join(repoRoot, 'src-tauri', 'target', 'debug', 'codepluse.exe'));

const die = (message) => {
  console.error(`❌ ${message}`);
  process.exit(2);
};
if (!existsSync(GO_EXE)) die('缺少 server-go/serve-codeg.exe，先 go build -o server-go/serve-codeg.exe ./cmd/serve');
if (!existsSync(RUST_EXE)) die(`缺少 ${RUST_EXE}，先 cargo build（或 --rust-exe 指向已构建副本）`);
const agentBundle = path.join(repoRoot, 'codeg-ui', 'browser-agent', 'agent.bundle.js');
if (!existsSync(agentBundle)) die('缺少 codeg-ui/browser-agent/agent.bundle.js（先构建 browser-agent）');

// ── twin workspaces（放系统临时目录：清理浏览器时以此为判据，绝不波及
//    真实工作区的 CDP 浏览器）────────────────────────────────────────────

const base = mkdtempSync(path.join(os.tmpdir(), 'parity-def-'));
const twin = (tag) => {
  const root = path.join(base, tag);
  mkdirSync(path.join(root, '.data'), { recursive: true });
  return root;
};
const rootA = twin('go');
const rootB = twin('rust');

// git 仓库（两边同样步骤初始化）。commit 的时间戳不同 → SHA 不同，比较时
// 40 位十六进制一律归一；工作树内容完全一致。
const seedGit = (root) => {
  const run = (cmd) => execSync(cmd, { cwd: root, stdio: 'pipe' });
  run('git init -b main');
  run('git config user.email parity@example.com');
  run('git config user.name parity');
  writeFileSync(path.join(root, 'a.txt'), 'base\n');
  run('git add -- a.txt');
  run('git commit -m baseline');
};
seedGit(rootA);
seedGit(rootB);

// browser-agent bundle 拷进孪生根，两边读到的注入脚本逐字节一致。
mkdirSync(path.join(rootA, 'codeg-ui', 'browser-agent'), { recursive: true });
mkdirSync(path.join(rootB, 'codeg-ui', 'browser-agent'), { recursive: true });
copyFileSync(agentBundle, path.join(rootA, 'codeg-ui', 'browser-agent', 'agent.bundle.js'));
copyFileSync(agentBundle, path.join(rootB, 'codeg-ui', 'browser-agent', 'agent.bundle.js'));

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

// 两个 sidecar + 一个 Rust 前门：触发命令（work_task_engine_*）必须落在
// rootB 的 sidecar 上，所以 Rust 前门不能像 ghi 那样借用 go-twin。
spawnServer(GO_EXE, rootA, {
  HARNESS_HUB_CODEG: '1',
  PORT: String(GO_PORT),
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
  HARNESS_HUB_WARM: '0',
}, 'go-twin');
spawnServer(GO_EXE, rootB, {
  HARNESS_HUB_CODEG: '1',
  PORT: String(GO_PORT_B),
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
  HARNESS_HUB_WARM: '0',
}, 'rust-twin-sidecar');
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

// ── CDP 浏览器清理（只杀「调试端口 933x + user-data-dir 在系统临时目录」的）──

const killHarnessBrowsers = () => {
  try {
    const raw = execSync(
      `powershell -NoProfile -Command "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*--remote-debugging-port=933*' } | Select-Object ProcessId,CommandLine | ConvertTo-Json"`,
      { stdio: 'pipe', timeout: 20_000 },
    ).toString().trim();
    if (!raw) return;
    const list = raw.startsWith('[') ? JSON.parse(raw) : [JSON.parse(raw)];
    for (const proc of list) {
      const line = String(proc.CommandLine ?? '');
      if (!line.includes('--user-data-dir=') || !line.includes(os.tmpdir())) continue;
      try {
        execSync(`taskkill /PID ${proc.ProcessId} /T /F`, { stdio: 'pipe' });
      } catch {}
    }
  } catch {}
  // 端口让给下一轮前留一点收尾时间。
  return new Promise((resolve) => setTimeout(resolve, 800));
};

// ── WS frame recorder (Rust front door) ─────────────────────────────────────

const wsFrames = [];
let wsSocket = null;
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
const UID = /\b[0-9a-fA-F]{32}\b/g;
const SHA = /\b[0-9a-f]{40}\b/g;

const maskText = (text) =>
  text
    .replaceAll(rootA.replaceAll('\\', '\\\\'), '<ROOT>')
    .replaceAll(rootB.replaceAll('\\', '\\\\'), '<ROOT>')
    .replaceAll(rootA.replaceAll('\\', '/'), '<ROOT>')
    .replaceAll(rootB.replaceAll('\\', '/'), '<ROOT>')
    .replaceAll(rootA, '<ROOT>')
    .replaceAll(rootB, '<ROOT>')
    .replace(TIME, '<TS>')
    .replace(SHA, '<SHA>')
    .replace(UID, '<UID>');

const maskValue = (value) => {
  if (typeof value === 'string') return maskText(value);
  if (Array.isArray(value)) return value.map(maskValue);
  if (value && typeof value === 'object') {
    // 快照代次是前端用来配对 act 的令牌，两边独立生成。
    if (value.generation !== undefined) value = { ...value, generation: '<GEN>' };
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
    // 挂死的服务端要变成步骤失败而不是 300s 的 undici 崩溃。
    signal: AbortSignal.timeout(90_000),
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

const check = (() => {
  let failures = 0;
  const check = (ok, message) => {
    if (!ok) {
      failures += 1;
      console.error(`❌ ${message}`);
    }
  };
  check.count = () => failures;
  return check;
})();

const compareResponse = (name, go, rust, diffs) => {
  if (go.status !== rust.status) {
    diffs.push(`状态码 ${go.status} != ${rust.status}\n   Go ${go.text.slice(0, 200)}\n   Rust ${rust.text.slice(0, 200)}`);
  } else if (!go.parsed || !rust.parsed) {
    if (maskText(go.text) !== maskText(rust.text)) {
      diffs.push(`非 JSON 响应体不一致：${go.text.slice(0, 120)} vs ${rust.text.slice(0, 120)}`);
    }
  } else {
    firstDiff(maskValue(go.json), maskValue(rust.json), '$', diffs);
  }
};

const compareFiles = (name, files) => {
  for (const file of files ?? []) {
    const a = maskText(readStore(rootA, file) ?? '');
    const b = maskText(readStore(rootB, file) ?? '');
    if (a !== b) {
      const at = [...a].findIndex((ch, index) => ch !== b[index]);
      check(false, `${name}: ${file} 落盘不一致 @${at}\n   Go   …${a.slice(Math.max(0, at - 40), at + 40)}…\n   Rust …${b.slice(Math.max(0, at - 40), at + 40)}…`);
    }
  }
};

// ── steps ───────────────────────────────────────────────────────────────────

const step = (name, command, body, options = {}) => ({ name, command, body, ...options });

// F 桌面控制：全部确定性响应（canon 排序的 map）。
const computerSteps = [
  step('computer.available', 'computer_available', {}),
  step('computer.settings.default', 'get_computer_tools_settings', {}),
  step('computer.set_enabled', 'set_computer_tools_enabled', { enabled: true }),
  step('computer.set_prefs', 'set_computer_tools_preferences', { prefs: { grantTtlMinutes: 30, stopShortcut: 'ctrl+alt+q', showIndicator: false } }),
  step('computer.settings.after', 'get_computer_tools_settings', {}),
  step('computer.set_prefs.bad', 'set_computer_tools_preferences', { prefs: 'x' }),
  step('computer.status', 'computer_status', {}),
  step('computer.share.new', 'computer_share_window', { targetId: '101', level: 'window' }),
  step('computer.share.update', 'computer_share_window', { targetId: '101', level: 'screen' }),
  step('computer.share.multi', 'computer_share_windows', { targetIds: ['202', '203'], level: 'window' }),
  step('computer.shared_state', 'computer_shared_state', {}),
  step('computer.indicator', 'computer_indicator_fit', { width: 800.9, height: '600' }),
  step('computer.stub.permission', 'computer_request_permission', {}),
  step('computer.stub.reveal', 'computer_reveal_helper', {}),
  step('computer.stub.app', 'computer_share_app', {}),
  step('computer.stub.screen', 'computer_share_screen', {}),
  step('computer.stub.driver_info', 'computer_driver_info', {}),
  step('computer.stub.driver_uninstall', 'computer_driver_uninstall', {}),
  step('computer.stop', 'computer_stop', {}),
  step('computer.stop_key', 'computer_stop_key_status', {}),
  step('computer.revoke', 'computer_revoke_all', {}),
  step('computer.status.after', 'computer_status', {}),
  step('computer.windows', 'computer_list_shareable_windows', {}, { compare: 'windows' }),
];

// D 工作任务：状态层 + 守卫 + 引擎路径。task1 = 引擎路径（agent 不存在 →
// 快速失败），task2 = 状态翻转与守卫，task3 = forge 来源。
const worktaskSteps = [
  step('wt.create', 'work_task_create', { draft: {
    folder_id: 1,
    title: ' 修复登录 ',
    config: {
      prompt_blocks: [{ type: 'text', text: '做点事' }],
      display_text: '',
      agent_type: 'parity-no-such-agent',
      config_values: { depth: 3.9, flag: true, model: 'gpt-x' },
      label_snapshot: { agent: 'Codex' },
    },
  } }, { files: ['codeg-worktasks.json'] }),
  step('wt.create.fallback', 'work_task_create', { draft: {
    folder_id: 1,
    config: { prompt_blocks: [{ type: 'text', text: '  第一行  ' }, { type: 'text', text: '第二行' }] },
  } }, { files: ['codeg-worktasks.json'] }),
  step('wt.create.no_folder', 'work_task_create', { draft: { folder_id: 999, title: 'x' } }),
  step('wt.create.empty', 'work_task_create', { draft: { folder_id: 1 } }),
  step('wt.list', 'work_task_list', { folderId: 1 }),
  step('wt.get', 'work_task_get', { id: 1 }),
  step('wt.update', 'work_task_update', { id: 1, draft: { title: '改题', config: { display_text: 'v2' } } }, { files: ['codeg-worktasks.json'] }),
  step('wt.update.keep_config', 'work_task_update', { id: 1, draft: { title: '改题2' } }, { files: ['codeg-worktasks.json'] }),
  step('wt.update.null_config', 'work_task_update', { id: 1, draft: { config: null } }, { files: ['codeg-worktasks.json'] }),
  step('wt.reorder', 'work_task_reorder', { folderId: 1, orderedIds: [2, 1] }, { files: ['codeg-worktasks.json'] }),
  step('wt.list.sorted', 'work_task_list', { folderId: 1 }),
  step('wt.events', 'work_task_events', { taskId: 1, limit: 5 }),
  step('wt.events.missing', 'work_task_events', { taskId: 999 }),
  step('wt.settings.set', 'work_task_settings_set', { folderId: 1, settings: { default_agent_type: 'codex', max_concurrent: 3.9, merge_strategy: 'squash', auto_merge: true, config_values: { k: 12 } } }, { files: ['codeg-worktasks.json'] }),
  step('wt.settings.own', 'work_task_settings_get_own', { folderId: 1 }),
  step('wt.settings.effective', 'work_task_settings_effective', { folderId: 1 }),
  step('wt.settings.bad_strategy', 'work_task_settings_set', { folderId: 1, settings: { merge_strategy: 'rebase' } }, { files: ['codeg-worktasks.json'] }),
  step('wt.settings.effective.reset', 'work_task_settings_effective', { folderId: 1 }),
  step('wt.settings.delete', 'work_task_settings_delete', { folderId: 1 }, { files: ['codeg-worktasks.json'] }),
  step('wt.settings.own.gone', 'work_task_settings_get_own', { folderId: 1 }),
  step('wt.template.save', 'work_task_template_save', { draft: { name: ' 日常 ', title: ' T ', config: { display_text: 'd', config_values: { a: 1 } } } }, { files: ['codeg-worktasks.json'] }),
  step('wt.template.update', 'work_task_template_save', { draft: { name: '日常', title: 'T2' } }, { files: ['codeg-worktasks.json'] }),
  step('wt.template.list', 'work_task_template_list', {}),
  step('wt.template.save.empty_name', 'work_task_template_save', { draft: { name: '  ' } }),
  step('wt.template.delete', 'work_task_template_delete', { id: 1 }, { files: ['codeg-worktasks.json'] }),
  step('wt.template.list.after', 'work_task_template_list', {}),
  step('wt.forge.create', 'work_task_create_from_forge', { draft: {
    folder_id: 1,
    source: { kind: 'issue', provider: 'github', server_host: 'github.com', owner_repo: 'acme/app', number: 42 },
    snapshot: { title: '按钮坏了', body: '点哪儿都没反应', url: 'https://github.com/acme/app/issues/42' },
  } }, { files: ['codeg-worktasks.json'] }),
  step('wt.forge.no_source', 'work_task_create_from_forge', { draft: { folder_id: 1, snapshot: {} } }),
  step('wt.lookup', 'work_task_lookup_by_source', { sourceKeys: ['github:github.com:acme/app:issue:42', 'github:github.com:acme/app:issue:43'] }),
  step('wt.schedule', 'work_task_schedule', { id: 2, scheduledAt: '2027-01-01T09:00:00Z' }, { files: ['codeg-worktasks.json'] }),
  step('wt.schedule.clear', 'work_task_schedule', { id: 2, scheduledAt: '' }, { files: ['codeg-worktasks.json'] }),
  step('wt.defer', 'work_task_defer', { id: 2, deferred: true }, { files: ['codeg-worktasks.json'] }),
  step('wt.defer.off', 'work_task_defer', { id: 2, deferred: false }, { files: ['codeg-worktasks.json'] }),
  step('wt.archive', 'work_task_archive', { id: 2, archived: true }, { files: ['codeg-worktasks.json'] }),
  step('wt.archive.off', 'work_task_archive', { id: 2, archived: false }, { files: ['codeg-worktasks.json'] }),
  step('wt.merge_unqueue', 'work_task_merge_unqueue', { id: 2 }, { files: ['codeg-worktasks.json'] }),
  step('wt.get.missing', 'work_task_get', { id: 999 }),
  step('wt.complete.todo', 'work_task_complete', { id: 2 }),
  step('wt.merge.todo', 'work_task_merge', { id: 2, message: 'm' }),
  step('wt.cancel', 'work_task_cancel', { id: 2, reason: '改需求' }, { files: ['codeg-worktasks.json'] }),
  step('wt.cancel.again', 'work_task_cancel', { id: 2 }),
  step('wt.update.canceled', 'work_task_update', { id: 2, draft: { title: 'x' } }),
  step('wt.requeue', 'work_task_requeue', { id: 2, note: '再来一次' }, { files: ['codeg-worktasks.json'] }),
  step('wt.retry.not_failed', 'work_task_retry', { id: 2 }),
  step('wt.return.not_review', 'work_task_return', { id: 2, feedback: '补个测试' }),
  step('wt.cleanup.none', 'work_task_cleanup', { id: 2 }),
  step('wt.diff.none', 'work_task_diff', { id: 2 }),
  step('wt.changed.none', 'work_task_changed_files', { id: 2 }),
  step('wt.deliver', 'work_task_deliver_pr', { id: 2, prTitle: 't' }),
  step('wt.unknown', 'work_task_nope', {}),
  // 引擎路径：task1 config.agent_type = parity-no-such-agent → 连接快速失败。
  // start 不比落盘（引擎异步写，两边节奏不同），settle 后两边都到终态再比。
  step('wt.start', 'work_task_start', { id: 1 }),
  step('wt.settle', 'work_task_get', { id: 1 }, { settleTask: 1, files: ['codeg-worktasks.json'] }),
  // 引擎已建好隔离工作树（两边同样步骤），脏化后比对 diff/changed_files。
  step('wt.diff', 'work_task_diff', { id: 1 }, { prepare: dirtyWorktree, files: ['codeg-worktasks.json'] }),
  step('wt.diff.file', 'work_task_diff', { id: 1, file: 'b.txt' }),
  step('wt.changed', 'work_task_changed_files', { id: 1 }),
  step('wt.complete.dirty', 'work_task_complete', { id: 1 }),
  step('wt.complete.ok', 'work_task_complete', { id: 1 }, { prepare: cleanWorktree, files: ['codeg-worktasks.json'] }),
  step('wt.complete.again', 'work_task_complete', { id: 1 }),
  step('wt.cleanup', 'work_task_cleanup', { id: 1 }, { files: ['codeg-worktasks.json'] }),
  step('wt.cleanup.again', 'work_task_cleanup', { id: 1 }),
  step('wt.diff.gone', 'work_task_diff', { id: 1 }),
  step('wt.delete', 'work_task_delete', { id: 2, deleteWorktree: false }, { files: ['codeg-worktasks.json'] }),
  step('wt.delete.missing', 'work_task_delete', { id: 999 }),
  step('wt.list.final', 'work_task_list', { folderId: 1 }, { files: ['codeg-worktasks.json'] }),
];

const DATA_URL =
  'data:text/html,<title>parity</title><button id="go">go</button><input id="box" value="anything">';

// E 浏览器：两轮顺序跑（先 Go 后 Rust），轮间清理 CDP 浏览器。
const browserSteps = [
  step('browser.capabilities', 'browser_capabilities', {}),
  step('browser.find', 'browser_find', {}),
  step('browser.stop', 'browser_stop', {}),
  step('browser.stub.pick', 'browser_pick_element', {}),
  step('browser.stub.bounds', 'browser_set_bounds', {}),
  step('browser.open', 'browser_open_tab', { url: DATA_URL }),
  step('browser.tabs', 'browser_tabs', {}),
  step('browser.eval.title', 'browser_eval_decide', (seen) => ({ tabId: seen.tabId, expression: 'document.title' })),
  step('browser.navigate', 'browser_navigate', (seen) => ({ tabId: seen.tabId, url: DATA_URL })),
  step('browser.eval.body', 'browser_eval_decide', (seen) => ({ tabId: seen.tabId, expression: 'document.getElementById("box").textContent' })),
  step('browser.reload', 'browser_reload', (seen) => ({ tabId: seen.tabId })),
  step('browser.go_back', 'browser_go_back', (seen) => ({ tabId: seen.tabId })),
  step('browser.snapshot', 'browser_agent_snapshot', (seen) => ({ tabId: seen.tabId, maxChars: 0 })),
  step('browser.act', 'browser_agent_act', (seen) => ({ tabId: seen.tabId, request: { kind: 'press', key: 'Enter' } })),
  step('browser.act.bad', 'browser_agent_act', (seen) => ({ tabId: seen.tabId, request: { kind: 'teleport' } })),
  step('browser.console', 'browser_agent_console', (seen) => ({ tabId: seen.tabId })),
  step('browser.capture', 'browser_page_capture', (seen) => ({ tabId: seen.tabId }), { compare: 'png' }),
  step('browser.close', 'browser_close_tab', (seen) => ({ tabId: seen.tabId })),
  step('browser.tabs.after', 'browser_tabs', {}),
];

function dirtyWorktree(root) {
  const worktree = path.join(root, '.worktrees', 'task-1');
  if (!existsSync(worktree)) return;
  writeFileSync(path.join(worktree, 'a.txt'), 'base\nchanged line\n');
  writeFileSync(path.join(worktree, 'b.txt'), 'brand new\n');
}

function cleanWorktree(root) {
  const worktree = path.join(root, '.worktrees', 'task-1');
  if (!existsSync(worktree)) return;
  writeFileSync(path.join(worktree, 'a.txt'), 'base\n');
  try { rmSync(path.join(worktree, 'b.txt')); } catch {}
}

// ── runner ──────────────────────────────────────────────────────────────────

const runParallelSteps = async (steps, seen) => {
  for (const { name, command, body, compare, settle, files, prepare, settleTask } of steps) {
    if (prepare) {
      prepare(rootA);
      prepare(rootB);
    }
    const resolved = typeof body === 'function' ? body(seen) : body;
    let [go, rust] = await Promise.all([
      post(GO_PORT, command, resolved),
      post(RUST_PORT, command, resolved),
    ]);
    if (settleTask) {
      // 引擎路径：等两边任务都落到终态（同 Go 引擎，最终一致），再取一对比较。
      const deadline = Date.now() + 45_000;
      const status = async (port) => {
        const { json } = await post(port, 'work_task_get', { id: settleTask });
        return json?.status;
      };
      for (;;) {
        const [gs, rs] = await Promise.all([status(GO_PORT), status(RUST_PORT)]);
        if ((gs === 'failed' || gs === 'done' || gs === 'canceled') && (rs === 'failed' || rs === 'done' || rs === 'canceled')) break;
        if (Date.now() > deadline) { console.error(`   （settle 超时：Go=${gs} Rust=${rs}）`); break; }
        await sleep(500);
      }
      [go, rust] = await Promise.all([
        post(GO_PORT, command, resolved),
        post(RUST_PORT, command, resolved),
      ]);
    }
    if (settle) await sleep(settle);

    const diffs = [];
    if (compare === 'windows') {
      compareWindows(name, go, rust, diffs);
    } else {
      compareResponse(name, go, rust, diffs);
    }
    report(name, diffs, go);
    compareFiles(name, files);
  }
};

const report = (name, diffs, go) => {
  if (diffs.length) {
    check(false, `${name}: 不一致\n   ${diffs.slice(0, 4).join('\n   ')}`);
  } else {
    console.log(`· ${name}: MATCH${go && go.status >= 400 ? `（${go.status}，错误文案一致）` : ''}`);
  }
};

// 窗口枚举：两次调用之间桌面可能变化 → 只比形状与字段类型。
const compareWindows = (name, go, rust, diffs) => {
  if (go.status !== rust.status) {
    diffs.push(`状态码 ${go.status} != ${rust.status}`);
    return;
  }
  const shape = (list) =>
    Array.isArray(list) && list.length > 0 && list.every((w) =>
      typeof w.target_id === 'string' && typeof w.title === 'string' &&
      typeof w.class === 'string' && Number.isInteger(w.process_id) &&
      w.bounds && Number.isInteger(w.bounds.Left) && Number.isInteger(w.bounds.Bottom));
  if (!shape(go.json?.windows) || !shape(rust.json?.windows)) {
    diffs.push(`窗口枚举形状不符：Go ${JSON.stringify(go.json?.windows?.[0] ?? null).slice(0, 160)} / Rust ${JSON.stringify(rust.json?.windows?.[0] ?? null).slice(0, 160)}`);
  }
};

// PNG 截图：像素是实时的 → 解码后只认魔数与非空。
const comparePng = (name, go, rust, diffs) => {
  if (go.status !== rust.status) {
    diffs.push(`状态码 ${go.status} != ${rust.status}`);
    return;
  }
  const magic = (response) => {
    const data = response.json?.base64;
    if (typeof data !== 'string' || data.length < 16) return '太短';
    const bytes = Buffer.from(data, 'base64');
    return bytes.slice(0, 8).toString('hex') === '89504e470d0a1a0a' ? 'png' : `非 PNG：${bytes.slice(0, 8).toString('hex')}`;
  };
  if (go.json?.mime_type !== 'image/png' || rust.json?.mime_type !== 'image/png') {
    diffs.push(`mime_type: Go=${go.json?.mime_type} Rust=${rust.json?.mime_type}`);
  }
  const a = magic(go);
  const b = magic(rust);
  if (a !== 'png' || b !== 'png') diffs.push(`截图内容：Go=${a} Rust=${b}`);
};

// 浏览器步骤：先 Go 一轮、后 Rust 一轮，轮间清浏览器；记录后逐对比较。
const runBrowserSteps = async () => {
  const pass = async (port) => {
    const seen = {};
    const recorded = [];
    for (const { name, command, body, compare } of browserSteps) {
      const resolved = typeof body === 'function' ? body(seen) : body;
      const response = await post(port, command, resolved);
      if (name === 'browser.tabs') {
        const rows = response.json?.tabs ?? [];
        const hit = rows.find((row) => String(row.url ?? '').startsWith('data:text/html'));
        // 用 targetId 而不是列表下标：目标列表在两次调用之间会重排，下标
        // 引用会关错页（Go/Rust 的 tabByRef 都先按 targetId 解析）。
        seen.tabId = hit ? String(hit.target_id) : '0';
      }
      recorded.push({ name, response, compare });
    }
    return recorded;
  };

  await killHarnessBrowsers();
  const goPass = await pass(GO_PORT);
  await killHarnessBrowsers();
  const rustPass = await pass(RUST_PORT);
  await killHarnessBrowsers();

  for (let index = 0; index < browserSteps.length; index += 1) {
    const { name, compare } = browserSteps[index];
    const go = goPass[index].response;
    const rust = rustPass[index].response;
    const diffs = [];
    if (compare === 'png') {
      comparePng(name, go, rust, diffs);
    } else if (name === 'browser.tabs' || name === 'browser.tabs.after') {
      // CDP 目标列表的顺序在两个浏览器实例间不稳定（初始页与新建页谁先
      // 谁后不定），按 url+id 排序后再比。
      const sortTabs = (response) => {
        if (!response.parsed || !Array.isArray(response.json?.tabs)) return response;
        const tabs = [...response.json.tabs].sort((a, b) =>
          `${a.url}|${a.id}`.localeCompare(`${b.url}|${b.id}`));
        return { ...response, json: { ...response.json, tabs } };
      };
      compareResponse(name, sortTabs(go), sortTabs(rust), diffs);
    } else {
      compareResponse(name, go, rust, diffs);
    }
    report(name, diffs, go);
  }
};

// ── run ─────────────────────────────────────────────────────────────────────

try {
  await waitHealth(GO_PORT);
  await waitHealth(GO_PORT_B);
  await waitHealth(RUST_PORT);
  wsSocket = await connectRecorder();

  console.log('── F 桌面控制（并行）──');
  await runParallelSteps(computerSteps, {});

  console.log('── D 工作任务（并行）──');
  await runParallelSteps(worktaskSteps, {});

  console.log('── E 浏览器（两轮顺序）──');
  await runBrowserSteps();

  // 事件注入：D/E/F 在 Go 里 emit 的 channel，Rust 侧必须同样出现。
  const channels = new Set(wsFrames.map((frame) => frame.channel));
  const hasChannel = (prefix) => [...channels].some((c) => c.startsWith(prefix));
  for (const expected of ['task://changed', 'computer://state', 'computer://agent-grant', 'browser://state']) {
    check(hasChannel(expected), `WS 注入缺少 ${expected}（收到：${[...channels].join(', ') || '无'}）`);
  }
  console.log(`· WS 注入帧：${wsFrames.length} 帧，channel：${[...channels].join(', ')}`);
} finally {
  if (wsSocket) {
    try { wsSocket.close(); } catch {}
  }
  for (const child of children) {
    try { child.kill(); } catch {}
  }
  await killHarnessBrowsers();
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

if (check.count() > 0) {
  console.error(`\n❌ ${check.count()} 处不一致`);
  process.exit(1);
}
console.log('\n✅ 双跑全部一致');
