import { beforeEach, describe, expect, it, vi } from "vitest"

const windowApi = vi.hoisted(() => ({
  startResizeDragging: vi.fn(async () => {}),
}))

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => windowApi,
}))

import { goDesktopBridge } from "./go-desktop"

describe("go desktop window bridge", () => {
  beforeEach(() => {
    windowApi.startResizeDragging.mockClear()
  })

  it("maps all eight edge grips to Tauri resize directions", async () => {
    const host = window as unknown as Record<string, unknown>
    host.__codegGoOpenWindow = vi.fn(async () => {})
    host.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" } },
    }

    try {
      const bridge = goDesktopBridge()
      expect(bridge?.beginResize).toBeTypeOf("function")

      for (const [edge, direction] of [
        ["n", "North"],
        ["ne", "NorthEast"],
        ["nw", "NorthWest"],
        ["s", "South"],
        ["se", "SouthEast"],
        ["sw", "SouthWest"],
        ["e", "East"],
        ["w", "West"],
      ] as const) {
        await bridge?.beginResize?.(edge)
        expect(windowApi.startResizeDragging).toHaveBeenLastCalledWith(
          direction
        )
      }
    } finally {
      delete host.__codegGoOpenWindow
      delete host.__TAURI_INTERNALS__
    }
  })
})
