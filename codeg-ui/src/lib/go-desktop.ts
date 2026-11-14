// Go WebView2 exposes only local-window lifecycle operations. All workspace
// commands continue using Codeg's original HTTP / WebSocket transport.
import { getCurrentWindow } from "@tauri-apps/api/window"

export type GoResizeDirection =
  | "n"
  | "ne"
  | "nw"
  | "s"
  | "se"
  | "sw"
  | "e"
  | "w"

export interface GoDesktopBridge {
  nativeWindowChrome?: boolean
  open: (path: string) => Promise<void>
  close: () => Promise<void>
  minimize: () => Promise<void>
  toggleMaximize: () => Promise<boolean>
  beginDrag: () => Promise<void>
  beginResize?: (direction: GoResizeDirection) => Promise<void>
  openExternal?: (url: string) => Promise<void>
}

const TAURI_RESIZE_DIRECTIONS = {
  n: "North",
  ne: "NorthEast",
  nw: "NorthWest",
  s: "South",
  se: "SouthEast",
  sw: "SouthWest",
  e: "East",
  w: "West",
} as const

export function goDesktopBridge(): GoDesktopBridge | null {
  if (typeof window === "undefined") return null
  const host = window as unknown as {
    __CODEG_NATIVE_WINDOW_CHROME__?: boolean
    __codegGoOpenWindow?: (path: string) => Promise<void>
    __codegGoCloseWindow?: () => Promise<void>
    __codegGoWindowMinimize?: () => Promise<void>
    __codegGoWindowToggleMaximize?: () => Promise<boolean>
    __codegGoWindowBeginDrag?: () => Promise<void>
    __codegGoOpenExternal?: (url: string) => Promise<void>
    __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: string } } }
  }
  if (typeof host.__codegGoOpenWindow !== "function") return null

  if (host.__TAURI_INTERNALS__?.metadata?.currentWindow?.label) {
    const currentWindow = getCurrentWindow()
    return {
      nativeWindowChrome: host.__CODEG_NATIVE_WINDOW_CHROME__ === true,
      open: host.__codegGoOpenWindow,
      close: () => currentWindow.close(),
      minimize: () => currentWindow.minimize(),
      toggleMaximize: async () => {
        await currentWindow.toggleMaximize()
        return currentWindow.isMaximized()
      },
      beginDrag: () => currentWindow.startDragging(),
      beginResize: (direction) =>
        currentWindow.startResizeDragging(TAURI_RESIZE_DIRECTIONS[direction]),
      openExternal: host.__codegGoOpenExternal,
    }
  }

  // Keep the bridge usable in the legacy Go WebView2 host and in browser-mode
  // callers that provide that host contract without Tauri internals.
  if (typeof host.__codegGoCloseWindow !== "function") return null
  const unsupported = (operation: string) =>
    Promise.reject(new Error(`窗口不支持${operation}`))
  return {
    open: host.__codegGoOpenWindow,
    close: host.__codegGoCloseWindow,
    minimize: host.__codegGoWindowMinimize ?? (() => unsupported("最小化")),
    toggleMaximize:
      host.__codegGoWindowToggleMaximize ?? (() => unsupported("最大化")),
    beginDrag: host.__codegGoWindowBeginDrag ?? (() => unsupported("拖动")),
    openExternal: host.__codegGoOpenExternal,
  }
}
