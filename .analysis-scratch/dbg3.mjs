import fs from 'node:fs'
const ROOT = fs.realpathSync(process.cwd())
const SRC = ROOT + '/codeg-ui/src'
const f = SRC + '/zcode/pages/ZCodeWorkspacePage.tsx'
const src = fs.readFileSync(f, 'utf8')
const RE = /(?:^|\n)[ \t]*(?:import|export)(?:\s+type)?[\s\S]{0,400}?from[ \t]*['"]([^'"]+)['"]/g
const specs = [...src.matchAll(RE)].map(m => m[1])
console.log('解析到', specs.length, '条')
for (const s of specs) {
  let p = null
  if (s.startsWith('@/')) p = SRC + '/' + s.slice(2)
  else if (s.startsWith('.')) p = null
  const hit = p ? ['', '.ts', '.tsx'].map(e => p + e).find(x => fs.existsSync(x)) : '(相对/别名)'
  console.log(hit ? 'OK   ' : 'MISS ', s, hit || '')
}
const rels = [...src.matchAll(/from\s+['"](\.[^'"]+)['"]/g)].map(m => m[1])
console.log('相对 import 条数', rels.length, rels.slice(0, 10))
