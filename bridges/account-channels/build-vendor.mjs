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
const frontendRequire = createRequire(path.join(directory, "../../codeg-ui/package.json"))
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
console.log("Built only the selected WorkBuddy/Qoder protocol exports.")
