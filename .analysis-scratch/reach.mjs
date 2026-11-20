// 一次性只读分析：从 Next 路由入口做 import BFS，量化未被引用的源文件规模。
// 纯静态正则，不改仓库。路径统一用正斜杠字符串，避免 win32 path.join 干扰。
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'

const ROOT = 'E:/code/codepluse'
const UI = ROOT + '/codepluse-ui'
const all = execFileSync('git', ['-C', ROOT, 'ls-files', 'codepluse-ui/src'], { maxBuffer: 1 << 28 })
  .toString()
  .split('\n')
  .filter((f) => f.endsWith('.ts') || f.endsWith('.tsx'))
  .filter((f) => !f.endsWith('.test.ts') && !f.endsWith('.test.tsx'))
  .map((f) => ROOT + '/' + f)
const set = new Set(all)
console.log('tracked 源文件', set.size, '样例', all[0])

const EXT = ['', '.ts', '.tsx', '/index.ts', '/index.tsx']
function resolve(spec, fromFile) {
  let base
  if (spec.startsWith('@/')) base = UI + '/src/' + spec.slice(2)
  else if (spec.startsWith('./') || spec.startsWith('../')) {
    const dir = fromFile.slice(0, fromFile.lastIndexOf('/'))
    const parts = (dir + '/' + spec).split('/')
    const st = []
    for (const s of parts) {
      if (s === '.' || s === '') continue
      if (s === '..') st.pop()
      else st.push(s)
    }
    base = st.join('/')
  } else return null
  for (const e of EXT) if (set.has(base + e)) return base + e
  return null
}

const RE =
  /(?:import|export)\s+(?:type\s+)?(?:[^'"()]*?\sfrom\s*)?['"]([^'"]+)['"]/g
const edges = new Map()
for (const f of all) {
  const src = fs.readFileSync(f, 'utf8')
  const out = new Set()
  for (const m of src.matchAll(RE)) {
    const r = resolve(m[1] || m[2], f)
    if (r && r !== f) out.add(r)
  }
  edges.set(f, out)
}

const isZ = (f) => f.includes('/src/zcode/')
const entries = all.filter(
  (f) => f.includes('/src/app/') && /\/(page|layout|route|template|default|error|loading)\.tsx$/.test(f) && !f.includes('.gallery.'),
)
const seen = new Set()
const stack = [...entries]
while (stack.length) {
  const f = stack.pop()
  if (seen.has(f)) continue
  seen.add(f)
  for (const n of edges.get(f) || []) if (!seen.has(n)) stack.push(n)
}
const dead = all.filter((f) => !seen.has(f))
const lc = (arr) => arr.reduce((a, f) => a + fs.readFileSync(f, 'utf8').split('\n').length, 0)
console.log('入口', entries.length, '| 可达', seen.size, '行', lc([...seen]), '| 不可达', dead.length, '行', lc(dead))
console.log('不可达中 zcode', dead.filter(isZ).length, '/', all.filter(isZ).length, ' 非 zcode', dead.filter((f) => !isZ(f)).length)
const byDir = new Map()
for (const f of dead) {
  const k = f.slice(UI.length + 1).split('/').slice(0, 2).join('/')
  const c = byDir.get(k) || [0, 0]
  c[0]++
  c[1] += fs.readFileSync(f, 'utf8').split('\n').length
  byDir.set(k, c)
}
console.log('\n不可达按目录:')
for (const [k, v] of [...byDir].sort((a, b) => b[1][1] - a[1][1])) console.log(String(v[0]).padStart(5), String(v[1]).padStart(8), k)
console.log('\n非 zcode 不可达清单:')
for (const f of dead.filter((x) => !isZ(x))) console.log('  ' + f.slice(ROOT.length + 1))
const src = fs.readFileSync(UI + '/src/app/workspace/page.tsx', 'utf8')
console.log('\nworkspace/page.tsx 边:', [...(edges.get(UI + '/src/app/workspace/page.tsx') || [])])
