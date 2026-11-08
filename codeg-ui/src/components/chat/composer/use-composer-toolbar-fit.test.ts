import { describe, expect, it, vi } from "vitest"

import { fitComposerToolbar } from "./use-composer-toolbar-fit"

function rect(width: number): DOMRect {
  return { width } as DOMRect
}

describe("ZCode composer toolbar fit", () => {
  it("compacts the permission control when space is tight and restores it when space returns", () => {
    const root = document.createElement("div")
    root.innerHTML = `<div data-composer-leading-actions><div data-composer-leading-content><button data-composer-collapse-priority="1">Permission</button></div></div><div data-composer-trailing-actions>Model</div>`
    const available = root.querySelector<HTMLElement>(
      "[data-composer-leading-actions]"
    )!
    const content = root.querySelector<HTMLElement>(
      "[data-composer-leading-content]"
    )!
    const permission = root.querySelector<HTMLElement>("button")!
    const trailing = root.querySelector<HTMLElement>(
      "[data-composer-trailing-actions]"
    )!
    let width = 180
    vi.spyOn(root, "getBoundingClientRect").mockImplementation(() =>
      rect(width)
    )
    vi.spyOn(available, "getBoundingClientRect").mockReturnValue(rect(80))
    vi.spyOn(content, "getBoundingClientRect").mockImplementation(() =>
      rect(permission.dataset.composerCompact ? 40 : 100)
    )
    vi.spyOn(trailing, "getBoundingClientRect").mockReturnValue(rect(60))

    fitComposerToolbar(root)
    expect(permission.dataset.composerCompact).toBe("true")
    width = 300
    vi.mocked(available.getBoundingClientRect).mockReturnValue(rect(200))
    fitComposerToolbar(root)
    expect(permission.dataset.composerCompact).toBeUndefined()
  })
})
