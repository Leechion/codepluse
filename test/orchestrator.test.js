'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const Core = require('../core/engine');
const { Orchestrator, json } = require('../core/orchestrator');

test('CLI prose around one JSON block is parsed without weakening validation', () => {
  assert.deepEqual(json('Plan follows:\n```json\n{"tasks":[]}\n```\nExplanation.'), { tasks: [] });
  assert.throws(() => json('No structured result'));
  assert.throws(() => json('```json\n{}\n```\n```json\n{}\n```'));
});

function setup(invoke) {
  const hub = new Core.Hub({ autoSeed: false, mode: 'real' });
  for (const id of ['manager', 'alpha', 'beta']) {
    const a = hub.createAgent({ id, type: 'codex', name: id, load: 0 }); a.real = true;
  }
  return { hub, orchestrator: new Orchestrator(hub, invoke) };
}
const plan = { tasks: [
  { id: 'design', agentId: 'alpha', title: 'Design', instructions: 'Analyze architecture' },
  { id: 'review', agentId: 'beta', title: 'Review', instructions: 'Check design', dependsOn: ['design'] }
] };
const report = (text, accepted = true) => JSON.stringify({ complete: accepted, report: text, taskVerdicts: plan.tasks.map(t => ({ id: t.id, accepted, reason: accepted ? 'Verified' : 'Blocked' })) });
const spec = { objective: 'Analyze the project', managerId: 'manager', workerIds: ['alpha', 'beta'] };

test('manager plans, workers follow dependencies and manager reports actual results', async () => {
  const calls = [];
  const { hub, orchestrator } = setup(async (id, prompt, options) => {
    calls.push(options.stage);
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify(plan) };
    if (options.stage === '宰相汇总汇报') {
      assert.match(prompt, /real review output/); return { ok: true, output: report('Final report with evidence') };
    }
    if (id === 'beta') assert.match(prompt, /real design output/);
    return { ok: true, output: JSON.stringify({ complete: true, output: id === 'alpha' ? 'real design output' : 'real review output' }) };
  });
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'completed');
  assert.equal(m.report, 'Final report with evidence');
  assert.deepEqual(calls, ['宰相拆解任务', 'Design · 第 1 轮', 'Review · 第 1 轮', '宰相汇总汇报']);
  assert.equal(hub.tasks.tasks.every(t => t.execution.mode === 'real'), true);
  assert.equal(hub.verifyAuditChain().ok, true);
  assert.equal(JSON.stringify(m).includes('completion'), false);
});

test('peer messages reach another Harness and wake completed work for a reply', async () => {
  let alpha = 0, beta = 0;
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify({ tasks: plan.tasks.map(t => ({ ...t, dependsOn: [] })) }) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: report('report') };
    if (id === 'alpha') {
      alpha++;
      return { ok: true, output: JSON.stringify({ complete: alpha >= 3, output: alpha >= 3 ? 'reconciled' : 'waiting', messages: alpha === 1 ? [{ to: 'review', text: 'please check contract' }] : [] }) };
    }
    beta++;
    if (beta === 2) assert.match(prompt, /please check contract/);
    return { ok: true, output: JSON.stringify({ complete: true, output: 'checked', messages: beta === 2 ? [{ to: 'design', text: 'contract checked' }] : [] }) };
  });
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'completed');
  assert.equal(alpha, 3); assert.equal(beta, 2);
  assert.equal(m.events.some(e => e.from === 'alpha' && e.to === 'beta' && e.text === 'please check contract'), true);
  assert.equal(m.events.some(e => e.from === 'beta' && e.to === 'alpha' && e.text === 'contract checked'), true);
});

test('failed worker blocks dependents and final status is partial', async () => {
  let workerCalls = 0;
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify(plan) };
    if (options.stage === '宰相汇总汇报') { assert.match(prompt, /false/); return { ok: true, output: report('Failed to execute design', false) }; }
    workerCalls++; return { ok: false, error: 'CLI failed' };
  });
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'partial'); assert.equal(workerCalls, 1);
  assert.equal(m.tasks[0].status, 'failed'); assert.equal(m.tasks[1].status, 'failed');
});

