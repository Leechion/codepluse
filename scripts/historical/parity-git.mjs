#!/usr/bin/env node
// Parity suite for the git surface.
//
// Every git command mutates or reads repository state, so the same call cannot
// simply be replayed against one working tree: the first call would change what
// the second one sees. Instead the suite builds one base repository and copies
// it twice — `go` and `rust` — then applies the identical sequence of calls to
// each side and compares the JSON after normalising the absolute repo path.
//
//   node scripts/parity-git.mjs [--go 8840] [--rust 8841] [--keep]
//
// Exit code 0 = every step matched, 1 = a difference, 2 = setup error.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const argv = process.argv.slice(2);
const opt = { go: "8840", rust: "8841", keep: false, root: "" };
for (let i = 0; i < argv.length; i += 1) {
  if (argv[i] === "--go") opt.go = argv[++i];
  else if (argv[i] === "--rust") opt.rust = argv[++i];
  else if (argv[i] === "--keep") opt.keep = true;
  else if (argv[i] === "--root") opt.root = argv[++i];
  else {
    console.error(`parity-git: 未知参数 ${argv[i]}`);
    process.exit(2);
  }
}

function git(cwd, args) {
  return execFileSync("git", ["--no-optional-locks", "-C", cwd, ...args], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, GIT_TERMINAL_PROMPT: "0", GIT_AUTHOR_DATE: "2024-01-01T00:00:00Z", GIT_COMMITTER_DATE: "2024-01-01T00:00:00Z" },
  });
}

function setupBase(dir) {
  fs.mkdirSync(dir, { recursive: true });
  git(dir, ["init", "-b", "master"]);
  git(dir, ["config", "user.email", "parity@example.com"]);
  git(dir, ["config", "user.name", "Parity"]);
  fs.writeFileSync(path.join(dir, "a.txt"), "hello\n");
  fs.writeFileSync(path.join(dir, "a2.txt"), "second\n");
  git(dir, ["add", "--", "a.txt", "a2.txt"]);
  git(dir, ["commit", "-m", "first"]);
  // A feature branch that also touches a3.txt so a merge can conflict later.
  fs.writeFileSync(path.join(dir, "a3.txt"), "base\n");
  git(dir, ["add", "--", "a3.txt"]);
  git(dir, ["commit", "-m", "add a3"]);
  git(dir, ["checkout", "-b", "feature"]);
  fs.writeFileSync(path.join(dir, "a3.txt"), "feature\n");
  git(dir, ["commit", "-am", "feature change"]);
  git(dir, ["checkout", "master"]);
  fs.writeFileSync(path.join(dir, "a3.txt"), "master\n");
  git(dir, ["commit", "-am", "master change"]);
}

function copyTree(from, to) {
  fs.cpSync(from, to, { recursive: true });
}

// Repositories live under --root when given: the default desktop build confines
// workdirs to the workspace root, so a temp-dir repo would be rejected by both
// sides and the suite would compare two identically-shaped errors.
const parent = opt.root ? path.resolve(opt.root) : os.tmpdir();
fs.mkdirSync(parent, { recursive: true });
const root = fs.mkdtempSync(path.join(parent, "codepluse-parity-git-"));
const base = path.join(root, "base");
const dirs = { go: path.join(root, "repo-go"), rust: path.join(root, "repo-rust") };
try {
  setupBase(base);
  for (const [side, dir] of Object.entries(dirs)) {
    if (fs.existsSync(dir)) fs.rmSync(dir, { recursive: true, force: true });
    copyTree(base, dir);
    void side;
  }
} catch (error) {
  console.error(`parity-git: 仓库准备失败：${error.message}`);
  process.exit(2);
}

