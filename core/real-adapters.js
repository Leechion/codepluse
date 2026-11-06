#!/usr/bin/env node
/**
 * Real Harness adapters - spawn actual Codex CLI / Claude Code processes.
 * ---------------------------------------------------------------------
 * Node only. Never loaded by the browser console.
 *
 * Implements the same contract as core/engine.js::HarnessAdapter:
 *   descriptor() / heartbeat() / dispatch() / send() / teardown()
 *
 * Security notes:
 *   - No shell is used; prompts are passed via stdin, never interpolated
 *     into a command line (prevents argument injection).
 *   - Three permission modes map to each CLI's own sandbox/permission flags.
 *   - Every invocation is bounded by a timeout and a hard stdout/stderr cap.
 */
'use strict';

const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');
const Core = require('./engine.js');

const HOME = os.homedir();
const NPM_GLOBAL = path.join(HOME, 'AppData', 'Roaming', 'npm', 'node_modules');

const BIN = {
  codex: {
    env: 'HARNESS_HUB_CODEX_BIN',
    candidates: [
      path.join(NPM_GLOBAL, '@openai', 'codex', 'bin', 'codex.js'),
      path.join(HOME, '.npm-global', 'lib', 'node_modules', '@openai', 'codex', 'bin', 'codex.js'),
      '/usr/local/lib/node_modules/@openai/codex/bin/codex.js'
    ],
    label: 'Codex CLI'
  },
  claude: {
    env: 'HARNESS_HUB_CLAUDE_BIN',
    candidates: [
      path.join(NPM_GLOBAL, '@anthropic-ai', 'claude-code', 'bin', 'claude.exe'),
      path.join(NPM_GLOBAL, '@anthropic-ai', 'claude-code', 'bin', 'claude'),
      path.join(HOME, '.npm-global', 'lib', 'node_modules', '@anthropic-ai', 'claude-code', 'bin', 'claude'),
      '/usr/local/lib/node_modules/@anthropic-ai/claude-code/bin/claude'
    ],
    label: 'Claude Code CLI'
  }
};

/** Three runtime-selectable permission modes. */
const PERMISSIONS = {
  readonly: {
    id: 'readonly', label: '只读分析',
    desc: '不写文件、不执行命令；仅分析与回答',
    codex: ['-s', 'read-only'],
    claude: ['--permission-mode', 'plan', '--tools', 'Read,Grep,Glob'],
    allowWrites: false, allowShell: false
  },
  workspace: {
    id: 'workspace', label: '限定目录内可读写',
    desc: '仅允许在指定工作目录内读写文件',
    codex: ['-s', 'workspace-write'],
    claude: ['--permission-mode', 'acceptEdits', '--tools', 'Read,Edit,Write,Grep,Glob'],
    allowWrites: true, allowShell: false
  },
  shell: {
    id: 'shell', label: '只读 + 允许 shell',
    desc: '允许执行命令，但不授予写权限',
    codex: ['-s', 'read-only'],
    claude: ['--permission-mode', 'plan', '--tools', 'Bash,Read,Grep,Glob'],
    allowWrites: false, allowShell: true
  }
};

function resolveBin(tool) {
  const cfg = BIN[tool];
  if (process.env[cfg.env] && fs.existsSync(process.env[cfg.env])) return process.env[cfg.env];
  for (const p of cfg.candidates) if (fs.existsSync(p)) return p;
  return null;
}

/**
 * The user's ~/.codex/config.toml may register heavyweight MCP servers
 * (e.g. native-pipe computer-use) that hang in headless environments.
 * Use an isolated CODEX_HOME carrying only auth.json for our subprocess.
 * Set HARNESS_HUB_CODEX_ISOLATE=0 to honour the user's full config instead.
 */
/**
 * Codex's default backend (chatgpt.com + Responses WebSocket) is often
 * unreachable behind a local proxy. If a compatible gateway is configured
 * for Claude Code we reuse it via an OpenAI-Responses model provider.
 * The token is only passed through a child-process env var - never logged.
 */
function gatewayConfig() {
  try {
    const s = JSON.parse(fs.readFileSync(path.join(HOME, '.claude', 'settings.json'), 'utf8'));
    const e = s.env || {};
    if (!e.ANTHROPIC_BASE_URL || !e.ANTHROPIC_AUTH_TOKEN) return null;
    return {
      baseUrl: String(e.ANTHROPIC_BASE_URL).replace(/\/+$/, ''),
      token: String(e.ANTHROPIC_AUTH_TOKEN),
      model: process.env.HARNESS_HUB_CODEX_MODEL || e.ANTHROPIC_MODEL || 'qwen3.8-flash'
    };
  } catch (_) { return null; }
}

