#!/usr/bin/env node
// Parity harness for the Rust migration.
//
// Sends the same request to the Go sidecar and to the Rust front door, then
// compares status + JSON. Every ported command must pass this before its Go
// handler can be removed.
//
//   node scripts/parity.mjs --command list_model_providers --go 8899 --rust 8900
//   node scripts/parity.mjs --command codepluse_provider_save --body '{"name":"x",...}'
//   node scripts/parity.mjs --path /api/health --method GET
//
// Exit code 0 = all checks matched, 1 = a difference, 2 = usage/runtime error.

const args = process.argv.slice(2);
const opt = {
  command: "",
  path: "",
  method: "POST",
  body: "{}",
  go: "8840",
  rust: "8841",
  normalizeTime: true,
  normalizeIds: true,
  quiet: false,
};

for (let i = 0; i < args.length; i += 1) {
  const key = args[i];
  const next = () => {
    const value = args[i + 1];
    if (value === undefined) fail(`缺少 ${key} 的参数值`);
    i += 1;
    return value;
  };
  switch (key) {
    case "--command":
      opt.command = next();
      break;
    case "--path":
      opt.path = next();
      break;
    case "--method":
      opt.method = next().toUpperCase();
      break;
    case "--body":
      opt.body = next();
      break;
    case "--go":
      opt.go = next();
      break;
    case "--rust":
      opt.rust = next();
      break;
    case "--no-normalize-time":
      opt.normalizeTime = false;
      break;
    case "--no-normalize-ids":
      opt.normalizeIds = false;
      break;
    case "--quiet":
      opt.quiet = true;
      break;
    case "--help":
    case "-h":
      usage();
      break;
    default:
      fail(`未知参数 ${key}`);
  }
}

function usage() {
  console.log(`用法：
  node scripts/parity.mjs --command <名称> [选项]

选项：
  --path <路径>          直接指定路径（默认 /api/<command>）
  --method <GET|POST>    默认 POST
  --body <json>          请求体，默认 {}
  --go <端口>            Go 侧端口，默认 8840
  --rust <端口>          Rust 前门端口，默认 8841
  --no-normalize-time    不把 created_at/updated_at 归一为 "T"
  --no-normalize-ids     不把 24 位十六进制 id 归一为 "ID"
  --quiet                只输出结论`);
  process.exit(0);
}

function fail(message) {
  console.error(`parity: ${message}`);
  process.exit(2);
}

const target = opt.path || (opt.command ? `/api/${opt.command}` : "");
if (!target) fail("需要 --command 或 --path");
if (opt.method !== "GET" && opt.method !== "POST") fail("--method 只支持 GET/POST");

function canonical(value, seen = new Set()) {
  if (Array.isArray(value)) return value.map((item) => canonical(item, seen));
  if (value && typeof value === "object") {
    if (seen.has(value)) return "[circular]";
    seen.add(value);
    const out = {};
    for (const key of Object.keys(value).sort()) out[key] = canonical(value[key], seen);
    seen.delete(value);
    return out;
  }
  return value;
}

function normalize(value) {
  const walk = (node) => {
    if (Array.isArray(node)) return node.forEach(walk);
    if (!node || typeof node !== "object") return;
    for (const [key, child] of Object.entries(node)) {
      if (opt.normalizeTime && (key === "created_at" || key === "updated_at") && typeof child === "string") {
        node[key] = "T";
      }
      walk(child);
    }
  };
  walk(value);
  let text = JSON.stringify(canonical(value));
  if (opt.normalizeIds) text = text.replace(/[0-9a-f]{24}/g, "ID");
  return text;
}

/** First differing path, so a mismatch is actionable instead of a wall of JSON. */
function firstDifference(a, b, path = "$") {
  if (typeof a !== typeof b) return `${path}: 类型不同 (${typeof a} vs ${typeof b})`;
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return `${path}: 长度不同 (${a.length} vs ${b.length})`;
    for (let i = 0; i < a.length; i += 1) {
      const found = firstDifference(a[i], b[i], `${path}[${i}]`);
      if (found) return found;
    }
    return null;
  }
  if (a && b && typeof a === "object") {
    const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
    for (const key of [...keys].sort()) {
      if (!(key in a)) return `${path}.${key}: 仅 Rust 有`;
      if (!(key in b)) return `${path}.${key}: 仅 Go 有`;
      const found = firstDifference(a[key], b[key], `${path}.${key}`);
      if (found) return found;
    }
    return null;
  }
  if (a !== b) return `${path}: ${JSON.stringify(a)} ≠ ${JSON.stringify(b)}`;
  return null;
}

async function call(port) {
  const init = { method: opt.method, headers: {} };
  if (opt.method === "POST") {
    init.headers["Content-Type"] = "application/json";
    init.body = opt.body;
  }
  try {
    const response = await fetch(`http://127.0.0.1:${port}${target}`, init);
    const text = await response.text();
    let json;
    try {
      json = JSON.parse(text);
    } catch {
      json = undefined;
    }
    return { status: response.status, text, json };
  } catch (error) {
    fail(`请求 127.0.0.1:${port}${target} 失败：${error.message}`);
  }
}

function report(name, ok, detail) {
  if (ok) {
    if (!opt.quiet) console.log(`✅ ${name}`);
    return true;
  }
  console.log(`❌ ${name}${detail ? `\n   ${detail}` : ""}`);
  return false;
}

let failed = 0;
const go = await call(opt.go);
const rust = await call(opt.rust);

const label = `${opt.method} ${target}`;
if (!report(`${label} 状态码 (${go.status} / ${rust.status})`, go.status === rust.status)) {
  failed += 1;
}

if (go.json !== undefined && rust.json !== undefined) {
  const diff = firstDifference(canonical(go.json), canonical(rust.json));
  if (!report(`${label} 响应 JSON`, diff === null, diff ?? undefined)) failed += 1;
} else if (!report(`${label} 响应非 JSON`, go.text === rust.text, `${go.text.slice(0, 120)} vs ${rust.text.slice(0, 120)}`)) {
  failed += 1;
}

if (failed > 0) {
  console.log(`\n结论：${failed} 项不一致`);
  process.exit(1);
}
console.log(`\n结论：全部一致`);
process.exit(0);
