// Go WebView2 exposes only local-window lifecycle operations. All workspace
// commands continue using Codeg's original HTTP / WebSocket transport.
export function goDesktopBridge() {
  if (typeof window === "undefined") return null
  const host = window as unknown as {
    __codegGoOpenWindow?: (path: string) => Promise<void>
    __codegGoCloseWindow?: () => Promise<void>
  }
  return typeof host.__codegGoOpenWindow === "function" &&
    typeof host.__codegGoCloseWindow === "function"
    ? { open: host.__codegGoOpenWindow, close: host.__codegGoCloseWindow }
    : null
}
