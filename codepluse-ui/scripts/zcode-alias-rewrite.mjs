// Vendor helper: rewrite "@/..." and out-of-tree relative imports inside the
// vendored ZCode tree to relative paths into src/zcode (a mirror of ZCode's
// packages/ui/src). Run AFTER re-copying; idempotent.
import fs from "node:fs";
import path from "node:path";

const ROOT = path.resolve(process.cwd(), "src/zcode");
const CODE_EXTS = [".ts", ".tsx", "/index.ts", "/index.tsx"];

function resolveMirrored(spec) {
  if (/\.(svg|png|jpg|jpeg|css|json|md|wasm)$/.test(spec)) {
    const raw = path.join(ROOT, spec);
    return fs.existsSync(raw) ? raw : null;
  }
  const tries = [spec];
  if (spec.endsWith(".js")) tries.push(spec.slice(0, -3));
  for (const candidate0 of tries) {
    const base = path.join(ROOT, candidate0);
    for (const ext of CODE_EXTS) {
      if (fs.existsSync(base + ext) && fs.statSync(base + ext).isFile()) return base + ext;
    }
    if (fs.existsSync(base) && fs.statSync(base).isDirectory()) {
      for (const ext of ["/index.ts", "/index.tsx"]) {
        if (fs.existsSync(base + ext)) return base + ext;
      }
    }
  }
  return null;
}

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, out);
    else if (/\.tsx?$/.test(entry.name)) out.push(full);
  }
  return out;
}

const IMPORT_RE = /(from\s*|import\s*\(\s*|import\s+)(["'])(@\/[^"']*|#src\/[^"']*|\.[^"']*)\2/g;

function rewriteFile(file) {
  const source = fs.readFileSync(file, "utf8");
  const orig = path.relative(ROOT, file).replaceAll("\\", "/");
  let changed = 0;
  const missing = [];

  const next = source.replace(IMPORT_RE, (full, lead, quote, spec) => {
    let origTarget;
    if (spec.startsWith("#src/")) {
      // ZCode package.json "imports": #src/* resolves against the importer's
      // OWN package root (services files → services/src, shared files → shared/src).
      const packageRoot = orig.startsWith("services/") ? "services" : "shared";
      origTarget = packageRoot + "/" + spec.slice("#src/".length);
    } else if (spec.startsWith("@/")) {
      origTarget = spec.slice(2);
    } else {
      origTarget = path.posix.normalize(path.posix.join(path.posix.dirname(orig), spec));
    }
    const resolved = resolveMirrored(origTarget);
    if (!resolved) {
      missing.push(origTarget);
      return full;
    }
    const rel = path.relative(path.dirname(file), resolved).replaceAll("\\", "/");
    const importPath = rel.startsWith(".") ? rel : "./" + rel;
    const withoutExt = importPath.replace(/(\.ts|\.tsx)$/, "").replace(/\/index$/, "");
    changed++;
    return lead + '"' + withoutExt + '"';
  });

  if (changed > 0) fs.writeFileSync(file, next);
  return { changed, missing };
}

const files = walk(ROOT);
let totalChanged = 0;
const missing = new Map();
for (const file of files) {
  const { changed, missing: fileMissing } = rewriteFile(file);
  totalChanged += changed;
  for (const m of fileMissing) missing.set(m, (missing.get(m) ?? 0) + 1);
}
console.log("files touched:", files.length, "| imports rewritten:", totalChanged);
for (const [spec, count] of [...missing.entries()].sort((a, b) => b[1] - a[1]).slice(0, 30)) {
  console.log(String(count).padStart(4), spec);
}
console.log("missing unique:", missing.size);
