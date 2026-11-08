// Complete the vendored mirror additively from the read-only reference repo.
// For each mapped (source, dest) pair: copy every file whose mirror counterpart
// is missing. Existing mirror files are NEVER overwritten (they carry the
// alias-rewritten import specifiers).
// Run: node scripts/zcode-sync-from-reference.mjs
import fs from "node:fs";
import path from "node:path";

const CWD = process.cwd();
const REF = process.env.ZCODE_REFERENCE || "E:/CODE/zcode-reference/packages";

const PAIRS = [
  ["ui/src", "src/zcode"],
  ["provider/src", "src/zcode/provider"],
  ["services/src", "src/zcode/services"],
  ["rpc/src", "src/zcode/rpc"],
  ["model-option-map/src", "src/zcode/model-option-map"],
];

const SKIP_DIRS = new Set(["node_modules", "__pycache__"]);
const SKIP_FILES = new Set([".DS_Store"]);

let copied = 0;
let skippedExisting = 0;

function walkCopy(srcDir, destDir) {
  for (const entry of fs.readdirSync(srcDir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (SKIP_DIRS.has(entry.name)) continue;
      walkCopy(path.join(srcDir, entry.name), path.join(destDir, entry.name));
      continue;
    }
    if (!entry.isFile() || SKIP_FILES.has(entry.name)) continue;
    const target = path.join(destDir, entry.name);
    if (fs.existsSync(target)) {
      skippedExisting++;
      continue;
    }
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.copyFileSync(path.join(srcDir, entry.name), target);
    copied++;
  }
}

for (const [relSrc, relDest] of PAIRS) {
  const srcDir = path.join(REF, relSrc);
  const destDir = path.join(CWD, relDest);
  if (!fs.existsSync(srcDir)) {
    console.error("missing reference dir:", srcDir);
    continue;
  }
  const before = copied;
  walkCopy(srcDir, destDir);
  console.log(`${relSrc} -> ${relDest}: copied ${copied - before}`);
}

console.log(`total copied: ${copied}, already present: ${skippedExisting}`);
