// Vendor import closure audit: BFS from entry files over resolvable imports,
// then report missing mirror files / missing npm packages / broken aliases
// WITHIN that closure. Read-only.
// Run: node scripts/zcode-import-audit.mjs entry1.tsx entry2.ts ...
import fs from "node:fs";
import path from "node:path";

const CWD = process.cwd();
const ROOT = path.join(CWD, "src/zcode");
const REFERENCE = process.env.ZCODE_REFERENCE || "E:/CODE/zcode-reference/packages/ui/src";
const ENTRIES = process.argv.slice(2).map((p) => path.resolve(CWD, p));

const CODE_EXTS = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".css", ".json", ".svg", ".png", ".wasm"];

function stat(p) {
  try {
    return fs.statSync(p);
  } catch {
    return null;
  }
}
const isFile = (p) => stat(p)?.isFile() ?? false;
const isDir = (p) => stat(p)?.isDirectory() ?? false;

function resolveFromRoot(root, spec) {
  if (/\.[a-z0-9]+$/i.test(spec) && isFile(path.join(root, spec))) return path.join(root, spec);
  const tries = [spec];
  if (spec.endsWith(".js")) tries.push(spec.slice(0, -3));
  if (spec.endsWith(".mjs")) tries.push(spec.slice(0, -4));
  for (const base0 of tries) {
    const base = path.join(root, base0);
    for (const ext of CODE_EXTS) if (isFile(base + ext)) return base + ext;
    if (isDir(base)) {
      for (const ext of ["/index.ts", "/index.tsx"]) if (isFile(base + ext)) return base + ext;
    }
  }
  return null;
}

// One-line-ish import scanner: handles single-line and multi-line from-imports.
function parseImports(source) {
  const specs = new Set();
  const fromRe = /(?:^|[\s;}])(?:import|export)\s+(?:type\s+)?[\s\S]*?from\s*["']([^"']+)["']/g;
  let m;
  while ((m = fromRe.exec(source))) specs.add(m[1]);
  const dynRe = /import\s*\(\s*["']([^"']+)["']\s*\)/g;
  while ((m = dynRe.exec(source))) specs.add(m[1]);
  const sideRe = /^\s*import\s*["']([^"']+)["']\s*;?\s*$/gm;
  while ((m = sideRe.exec(source))) specs.add(m[1]);
  return [...specs];
}

const visited = new Set();
const queue = [...ENTRIES];
const missingMirror = new Map();
const missingAlias = new Map();
const missingPackages = new Map();

function addMissing(map, key, file) {
  if (!map.has(key)) map.set(key, new Set());
  map.get(key).add(path.relative(CWD, file).replaceAll("\\", "/"));
}

while (queue.length) {
  const file = queue.pop();
  const key = path.relative(CWD, file).replaceAll("\\", "/");
  if (visited.has(key)) continue;
  visited.add(key);
  if (!isFile(file)) continue;
  const source = fs.readFileSync(file, "utf8");
  for (const spec of parseImports(source)) {
    if (spec.startsWith("@zcode/")) {
      const mapped =
        spec === "@zcode/shared"
          ? "shared/index.ts"
          : spec === "@zcode/services"
            ? "services/index.ts"
            : spec === "@zcode/provider"
              ? "provider/index.ts"
              : spec === "@zcode/rpc"
                ? "rpc/index.ts"
                : spec.startsWith("@zcode/shared/")
                  ? "shared/" + spec.slice("@zcode/shared/".length)
                  : null;
      if (!mapped) {
        addMissing(missingAlias, spec, file);
        continue;
      }
      const resolved = resolveFromRoot(ROOT, mapped);
      if (!resolved) addMissing(missingAlias, `${spec} -> src/zcode/${mapped}`, file);
      else queue.push(resolved);
      continue;
    }
    if (spec.startsWith("@/")) {
      const local = path.join(CWD, "src", spec.slice(2));
      const resolved = resolveFromRoot(path.join(CWD, "src"), spec.slice(2));
      if (resolved) queue.push(resolved);
      else if (spec.slice(2).startsWith("zcode/")) {
        const ref = resolveFromRoot(REFERENCE, spec.slice("zcode/".length));
        if (ref) addMissing(missingMirror, path.relative(ROOT, ref).replaceAll("\\", "/"), file);
        else addMissing(missingMirror, spec, file);
      } else addMissing(missingMirror, spec + " (src/)", file);
      void local;
      continue;
    }
    if (spec.startsWith(".")) {
      const fromDir = path.dirname(file);
      const abs = path.resolve(fromDir, spec);
      if (abs.startsWith(ROOT) || abs.includes("src" + path.sep + "zcode")) {
        const resolved = resolveFromRoot(path.dirname(abs), path.basename(abs));
        if (resolved) queue.push(resolved);
        else {
          const ref = resolveFromRoot(REFERENCE, path.relative(ROOT, abs).replaceAll("\\", "/"));
          if (ref) addMissing(missingMirror, path.relative(ROOT, ref).replaceAll("\\", "/"), file);
          else addMissing(missingMirror, path.relative(CWD, abs).replaceAll("\\", "/"), file);
        }
      } else {
        const relRoot = path.join(CWD, "src");
        const rel = path.relative(relRoot, abs);
        const resolved = resolveFromRoot(path.dirname(path.join(relRoot, rel)), path.basename(rel));
        if (resolved) queue.push(resolved);
        else addMissing(missingMirror, "../" + path.relative(ROOT, abs).replaceAll("\\", "/"), file);
      }
      continue;
    }
    const parts = spec.startsWith("@")
      ? spec.split("/").slice(0, 2).join("/")
      : spec.split("/")[0];
    if (!isDir(path.join(CWD, "node_modules", parts))) addMissing(missingPackages, parts, file);
  }
}

console.log("closure size:", visited.size);
console.log("\n== missing mirror files ==", missingMirror.size);
for (const [k, v] of [...missingMirror.entries()].sort()) {
  console.log("  " + k);
  for (const f of v) console.log("      <- " + f);
}
console.log("\n== broken @zcode aliases ==", missingAlias.size);
for (const [k, v] of [...missingAlias.entries()].sort()) {
  console.log("  " + k);
  for (const f of v) console.log("      <- " + f);
}
console.log("\n== missing npm packages in closure ==", missingPackages.size);
for (const [k, v] of [...missingPackages.entries()].sort()) {
  console.log("  " + k + "  (" + v.size + " refs)");
}
