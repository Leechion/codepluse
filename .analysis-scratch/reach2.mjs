import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
const ROOT = fs.realpathSync(process.cwd())
const SRC = ROOT + '/codepluse-ui/src'
const files = execFileSync('git', ['ls-files', 'codepluse-ui/src'], { maxBuffer: 1 << 28 })
  .toString().split('\n').filter(f => /\.tsx?$/.test(f) && !/\.test\.tsx?$/.test(f)).map(f => ROOT + '/' + f)
const set = new Set(files)
const lc = f => fs.readFileSync(f, 'utf8').split('\n').length
const ALIAS = { '@zcode/shared': '/zcode/shared', '@zcode/services': '/zcode/services', '@zcode/provider': '/zcode/provider', '@zcode/rpc': '/zcode/rpc', '@zcode/model-option-map': '/zcode/model-option-map' }
function resolve(spec, fromFile) {
  let base = null
  for (const [a, t] of Object.entries(ALIAS)) if (spec === a || spec.startsWith(a + '/')) { base = SRC + t + spec.slice(a.length); break }
  if (!base && spec.startsWith('@/')) base = SRC + '/' + spec.slice(2)
  if (!base && (spec.startsWith('./') || spec.startsWith('../'))) {
    const st = []
    for (const s of (fromFile.slice(0, fromFile.lastIndexOf('/')) + '/' + spec).split('/')) { if (!s || s === '.') continue; if (s === '..') st.pop(); else st.push(s) }
    base = st.join('/')
  }
  if (!base) return null
  for (const e of ['', '.ts', '.tsx', '/index.ts', '/index.tsx']) if (set.has(base + e)) return base + e
  return null
}
const RE = /(?:^|\n)[ \t]*(?:import|export)(?:\s+type)?[\s\S]{0,400}?from[ \t]*['"]([^'"]+)['"]/g
const edges = new Map()
for (const f of files) { const out = new Set(); for (const m of fs.readFileSync(f, 'utf8').matchAll(RE)) { const r = resolve(m[1], f); if (r && r !== f) out.add(r) } edges.set(f, out) }
const isZ = f => f.startsWith(SRC + '/zcode/')
const entries = files.filter(f => f.startsWith(SRC + '/app/') && /\/(page|layout|route|template|default|error|loading)\.tsx$/.test(f) && !f.includes('.gallery.'))
const seen = new Set(); const stack = [...entries]
while (stack.length) { const f = stack.pop(); if (seen.has(f)) continue; seen.add(f); for (const n of edges.get(f) || []) if (!seen.has(n)) stack.push(n) }
const dead = files.filter(f => !seen.has(f))
const sum = a => a.reduce((x, f) => x + lc(f), 0)
console.log('源文件', files.length, '行', sum(files), '| 入口', entries.length)
console.log('可达', seen.size, '行', sum([...seen]))
console.log('不可达', dead.length, '行', sum(dead), '| zcode 不可达', dead.filter(isZ).length, '/', files.filter(isZ).length, '| 非 zcode 不可达', dead.filter(f => !isZ(f)).length)
const byDir = new Map()
for (const f of dead) { const k = f.slice(SRC.length + 1).split('/').slice(0, isZ(f) ? 2 : 1).join('/'); const c = byDir.get(k) || [0, 0]; c[0]++; c[1] += lc(f); byDir.set(k, c) }
console.log('\n不可达 top 20:')
for (const [k, v] of [...byDir].sort((a, b) => b[1][1] - a[1][1]).slice(0, 20)) console.log(String(v[0]).padStart(5), String(v[1]).padStart(8), k)
console.log('\n非 zcode 不可达清单:')
for (const f of dead.filter(x => !isZ(x))) console.log('  ' + f.slice(ROOT.length + 1))
