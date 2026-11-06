'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { execFile } = require('node:child_process');

// Git-worktree isolation for unattended missions (Codeg's To-dos model): each
// mission runs in its own worktree/branch; a human reviews the diff and then
// merges or discards. Missions in non-git directories fall back to the shared
// workdir instead of failing.
const MAX_DIFF = 200000;

class Worktrees {
  constructor({ root, dir } = {}) {
    this.root = root;
    this.dir = dir || path.join(root, '.worktrees');
  }
  path(id) { return path.join(this.dir, id); }
  branch(id) { return 'hh/' + id; }
  git(cwd, args, { allowFail = false } = {}) {
    return new Promise((resolve, reject) => {
      execFile('git', args, { cwd, windowsHide: true, maxBuffer: 16 * 1024 * 1024 }, (err, stdout, stderr) => {
        if (err && !allowFail) return reject(new Error('git ' + args[0] + ' 失败：' + String(stderr || err.message).slice(-400)));
        resolve(String(stdout || ''));
      });
    });
  }
  async available(baseWorkdir) {
    try { return (await this.git(baseWorkdir, ['rev-parse', '--is-inside-work-tree'])).trim() === 'true'; }
    catch (_) { return false; }
  }
  async for(id, baseWorkdir) {
    if (!(await this.available(baseWorkdir))) throw new Error('目标目录不是 git 仓库，无法隔离');
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(id)) throw new Error('工作树名称无效：' + id);
    const target = this.path(id);
    if (fs.existsSync(target)) return target;
    fs.mkdirSync(this.dir, { recursive: true });
    const listed = await this.git(baseWorkdir, ['worktree', 'list', '--porcelain']);
    if (!listed.split('\n').some(l => l === 'worktree ' + target)) {
      try {
        await this.git(baseWorkdir, ['worktree', 'add', target, '-b', this.branch(id)]);
      } catch (e) {
        // A failed attempt takes away only what is provably its own — the
        // branch it just tried to create (codeg's worktree.rs rule).
        await this.git(baseWorkdir, ['branch', '-D', this.branch(id)], { allowFail: true });
        throw e;
      }
    }
    return target;
  }
  async diff(id, baseWorkdir) {
    const target = this.path(id);
    if (!fs.existsSync(target)) throw new Error('该任务没有隔离工作树');
    const branch = this.branch(id);
    const base = (await this.git(baseWorkdir, ['merge-base', 'HEAD', branch], { allowFail: true })).trim();
    const committed = base ? await this.git(baseWorkdir, ['diff', base + '..' + branch], { allowFail: true }) : '';
    const dirty = await this.git(target, ['diff', 'HEAD'], { allowFail: true });
    const untracked = (await this.git(target, ['ls-files', '--others', '--exclude-standard'], { allowFail: true }))
      .split('\n').filter(Boolean);
    // Untracked output files never appear in `git diff`, but a reviewer needs them.
    let newFiles = '';
    for (const rel of untracked.slice(0, 20)) {
      const file = path.join(target, rel);
      let body = '';
      try {
        if (fs.statSync(file).isFile()) body = fs.readFileSync(file, 'utf8').slice(0, 20000);
      } catch (_) { continue; }
      newFiles += '\ndiff --git a/' + rel + ' b/' + rel + ' (new file)\n--- /dev/null\n+++ b/' + rel + '\n' +
        body.split('\n').map(l => '+' + l).join('\n') + '\n';
    }
    const text = [committed, dirty, newFiles].filter(s => s.trim()).join('\n').slice(0, MAX_DIFF);
    return { branch, hasDiff: !!text.trim(), truncated: committed.length + dirty.length + newFiles.length > MAX_DIFF,
      untracked: untracked.slice(0, 200), text };
  }
  async merge(id, baseWorkdir, message = 'Merge ' + this.branch(id)) {
    const target = this.path(id);
    if (!fs.existsSync(target)) throw new Error('该任务没有隔离工作树');
    // The .worktrees directory lives inside the repo; it is not user dirt.
    const dirty = (await this.git(baseWorkdir, ['status', '--porcelain', '--', '.', ':!.worktrees']))
      .split('\n').filter(l => l.trim() && !l.includes('.worktrees'));
    if (dirty.length) throw new Error('主工作区有未提交改动，请先提交或暂存后再合并');
    // ACP sessions rarely commit; auto-commit whatever the agent left behind so
    // uncommitted output is not silently dropped by the merge.
    await this.git(target, ['add', '-A']);
    await this.git(target, ['commit', '-m', 'Agent work (auto-committed before review merge)'], { allowFail: true });
    await this.git(baseWorkdir, ['merge', '--no-ff', this.branch(id), '-m', message.slice(0, 200)]);
    await this.cleanup(id, baseWorkdir);
    return { merged: true };
  }
  async discard(id, baseWorkdir) {
    if (!fs.existsSync(this.path(id))) throw new Error('该任务没有隔离工作树');
    await this.cleanup(id, baseWorkdir);
    return { discarded: true };
  }
  async cleanup(id, baseWorkdir) {
    await this.git(baseWorkdir, ['worktree', 'remove', '--force', this.path(id)]);
    await this.git(baseWorkdir, ['branch', '-D', this.branch(id)]);
  }
}
module.exports = { Worktrees };
