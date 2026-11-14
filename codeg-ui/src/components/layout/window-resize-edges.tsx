"use client"

import { useEffect } from "react"

import { goDesktopBridge } from "@/lib/go-desktop"

/**
 * Resize grips for the frameless Tauri shell.
 *
 * `decorations(false)` removes the native resize frame that the WebView2 shell
 * keeps (`WS_THICKFRAME`), and a custom titlebar cannot bring it back by
 * itself. These strips sit on the window edge and start the platform's own
 * resize gesture, so dragging behaves exactly like a native border.
 *
 * Mounted only when the bridge exposes `beginResize` — the WebView2 shell has a
 * real frame already and must not get invisible strips inside its client area.
 *
 * The strips are created imperatively rather than as JSX: the page is
 * prerendered at build time, so a render-time `window` check would disagree
 * with the server output, and a state flag would trip
 * `react-hooks/set-state-in-effect`. Window chrome is not React state.
 */
const EDGES = [
  { dir: "n", className: "top-0 left-3 right-3 h-1.5 cursor-ns-resize" },
  { dir: "s", className: "bottom-0 left-3 right-3 h-1.5 cursor-ns-resize" },
  { dir: "w", className: "left-0 top-3 bottom-3 w-1.5 cursor-ew-resize" },
  { dir: "e", className: "right-0 top-3 bottom-3 w-1.5 cursor-ew-resize" },
  { dir: "nw", className: "left-0 top-0 h-3 w-3 cursor-nwse-resize" },
  { dir: "ne", className: "right-0 top-0 h-3 w-3 cursor-nesw-resize" },
  { dir: "sw", className: "bottom-0 left-0 h-3 w-3 cursor-nesw-resize" },
  { dir: "se", className: "bottom-0 right-0 h-3 w-3 cursor-nwse-resize" },
] as const

export function WindowResizeEdges() {
  useEffect(() => {
    const host = goDesktopBridge()
    if (!host?.beginResize) return
    const beginResize = host.beginResize

    const strips = EDGES.map(({ dir, className }) => {
      const strip = document.createElement("div")
      strip.setAttribute("aria-hidden", "true")
      strip.dataset.windowResizeEdge = dir
      strip.className = `fixed z-[2147483000] ${className}`
      strip.addEventListener("pointerdown", (event) => {
        // Left button only, and never let the strip double as a click.
        if (event.button !== 0) return
        event.preventDefault()
        event.stopPropagation()
        void beginResize(dir)
      })
      document.body.appendChild(strip)
      return strip
    })

    return () => {
      for (const strip of strips) strip.remove()
    }
  }, [])

  return null
}
