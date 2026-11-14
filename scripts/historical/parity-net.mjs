#!/usr/bin/env node
// Dual-run for the HTTPS batch (net.rs + forge slice + check_app_update).
//
// Twin workspaces (same seed) again, plus:
//   - a local HTTP manifest server both sides fetch via HARNESS_HUB_UPDATE_MANIFEST;
//   - each twin seeded with a git repo whose origin is a GitHub remote, so
//     folder_forge_remote / validate_github_token (Go's dead path) compare;
//   - one real api.github.com call per side with a bogus token (401 text).
// forge account state is process memory: the token-bearing update is only
// exercised through its failure path, which leaves both stores empty.
//
//   node scripts/parity-net.mjs [--go 28895] [--rust 28896] [--keep]

import { spawn } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { execSync } from 'node:child_process';
import process from 'node:process';

const repoRoot = path.resolve(new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
const args = process.argv.slice(2);
const option = (name, fallback) => {
  const at = args.indexOf(`--${name}`);
  return at >= 0 ? args[at + 1] : fallback;
};
const keep = args.includes('--keep');
const GO_PORT = Number(option('go', '28895'));
const RUST_PORT = Number(option('rust', '28896'));
const GO_EXE = path.join(repoRoot, 'server-go', 'serve-codeg.exe');
const RUST_EXE = path.join(repoRoot, 'src-tauri', 'target', 'debug', 'codepluse.exe');
if (!existsSync(GO_EXE)) die('缺少 server-go/serve-codeg.exe');
if (!existsSync(RUST_EXE)) die('缺少 src-tauri/target/debug/codepluse.exe');

function die(message) {
  console.error(`❌ ${message}`);
  process.exit(2);
}

// ── twin workspaces ─────────────────────────────────────────────────────────

const base = mkdtempSync(path.join(repoRoot, '.superpowers', 'sdd', 'parity-net-'));
const twin = (tag) => {
  const root = path.join(base, tag);
  mkdirSync(path.join(root, '.data'), { recursive: true });
  const project = path.join(root, 'proj');
  mkdirSync(project, { recursive: true });
  const git = (gitArgs) =>
    execSync(`git ${gitArgs.join(' ')}`, { cwd: project, stdio: 'ignore' });
  git(['init', '-q']);
  git(['remote', 'add', 'origin', 'git@github.com:acme/widget.git']);
  writeFileSync(path.join(project, 'README.md'), '# widget\n');
  return { root, project };
};
const goTwin = twin('go');
const rustTwin = twin('rust');

const seedStore = (project) =>
  JSON.stringify({
    next: 2,
    folders: [
      { id: 1, name: 'proj', path: project, kind: 'regular', open: true, sort_order: 0, color: 'default', group_id: null, alias: null },
    ],
    conversations: [],
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
  });
writeFileSync(path.join(goTwin.root, '.data', 'codeg-workspace.json'), seedStore(goTwin.project));
writeFileSync(path.join(rustTwin.root, '.data', 'codeg-workspace.json'), seedStore(rustTwin.project));

// ── local manifest server (plain HTTP, both sides fetch it) ────────────────

const manifestBody = JSON.stringify({
  version: '9.9.9-go',
  body: 'parity manifest',
  date: '2026-11-14',
  url: 'https://example.invalid/download',
});
const manifestServer = http.createServer((request, response) => {
  response.writeHead(200, { 'Content-Type': 'application/json' });
  response.end(manifestBody);
});
await new Promise((resolve) => manifestServer.listen(0, '127.0.0.1', resolve));
const manifestUrl = `http://127.0.0.1:${manifestServer.address().port}/manifest.json`;

// ── mock GitHub API（双方同源：Go 用 -ldflags -X 改 githubAPI，Rust 用 env）──

const githubIssue = {
  number: 7, title: 'Fix', body: 'b', state: 'open',
  labels: [{ name: 'bug', color: 'ff0000' }],
  user: { login: 'ann', avatar_url: 'a.png' },
  updated_at: '2026-11-14T00:00:00Z', html_url: 'h', comments: 2,
};
const githubPr = {
  number: 9, title: 'PR', body: 'p', state: 'closed', merged_at: '2026-11-14T01:00:00Z',
  pull_request: { draft: true }, user: { login: 'bo', avatar_url: 'b.png' },
  head: { ref: 'h1', sha: 's1', repo: { full_name: 'acme/fork' } }, base: { ref: 'main' },
  mergeable: true, mergeable_state: 'clean', updated_at: 'u', html_url: 'h', comments: 0,
};
const githubComment = { id: 5, user: { login: 'ann', avatar_url: 'a.png' }, body: 'hi', created_at: 't', html_url: 'h' };
const postedComment = { id: 9, user: { login: 'ann', avatar_url: 'a.png' }, body: 'c', created_at: 't', html_url: 'h' };

const json = (payload) => JSON.stringify(payload);
const GITHUB_ROUTES = [
  // 最长前缀优先；/user 按 Authorization 头区分 401/200。
  ['GET', '/repos/acme/widget/issues/7/comments', 200, () => json([githubComment])],
  ['POST', '/repos/acme/widget/issues/7/comments', 200, () => json(postedComment)],
  ['PATCH', '/repos/acme/widget/pulls/9', 200, () => json(githubPr)],
  ['PUT', '/repos/acme/widget/pulls/7/merge', 200, () => json({ merged: true })],
  ['POST', '/repos/acme/widget/issues', 200, () => json(githubIssue)],
  ['GET', '/repos/acme/widget/pulls/9/files', 200, () => json([{ filename: 'a.go', additions: 1, deletions: 2, changes: 3, status: 'modified' }])],
  ['GET', '/repos/acme/widget/commits/s1/check-runs', 200, () => json({ check_runs: [
    { name: 'ci', conclusion: 'success', html_url: 'c' },
    { name: 'build', conclusion: '', status: 'in_progress', html_url: 'b' },
  ] })],
  ['GET', '/repos/acme/widget/pulls/9', 200, () => json(githubPr)],
  ['GET', '/repos/acme/widget/pulls/7', 200, () => json(githubPr)],
  ['GET', '/repos/acme/widget/labels', 200, () => json([{ name: 'bug', color: 'ff0000' }, { name: 'no-color', color: '' }])],
  ['GET', '/repos/acme/widget', 200, () => json({ allow_merge_commit: false, allow_squash_merge: true, allow_rebase_merge: true })],
  ['GET', '/search/issues', 200, () => json({ total_count: 1200, items: [githubIssue, githubPr] })],
  ['GET', '/user', 200, () => json({ login: 'octocat', name: 'Octo', avatar_url: 'a.png' })],
];

const githubServer = http.createServer((request, response) => {
  const url = request.url.split('?')[0];
  const authorization = request.headers.authorization ?? '';
  let hit = null;
  if (url === '/user' && authorization.includes('invalid')) {
    response.writeHead(401, { 'Content-Type': 'application/json' });
    response.end(json({ message: 'Bad credentials', status: '401' }));
    return;
  }
  for (const [method, prefix, status, payload] of GITHUB_ROUTES) {
    if (method === request.method && url.startsWith(prefix)) {
      hit = [status, payload];
      break;
    }
  }
  if (!hit) {
    response.writeHead(404, { 'Content-Type': 'application/json' });
    response.end('{}');
    return;
  }
  response.writeHead(hit[0], { 'Content-Type': 'application/json' });
  response.end(hit[1]());
});
await new Promise((resolve) => githubServer.listen(0, '127.0.0.1', resolve));
const githubApi = `http://127.0.0.1:${githubServer.address().port}`;

// ── mock model gateway（providermodels 双跑）────────────────────────────────

const modelGateway = http.createServer((request, response) => {
  if (request.url.startsWith('/v1/models')) {
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(json({
      data: [
        { id: 'glm-4.7', display_name: 'GLM' },
        { id: 'glm-air', display_name: 'Air', context_window: 128000 },
      ],
    }));
    return;
  }
  if (request.url.startsWith('/models')) {
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(json({ data: [{ id: 'o-model', display_name: 'O' }] }));
    return;
  }
  response.writeHead(404, { 'Content-Type': 'application/json' });
  response.end('{}');
});
await new Promise((resolve) => modelGateway.listen(0, '127.0.0.1', resolve));
const gatewayUrl = `http://127.0.0.1:${modelGateway.address().port}`;

// Go 侧把 githubAPI 变量重定向到 mock（forge_github.go 专为测试留的 var）。
const GO_MOCK_EXE = path.join(base, 'serve-codeg-mock.exe');
execSync(
  `go build -ldflags "-X harnesshub/internal/codeg.githubAPI=${githubApi}" -o "${GO_MOCK_EXE}" ./cmd/serve`,
  { cwd: path.join(repoRoot, 'server-go'), stdio: 'pipe' },
);

// executor 供应商种子：url 指向本地模型网关（127.0.0.1，loopback 目标两侧都不走代理）。
for (const twin of [goTwin, rustTwin]) {
  writeFileSync(
    path.join(twin.root, '.data', 'api-providers.json'),
    JSON.stringify({
      providers: [{
        id: 'abc123', name: '网关', agent: 'claude', url: gatewayUrl,
        model: 'glm-4.7', key: 'sk-gw-test', hasKey: true,
        models: [{ id: 'glm-4.7', name: 'GLM' }, { id: 'glm-4.6', name: '', enabled: false }],
        protocol: 'anthropic', api: false,
      }],
      active: { claude: 'abc123' },
    }, null, 2),
  );
}

// ── servers ─────────────────────────────────────────────────────────────────

const children = [];
const spawnServer = (exe, cwd, env, label) => {
  const child = spawn(exe, [], { cwd, env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stderr.on('data', (chunk) => process.env.PARITY_VERBOSE && process.stderr.write(`[${label}] ${chunk}`));
  children.push(child);
  return child;
};
spawnServer(GO_MOCK_EXE, goTwin.root, {
  HARNESS_HUB_CODEG: '1',
  PORT: String(GO_PORT),
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
  HARNESS_HUB_UPDATE_MANIFEST: manifestUrl,
}, 'go');
spawnServer(RUST_EXE, rustTwin.root, {
  HARNESS_HUB_ALLOW_ANY_PATH: '1',
  CODEPLUSE_HEADLESS: '1',
  CODEPLUSE_PORT: String(RUST_PORT),
  CODEPLUSE_ROOT: rustTwin.root,
  HARNESS_HUB_UPDATE_MANIFEST: manifestUrl,
  CODEPLUSE_GITHUB_API: githubApi,
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

// ── comparison (same rules as parity-store) ─────────────────────────────────

const TIME = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z/g;
const maskText = (text) =>
  text
    .replaceAll(goTwin.root.replaceAll('\\', '\\\\'), '<ROOT>')
    .replaceAll(rustTwin.root.replaceAll('\\', '\\\\'), '<ROOT>')
    .replaceAll(goTwin.root, '<ROOT>')
    .replaceAll(rustTwin.root, '<ROOT>')
    .replace(TIME, '<TS>');

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
    // 无超时的 fetch 会把整条流水线挂死：10s 足够任何本地命令。
    signal: AbortSignal.timeout(10_000),
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

const steps = [
  // 无清单（env 未设）的路径由单测覆盖：两个服务进程都带着 manifest env 起。
  { name: 'update.manifest', command: 'check_app_update' },
  { name: 'accounts.empty', command: 'get_github_accounts' },
  { name: 'accounts.update.bad-token', command: 'update_github_accounts', body: { settings: { accounts: [{ name: 'x', token: 'ghp_parity_invalid' }] } } },
  { name: 'accounts.empty.after', command: 'get_github_accounts' },
  { name: 'identity.no-account', command: 'forge_identity', body: {} },
  { name: 'remote.github', command: 'folder_forge_remote', body: { folderId: 1 } },
  { name: 'validate.dead-path', command: 'validate_github_token', body: { token: 't', folderId: 1 } },
  { name: 'settings.get', command: 'forge_settings_get', body: { folderId: 1 } },
  { name: 'settings.set.global', command: 'forge_settings_set', body: { folderId: 0, settings: { pinned: 3, z: { a: 1 } } }, file: 'codeg-forge.json' },
  { name: 'settings.set.folder', command: 'forge_settings_set', body: { folderId: 1, settings: { q: 9 } }, file: 'codeg-forge.json' },
  { name: 'settings.set.delete', command: 'forge_settings_set', body: { folderId: 1, settings: null }, file: 'codeg-forge.json' },
  { name: 'auto.status', command: 'forge_auto_status' },
  { name: 'auto.scan', command: 'forge_auto_scan_now' },
  // ---- GitHub workbench（mock API；先建立账户再走 12 条 REST）----
  { name: 'accounts.update.valid', command: 'update_github_accounts', body: { settings: { accounts: [{ name: 'main', token: 'tok' }] } } },
  { name: 'identity', command: 'forge_identity', body: {} },
  { name: 'forge.list_issues', command: 'forge_list_issues', body: { folderId: 1, query: { tab: 'pulls', state: 'all', page: 2, perPage: 50, assignedMe: true, labels: ['bug', 'ui'], search: 'in:title fix', sort: 'updated' } } },
  { name: 'forge.tab_count', command: 'forge_tab_count', body: { folderId: 1, query: {} } },
  { name: 'forge.issue_detail', command: 'forge_issue_detail', body: { folderId: 1, query: { number: 7 } } },
  { name: 'forge.list_labels', command: 'forge_list_labels', body: { folderId: 1, accountId: '' } },
  { name: 'forge.list_comments', command: 'forge_list_comments', body: { folderId: 1, filters: { number: 7 } } },
  { name: 'forge.create_comment', command: 'forge_create_comment', body: { folderId: 1, draft: { number: 7, body: 'c' } } },
  { name: 'forge.set_item_state', command: 'forge_set_item_state', body: { folderId: 1, request: { kind: 'pr', number: 9, action: 'reopen' } } },
  { name: 'forge.create_issue', command: 'forge_create_issue', body: { folderId: 1, draft: { title: 'T', body: '', labels: [] } } },
  { name: 'forge.change_detail', command: 'forge_change_detail', body: { folderId: 1, query: { number: 9 } } },
  { name: 'forge.change_files', command: 'forge_change_files', body: { folderId: 1, query: { number: 9 } } },
  { name: 'forge.merge_options', command: 'forge_merge_options', body: { folderId: 1, accountId: '' } },
  { name: 'forge.merge_change', command: 'forge_merge_change', body: { folderId: 1, request: { number: 7, method: 'squash' } } },
  // ---- provider/模型（B 组收尾）----
  { name: 'providers.list', command: 'codepluse_providers_list', body: {} },
  { name: 'providers.models', command: 'codepluse_provider_models', body: { providerId: 'abc123' } },
  { name: 'models.list', command: 'list_provider_models', body: { params: { protocol: 'anthropic', baseUrl: gatewayUrl, keySource: { kind: 'executor', providerId: 'abc123' } } } },
  { name: 'models.host-mismatch', command: 'list_provider_models', body: { params: { protocol: 'openai', baseUrl: 'https://evil.example', keySource: { kind: 'executor', providerId: 'abc123' } } } },
  { name: 'providers.activate', command: 'codepluse_provider_activate', body: { agent: 'claude', id: 'abc123' }, file: 'api-providers.json' },
  { name: 'providers.activate.bad', command: 'codepluse_provider_activate', body: { agent: 'claude', id: 'nope' } },
  { name: 'ccswitch.config.bad', command: 'ccswitch_provider_config', body: { id: 'store:1' } },
  { name: 'acp.registry.catalog', command: 'acp_fetch_registry_catalog', body: {} },
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
      if (maskText(go.text) !== maskText(rust.text)) {
        diffs.push(`非 JSON 响应体不一致：${go.text.slice(0, 160)} vs ${rust.text.slice(0, 160)}`);
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
      const a = maskText(readStore(goTwin.root, file) ?? '');
      const b = maskText(readStore(rustTwin.root, file) ?? '');
      if (a !== b) {
        const at = [...a].findIndex((ch, index) => ch !== b[index]);
        check(false, `${name}: ${file} 落盘不一致 @${at}\n   Go   …${a.slice(Math.max(0, at - 40), at + 40)}…\n   Rust …${b.slice(Math.max(0, at - 40), at + 40)}…`);
      }
    }
  }
} finally {
  for (const child of children) {
    try {
      child.kill();
    } catch {}
  }
  manifestServer.close();
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
// mock 服务器的 keep-alive 连接会让事件循环挂着不退——显式收尾。
process.exit(0);
