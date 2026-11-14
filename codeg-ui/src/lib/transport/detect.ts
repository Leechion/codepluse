export type TransportEnvironment = "tauri" | "web"

/** True inside any Tauri webview (full-Tauri build or the Go-sidecar shell). */
export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window
}

/**
 * True when Tauri is only the window and the backend is the Go sidecar.
 *
 * `src-tauri/src/main.rs` injects this marker as an initialization script.
 * It must keep the app on the HTTP/WebSocket transport: `TauriTransport`
 * forwards command names straight to `invoke()`, and those Rust commands do
 * not exist in this repository — the Go server is the backend.
 */
export function isTauriSidecarShell(): boolean {
  if (!isTauriRuntime()) return false
  return (
    (window as unknown as { __CODEG_TAURI_SHELL__?: boolean })
      .__CODEG_TAURI_SHELL__ === true
  )
}

export function detectEnvironment(): TransportEnvironment {
  if (isTauriSidecarShell()) return "web"
  if (isTauriRuntime()) return "tauri"
  return "web"
}
