"use client"

import { useEffect, useState, useSyncExternalStore } from "react"
import { Copy, Minus, Square, X } from "lucide-react"
import { goDesktopBridge } from "@/lib/go-desktop"

type GoWindowBridge = NonNullable<ReturnType<typeof goDesktopBridge>>

let bridgeHost: Window | null = null
let bridgeSnapshot: GoWindowBridge | null = null

function getBridgeSnapshot(): GoWindowBridge | null {
  if (typeof window === "undefined") return null
  if (bridgeHost !== window) {
    bridgeHost = window
    bridgeSnapshot = goDesktopBridge()
    if (bridgeSnapshot?.nativeWindowChrome) bridgeSnapshot = null
  }
  return bridgeSnapshot
}

function subscribeToBridge() {
  return () => undefined
}

function getServerBridgeSnapshot() {
  return null
}

export function GoWindowTitlebar() {
  const bridge = useSyncExternalStore(
    subscribeToBridge,
    getBridgeSnapshot,
    getServerBridgeSnapshot
  )
  const [maximized, setMaximized] = useState(false)

  useEffect(() => {
    if (!bridge) return
    document.body.dataset.goWindowChrome = "true"
    return () => {
      delete document.body.dataset.goWindowChrome
    }
  }, [bridge])

  if (!bridge) return null

  const toggleMaximize = async () => {
    try {
      setMaximized(await bridge.toggleMaximize())
    } catch (error) {
      console.error("[GoWindowTitlebar] failed to toggle maximize:", error)
    }
  }

  return (
    <header
      className="cp-go-titlebar"
      aria-label="Codepluse 窗口标题栏"
      onMouseDown={(event) => {
        if (
          event.buttons !== 1 ||
          (event.target as HTMLElement).closest("button")
        ) {
          return
        }
        if (event.detail === 2) {
          void toggleMaximize()
          return
        }
        void bridge.beginDrag().catch((error) => {
          console.error(
            "[GoWindowTitlebar] failed to begin window drag:",
            error
          )
        })
      }}
    >
      <div className="cp-go-titlebar-brand" aria-label="codepluse">
        <img src="/icon-32x32.png" alt="" width="16" height="16" />
        <span>codepluse</span>
      </div>
      <div className="cp-go-titlebar-controls" data-tauri-drag-region="false">
        <button
          type="button"
          aria-label="最小化窗口"
          title="最小化"
          onPointerDown={(event) => event.stopPropagation()}
          onClick={() => void bridge.minimize()}
        >
          <Minus size={15} strokeWidth={1.5} />
        </button>
        <button
          type="button"
          aria-label={maximized ? "还原窗口" : "最大化窗口"}
          title={maximized ? "还原" : "最大化"}
          onPointerDown={(event) => event.stopPropagation()}
          onClick={() => void toggleMaximize()}
        >
          {maximized ? (
            <Copy size={13} strokeWidth={1.5} />
          ) : (
            <Square size={12} strokeWidth={1.5} />
          )}
        </button>
        <button
          type="button"
          className="cp-go-titlebar-close"
          aria-label="关闭窗口"
          title="关闭"
          onPointerDown={(event) => event.stopPropagation()}
          onClick={() => void bridge.close()}
        >
          <X size={15} strokeWidth={1.5} />
        </button>
      </div>
    </header>
  )
}
