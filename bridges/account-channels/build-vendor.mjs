// Rebuild the selected MIT protocol exports from an installed OFM package.
// Runtime users need only Node.js; esbuild comes from the frontend's dev tree.
import fs from "node:fs/promises"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { createRequire } from "node:module"
import { createHash } from "node:crypto"

const sourceRoot = path.resolve(process.argv[2] || "")
if (!process.argv[2]) throw new Error("Usage: node build-vendor.mjs <dsh-our-free-model package directory>")
const directory = path.dirname(fileURLToPath(import.meta.url))
const frontendRequire = createRequire(path.join(directory, "../../codepluse-ui/package.json"))
const esbuild = frontendRequire(frontendRequire.resolve("esbuild", { paths: [path.dirname(frontendRequire.resolve("vite"))] }))
const sourceFile = path.join(sourceRoot, "packages/standalone/channels/business.mjs")
const original = await fs.readFile(sourceFile, "utf8")
const hash = createHash("sha256").update(original).digest("hex")
const source = original.replace("../../../vendor/channel-pack/qoder-auth-wasm.wasm", "./qoder-auth-wasm.wasm") +
  "\nexport { AccountPool, BuddyAuth, QoderAuth, CODEBUDDY, WORKBUDDY, QODER, QODER_CN, registerBuddyLlm, registerQoderLlm };\n"
const selected = ["AccountPool", "BuddyAuth", "QoderAuth", "CODEBUDDY", "WORKBUDDY", "QODER", "QODER_CN", "registerBuddyLlm", "registerQoderLlm", "toGenerateOptions", "normalizeReasoningEffort", "normalizeMaxTokens", "makeReadImage", "makeReadImageRequest"]
await esbuild.build({
  stdin: { contents: `export { ${selected.join(", ")} } from "ofm-selected-protocol"`, resolveDir: directory },
  outfile: path.join(directory, "vendor/business.mjs"), bundle: true, treeShaking: true,
  platform: "node", format: "esm", target: "node22", charset: "utf8", legalComments: "inline",
  banner: { js: `// Selected WorkBuddy/Qoder protocol bundle; OFM 2.3.0 original SHA256 ${hash}.\n// MIT: see LICENSE and LICENSE-OFM. Rebuild using ../build-vendor.mjs.` },
  plugins: [{ name: "selected-protocol", setup(build) {
    build.onResolve({ filter: /^ofm-selected-protocol$/ }, () => ({ path: sourceFile, namespace: "ofm" }))
    build.onLoad({ filter: /.*/, namespace: "ofm" }, () => ({ contents: source, loader: "js", resolveDir: path.dirname(sourceFile) }))
    build.onResolve({ filter: /contracts\.mjs$/ }, () => ({ path: "./contracts.mjs", external: true }))
  } }],
})
// codepluse patch ⇢ 打包产物：上游把 `resolveChannelPackHome` 写成「不是字符串就抛
// `独立渠道缺少数据目录`」，但它的三个调用点（账号池、永久锁定开关、**已失效模型记录**）
// 都写了「拿不到 home 就退化到内存存储」的分支，等于被这句 throw 堵死。
// 触发面很窄：`withDeadModelPruning` 只在模型列表**非空**时调用 `deadModelIdsFor()`
// → `ensureStore()` → 模块级 `injectedCtx`（本 bundle 里从未被赋值）→ 抛错。
// 表现为「未登录时模型列表正常、一登录 WorkBuddy 就报独立渠道缺少数据目录」。
// 重新打包会把这里冲掉，所以补丁写进构建脚本，而不是只改 vendor/business.mjs。
const bundled = path.join(directory, "vendor/business.mjs")
const built = await fs.readFile(bundled, "utf8")
// 产物可能是 LF（esbuild 默认）也可能是 CRLF（本仓库 core.autocrlf=true），锚点必须按实际行尾构造。
const nl = built.includes("\r\n") ? "\r\n" : "\n"
const before = [
  "function resolveChannelPackHome(ctx) {",
  '  if (typeof ctx.platform?.channelHome !== "string") throw new Error("独立渠道缺少数据目录");',
  "  return ctx.platform.channelHome;",
  "}",
].join(nl)
const after = [
  "// codepluse patch: 未注入 ctx 时（死模型记录、永久锁定开关的模块级旁路）不再抛错，",
  "// 返回 undefined 让三个调用点走各自已有的「退化到内存存储」分支。",
  "function resolveChannelPackHome(ctx) {",
  '  return typeof ctx?.platform?.channelHome === "string" ? ctx.platform.channelHome : void 0;',
  "}",
].join(nl)
if (!built.includes(after)) {
  if (!built.includes(before)) throw new Error("vendor patch 目标未找到：上游 resolveChannelPackHome 已改动，请人工复核")
  await fs.writeFile(bundled, built.replace(before, after), "utf8")
}
console.log("Built only the selected WorkBuddy/Qoder protocol exports.")
