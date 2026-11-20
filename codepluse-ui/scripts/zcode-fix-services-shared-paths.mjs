// Fix-up: services files whose "#src/X" imports were mis-rewritten to
// "../shared/X" style relative paths (the services package maps #src to its
// OWN src). Re-point them at src/zcode/services/X.
import fs from "node:fs";
import path from "node:path";

const SERVICES = path.resolve(process.cwd(), "src/zcode/services");

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, out);
    else if (/\.tsx?$/.test(entry.name)) out.push(full);
  }
  return out;
}

function resolveServiceFile(rel) {
  const base = path.join(SERVICES, rel);
  for (const ext of [".ts", ".tsx", "/index.ts", "/index.tsx"]) {
    if (fs.existsSync(base + ext) && fs.statSync(base + ext).isFile()) return base + ext;
  }
  return null;
}

let fixed = 0;
const missing = [];
for (const file of walk(SERVICES)) {
  const source = fs.readFileSync(file, "utf8");
  let changed = 0;
  const next = source.replace(
    /(from\s*|import\s*\(\s*)(["'])(\.{1,2}\/)+shared\/([^"']*)\2/g,
    (full, lead, quote, slashes, rest) => {
      const resolved = resolveServiceFile(rest);
      if (!resolved) {
        missing.push(rest);
        return full;
      }
      const rel = path.relative(path.dirname(file), resolved).replaceAll("\\", "/");
      const importPath = rel.startsWith(".") ? rel : "./" + rel;
      changed++;
      return lead + '"' + importPath.replace(/(\.ts|\.tsx)$/, "").replace(/\/index$/, "") + '"';
    },
  );
  if (changed > 0) {
    fs.writeFileSync(file, next);
    fixed += changed;
  }
}
console.log("services imports fixed:", fixed);
if (missing.length > 0) {
  console.log("unresolved:");
  for (const m of [...new Set(missing)]) console.log("  ", m);
}
