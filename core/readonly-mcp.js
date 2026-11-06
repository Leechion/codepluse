'use strict';
const fs = require('node:fs');
const path = require('node:path');
const readline = require('node:readline');

// Read-only MCP stdio server. Both lexical paths and symlink targets must stay
// inside the explicitly supplied workspace. No command or write tools exist.
function createFiles(root) {
  root = fs.realpathSync(root);
  function resolve(input = '.') {
    const candidate = fs.realpathSync(path.resolve(root, input));
    const relative = path.relative(root, candidate);
    if (relative === '..' || relative.startsWith('..' + path.sep) || path.isAbsolute(relative)) throw new Error('Path is outside the workspace');
    return candidate;
  }
  return function call(name, args = {}) {
    const file = resolve(args.path);
    if (name === 'list_directory') return fs.readdirSync(file, { withFileTypes: true }).slice(0, 500).map(e => ({ name: e.name, directory: e.isDirectory() }));
    if (name !== 'read_file') throw new Error('Unknown tool');
    if (!fs.statSync(file).isFile()) throw new Error('Not a regular file');
    const start = Math.max(1, Math.floor(Number(args.startLine) || 1));
    const count = Math.max(1, Math.min(400, Math.floor(Number(args.lineCount) || 200)));
    // Never read an unbounded file into the model context.
    const fd = fs.openSync(file, 'r');
    try {
      const buf = Buffer.alloc(256 * 1024);
      const size = fs.readSync(fd, buf, 0, buf.length, 0);
      const lines = buf.subarray(0, size).toString('utf8').split(/\r?\n/);
      return { path: path.relative(root, file), truncated: fs.fstatSync(fd).size > size,
        text: lines.slice(start - 1, start - 1 + count).map((line, i) => (start + i) + ': ' + line).join('\n') };
    } finally { fs.closeSync(fd); }
  };
}
if (require.main === module) {
  const call = createFiles(process.argv[2]);
  const tools = [
    { name: 'read_file', description: 'Read workspace file with line numbers. Use this instead of shell commands.', inputSchema: { type: 'object', properties: { path: { type: 'string' }, startLine: { type: 'integer' }, lineCount: { type: 'integer' } }, required: ['path'] }, annotations: { readOnlyHint: true } },
    { name: 'list_directory', description: 'List workspace directory entries.', inputSchema: { type: 'object', properties: { path: { type: 'string' } } }, annotations: { readOnlyHint: true } }
  ];
  readline.createInterface({ input: process.stdin }).on('line', line => {
    let req;
    try { req = JSON.parse(line); } catch (_) { return; }
    if (req.id == null) return;
    let result;
    try {
      if (req.method === 'initialize') result = { protocolVersion: req.params?.protocolVersion || '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'harness-workspace-files', version: '1.0.0' } };
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
module.exports = { createFiles };