function codexHome() {
  const dir = path.join(os.tmpdir(), 'harness-hub-codex-home');
  try {
    fs.mkdirSync(dir, { recursive: true });
    const src = path.join(HOME, '.codex', 'auth.json');
    const dst = path.join(dir, 'auth.json');
    if (fs.existsSync(src)) {
      let copy = true;
      if (fs.existsSync(dst)) {
        try { copy = fs.statSync(src).mtimeMs > fs.statSync(dst).mtimeMs; } catch (_) { copy = true; }
      }
      if (copy) fs.copyFileSync(src, dst);
    }
    const gw = process.env.HARNESS_HUB_CODEX_GATEWAY === '0' ? null : gatewayConfig();
    if (gw) {
      fs.writeFileSync(path.join(dir, 'config.toml'), [
        'model = "' + gw.model.replace(/"/g, '\\"') + '"',
        'model_provider = "gateway"',
        'model_reasoning_effort = "low"',
        '',
        '[model_providers.gateway]',
        'name = "Local Gateway"',
        'base_url = "' + gw.baseUrl + '/v1"',
        'wire_api = "responses"',
        'env_key = "HH_GATEWAY_KEY"',
        'requires_openai_auth = false',
        ''
      ].join('\n'));
    }
  } catch (_) { /* fall through to a plain isolated home */ }
  return dir;
}

function buildEnv(tool) {
  const env = Object.assign({}, process.env, { NO_COLOR: '1', CI: '1' });
  if (tool === 'claude') {
    // ACP's SDK process also needs the provider settings the native CLI reads.
    try {
      const settings = JSON.parse(fs.readFileSync(path.join(HOME, '.claude', 'settings.json'), 'utf8'));
      for (const [key, value] of Object.entries(settings.env || {})) {
        if (/^ANTHROPIC_/.test(key) && env[key] == null && typeof value === 'string') env[key] = value;
      }
    } catch (_) { /* the native SDK may instead use its logged-in account */ }
  }
  // Claude Code (bundled native binary) rejects the injected corporate CA
  // bundle. The local proxy still works with the built-in trust store.
  if (process.env.HARNESS_HUB_KEEP_CA !== '1') delete env.NODE_EXTRA_CA_CERTS;
  if (tool === 'codex' && process.env.HARNESS_HUB_CODEX_ISOLATE !== '0') {
    env.CODEX_HOME = codexHome();
    const gw = process.env.HARNESS_HUB_CODEX_GATEWAY === '0' ? null : gatewayConfig();
    if (gw) {
      env.HH_GATEWAY_KEY = gw.token;
      env.ALL_PROXY = env.ALL_PROXY || env.HTTPS_PROXY || '';
      env.all_proxy = env.all_proxy || env.https_proxy || '';
    }
  }
  return env;
}

function cap(s, n) { return String(s || '').slice(0, n); }

function classifyError(stderr, exitCode) {
  const t = (stderr || '').toLowerCase();
  if (/chatgpt\.com\/backend-api|responses_websocket|rmcp::transport/.test(t)) {
    return '网络代理不支持 Codex 的 chatgpt.com 后端 / Responses WebSocket';
  }
  if (/auth|login|unauthorized|401|api key|credential/.test(t)) return '认证失效，请先登录对应 CLI';
  if (/timed out|timeout|etimedout|econnreset|enotfound|network/.test(t)) return '网络/接口超时，CLI 无法访问模型服务';
  if (/certificate|tls|ca bundle|self.signed/.test(t)) return 'TLS 证书校验失败';
  if (/input must be provided|unrecognized|unknown option/.test(t)) return 'CLI 参数不兼容';
  return 'CLI 退出码 ' + exitCode;
}

/** Parse Claude Code `--output-format json` into a compact result. */
function parseClaudeOutput(stdout) {
  const raw = String(stdout || '').trim();
  if (!raw) return { text: '', meta: {} };
  const lines = raw.split(/\r?\n/).filter(Boolean);
  let obj = null;
  for (let i = lines.length - 1; i >= 0; i--) {
    try { const v = JSON.parse(lines[i]); if (v && typeof v === 'object') { obj = v; break; } } catch (_) { /* next */ }
  }
  if (!obj) {
    try { obj = JSON.parse(raw); } catch (_) { return { text: cap(raw, 20000), meta: {} }; }
  }
  return {
    text: String(obj.result != null ? obj.result : (obj.message || cap(raw, 20000))),
    meta: {
      model: obj.model || (obj.modelUsage && Object.keys(obj.modelUsage)[0]) || null,
      costUsd: obj.total_cost_usd != null ? obj.total_cost_usd : null,
      sessionId: obj.session_id || null,
      inputTokens: obj.usage && obj.usage.input_tokens,
      outputTokens: obj.usage && obj.usage.output_tokens,
      stopReason: obj.stop_reason || null
    }
  };
}

class RealHarnessAdapter extends Core.HarnessAdapter {
  constructor(spec) {
    super(spec || {});
    this.tool = (spec && spec.tool) || 'claude';
    this.permission = (spec && spec.permission) || 'readonly';
    this.model = (spec && spec.model) || '';
    this.workdir = (spec && spec.workdir) || process.cwd();
    this.timeoutMs = (spec && spec.timeoutMs) || 180000;
    this.maxConcurrent = (spec && spec.maxConcurrent) || 1;
    this.running = 0;
    this.children = new Set();
    this.bin = resolveBin(this.tool);
  }

  descriptor() {
    const p = PERMISSIONS[this.permission] || PERMISSIONS.readonly;
    return {
      label: BIN[this.tool].label,
      vendor: this.tool === 'codex' ? 'OpenAI' : 'Anthropic',
      transport: 'process',
      capabilities: this.tool === 'codex'
        ? ['code.write', 'code.review', 'shell', 'refactor']
        : ['code.write', 'code.review', 'analysis', 'shell', 'long-context'],
      maxConcurrency: this.maxConcurrent,
      reliability: 0.95,
      clearance: Core.CONST.SENS.CONFIDENTIAL,
      real: true,
      permission: this.permission,
      bin: this.bin
    };
  }

  available() {
    return { ok: !!this.bin, bin: this.bin, reason: this.bin ? '' : ('未找到 ' + BIN[this.tool].label + '，可设置 ' + BIN[this.tool].env) };
  }

  /** Local, free health check - never calls the model API. */
  heartbeat() {
    const a = this.available();
    return {
      ok: a.ok,
      load: clamp01(this.running / this.maxConcurrent),
      status: a.ok ? 'online' : 'error',
      detail: a.ok ? '真实 CLI 就绪' : a.reason
    };
  }

  /** Build argv for one tool. Prompt always goes through stdin. */
  _argv(outFile) {
    const p = PERMISSIONS[this.permission] || PERMISSIONS.readonly;
    if (this.tool === 'codex') {
      return {
        cmd: process.execPath,
        args: [this.bin, 'exec']
          .concat(p.codex)
          .concat(this.model ? ['--model', this.model] : [])
          .concat(['-C', this.workdir, '--skip-git-repo-check', '--ephemeral', '-o', outFile, '-'])
      };
    }
    return {
      cmd: this.bin,
      args: ['-p', '--output-format', 'json']
        .concat(p.claude)
        .concat(this.model ? ['--model', this.model] : [])
        .concat(['--add-dir', this.workdir])
    };
  }

  /**
   * Actually run the CLI. Returns { ok, output, error, meta }.
   * `task.prompt` overrides the composed prompt; contextView is injected
   * only when the task has been authorized to read it.
   */
  async dispatch(task, contextView) {
    const a = this.available();
    if (!a.ok) return this._fail(task, a.reason, { code: 'BIN_NOT_FOUND' });
    if (this.running >= this.maxConcurrent) {
      return this._fail(task, '真实执行并发已满，请稍后重试', { code: 'BUSY' });
    }

    const perm = PERMISSIONS[this.permission] || PERMISSIONS.readonly;
    const prompt = this._composePrompt(task, contextView, perm);
    const outFile = path.join(os.tmpdir(), 'harness-hub-' + process.pid + '-' + Date.now() + '-' + Math.random().toString(36).slice(2, 8) + '.out');
    const argv = this._argv(outFile);
    const started = Date.now();

    this.running += 1;
    return new Promise(resolve => {
      let stdout = '', stderr = '', settled = false, timedOut = false;
      let child;
      try {
        child = spawn(argv.cmd, argv.args, {
          cwd: this.workdir,
          windowsHide: true,
          stdio: ['pipe', 'pipe', 'pipe'],
          env: buildEnv(this.tool)
        });
      } catch (e) {
        this.running -= 1;
        return resolve(this._fail(task, '进程启动失败：' + e.message, { code: 'SPAWN_ERROR' }));
      }
      this.children.add(child);

      const timer = setTimeout(() => {
        timedOut = true;
        try { child.kill(); } catch (_) { /* already gone */ }
      }, this.timeoutMs);

      function finish(self, exitCode) {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        self.children.delete(child);
        self.running -= 1;
        const durationMs = Date.now() - started;
        let text = '', meta = {};
        if (self.tool === 'claude') {
          const parsed = parseClaudeOutput(stdout);
          text = parsed.text; meta = parsed.meta;
        } else {
          try { text = fs.readFileSync(outFile, 'utf8').trim(); } catch (_) { text = ''; }
          if (!text) text = cap(stdout.split(/\r?\n/).filter(Boolean).pop() || '', 20000);
        }
        try { fs.unlinkSync(outFile); } catch (_) { /* noop */ }

        const base = {
          tool: self.tool,
          bin: self.bin,
          permission: self.permission,
          permissionLabel: perm.label,
          workdir: self.workdir,
          durationMs,
          exitCode,
          timedOut,
          promptChars: prompt.length,
          stdout: cap(stdout, 4000),
          stderr: cap(stderr, 2000)
        };
        Object.assign(base, meta);
        if (timedOut) {
          const hint = classifyError(stderr, 'timeout');
          return resolve(self._fail(task,
            '执行超时（' + Math.round(self.timeoutMs / 1000) + 's）' +
            (hint !== 'CLI 退出码 timeout' ? ' · ' + hint : ''), base));
        }
        if (exitCode !== 0) return resolve(self._fail(task, classifyError(stderr, exitCode) + (stderr ? '：' + cap(stderr.trim(), 160) : ''), base));
        if (!text) return resolve(self._fail(task, 'CLI 未返回有效输出', base));

        self.emitLog && self.emitLog('success', '真实执行', BIN[self.tool].label + ' 返回 ' + text.length + ' 字符（' + durationMs + 'ms）');
        resolve({
          ok: true,
          output: cap(text, 20000),
          meta: Object.assign({}, base, { status: 'done' }),
          error: ''
        });
      }

      child.stdout.on('data', d => { if (stdout.length < 400000) stdout += d.toString(); });
      child.stderr.on('data', d => { if (stderr.length < 100000) stderr += d.toString(); });
      child.on('error', e => { stderr += ' SPAWN_ERROR ' + e.message; finish(this, -1); });
      child.on('close', code => finish(this, code));

      // Prompt is delivered through stdin and the stream is closed immediately
      // (both CLIs otherwise wait for additional input).
      try {
        child.stdin.on('error', () => {});
        child.stdin.write(prompt, 'utf8');
        child.stdin.end();
      } catch (e) {
        stderr += ' STDIN_ERROR ' + e.message;
      }
    });
  }

  _composePrompt(task, contextView, perm) {
    const parts = [];
    parts.push(task.prompt || task.title);
    if (task.description) parts.push('\n补充要求：' + task.description);
    if (contextView && contextView.entries && contextView.entries.length) {
      parts.push('\n已授权的会话上下文（已按密级脱敏）：');
      contextView.entries.forEach(e => parts.push('- ' + e.key + ': ' + e.value));
    }
    parts.push('\n权限约束：' + perm.label + '。' +
      (perm.allowWrites ? '仅可在当前工作目录内修改文件。' : '不要修改任何文件。') +
      (this.tool === 'codex' && !perm.allowShell
        ? (perm.allowWrites ? '可执行当前任务所需的命令，写入仅限当前工作目录。' : '可以执行读取文件与检索所需的只读命令，不得运行构建、测试或修改文件的命令。')
        : (perm.allowShell ? '可执行只读命令。' : '不要执行 shell 命令。')) +
      ' 请直接给出结论，保持简洁。');
    return parts.join('\n');
  }

  _fail(task, error, meta) {
    this.emitLog && this.emitLog('error', '真实执行', (BIN[this.tool] || {}).label + ' 失败：' + error);
    return { ok: false, output: '', error: error, meta: Object.assign({ tool: this.tool, status: 'failed' }, meta || {}) };
  }

  send() { return { ok: true, note: 'control messages are handled by the coordinator' }; }

  teardown() {
    this.children.forEach(c => { try { c.kill(); } catch (_) { /* noop */ } });
    this.children.clear();
  }
}

function clamp01(v) { return Math.max(0, Math.min(1, v)); }

module.exports = {
  buildEnv,
  RealHarnessAdapter,
  PERMISSIONS,
  resolveBin,
  BIN,
  status: function () {
    const out = {};
    Object.keys(BIN).forEach(t => { out[t] = { available: !!resolveBin(t), bin: resolveBin(t), label: BIN[t].label }; });
    return { tools: out, permissions: Object.keys(PERMISSIONS).map(k => PERMISSIONS[k]) };
  }
};