function normalize(value, dir) {
  let text = JSON.stringify(value);
  for (const variant of [dir, dir.replace(/\\/g, "/"), dir.replace(/\\/g, "\\\\")]) {
    text = text.split(variant).join("<REPO>");
  }
  // The two sides run a second apart, so stash/commit timestamps and the
  // resulting object hashes can never be byte-equal. Both are wall-clock
  // artefacts, not behaviour differences.
  text = text.replace(/\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [+-]\d{4}/g, "<DATE>");
  text = text.replace(/\b[0-9a-f]{40}\b/g, "<HASH>");
  text = text.replace(/\b[0-9a-f]{7,12}\b/g, "<HASH>");
  return JSON.parse(text);
}

async function call(port, command, body) {
  const response = await fetch(`http://127.0.0.1:${port}/api/${command}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const text = await response.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    json = text;
  }
  return { status: response.status, json };
}

function firstDifference(a, b, where = "$") {
  if (typeof a !== typeof b) return `${where}: 类型不同 (${typeof a} vs ${typeof b})`;
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return `${where}: 长度不同 (${a.length} vs ${b.length})`;
    for (let i = 0; i < a.length; i += 1) {
      const found = firstDifference(a[i], b[i], `${where}[${i}]`);
      if (found) return found;
    }
    return null;
  }
  if (a && b && typeof a === "object") {
    const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
    for (const key of [...keys].sort()) {
      if (!(key in a)) return `${where}.${key}: 仅 Rust 有`;
      if (!(key in b)) return `${where}.${key}: 仅 Go 有`;
      const found = firstDifference(a[key], b[key], `${where}.${key}`);
      if (found) return found;
    }
    return null;
  }
  if (a !== b) return `${where}: ${JSON.stringify(a)} ≠ ${JSON.stringify(b)}`;
  return null;
}

let failed = 0;
let checked = 0;

/** Runs one command on both sides; `mutate` writes the same bytes into both
 *  working trees first so state-dependent steps stay aligned. */
async function step(name, command, body = {}, mutate = null) {
  if (mutate) {
    for (const dir of Object.values(dirs)) mutate(dir);
  }
  const goOut = await call(opt.go, command, { path: dirs.go, ...body });
  const rustOut = await call(opt.rust, command, { path: dirs.rust, ...body });
  checked += 1;
  if (goOut.status !== rustOut.status) {
    failed += 1;
    console.log(`❌ ${name} 状态码 (${goOut.status} / ${rustOut.status})`);
    return;
  }
  const diff = firstDifference(normalize(goOut.json, dirs.go), normalize(rustOut.json, dirs.rust));
  if (diff) {
    failed += 1;
    console.log(`❌ ${name}\n   ${diff}`);
  }
}

const steps = [
  ["get_git_branch", "get_git_branch"],
  ["git_status 干净", "git_status"],
  ["git_status 含未跟踪", "git_status", { showAllUntracked: true }],
  ["git_current_user", "git_current_user"],
  ["git_list_branches", "git_list_branches"],
  ["git_list_all_branches", "git_list_all_branches"],
  ["git_log", "git_log", { limit: 10 }],
  ["git_log 带文件", "git_log", { limit: 2, withFiles: true }],
  ["git_log 全分支", "git_log", { limit: 5, allBranches: true }],
  ["git_log 作者过滤", "git_log", { limit: 5, author: "Parity" }],
  ["git_search_authors", "git_search_authors", { query: "parity" }],
  ["git_list_remotes 空", "git_list_remotes"],
  ["git_push_info 无跟踪", "git_push_info"],
  ["git_add_remote", "git_add_remote", { name: "upstream", url: "https://example.com/r.git" }],
  ["git_list_remotes 有值", "git_list_remotes"],
  ["git_set_remote_url", "git_set_remote_url", { name: "upstream", url: "https://example.com/s.git" }],
  ["git_push_info 仍无跟踪", "git_push_info"],
  ["git_remove_remote", "git_remove_remote", { name: "upstream" }],
  ["git_show_file", "git_show_file", { file: "a.txt" }],
  ["git_show_file 缺失", "git_show_file", { file: "nope.txt" }],
  ["git_show_file_base64", "git_show_file_base64", { file: "a.txt" }],
  ["git_show_file_base64 缺失", "git_show_file_base64", { file: "nope.txt" }],
  ["git_show_file_base64 上限", "git_show_file_base64", { file: "a.txt", maxBytes: 3 }],
  ["git_is_tracked 是", "git_is_tracked", { file: "a.txt" }],
  ["git_is_tracked 否", "git_is_tracked", { file: "nope.txt" }],
  ["git_diff 干净", "git_diff"],
  [
    "git_diff 有改动",
    "git_diff",
    {},
    (dir) => fs.writeFileSync(path.join(dir, "a.txt"), "changed\n"),
  ],
  [
    "git_status 有改动",
    "git_status",
    {},
    null,
  ],
  ["git_add_files", "git_add_files", { files: ["a.txt"] }],
  ["git_status 已暂存", "git_status"],
  [
    "git_diff staged 回退",
    "git_diff",
    {},
    null,
  ],
  ["git_rollback_file", "git_rollback_file", { file: "a.txt" }],
  [
    "git_stash_push",
    "git_stash_push",
    { message: "wip" },
    (dir) => fs.writeFileSync(path.join(dir, "a.txt"), "stashed\n"),
  ],
  ["git_stash_list", "git_stash_list"],
  ["git_stash_show", "git_stash_show", { stashRef: "stash@{0}" }],
  ["git_stash_pop", "git_stash_pop", { stashRef: "stash@{0}" }],
  ["git_diff_with_branch", "git_diff_with_branch", { branch: "feature" }],
  ["git_show_diff", "git_show_diff", { commit: "HEAD" }],
  ["git_commit_files", "git_commit_files", { commit: "HEAD" }],
  ["git_commit_branches", "git_commit_branches", { commit: "HEAD" }],
  ["git_new_branch", "git_new_branch", { branchName: "topic" }],
  ["get_git_branch topic", "get_git_branch"],
  ["git_checkout master", "git_checkout", { branchName: "master" }],
  ["git_delete_branch", "git_delete_branch", { branchName: "topic", force: true }],
  ["git_merge 冲突", "git_merge", { branchName: "feature" }],
  ["git_list_conflicts", "git_list_conflicts"],
  ["git_has_merge_head", "git_has_merge_head"],
  ["git_conflict_file_versions", "git_conflict_file_versions", { file: "a3.txt" }],
  ["git_abort_operation", "git_abort_operation", { operation: "merge" }],
  ["git_list_conflicts 已中止", "git_list_conflicts"],
  ["git_reset", "git_reset", { commit: "HEAD~1", mode: "soft" }],
  ["git_status 重置后", "git_status"],
  ["git_reset hard", "git_reset", { commit: "HEAD", mode: "hard" }],
  ["git_reset 非法模式", "git_reset", { commit: "HEAD", mode: "nope" }],
  ["git_abort 非法操作", "git_abort_operation", { operation: "nope" }],
  ["git_checkout 非法引用", "git_checkout", { branchName: "-x" }],
  ["git_log 非法引用", "git_log", { branch: "-x" }],
  ["git_fetch 无远端", "git_fetch"],
  ["open_commit_window", "open_commit_window", { folderId: 7, branch: "master" }],
  ["open_push_window", "open_push_window", { folderId: 7 }],
  ["open_stash_window", "open_stash_window", { folderId: 7, operation: "pop" }],
  ["open_merge_window", "open_merge_window", { folderId: 7, upstreamCommit: "abc" }],
  ["git_worktree_add", "git_worktree_add", { branchName: "feature", worktreePath: "wt" }],
  ["git_init 幂等", "git_init"],
];

for (const [name, command, body, mutate] of steps) {
  await step(name, command, body ?? {}, mutate ?? null);
}

if (!opt.keep) fs.rmSync(root, { recursive: true, force: true });

console.log(`\n检查 ${checked} 项，${failed === 0 ? "全部一致" : `${failed} 项不一致`}`);
process.exit(failed === 0 ? 0 : 1);
