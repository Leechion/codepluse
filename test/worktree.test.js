'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { Worktrees } = require('../core/worktree');
const Core = require('../core/engine');
const { Orchestrator } = require('../core/orchestrator');

function tmpDir() { return fs.mkdtempSync(path.join(os.tmpdir(), 'harness-wt-')); }
async function initRepo(dir) {
  const wt = new Worktrees({ root: dir });
  await wt.git(dir, ['init']);
  await wt.git(dir, ['config', 'user.email', 'test@example.com']);
  await wt.git(dir, ['config', 'user.name', 'Test']);
  fs.writeFileSync(path.join(dir, 'base.txt'), 'base');
  await wt.git(dir, ['add', '.']);
  await wt.git(dir, ['commit', '-m', 'init']);
  return wt;
}

test('missions run in an isolated worktree and a reviewed merge lands on the base branch', async () => {
  const repo = tmpDir();
  const wt = await initRepo(repo);
  const dir = await wt.for('mission_m', repo);
  assert.equal(fs.existsSync(path.join(dir, 'base.txt')), true, 'worktree starts from the repo content');
  assert.equal(await wt.for('mission_m', repo), dir, 'provisioning is idempotent');
  fs.writeFileSync(path.join(dir, 'result.txt'), 'agent output');
  const diff = await wt.diff('mission_m', repo);
  assert.equal(diff.hasDiff, true);
  assert.match(diff.text, /result\.txt/);
  assert.equal(diff.untracked.includes('result.txt'), true);
  fs.writeFileSync(path.join(repo, 'human-draft.txt'), 'draft');
  await assert.rejects(wt.merge('mission_m', repo), /主工作区有未提交改动/);
  fs.rmSync(path.join(repo, 'human-draft.txt'));
  await wt.merge('mission_m', repo, 'Merge reviewed agent work');
  assert.equal(fs.readFileSync(path.join(repo, 'result.txt'), 'utf8'), 'agent output', 'uncommitted agent output is merged too');
  assert.equal(fs.existsSync(dir), false, 'the worktree is removed after merge');
  fs.rmSync(repo, { recursive: true, force: true });
});

test('discard removes the worktree and branch without touching the base', async () => {
  const repo = tmpDir();
  const wt = await initRepo(repo);
  const dir = await wt.for('mission_d', repo);
  fs.writeFileSync(path.join(dir, 'junk.txt'), 'unwanted');
  await wt.discard('mission_d', repo);
  assert.equal(fs.existsSync(dir), false);
  assert.equal(fs.existsSync(path.join(repo, 'junk.txt')), false);
  fs.rmSync(repo, { recursive: true, force: true });
});

test('non-git directories refuse isolation instead of faking it', async () => {
  const plain = tmpDir();
  const wt = new Worktrees({ root: plain });
  assert.equal(await wt.available(plain), false);
  await assert.rejects(wt.for('mission_x', plain), /不是 git 仓库/);
  fs.rmSync(plain, { recursive: true, force: true });
});

function setup(invoke, worktrees) {
  const hub = new Core.Hub({ autoSeed: false, mode: 'real' });
  for (const id of ['manager', 'alpha']) {
    const a = hub.createAgent({ id, type: 'codex', name: id, load: 0 }); a.real = true;
  }
  return { hub, orchestrator: new Orchestrator(hub, invoke, null, worktrees) };
}
const spec = { objective: 'Work in isolation', managerId: 'manager', workerIds: ['alpha'],
  workdir: os.tmpdir(), isolate: 'worktree' };

test('worktree missions execute inside the isolated copy and fall back when provisioning fails', async () => {
  const worker = JSON.stringify({ output: 'isolation checked', complete: true });
  let seenDir = null;
  const failing = { for: async () => { throw new Error('git missing'); } };
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify({ tasks: [{ id: 't1', agentId: 'alpha', title: 'T', instructions: 'do it' }] }) };
    if (options.stage === '宰相汇总汇报') {
      return { ok: true, output: JSON.stringify({ complete: true, report: 'done', taskVerdicts: [{ id: 't1', accepted: true, reason: 'ok' }] }) };
    }
    seenDir = options.workdir;
    return { ok: true, output: worker };
  }, failing);
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'completed');
  assert.match(m.isolationError, /回退共享目录/);
  assert.equal(m.worktree, undefined);
  assert.equal(seenDir, os.tmpdir(), 'the shared workdir is used when isolation fails');

  const isolated = { for: async (id, base) => { const dir = path.join(base, 'wt-' + id); fs.mkdirSync(dir, { recursive: true }); return dir; } };
  let isolatedDir = null;
  const { orchestrator: o2 } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify({ tasks: [{ id: 't1', agentId: 'alpha', title: 'T', instructions: 'do it' }] }) };
    if (options.stage === '宰相汇总汇报') {
      return { ok: true, output: JSON.stringify({ complete: true, report: 'done', taskVerdicts: [{ id: 't1', accepted: true, reason: 'ok' }] }) };
    }
    isolatedDir = options.workdir;
    return { ok: true, output: worker };
  }, isolated);
  const m2 = o2.create(spec); await m2.completion;
  assert.equal(m2.status, 'completed');
  assert.match(m2.workdir, /wt-mission_/);
  assert.equal(isolatedDir, m2.workdir, 'worker calls run inside the worktree');
  assert.equal(m2.baseWorkdir, os.tmpdir());
});
