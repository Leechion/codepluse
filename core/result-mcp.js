'use strict';
const readline = require('node:readline');

// Structured-result sink (the hub-side counterpart of codeg's `submit_result`).
// Orchestrator turns inject this MCP server via `session/new` with the stage's
// output schema as an argv JSON document: a submission that does not match is
// REFUSED with the list of what to fix, so the agent retries inside the same
// turn; after MAX_REFUSED refusals it lets submissions through again so a
// stubborn model cannot loop forever, and the hub-side validation has the
// final say. The hub captures the arguments from the ACP `tool_call` rawInput.
const MAX_REFUSED = 5;
const MAX_ERRORS = 8;

function typeOf(value) {
  if (Array.isArray(value)) return 'array';
  if (value === null) return 'null';
  return typeof value;
}
function validateAgainst(value, schema, path, errors) {
  if (errors.length >= MAX_ERRORS) return;
  const where = path || '提交参数';
  if (schema.enum) {
    if (!schema.enum.includes(value)) errors.push(where + ' 必须是 ' + schema.enum.map(v => JSON.stringify(v)).join(' / '));
    return;
  }
  const expected = schema.type;
  if (expected && typeOf(value) !== expected) {
    const names = { string: '字符串', boolean: '布尔值', array: '数组', object: '对象', number: '数字' };
    errors.push(where + ' 必须是' + (names[expected] || expected) + '，收到 ' + typeOf(value));
    return;
  }
  if (expected === 'object' && schema.properties) {
    for (const key of schema.required || []) {
      if (!(key in (value || {}))) errors.push(where + '缺少必填字段 ' + key);
    }
    for (const [key, child] of Object.entries(schema.properties)) {
      if (value && key in value) validateAgainst(value[key], child, path ? path + '.' + key : key, errors);
    }
    return;
  }
  if (expected === 'array' && schema.items && Array.isArray(value)) {
    value.slice(0, 20).forEach((item, i) => validateAgainst(item, schema.items, (path || '提交参数') + '[' + i + ']', errors));
  }
}
function makeValidator(schema) {
  if (!schema || schema.type !== 'object') return () => [];
  return args => {
    const errors = [];
    validateAgainst(args, schema, '', errors);
    return errors.slice(0, MAX_ERRORS);
  };
}
function createSink(schema) {
  const check = makeValidator(schema);
  let refused = 0;
  return function call(name, args) {
    if (name !== 'submit_result') throw new Error('Unknown tool');
    if (!args || typeof args !== 'object' || Array.isArray(args) || !Object.keys(args).length) {
      throw new Error('请把约定的 JSON 字段作为工具参数对象提交');
    }
    if (schema) {
      const errors = check(args);
      if (errors.length) {
        if (refused >= MAX_REFUSED) return { received: true, note: '已多次拒绝，本次放行，由服务端最终校验' };
        refused++;
        throw new Error('提交未通过 schema 校验，请修正后重新调用：' + errors.join('；'));
      }
    }
    return { received: true };
  };
}

const TOOL_DESCRIPTION = '向 Harness Hub 提交本回合的结构化结果。把要求的 JSON 的每个字段直接作为工具参数传入。若提交被拒绝，按错误说明修正后重新调用；只在被明确要求提交结构化结果时调用。';

if (require.main === module) {
  let schema = null;
  try { schema = process.argv[2] ? JSON.parse(process.argv[2]) : null; } catch (_) { schema = null; }
  const call = createSink(schema);
  const tools = [{
    name: 'submit_result',
    description: TOOL_DESCRIPTION,
    inputSchema: schema && schema.type === 'object' ? schema : { type: 'object' }
  }];
  readline.createInterface({ input: process.stdin }).on('line', line => {
    let req;
    try { req = JSON.parse(line); } catch (_) { return; }
    if (req.id == null) return;
    let result;
    try {
      if (req.method === 'initialize') result = { protocolVersion: req.params?.protocolVersion || '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'harness-result', version: '1.1.0' } };
      else if (req.method === 'ping') result = {};
      else if (req.method === 'tools/list') result = { tools };
      else if (req.method === 'tools/call') {
        try { result = { content: [{ type: 'text', text: JSON.stringify(call(req.params.name, req.params.arguments)) }] }; }
        catch (e) { result = { isError: true, content: [{ type: 'text', text: e.message }] }; }
      } else { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: req.id, error: { code: -32601, message: 'Method not found' } }) + '\n'); return; }
      process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: req.id, result }) + '\n');
    } catch (e) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: req.id, error: { code: -32602, message: e.message } }) + '\n'); }
  });
}
module.exports = { createSink, makeValidator, MAX_REFUSED, MAX_ERRORS };
