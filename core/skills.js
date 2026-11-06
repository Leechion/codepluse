'use strict';
const fs = require('node:fs');
const path = require('node:path');

// Codeg-style Skill Packs: reusable instruction snippets assigned to agents as
// a matrix (agent -> [skill names]). Bodies live in skills/<name>.md and are
// appended verbatim to the matching agent's prompts by the orchestrator.
const NAME = /^[a-zA-Z0-9_-]{1,64}$/;
const MAX_BODY = 4000;
const MAX_PER_AGENT = 3;

class Skills {
  constructor({ dir = path.join(__dirname, '../skills') } = {}) {
    this.dir = dir;
    this.cache = new Map();
  }
  list() {
    if (!fs.existsSync(this.dir)) return [];
    return fs.readdirSync(this.dir).filter(f => f.endsWith('.md')).map(f => {
      const name = f.slice(0, -3);
      const content = this.load(name) || '';
      return { name, summary: (content.split('\n').find(l => l.trim()) || '').replace(/^#+\s*/, '').slice(0, 120), length: content.length };
    }).sort((a, b) => a.name.localeCompare(b.name));
  }
  load(name) {
    if (!NAME.test(name)) return null;
    if (this.cache.has(name)) return this.cache.get(name);
    let body = null;
    try {
      const file = path.join(this.dir, name + '.md');
      if (fs.existsSync(file)) body = fs.readFileSync(file, 'utf8').trim();
    } catch (_) { body = null; }
    this.cache.set(name, body);
    return body;
  }
  // Validates an assignment list and returns the sanitized names.
  assign(names) {
    if (!Array.isArray(names) || names.length > 8) throw new Error('技能列表无效');
    return [...new Set(names.map(n => {
      if (typeof n !== 'string' || !NAME.test(n)) throw new Error('技能名称无效：' + n);
      return n;
    }))];
  }
  // Renders the prompt fragment for one agent: at most 3 skills, 4000 chars each.
  promptFor(agent) {
    const names = (agent?.skills || []).slice(0, MAX_PER_AGENT);
    if (!names.length) return '';
    const parts = [];
    for (const name of names) {
      const body = this.load(name);
      if (body) parts.push('【' + name + '】\n' + body.slice(0, MAX_BODY));
    }
    if (!parts.length) return '';
    return '\n以下是本执行端被指派的技能指引，请在完成工作时遵循：\n' + parts.join('\n\n');
  }
  dropCache() { this.cache.clear(); }
}
module.exports = { Skills };
