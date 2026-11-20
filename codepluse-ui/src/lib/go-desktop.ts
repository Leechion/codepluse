// Go WebView2 exposes only local-window lifecycle operations. All workspace
// commands continue using Codepluse's original HTTP / WebSocket transport.
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
    __CODEPLUSE_NATIVE_WINDOW_CHROME__?: boolean
    __codepluseGoOpenWindow?: (path: string) => Promise<void>
    __codepluseGoCloseWindow?: () => Promise<void>
    __codepluseGoWindowMinimize?: () => Promise<void>
    __codepluseGoWindowToggleMaximize?: () => Promise<boolean>
    __codepluseGoWindowBeginDrag?: () => Promise<void>
    __codepluseGoOpenExternal?: (url: string) => Promise<void>
    __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: string } } }
  }
  if (typeof host.__codepluseGoOpenWindow !== "function") return null

  if (host.__TAURI_INTERNALS__?.metadata?.currentWindow?.label) {
    const currentWindow = getCurrentWindow()
    return {
      nativeWindowChrome: host.__CODEPLUSE_NATIVE_WINDOW_CHROME__ === true,
      open: host.__codepluseGoOpenWindow,
      close: () => currentWindow.close(),
      minimize: () => currentWindow.minimize(),
      toggleMaximize: async () => {
        await currentWindow.toggleMaximize()
        return currentWindow.isMaximized()
      },
      beginDrag: () => currentWindow.startDragging(),
      beginResize: (direction) =>
        currentWindow.startResizeDragging(TAURI_RESIZE_DIRECTIONS[direction]),
      openExternal: host.__codepluseGoOpenExternal,
    }
  }

  // Keep the bridge usable in the legacy Go WebView2 host and in browser-mode
  // callers that provide that host contract without Tauri internals.
  if (typeof host.__codepluseGoCloseWindow !== "function") return null
  const unsupported = (operation: string) =>
    Promise.reject(new Error(`窗口不支持${operation}`))
  return {
    open: host.__codepluseGoOpenWindow,
    close: host.__codepluseGoCloseWindow,
    minimize: host.__codepluseGoWindowMinimize ?? (() => unsupported("最小化")),
    toggleMaximize:
      host.__codepluseGoWindowToggleMaximize ?? (() => unsupported("最大化")),
    beginDrag: host.__codepluseGoWindowBeginDrag ?? (() => unsupported("拖动")),
    openExternal: host.__codepluseGoOpenExternal,
  }
}
