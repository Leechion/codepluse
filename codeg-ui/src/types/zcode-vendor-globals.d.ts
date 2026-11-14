// Global ambient augmentations kept for the app (the vendored ZCode tree that
// used to be referenced here has been removed).

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
