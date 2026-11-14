/**
 * Opens a URL in the user's browser.
 *
 * The desktop shells inject `window.__codegGoOpenExternal` (the Go WebView2
 * shell binds it, the Tauri shell injects it in its init script), so the app
 * page never navigates away from itself. In a plain browser tab we fall back
 * to `window.open`.
 *
 * Only the shells validate the URL (HTTPS only); the browser tab path keeps
 * the page in place with `noopener`.
 */
export async function openExternalUrl(url: string): Promise<void> {
  if (typeof window === "undefined") return
  const host = window as unknown as {
    __codegGoOpenExternal?: (url: string) => Promise<void> | void
  }
  if (typeof host.__codegGoOpenExternal === "function") {
    await host.__codegGoOpenExternal(url)
    return
  }
  window.open(url, "_blank", "noopener,noreferrer")
}
