import { describe, expect, it } from "vitest"

import { settingsNavItemsFor } from "./settings-shell"

describe("settingsNavItemsFor", () => {
  it("keeps the supported Go navigation in both browser and desktop", () => {
    const labels = settingsNavItemsFor("web").map((item) => item.labelKey)
    expect(labels).toEqual(
      settingsNavItemsFor("tauri").map((item) => item.labelKey)
    )
    expect(labels).toEqual([
      "appearance",
      "general",
      "collaboration",
      "mcp",
      "skills",
      "agents",
      "browser",
      "shortcuts",
      "version_control",
      "logs",
    ])
  })

  it("keeps collaboration under General and the working browser settings", () => {
    const labels = settingsNavItemsFor("tauri").map((item) => item.labelKey)
    const after = (label: (typeof labels)[number]) =>
      labels[labels.indexOf(label) + 1]
    expect(after("general")).toBe("collaboration")
    expect(after("agents")).toBe("browser")
  })

  it("hides merged categories and unfinished services on desktop too", () => {
    const labels = settingsNavItemsFor("tauri").map((item) => item.labelKey)
    for (const removed of [
      "web_service",
      "computer_use",
      "chat_channels",
      "skill_packs",
      "quick_messages",
      "system",
    ]) {
      expect(labels).not.toContain(removed)
    }
  })
})
