'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');
const { createFiles } = require('../core/readonly-mcp');

test('workspace file tool reads with line numbers and rejects outside paths', () => {
  const call = createFiles(__dirname);
  const result = call('read_file', { path: 'readonly-mcp.test.js', startLine: 1, lineCount: 2 });
  assert.match(result.text, /^1: 'use strict';/);
  assert.equal(result.text.split('\n').length, 2);
  assert.throws(() => call('read_file', { path: '../package.json' }), /outside/);
  assert.throws(() => call('write_file', { path: 'readonly-mcp.test.js' }), /Unknown/);
});

test('MCP stdio handshake and tool call return only JSON-RPC responses', () => {
  const requests = [
    { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05' } },
    { jsonrpc: '2.0', method: 'notifications/initialized' },
    { jsonrpc: '2.0', id: 2, method: 'tools/list' },
    { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'read_file', arguments: { path: 'package.json' } } }
  ];
  const result = spawnSync(process.execPath, [path.resolve(__dirname, '../core/readonly-mcp.js'), path.resolve(__dirname, '..')], {
    input: requests.map(r => JSON.stringify(r)).join('\n') + '\n', encoding: 'utf8', windowsHide: true, timeout: 5000
  });
  assert.equal(result.status, 0);
  const replies = result.stdout.trim().split('\n').map(line => JSON.parse(line));
  assert.equal(replies.length, 3);
  assert.equal(replies[0].result.protocolVersion, '2024-11-05');
  assert.deepEqual(replies[1].result.tools.map(t => t.name), ['read_file', 'list_directory']);
  assert.match(replies[2].result.content[0].text, /harness-hub/);
});
