#!/usr/bin/env node
import fs from "node:fs"
import path from "node:path"

const repo = "E:/code/codepluse"
const ui = path.join(repo, "codeg-ui")
const zc = path.join(ui, "src/zcode")

function list(dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name)
    if (e.isDirectory()) list(p, out)
    else if (/\.(ts|tsx)$/.test(e.name) && !/\.d\.ts$/.test(e.name)) out.push(p)
  }
  return out
}

const files = list(zc).sort()
const rel = (p) => path.relative(ui, p).replaceAll("\\", "/")
const relZc = (p) => path.relative(zc, p).replaceAll("\\", "/")
const norm = (p) => rel(p).replaceAll("\\", "/")

const importRx =
  /(?:\bfrom\s*|\bimport\s*\(\s*|\brequire\s*\(\s*)['"]([^'"]+)['"]|(?:\bimport\s*|\bexport\s*)(?:[\s\S]*?\bfrom\s*)?['"]([^'"]+\.(?:css|scss|svg|png|webp|js|json))['"]|(?:\bimport\s*(?![^;]*from)[^;]*?\b['"])([^'"]+)['"]/g;

function localSpecs(text) {
  const out = [];
  for (const m of text.matchAll(importRx)) {
    const s = m[1] || m[2] || m[3];
    if (!s || s.startsWith("\0")) continue;
    if (s.startsWith("@/zcode/") || s.startsWith("@zcode/") || s.startsWith("./") || s.startsWith("../")) out.push(s);
  }
  return out;
}

function resolve(fromFile, spec) {
  let base;
  if (spec.startsWith("@/zcode/")) base = path.join(zc, spec.slice(8));
  else if (spec.startsWith("@zcode/")) {
    const rest = spec.slice(8);
    const entries = [
      ["shared", "shared/index.ts"],
      ["services", "services/index.ts"],
      ["provider", "provider/index.ts"],
      ["rpc", "rpc/index.ts"],
      ["model-option-map", "model-option-map/index.ts"],
      ["zcode-cua", "zcode-cua/index.d.ts"],
    ];
    const hit = entries.find(([pref]) => rest === pref || rest.startsWith(pref + "/"));
    if (!hit) return null;
    base = hit[0] === rest ? path.join(zc, hit[1]) : path.join(zc, rest);
  } else base = path.resolve(path.dirname(fromFile), spec);
  const candidates = [
    base,
    base + ".ts",
    base + ".tsx",
    base + ".js",
    base + ".jsx",
    path.join(base, "index.ts"),
    path.join(base, "index.tsx"),
    path.join(base, "index.js"),
  ];
  for (const c of candidates) {
    try {
      if (fs.statSync(c).isFile()) return c;
    } catch {}
  }
  return null;
}

const graph = new Map();
for (const f of files) graph.set(f, new Set());
for (const f of files) {
  const text = fs.readFileSync(f, "utf8");
  for (const s of localSpecs(text)) {
    const to = resolve(f, s);
    if (to) graph.get(f).add(to);
  }
}

const tests = files.filter((f) => /\.test\.tsx?$/.test(f));
const roots = ["src/app/page.tsx", "src/app/layout.tsx", "src/app/workspace/page.tsx", "src/app/gallery/page.tsx", "src/app/first-run/page.tsx"].map((r) => path.join(ui, r));
for (const r of roots) if (!fs.existsSync(r)) console.error("missing root " + r);

function reach(seeds) {
  const seen = new Set();
  const stack = seeds.filter((s) => fs.existsSync(s));
  while (stack.length) {
    const u = stack.pop();
    if (seen.has(u)) continue;
    seen.add(u);
    for (const v of graph.get(u) || []) stack.push(v);
    const abs = graph.get(u) || [];
    for (const v of abs) stack.push(v);
  }
  return seen;
}

const mainReach = new Set();
const outside = new Map();
const outsideSeeds = [];
for (const f of ["src/app/page.tsx", "src/app/layout.tsx", "src/app/workspace/page.tsx", "src/app/gallery/page.tsx", "src/app/first-run/page.tsx"].map((r) => path.join(ui, r))) {
  if (!fs.existsSync(f)) continue;
  const text = fs.readFileSync(f, "utf8");
  for (const m of text.matchAll(/from\s*['"]([^'"]+)['"]/g)) {
    const s = m[1];
    if (s.startsWith("@/zcode/")) {
      const to = resolve(f, s);
      if (to) outsideSeeds.push(to);
    }
  }
}
// transitive all imports (including cross-group)
const stack = [...outsideSeeds];
while (stack.length) {
  const u = stack.pop();
  if (mainReach.has(u)) continue;
  mainReach.add(u);
  for (const v of graph.get(u) || []) stack.push(v);
}

// test-only reach
const allTestReach = new Set();
for (const f of tests) {
  const text = fs.readFileSync(f, "utf8");
  for (const m of text.matchAll(/from\s*['"]([^'"]+)['"]/g)) {
    const s = m[1];
    if (s.startsWith("@/zcode/")) {
      const to = resolve(f, s);
      if (to) {
        const st = [to];
        while (st.length) {
          const u = st.pop();
          if (allTestReach.has(u)) continue;
          allTestReach.add(u);
          for (const v of graph.get(u) || []) st.push(v);
        }
      }
    }
  }
}

// non-test internal seeds in top subsystems, for self-reach; then classify
const subsystems = [...new Set(files.map((f) => relZc(f).split("/")[0] === relZc(f) ? "root" : relZc(f).split("/")[0]))];
const info = [];
for (const f of files) {
  const p = relZc(f);
  const top = p.includes("/") ? p.split("/")[0] : "root";
  const inMain = mainReach.has(f);
  const inTest = allTestReach.has(f);
  let cls = inMain ? "main-reachable" : inTest ? "test-only" : "unreachable";
  info.push({ f, rel: rel(f), p, top, cls, loc: fs.readFileSync(f, "utf8").split(/\r?\n/).length });
}
const byCls = {};
for (const x of info) {
  byCls[x.cls] ||= { n: 0, loc: 0 };
  byCls[x.cls].n++;
  byCls[x.cls].loc += x.loc;
}
const byTop = {};
for (const x of info) {
  byTop[x.top] ||= { n: 0, loc: 0, main: 0, mainLoc: 0, test: 0, testLoc: 0, un: 0, unLoc: 0 };
  const t = byTop[x.top];
  t.n++;
  t.loc += x.loc;
  if (x.cls === "main-reachable") {
    t.main++;
    t.mainLoc += x.loc;
  } else if (x.cls === "test-only") {
    t.test++;
    t.testLoc += x.loc;
  } else {
    t.un++;
    t.unLoc += x.loc;
  }
}
// outgoing edges in whole tree
const indeg = new Map();
for (const f of files) indeg.set(f, 0);
for (const f of files) for (const v of graph.get(f) || []) if (indeg.has(v)) indeg.set(v, indeg.get(v) + 1);
const nonTestNoIn = files.filter((f) => !/\.test\.tsx?$/.test(f) && indeg.get(f) === 0);
// self reach for subsystems excluding main reach: count files that have internal edge but not main/reverse
const internalUnreachable = info.filter((x) => x.cls === "unreachable" && [...graph.values()].some((s) => s.has(x.f)));

console.log(JSON.stringify({ summary: { total: files.length, totalLoc: info.reduce((s,x)=>s+x.loc,0), byCls, topStats: byTop, nonTestNoIn: nonTestNoIn.map((f)=>({f:rel(f), loc:fs.readFileSync(f,'utf8').split(/\r?\n/).length})), selfReachUnreachable: internalUnreachable.map(x=>x.p), entries: outsideSeeds.map(rel), mainN: mainReach.size, testN: allTestReach.size, testFiles: tests.map(rel) }, version: process.version }, null, 1));