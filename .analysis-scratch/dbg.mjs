import fs from 'node:fs'
import path from 'node:path'

const ROOT = 'E:/code/codepluse'
const UI = path.join(ROOT, 'codeg-ui')
const p = path.join(UI, 'src/app/workspace/page.tsx')
console.log('page:', p)
console.log('exists:', fs.existsSync(p))
const src = fs.readFileSync(p, 'utf8')
console.log('content:', JSON.stringify(src))

const RE =
  /(?:import\s+(?:type\s+)?(?:[^'"]*?\sfrom\s*)?|import\s*\(|export\s+(?:type\s+)?(?:\*|\{[^}]*\}\s*from))\s*['"]([^'"]+)['"]/g
for (const m of src.matchAll(RE)) console.log('spec:', m[1])

const spec = '@/zcode/pages/ZCodeWorkspacePage'
const base = path.join(UI, 'src', spec.slice(2)).replace(/\\/g, '/')
console.log('base:', base)
for (const e of ['', '.ts', '.tsx', '/index.ts', '/index.tsx']) {
  console.log('  try', base + e, fs.existsSync(base + e))
}
