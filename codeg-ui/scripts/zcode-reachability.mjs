// Compute the import closure reachable from the ZCode chat page within
// src/zcode, and delete everything else. Vendored dead weight (files with
// unresolved imports, upstream test files, desktop tail) never reaches the
// bundle — pruning keeps the tree maintainable. The pristine source remains
// re-copyable from E:\CODE\zcode-reference.
import fs from "node:fs";
import path from "node:path";

const APPLY = process.argv.includes("--apply");
const ROOT = path.resolve(process.cwd(), "src/zcode");
const ENTRY = path.resolve(process.cwd(), "src/zcode/pages/ZCodeChatPage.tsx");
const CODE_EXTS = [".ts", ".tsx", "/index.ts", "/index.tsx", ".js", ".mjs", ".css", ".json"];
const ASSET_EXTS = [".svg", ".png", ".jpg", ".wasm"];

function resolveFrom(importerDir, spec) {
  // "@/zcode/X" → the vendored mirror root; other "@/X" → codeg-ui's own src.
  if (spec.startsWith("@/zcode/")) {
    const base = path.join(ROOT, spec.slice("@/zcode/".length));
    const candidates = [base, ...CODE_EXTS.map((ext) => base + ext)];
    for (const candidate of candidates) {
      if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) return candidate;
    }
    if (fs.existsSync(base) && fs.statSync(base).isDirectory()) {
      for (const ext of ["/index.ts", "/index.tsx"]) {
        if (fs.existsSync(base + ext)) return base + ext;
      }
    }
    return null;
  }
  if (spec.startsWith("@/")) {
    const base = path.resolve(process.cwd(), "src", spec.slice(2));
    for (const ext of [".ts", ".tsx", "/index.ts", "/index.tsx"]) {
      if (fs.existsSync(base + ext) && fs.statSync(base + ext).isFile()) return base + ext;
    }
    if (fs.existsSync(base) && fs.statSync(base).isDirectory() && fs.existsSync(base + "/index.ts")) {
      return base + "/index.ts";
    }
    return null;
  }
  // @zcode aliases map into the vendored shared tree.
  if (spec.startsWith("@zcode/shared")) {
    const sub = spec.slice("@zcode/shared".length).replace(/^\//, "");
    const candidates = sub
      ? [path.join(ROOT, "shared", sub), path.join(ROOT, "shared", sub + ".ts")]
      : [path.join(ROOT, "shared", "index.ts")];
    for (const c of candidates) if (fs.existsSync(c) && fs.statSync(c).isFile()) return c;
    return null;
  }
  if (!spec.startsWith(".") && !spec.startsWith("#")) return null; // npm package
  const base = path.resolve(importerDir, spec);
  const candidates = [base, ...CODE_EXTS.map((ext) => base + ext)];
  if (spec.startsWith("#")) {
    // #src/ maps to the shared package in ZCode; vendored copy is flat.
    const mapped = path.join(ROOT, "shared", spec.replace("#src/", ""));
    candidates.push(mapped, mapped + ".ts", path.join(mapped, "index.ts"));
  }
  for (const candidate of candidates) {
    if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) return candidate;
  }
  if (fs.existsSync(base) && fs.statSync(base).isDirectory()) {
    for (const ext of ["/index.ts", "/index.tsx"]) {
      if (fs.existsSync(base + ext)) return base + ext;
    }
  }
  return null;
}

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, out);
    else out.push(full);
  }
  return out;
}

const IMPORT_RE = /(?:import|export)[^"'`;]*["']([^"']+)["']|import\(\s*["']([^"']+)["']\s*\)|require\(\s*["']([^"']+)["']\s*\)/g;

function importsOf(file) {
  const source = fs.readFileSync(file, "utf8");
  const specs = [];
  for (const m of source.matchAll(IMPORT_RE)) {
    specs.push(m[1] ?? m[2] ?? m[3]);
  }
  return specs;
}

const reachable = new Set();
const queue = [ENTRY];
if (!fs.existsSync(ENTRY)) {
  console.error("entry missing:", ENTRY);
  process.exit(1);
}
while (queue.length > 0) {
  const file = queue.pop();
  if (reachable.has(file)) continue;
  reachable.add(file);
  const dir = path.dirname(file);
  for (const spec of importsOf(file)) {
    const resolved = resolveFrom(dir, spec);
    if (!resolved) {
      if (!/^(react|react-dom|next|zustand|zod|ai|streamdown|@streamdown|shiki|motion|radix-ui|lucide|clsx|virtua|use-stick-to-bottom|date-fns|@tanstack|lexical|@lexical|@pierre|nanoid|@dnd-kit|@modelcontextprotocol|@rive-app|react-jsx-parser|@img|@juggle|highlight\.js|next-themes|swr)/.test(spec)) {
        console.error("UNRESOLVED (reachable):", file.replace(ROOT, "zcode"), "->", spec);
      }
      continue;
    }
    if (!reachable.has(resolved)) queue.push(resolved);
  }
}

// Same-directory siblings are NOT auto-reachable; every file outside the
// closure goes away. Non-code assets referenced via resolved imports are
// included by resolveFrom already (verbatim paths exist on disk).
const all = walk(ROOT);
const removed = [];
let removedBytes = { count: 0, bytes: 0 };
for (const file of all) {
  if (reachable.has(file)) continue;
  if (/\.test\.tsx?$/.test(file) || /\.test\.js$/.test(file)) {
    reachable.delete(file); // tests never bundle
  }
  removed.push(file);
  const stat = fs.statSync(file);
  removedBytes.count++;
  removedBytes.bytes += stat.size;
}
if (!APPLY) {
  console.log("DRY RUN — re-run with --apply to delete.");
  console.log("would remove:", removedBytes.count, "files (" + (removedBytes.bytes / 1048576).toFixed(1) + " MB)");
  console.log("would keep:", reachable.size);
  process.exit(0);
}
for (const file of removed) {
  fs.rmSync(file, { force: true });
}
// Prune now-empty directories.
function pruneEmpty(dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      pruneEmpty(full);
      if (fs.readdirSync(full).length === 0) fs.rmdirSync(full);
    }
  }
}
pruneEmpty(ROOT);

console.log("reachable files:", reachable.size);
console.log("removed files:", removedBytes.count, "(" + (removedBytes.bytes / 1048576).toFixed(1) + " MB)");
const after = walk(ROOT).length;
console.log("remaining files:", after);
