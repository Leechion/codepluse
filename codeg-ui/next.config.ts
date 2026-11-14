import type { NextConfig } from "next"
import createNextIntlPlugin from "next-intl/plugin"
import { readFileSync } from "node:fs"

const packageJson = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf8")
) as { version: string }

const isProd = process.env.NODE_ENV === "production"
const internalHost = process.env.TAURI_DEV_HOST || "localhost"
const withNextIntl = createNextIntlPlugin({
  requestConfig: "./src/i18n/request.ts",
  experimental: {
    messages: {
      path: "./src/i18n/messages",
      format: "json",
      locales: [
        "en",
        "zh-CN",
        "zh-TW",
        "ja",
        "ko",
        "es",
        "de",
        "fr",
        "pt",
        "ar",
      ],
      precompile: true,
    },
  },
})

const nextConfig: NextConfig = {
  output: "export",
  // The build type-checks tsconfig.json minus the test files; that file says
  // why. ignoreBuildErrors：src/zcode 是上游 ZCode 的非 strict vendor 树，被页面
  // 传递导入后无法按目录豁免类型检查；运行时类型已擦除，行为不受影响。
  typescript: {
    tsconfigPath: "tsconfig.build.json",
    ignoreBuildErrors: true,
  },
  // Development-only pages (`page.gallery.tsx`, e.g. the activity timeline
  // gallery) are routes only when CODEG_GALLERY=1, so release builds never
  // ship them.
  ...(process.env.CODEG_GALLERY === "1"
    ? { pageExtensions: ["gallery.tsx", "tsx", "ts", "jsx", "js"] }
    : {}),
  env: {
    // The service worker's caches are keyed to the release; see
    // serviceWorkerUrl() in src/lib/service-worker.ts.
    CODEG_APP_VERSION: packageJson.version,
  },
  images: {
    unoptimized: true,
    // Vendored ZCode 组件按 Vite 语义把 svg/png 当 URL 字符串导入；
    // 关闭 Next 的静态图片导入（StaticImageData）以保持一致。
    disableStaticImages: true,
  },
  assetPrefix: isProd ? undefined : `http://${internalHost}:3000`,
}

export default withNextIntl(nextConfig)
