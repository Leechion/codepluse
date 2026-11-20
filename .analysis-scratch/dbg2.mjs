import fs from 'node:fs'
const RE = /(?:import|export)\s+(?:type\s+)?(?:[^'"()]*?\sfrom\s*)?['"]([^'"]+)['"]/g
const f = 'E:/code/codepluse/codepluse-ui/src/zcode/pages/ZCodeWorkspacePage.tsx'
const src = fs.readFileSync(f, 'utf8')
const specs = [...src.matchAll(RE)].map((m) => m[1])
console.log('解析出', specs.length, '条:', specs.slice(0, 12))
const raw = [...src.matchAll(/from\s*['"]([^'"]+)['"]/g)].map((m) => m[1])
console.log('朴素 from 正则', raw.length, '条')
console.log('前 3 行:', JSON.stringify(src.split('\n').slice(0, 3)))