test('invalid plan and unavailable agents never create fake completed work', async () => {
  const { orchestrator } = setup(async () => ({ ok: true, output: 'not valid JSON' }));
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'failed'); assert.equal(m.tasks.length, 0);
  assert.throws(() => orchestrator.create({ ...spec, workerIds: ['missing'] }), /未连接/);
  assert.throws(() => orchestrator.validatePlan(m, { tasks: [{ ...plan.tasks[0], dependsOn: ['design'] }] }), /循环/);
});

test('manager rejects a misleading completed flag based on actual evidence', async () => {
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify(plan) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: report('Could not verify files', false) };
    return { ok: true, output: JSON.stringify({ complete: true, output: 'Blocked: no file access' }) };
  });
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'partial');
  assert.equal(m.tasks.every(t => t.status === 'failed'), true);
});

test('one invalid agent ID triggers a bounded real planner correction', async () => {
  let corrections = 0;
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify({ tasks: [{ ...plan.tasks[0], agentId: 'wrong-id' }] }) };
    if (options.stage === '宰相修正计划') { corrections++; assert.match(prompt, /alpha/); return { ok: true, output: JSON.stringify(plan) }; }
    if (options.stage === '宰相汇总汇报') return { ok: true, output: report('Verified') };
    return { ok: true, output: JSON.stringify({ output: 'Verified output', complete: true }) };
  });
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'completed'); assert.equal(corrections, 1);
});

test('requested files are actually read and supplied on the next worker turn', async () => {
  const { createFiles } = require('../core/readonly-mcp');
  let turns = 0;
  const { hub } = setup(async () => {});
  const orchestrator = new Orchestrator(hub, async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify({ tasks: [plan.tasks[0]] }) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: JSON.stringify({ complete: true, report: 'Actual file checked', taskVerdicts: [{ id: 'design', accepted: true, reason: 'Read actual file' }] }) };
    turns++;
    if (turns === 1) return { ok: true, output: JSON.stringify({ output: 'Request file', complete: true, reads: [{ path: 'package.json' }] }) };
    assert.match(prompt, /harness-hub/);
    assert.match(prompt, /node server.js/);
    return { ok: true, output: JSON.stringify({ output: 'Verified actual package fields', complete: true }) };
  }, (m, request) => createFiles(require('node:path').resolve(__dirname, '..'))('read_file', request));
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'completed'); assert.equal(turns, 2);
  assert.equal(m.events.some(e => e.kind === 'ctx.sync'), true);
});

test('explicit peer communication cannot be satisfied by board-only claims', async () => {
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify({ ...plan, requiresPeerCommunication: true }) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: report('Claims complete but no messages') };
    return { ok: true, output: JSON.stringify({ output: 'I checked the board', complete: true }) };
  });
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.communicationRequested, true);
  assert.equal(m.status, 'partial');
  assert.equal(orchestrator.peerConfirmed(m), false);
});

test('workspace execution is sequential and non-converging collaboration stays partial', async () => {
  let active = 0, max = 0;
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify({ tasks: plan.tasks.map(t => ({ ...t, dependsOn: [] })) }) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: report('Unfinished work remains', false) };
    active++; max = Math.max(active, max); await new Promise(resolve => setImmediate(resolve)); active--;
    return { ok: true, output: JSON.stringify({ complete: false, output: 'Still waiting' }) };
  });
  const m = orchestrator.create({ ...spec, permission: 'workspace' }); await m.completion;
  assert.equal(max, 1); assert.equal(m.status, 'partial'); assert.equal(m.round, 6);
});

test('a captured submit_result tool call beats prose JSON at every stage', async () => {
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: 'plain text garbage', structured: plan };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: 'plain text garbage', structured: { complete: true, report: 'Tool report with evidence', taskVerdicts: plan.tasks.map(t => ({ id: t.id, accepted: true, reason: 'Verified' })) } };
    assert.match(prompt, /submit_result/);
    return { ok: true, output: 'plain text garbage', structured: { output: id === 'alpha' ? 'real design output' : 'real review output', complete: true } };
  });
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'completed');
  assert.equal(m.report, 'Tool report with evidence');
  assert.equal(m.tasks.map(t => t.output).join('|'), 'real design output|real review output');
});

