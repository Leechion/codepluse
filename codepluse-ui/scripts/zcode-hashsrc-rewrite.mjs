// Fix-up pass v3 for the `#src/*` rewrite series:
// For every RELATIVE import that fails to resolve file-relatively, re-anchor
// it at the package root (the original `#src` semantics) and emit an up-path
// from the file. Candidate "rest" forms are tried in order:
//   1. the specifier without "./" (root + full rest);
//   2. rest with the file's own root-relative dir prefix stripped;
//   3. rest with only its first segment stripped.
// Idempotent: resolvable imports are left untouched.
import fs from "node:fs";
import path from "node:path";

const ROOTS = [
  path.resolve(process.cwd(), "src/zcode/services"),
  path.resolve(process.cwd(), "src/zcode/shared"),
];

function resolveTs(base) {
  for (const ext of [".ts", ".tsx", ".js", ".jsx", "/index.ts", "/index.tsx"]) {
    const p = base + ext;
    if (fs.existsSync(p) && fs.statSync(p).isFile()) return p;
  }
  return null;
}

const RE = /(from\s*|import\s*\(\s*|import\s*)(["'])(\.\.?\/[^"']+)\2/g;

let touched = 0;
let rewritten = 0;

for (const ROOT of ROOTS) {
  const files = [];
  (function walk(d) {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const f = path.join(d, e.name);
      if (e.isDirectory()) walk(f);
      else if (/\.(ts|tsx)$/.test(e.name)) files.push(f);
    }
  })(ROOT);
  for (const file of files) {
    const src = fs.readFileSync(file, "utf8");
    const fileDir = path.dirname(file);
    const rel = path.relative(ROOT, fileDir).split(path.sep).join("/");
    const relDepth = rel ? rel.split("/").length : 0;
    let next = src;
    next = next.replace(RE, (m, lead, q, spec) => {
      if (resolveTs(path.resolve(fileDir, spec))) return m; // fine as-is
      if (spec.startsWith("../")) return m; // cross-package relative: not ours
      if (relDepth === 0) return m; // depth 0: root == file dir, would already resolve
      let rest = spec.replace(/^\.\//, "");
      const relPrefix = rel + "/";
      let target = null;
      for (const candidate of [
        rest,
        rest.startsWith(relPrefix) ? rest.slice(relPrefix.length) : null,
        rest.includes("/") ? rest.slice(rest.indexOf("/") + 1) : null,
      ]) {
        if (!candidate) continue;
        const resolved = resolveTs(path.resolve(ROOT, candidate));
        if (resolved) {
          target = candidate;
          break;
        }
      }
      if (!target) return m;
      rewritten++;
      const up = "../".repeat(relDepth);
      return lead + q + up + target + q;
    });
    if (next !== src) {
      fs.writeFileSync(file, next);
      touched++;
    }
  }
}
console.log("touched:", touched, "rewritten:", rewritten);
