// Vendored ZCode 树（src/zcode）的 ambient 声明入口。tsconfig.build.json 把
// src/zcode 排除出根文件集合，这些 .d.ts 不会被自动纳入程序；这里用
// triple-slash reference 显式拉入，供 vendor 源码的类型检查使用。
/// <reference path="../zcode/assets.d.ts" />
/// <reference path="../zcode/css.d.ts" />
/// <reference path="../zcode/electron-webview.d.ts" />
/// <reference path="../zcode/office-preview-assets.d.ts" />

// vendor 树按更新版本的 DOM 规范使用 moveBefore（TS 5.8 的 lib.dom 尚未收录）；
// Chromium 的实现允许第二参传 null。
interface HTMLElement {
  moveBefore(
    node: Node,
    options?: boolean | { relativeTo?: Node } | null,
  ): void;
}

// vendor 树沿用 Vite 的 import.meta.env 读取构建期注入；Turbopack 下仅保留类型面。
interface ImportMetaEnv {
  readonly BASE_URL?: string
  readonly DEV?: boolean
  readonly PROD?: boolean
  readonly MODE?: string
  readonly VITE_ZCODE_E2E_STORE_BRIDGE?: string
  readonly VITE_REWARDS_WEBVIEW_ORIGIN?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
