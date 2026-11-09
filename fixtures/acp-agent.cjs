'use strict';
// Protocol fixture for tests. No models or external services are called.
const readline = require('node:readline');
const { randomUUID } = require('node:crypto');
const sessions = new Map(), waiting = new Map(); let next = 1000;
const resumeOnly = process.argv.includes('--resume-only');
const forkEnabled = process.argv.includes('--fork');
const imagesEnabled = process.argv.includes('--images');
const embeddedEnabled = process.argv.includes('--embedded');
const steeringEnabled = process.argv.includes('--steer');
const send = msg => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', ...msg }) + '\n');
const result = (id, value) => send({ id, result: value });
const update = (id, value) => send({ method: 'session/update', params: { sessionId: id, update: value } });
const configs = s => ({ modes: { currentModeId: s.mode, availableModes: [{ id: 'read-only', name: 'Read only' }, { id: 'workspace-write', name: 'Workspace' }, { id: 'agent-full-access', name: 'Full access' }] },
  models: { currentModelId: s.model, availableModels: [{ modelId: 'fixture-v1', name: 'Fixture 1' }, { modelId: 'fixture-v2', name: 'Fixture 2' }] } });
function request(method, params) { const id = ++next; return new Promise(resolve => { waiting.set(id, resolve); send({ id, method, params }); }); }
readline.createInterface({ input: process.stdin }).on('line', async line => {
  const msg = JSON.parse(line);
  if (!msg.method) { const resolve = waiting.get(msg.id); waiting.delete(msg.id); if (resolve) resolve(msg.error ? { error: msg.error } : msg.result); return; }
  const p = msg.params || {};
  if (msg.method === 'initialize') return result(msg.id, { protocolVersion: 1, agentCapabilities: resumeOnly ? {sessionCapabilities:{resume:{}}} : { loadSession: true, ...((imagesEnabled || embeddedEnabled) ? {promptCapabilities:{image:imagesEnabled,embeddedContext:embeddedEnabled}} : {}), ...(forkEnabled ? {sessionCapabilities:{fork:{}}} : {}) }, ...(steeringEnabled ? {_meta:{steering:{supported:true}}} : {}), agentInfo: forkEnabled ? { name: '@agentclientprotocol/codex-acp', version: '2.5.0' } : { name: 'test-acp', version: '1' } });
  if (resumeOnly && (msg.method === 'session/load' || msg.method === 'session/set_mode')) return send({id:msg.id,error:{code:-32601,message:'unsupported'}});
  if (msg.method === 'session/new' || msg.method === 'session/load' || (resumeOnly && msg.method === 'session/resume')) {
    if ((p.mcpServers || []).some(server => !Array.isArray(server.env))) return send({ id: msg.id, error: { code: -32602, message: 'MCP stdio env must be an array' } });
    const id = p.sessionId || randomUUID();
    const s = { mode: 'read-only', model: 'fixture-v1', memory: '', turns: 0, history: [], mcp: Array.isArray(p.mcpServers) ? p.mcpServers.length : -1 }; sessions.set(id, s);
    return result(msg.id, { ...(msg.method === 'session/new' ? { sessionId: id } : {}), ...(resumeOnly ? {} : configs(s)) });
  }
  const s = sessions.get(p.sessionId);
  if (msg.method === 'session/fork') {
    const point = p._meta?.jetbrains?.air?.fork;
    const match = /^fixture-message-(\d+)$/.exec(point?.messageId || '');
    const index = match ? Number(match[1]) - 1 : -1;
    if (!forkEnabled || !s || point?.version !== 1 || index < 0 || index >= s.history.length) return send({ id: msg.id, error: { code: -32602, message: 'Unknown fork point' } });
    const forkId = randomUUID();
    const fork = { ...s, memory: s.history[index], turns: index + 1, history: s.history.slice(0, index + 1), mcp: Array.isArray(p.mcpServers) ? p.mcpServers.length : -1 };
    sessions.set(forkId, fork);
    return result(msg.id, { sessionId: forkId, ...configs(fork) });
  }
  if (msg.method === 'session/set_mode') { s.mode = p.modeId; return result(msg.id, {}); }
  if (msg.method === 'session/set_config_option') {
    if (p.configId === 'mode') { s.mode = p.value; return result(msg.id, configs(s)); }
    if (!['fixture-v1', 'fixture-v2'].includes(p.value)) return send({ id: msg.id, error: { code: -32602, message: 'Unknown model' } });
    s.model = p.value;
    return result(msg.id, { configOptions: [{ id: 'model', name: 'Model', category: 'model', type: 'select', currentValue: s.model,
      options: [{ value: 'fixture-v1', name: 'Fixture 1' }, { value: 'fixture-v2', name: 'Fixture 2' }] }] });
  }
  if (msg.method === 'session/cancel') { if (s.promptId) { result(s.promptId, { stopReason: 'cancelled' }); s.promptId = null; } return; }
  if (msg.method === '_session/steering') {
    if (!steeringEnabled || p._meta?.steering?.idleBehavior !== 'promptRequired') return send({id:msg.id,error:{code:-32602,message:'steering not available'}});
    if (!s?.promptId) return result(msg.id, {outcome:'promptRequired',reason:'noRunningTurn'});
    update(p.sessionId, {sessionUpdate:'agent_message_chunk', content:{type:'text', text:'steered:' + p.prompt.map(x=>x.type === 'image' ? `image:${x.mimeType}:${x.data}` : x.type === 'resource_link' ? `resource:${x.name}:${x.uri}` : x.type === 'resource' ? `embedded:${x.resource.uri}:${x.resource.text ?? x.resource.blob}` : (x.text || '')).join('')}});
    result(msg.id, {outcome:'injected'});
    result(s.promptId, {stopReason:'end_turn'}); s.promptId = null; return;
  }
  if (msg.method !== 'session/prompt') return send({ id: msg.id, error: { code: -32601, message: 'Unknown method' } });
  const text = p.prompt[0].text; s.turns++;
  if (text === 'refusal') return result(msg.id,{stopReason:'refusal',_meta:{'codebuddy.ai/errorMessage':JSON.stringify({message:'Authentication required',data:{details:'Please sign in to WorkBuddy'}})}});
  if (text === 'wait' || text.endsWith('special-wait')) { s.promptId = msg.id; return; }
  if (text === 'stream-slow') {
    s.promptId = msg.id;
    for (let i = 0; i < 5; i++) {
      await new Promise(resolve => setTimeout(resolve, 600));
      if (s.promptId !== msg.id) return;
      update(p.sessionId, { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: '.' } });
    }
    s.promptId = null;
    return result(msg.id, { stopReason: 'end_turn' });
  }
  let output = text;
  if (text.endsWith('thinking-test')) {
    update(p.sessionId, {sessionUpdate:'agent_thought_chunk',content:{type:'text',text:'Inspecting '}});
    update(p.sessionId, {sessionUpdate:'agent_thought_chunk',content:{type:'text',text:'the repository'}});
    update(p.sessionId, {sessionUpdate:'agent_thought_chunk',_meta:{claudeCode:{parentToolUseId:'child-tool'}},content:{type:'text',text:'child thought'}});
    output = 'main answer';
  }
  if (p.prompt.length > 1) output = p.prompt.slice(1).map(item => item.type === 'text' ? `text:${item.text}` : item.type === 'resource_link' ? `resource:${item.name}:${item.uri}:${item.mimeType || ''}` : item.type === 'resource' ? `embedded:${item.resource.uri}:${item.resource.mimeType}:${item.resource.text ?? item.resource.blob}` : `image:${item.mimeType}:${item.data}`).join('|');
  if (text.startsWith('remember ')) { s.memory = text.slice(9); output = 'remembered'; }
  if (text === 'recall') output = s.memory;
  s.history[s.turns - 1] = s.memory;
  if (text === 'tool') {
    update(p.sessionId, { sessionUpdate: 'tool_call', toolCallId: 'file-1', title: 'Read package.json', kind: 'read', status: 'in_progress' });
    update(p.sessionId, { sessionUpdate: 'tool_call_update', toolCallId: 'file-1', status: 'completed', content: [{ type: 'content', content: { type: 'text', text: 'actual fixture output' } }] });
  }
  if (text === 'mcp') output = 'mcp:' + s.mcp;
  if (text === 'submit') {
    update(p.sessionId, { sessionUpdate: 'tool_call', toolCallId: 'result-1', title: 'submit_result', kind: 'other', status: 'in_progress', rawInput: { output: 'tool says done', complete: true } });
    update(p.sessionId, { sessionUpdate: 'tool_call_update', toolCallId: 'result-1', status: 'completed' });
    output = '已通过 submit_result 提交';
  }
  if (text === 'permission' || text === 'permission-internal') {
    const permission = await request('session/request_permission', { sessionId: p.sessionId, toolCall: { toolCallId: 'edit-1', title: text === 'permission-internal' ? 'mcp__codeg-mcp__list_agents' : 'Edit file', kind: 'edit', status: 'pending' },
      options: [{ optionId: 'yes', name: 'Allow once', kind: 'allow_once' }, { optionId: 'no', name: 'Reject', kind: 'reject_once' }] });
    output = permission.outcome.outcome === 'selected' ? permission.outcome.optionId : 'cancelled';
  }
  if (text === 'read') {
    const read = await request('fs/read_text_file', { sessionId: p.sessionId, path: 'package.json' }); output = read.content;
  }
  for (const chunk of [output.slice(0, 2), output.slice(2)]) update(p.sessionId, { sessionUpdate: 'agent_message_chunk', messageId: 'fixture-message-' + s.turns, content: { type: 'text', text: chunk } });
  result(msg.id, { stopReason: 'end_turn' });
});
