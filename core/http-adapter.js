'use strict';

const Core = require('./engine');

// A bridge implements POST /messages -> { ok: true, output?: string }.
// It receives the canonical envelope, not an API or vendor specific prompt.
class HttpHarnessAdapter extends Core.HarnessAdapter {
  constructor(spec) {
    super(spec);
    const endpoint = new URL(spec.endpoint);
    if (!['http:', 'https:'].includes(endpoint.protocol) || endpoint.username || endpoint.password) {
      throw new Error('Harness endpoint must be an HTTP(S) URL without embedded credentials');
    }
    this.endpoint = endpoint.href.replace(/\/$/, '');
    this.token = spec.token || '';
    this.timeoutMs = Math.max(1000, Math.min(600000, Number(spec.timeoutMs) || 180000));
    this.running = 0;
    this.maxConcurrent = Math.max(1, Number(spec.maxConcurrency) || 2);
  }

  heartbeat() { return { ok: true, load: this.running / this.maxConcurrent }; }

  async send(message) {
    if (this.running >= this.maxConcurrent) throw new Error('Harness 通信并发已满');
    this.running++;
    try {
      const response = await fetch(this.endpoint + '/messages', {
        method: 'POST', redirect: 'error',
        headers: Object.assign({ 'Content-Type': 'application/json' },
          this.token ? { Authorization: 'Bearer ' + this.token } : {}),
        body: JSON.stringify(message), signal: AbortSignal.timeout(this.timeoutMs)
      });
      if (!response.ok) throw new Error('Harness HTTP ' + response.status);
      // Bound the response while reading, including servers without Content-Length.
      const reader = response.body.getReader();
      const chunks = []; let size = 0;
      try {
        while (true) {
          const { done, value } = await reader.read();
          if (done) break;
          size += value.length;
          if (size > 1024 * 1024) throw new Error('Harness 回复超过 1 MiB');
          chunks.push(Buffer.from(value));
        }
      } finally { await reader.cancel(); }
      const result = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      if (!result || result.ok !== true) throw new Error('Harness 未确认接收消息');
      if (result.output != null && typeof result.output !== 'string') throw new Error('Harness output 必须为字符串');
      return { ok: true, output: (result.output || '').slice(0, 20000) };
    } finally { this.running--; }
  }
}

module.exports = HttpHarnessAdapter;
