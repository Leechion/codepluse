// Go WebView2 exposes only local-window lifecycle operations. All workspace
// commands continue using Codeg's original HTTP / WebSocket transport.
export function goDesktopBridge() {
  if (typeof window === "undefined") return null
  const host = window as unknown as {
    __codegGoOpenWindow?: (path: string) => Promise<void>
    __codegGoCloseWindow?: () => Promise<void>
    __codegGoWindowMinimize?: () => Promise<void>
    __codegGoWindowToggleMaximize?: () => Promise<boolean>
    __codegGoWindowBeginDrag?: () => Promise<void>
    __codegGoOpenExternal?: (url: string) => Promise<void>
  }
  return typeof host.__codegGoOpenWindow === "function" &&
    typeof host.__codegGoCloseWindow === "function" &&
    typeof host.__codegGoWindowMinimize === "function" &&
    typeof host.__codegGoWindowToggleMaximize === "function" &&
    typeof host.__codegGoWindowBeginDrag === "function"
    ? {
        open: host.__codegGoOpenWindow,
        close: host.__codegGoCloseWindow,
        minimize: host.__codegGoWindowMinimize,
        toggleMaximize: host.__codegGoWindowToggleMaximize,
        beginDrag: host.__codegGoWindowBeginDrag,
        openExternal: host.__codegGoOpenExternal,
      }
    : null
}
