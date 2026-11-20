import { defineConfig } from "vitest/config"
import react from "@vitejs/plugin-react"
import path from "node:path"

const cuaDir = path.resolve(__dirname, "./src/zcode/zcode-cua")

// The Computer-Use placeholder ships as a flat directory of .js stubs, while
// the vendored sources import nested specifiers. A plain prefix alias cannot
// express this (broker/ports -> broker-ports.js), so map each specifier
// exactly. Regexes make the match independent of alias order.
const cuaStubs: Record<string, string> = {
  "broker/helperConstants": "broker-helper-constants.js",
  "broker/helperHealth": "broker-helper-health.js",
  "broker/socketPath": "broker-socket-path.js",
  "broker/server": "broker-server.js",
  "broker/ports": "broker-ports.js",
  broker: "broker.js",
  "pip-session/node": "pip-session-node.js",
  "pip-session": "pip-session.js",
  "src/broker/ports": "broker-ports.js",
}

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: [
      ...Object.entries(cuaStubs).map(([subpath, file]) => ({
        find: new RegExp(`^@zcode/zcode-cua/${subpath}$`),
        replacement: path.resolve(cuaDir, file),
      })),
      // The package ships an ESM file but its declared CJS main is absent.
      // Vite needs the concrete module when a real TerminalView is imported.
      {
        find: "@xterm/addon-ligatures",
        replacement: path.resolve(
          __dirname,
          "./node_modules/@xterm/addon-ligatures/lib/addon-ligatures.mjs"
        ),
      },
      { find: "@", replacement: path.resolve(__dirname, "./src") },
      // Vendored ZCode tree: packages/ui imports these package aliases.
      // Each alias points at its directory, not index.ts: Vite resolves the
      // bare specifier through index.ts, while a subpath such as
      // @zcode/shared/zcode-protocol-v4 must become <dir>/zcode-protocol-v4.
      // A trailing-slash key never matches a subpath in Vite, so the old
      // "<name>/" entries were dead config.
      { find: "@zcode/shared", replacement: path.resolve(__dirname, "./src/zcode/shared") },
      { find: "@zcode/services", replacement: path.resolve(__dirname, "./src/zcode/services") },
      { find: "@zcode/provider", replacement: path.resolve(__dirname, "./src/zcode/provider") },
      { find: "@zcode/rpc", replacement: path.resolve(__dirname, "./src/zcode/rpc") },
      {
        find: "@zcode/model-option-map",
        replacement: path.resolve(__dirname, "./src/zcode/model-option-map"),
      },
      // The same alias esbuild resolves for the bundle: Playwright's sources
      // import each other through it, and honouring it here is what lets them
      // stay byte-identical to upstream.
      {
        find: "@isomorphic",
        replacement: path.resolve(
          __dirname,
          "./browser-agent/vendor/playwright/isomorphic"
        ),
      },
    ],
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
    // `browser-agent/` is the isolated-world bundle's source. It is outside
    // `src/` because it is built by esbuild, not Next, and its vendored half
    // is held to Playwright's compiler settings rather than ours.
    include: [
      "src/**/*.{test,spec}.{ts,tsx}",
      "browser-agent/**/*.{test,spec}.ts",
    ],
    exclude: ["node_modules", "out", ".next", "src-tauri"],
    coverage: {
      provider: "v8",
      reporter: ["text", "html"],
      include: ["src/**/*.{ts,tsx}"],
      exclude: [
        "browser-agent/vendor/**",
        "node_modules/",
        ".next/**",
        "out/**",
        "src/test-setup.ts",
        "**/*.test.{ts,tsx}",
        "**/*.spec.{ts,tsx}",
        "**/*.config.*",
        "**/*.d.ts",
        "src-tauri/**",
      ],
    },
  },
})
