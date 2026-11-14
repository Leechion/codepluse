import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const root = path.resolve(import.meta.dirname, "..");
const uiRoot = path.join(root, "codeg-ui", "src");
const backendRoot = path.join(root, "src-tauri", "src");

function walk(dir, accept) {
  const files = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) files.push(...walk(full, accept));
    else if (accept(full)) files.push(full);
  }
  return files;
}

const frontendFiles = walk(
  uiRoot,
  (file) => /\.(?:ts|tsx|js|jsx)$/.test(file) && !/\.test\./.test(file),
);
const backendFiles = walk(backendRoot, (file) => file.endsWith(".rs"));
const calls = new Map();
// API wrappers commonly use `.call<ResultType>(...)`; match both generic and
// non-generic forms, including multi-line calls, so the audit cannot silently
// skip whole feature groups such as backup/restore.
const callPattern =
  /(?:getTransport\(\)|transport)\.call(?:<[^;{}()]*?>)?\(\s*["']([a-zA-Z0-9_|-]+)/g;

for (const file of frontendFiles) {
  const source = fs.readFileSync(file, "utf8");
  for (const match of source.matchAll(callPattern)) {
    const command = match[1];
    const locations = calls.get(command) ?? [];
    locations.push(path.relative(root, file));
    calls.set(command, locations);
  }
}

const owners = new Set();
for (const file of backendFiles) {
  const source = fs.readFileSync(file, "utf8");
  // Every HTTP module exposes its command list through `owns()`. Looking only
  // for a literal anywhere in Rust caused false negatives when the same string
  // appeared only as a persisted setting key or in a comment (for example
  // `get_main_bot_settings`). Scope this check to the dispatch declarations.
  const ownsPattern = /pub fn owns\(command: &str\)\s*->\s*bool\s*\{([\s\S]*?)\n\}/g;
  for (const match of source.matchAll(ownsPattern)) {
    for (const commandMatch of match[1].matchAll(/"([a-zA-Z0-9_|-]+)"/g)) {
      owners.add(commandMatch[1]);
    }
  }
}
// A few legacy handlers are routed by explicit branches in api_dispatch and
// expose their own match table instead of a module-level owns() function.
for (const command of [
  "git_commit",
  "list_model_providers",
  "create_model_provider",
  "update_model_provider",
  "delete_model_provider",
  "codepluse_provider_save",
]) {
  owners.add(command);
}
const missing = [...calls]
  .filter(
    ([command]) =>
      !owners.has(command) &&
      // `/api/health` is a local non-command route rather than an `owns()` arm.
      command !== "health" &&
      !command.startsWith("automation_") &&
      !command.startsWith("codepluse_channel_"),
  )
  .sort(([a], [b]) => a.localeCompare(b));

console.log(
  `Frontend literal calls: ${calls.size}; calls without a Rust command owner: ${missing.length}`,
);
for (const [command, locations] of missing) {
  console.log(`${command}\t${[...new Set(locations)].join(", ")}`);
}

if (process.argv.includes("--check") && missing.length > 0) {
  process.exitCode = 1;
}
