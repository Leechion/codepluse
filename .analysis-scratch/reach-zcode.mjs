import { execFileSync } from 'node:child_process'
import fs from 'node:fs'

const ROOT = fs.realpathSync(process.cwd())
const ZC = ROOT + '/codeg-ui/src/zcode'
const files = execFileSync('git', ['ls-files', 'codeg-ui/src/zcode'], { maxBuffer: 1 << 28 })
  .toString()
  .split('\n')
  .filter((f) => /\.tsx?$/.test(f) && !/\.test\.tsx?$/.test(f))
  .map((f) => ROOT + '/' + f)
const set = new Set(files)
const lc = (f) => fs.readFileSync(f, 'utf8').split('\n').length
console.log('zcode 源文件', files.length, '总行', files.reduce((a, f) => a + lc(f), 0))

function resolve(spec, fromFile) {
  let base
  if (spec.startsWith('@/')) base = ROOT + '/codeg-ui/src/' + spec.slice(2)
  else if (spec.startsWith('./') || spec.startsWith('../')) {
    const parts = (fromFile.slice(0, fromFile.lastIndexOf('/')) + '/' + spec).split('/')
    const st = []
    for (const s of parts) {
      if (s === '.' || s === '') continue
      if (s === '..') st.pop()
      else st.push(s)
    }
    base = st.join('/')
  } else return null
  for (const e of ['', '.ts', '.tsx', '/index.ts', '/index.tsx']) if (set.has(base + e)) return base + e
  return null
}

const RE = /(?:^|\n)[ \t]*(?:import|export)(?:\s+type)?[\s\S]{0,400}?from[ \t]*['"]([^'"]+)['"]/g
const edges = new Map()
const unresolved = []
for (const f of files) {
  const src = fs.readFileSync(f, 'utf8')
  const out = new Set()
  for (const m of src.matchAll(RE)) {
    const spec = m[1]
    const r = resolve(spec, f)
    if (r) {
      if (r !== f) out.add(r)
    } else if (spec.startsWith('.') || spec.startsWith('@/zcode')) unresolved.push(f.slice(ROOT.length + 1) + ' <- ' + spec)
  }
  edges.set(f, out)
}
console.log('本树内未能解析的 import 条数', unresolved.length)
if (unresolved.length) console.log(unresolved.slice(0, 5).join('\n'))

const entry = files.find((f) => f.endsWith('/pages/ZCodeWorkspacePage.tsx'))
const seen = new Set()
const stack = [entry]
while (stack.length) {
  const f = stack.pop()
  if (seen.has(f)) continue
  seen.add(f)
  for (const n of edges.get(f) || []) if (!seen.has(n)) stack.push(n)
}
const dead = files.filter((x) => !seen.has(x))
console.log('入口 1 | 可达', seen.size, '行', files.filter((x) => seen.has(x)).reduce((a, f) => a + lc(f), 0))
console.log('不可达', dead.length, '行', dead.reduce((a, f) => a + lc(f), 0))
const byDir = new Map()
for (const f of dead) {
  const k = f.slice(ZC.length + 1).split('/')[0]
  const c = byDir.get(k) || [0, 0]
  c[0]++
  c[1] += lc(f)
  byDir.set(k, c)
}
console.log('不可达按一级目录:')
for (const [k, v] of [...byDir].sort((a, b) => b[1][1] - a[1][1]).slice(0, 18))
  console.log(String(v[0]).padStart(5), String(v[1]).padStart(8), k)