test('fork branches a finished board into a fresh mission that re-runs with context', async () => {
  let forked = false;
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify(plan) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: report('Fork verified', true) };
    if (!forked) return { ok: true, output: JSON.stringify({ complete: true, output: 'first pass' }) };
    assert.match(prompt, /first pass/, 'the forked worker sees the original output on the board');
    return { ok: true, output: JSON.stringify({ complete: true, output: 'second pass' }) };
  });
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'completed');
  const f = orchestrator.fork(m.id, 'review'); // keeps review + its dependency design
  forked = true;
  assert.equal(f.forkedFrom, m.id);
  assert.deepEqual(f.tasks.map(t => t.id), ['design', 'review']);
  await f.completion;
  assert.equal(f.status, 'completed');
  assert.equal(f.tasks.every(t => t.output === 'second pass'), true);
  assert.equal(m.tasks.every(t => t.output === 'first pass'), true, 'the source mission is untouched');
  assert.throws(() => orchestrator.fork(m.id, 'nope'), /子任务不存在/);
});

test('each stage carries its output schema and manager sessions split by schema family', async () => {
  const { OUTPUT_SCHEMAS, schemaKeyFor } = require('../core/orchestrator');
  assert.equal(schemaKeyFor('宰相拆解任务'), 'plan');
  assert.equal(schemaKeyFor('宰相修正计划'), 'plan');
  assert.equal(schemaKeyFor('宰相汇总汇报'), 'report');
  assert.equal(schemaKeyFor('宰相对话'), 'decision');
  assert.equal(schemaKeyFor('会话移交简报'), 'briefing');
  assert.equal(schemaKeyFor('随便什么任务 · 第 1 轮'), 'worker');
  const seen = [];
  const { orchestrator } = setup(async (id, prompt, options) => {
    seen.push({ stage: options.stage, schema: options.outputSchema, sessionKey: options.sessionKey });
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify(plan) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: report('Schema report') };
    return { ok: true, output: JSON.stringify({ complete: true, output: 'worker ok' }) };
  });
  const m = orchestrator.create(spec); await m.completion;
  assert.deepEqual(seen.find(s => s.stage === '宰相拆解任务').schema, OUTPUT_SCHEMAS.plan);
  assert.deepEqual(seen.find(s => s.stage === '宰相汇总汇报').schema, OUTPUT_SCHEMAS.report);
  assert.match(seen.find(s => s.stage === '宰相汇总汇报').sessionKey, /manager:report$/, 'the report family gets its own session');
  assert.match(seen.find(s => s.stage === '宰相拆解任务').sessionKey, /manager:plan$/);
  assert.doesNotMatch(seen.find(s => s.stage?.includes('· 第 1 轮')).sessionKey, /manager/);
});

test('fork depth is capped so chains cannot nest forever', async () => {
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify({ tasks: plan.tasks.map(t => ({ ...t, dependsOn: [] })) }) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: report('ok') };
    return { ok: true, output: JSON.stringify({ complete: true, output: 'pass' }) };
  });
  const m = orchestrator.create(spec); await m.completion;
  let last = m;
  for (let i = 0; i < 5; i++) {
    const f = orchestrator.fork(last.id); await f.completion;
    assert.equal(f.status, 'completed');
    assert.equal(orchestrator.forkDepth(f), i + 1);
    last = f;
  }
  assert.throws(() => orchestrator.fork(last.id), /分叉嵌套已达上限/);
});

test('prose JSON still works when no submit_result capture exists', async () => {
  const { orchestrator } = setup(async (id, prompt, options) => {
    if (options.stage === '宰相拆解任务') return { ok: true, output: JSON.stringify(plan) };
    if (options.stage === '宰相汇总汇报') return { ok: true, output: report('Text fallback report') };
    return { ok: true, output: JSON.stringify({ complete: true, output: 'text fallback output' }) };
  });
  const m = orchestrator.create(spec); await m.completion;
  assert.equal(m.status, 'completed');
  assert.equal(m.tasks.every(t => t.output === 'text fallback output'), true);
});
