'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { Skills } = require('../core/skills');
const Core = require('../core/engine');
const { Orchestrator } = require('../core/orchestrator');

test('skills list, load, validate and render prompt fragments', () => {
  const skills = new Skills({ dir: path.resolve(__dirname, '../skills') });
  const list = skills.list();
  assert.equal(list.some(s => s.name === 'code-review'), true);
  assert.match(skills.load('code-review'), /代码复核/);
  assert.equal(skills.load('..\\escape'), null);
  assert.equal(skills.load('missing'), null);
  assert.deepEqual(skills.assign(['code-review', 'code-review']), ['code-review']);
  assert.throws(() => skills.assign('nope'), /技能列表无效/);
  assert.throws(() => skills.assign(['../etc/passwd']), /技能名称无效/);
});

test('only assigned agents get skill text appended to their prompts', async () => {
  const hub = new Core.Hub({ autoSeed: false, mode: 'real' });
  for (const id of ['manager', 'reviewer']) {
    const a = hub.createAgent({ id, type: 'codex', name: id }); a.real = true;
  }
  hub.agents.get('reviewer').skills = ['code-review'];
  const skills = new Skills({ dir: path.resolve(__dirname, '../skills') });
  // Skill text reaches reviewer turns but never the manager's planning call.
  const spec = { objective: 'Review the code', managerId: 'manager', workerIds: ['reviewer'] };
  let reviewerPrompt = '';
  const o2 = new Orchestrator(hub, async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') {
      assert.doesNotMatch(prompt, /技能指引/, 'manager planning stays unskilled');
      return { ok: true, output: JSON.stringify({ tasks: [{ id: 't1', agentId: 'reviewer', title: 'T', instructions: 'review it' }] }) };
    }
    if (options.stage === '宰相汇总汇报') return { ok: true, output: JSON.stringify({ complete: true, report: 'ok', taskVerdicts: [{ id: 't1', accepted: true, reason: 'ok' }] }) };
    reviewerPrompt = prompt;
    return { ok: true, output: JSON.stringify({ output: 'reviewed', complete: true }) };
  }, null, null, skills);
  const m = o2.create(spec); await m.completion;
  assert.equal(m.status, 'completed');
  assert.match(reviewerPrompt, /技能指引/);
  assert.match(reviewerPrompt, /代码复核/);
});

test('the skills directory is optional', () => {
  const skills = new Skills({ dir: path.join(os.tmpdir(), 'no-such-skills-dir') });
  assert.deepEqual(skills.list(), []);
  assert.equal(skills.promptFor({ skills: ['x'] }), '');
});
