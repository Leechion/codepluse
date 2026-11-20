import { fireEvent, render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { beforeEach, describe, expect, it, vi } from "vitest"
import { useEffect } from "react"
import { SettingsTabs } from "./settings-tabs"
import { settingsRedirectTarget } from "./settings-redirect"

const state = vi.hoisted(() => ({ search: "", replace: vi.fn() }))
vi.mock("next/navigation", () => ({
  useSearchParams: () => new URLSearchParams(state.search),
  usePathname: () => "/settings/skills",
  useRouter: () => ({ replace: state.replace }),
}))

function Packs({ load }: { load: () => void }) {
  useEffect(load, [load])
  return <div>Pack catalogue</div>
}

function Panels({ load }: { load: () => void }) {
  return (
    <SettingsTabs
      panels={[
        {
          id: "installed",
          label: "Installed",
          content: <input aria-label="Draft" defaultValue="" />,
        },
        { id: "packs", label: "Packs", content: <Packs load={load} /> },
      ]}
    />
  )
}

beforeEach(() => {
  state.search = "locale=zh_cn&remoteConnectionId=7"
  state.replace.mockReset()
})

describe("merged settings tabs", () => {
  it("loads a panel only on first visit and retains the draft when switching back", async () => {
    const user = userEvent.setup()
    const load = vi.fn()
    render(<Panels load={load} />)
    expect(load).not.toHaveBeenCalled()
    fireEvent.change(screen.getByLabelText("Draft"), {
      target: { value: "Unsaved" },
    })
    await user.click(screen.getByRole("tab", { name: "Packs" }))
    expect(load).toHaveBeenCalledOnce()
    expect(state.replace).toHaveBeenLastCalledWith(
      "/settings/skills?locale=zh_cn&remoteConnectionId=7&view=packs",
      { scroll: false }
    )
    await user.click(screen.getByRole("tab", { name: "Installed" }))
    expect(screen.getByLabelText("Draft")).toHaveValue("Unsaved")
    await user.click(screen.getByRole("tab", { name: "Packs" }))
    expect(load).toHaveBeenCalledOnce()
  })

  it("opens the requested view directly without loading the other panel", () => {
    state.search += "&view=packs&tab=science"
    render(<Panels load={vi.fn()} />)
    expect(screen.getByRole("tab", { name: "Packs" })).toHaveAttribute(
      "aria-selected",
      "true"
    )
    expect(screen.queryByLabelText("Draft")).not.toBeInTheDocument()
  })

  it("keeps context and nested pack links when merging an old route", () => {
    expect(
      settingsRedirectTarget(
        "/settings/skills",
        "locale=zh_cn&remoteConnectionId=7&tab=science",
        "packs"
      )
    ).toBe(
      "/settings/skills?locale=zh_cn&remoteConnectionId=7&tab=science&view=packs"
    )
    expect(
      settingsRedirectTarget(
        "/settings/general",
        "locale=zh_cn",
        "quick-messages"
      )
    ).toBe("/settings/general?locale=zh_cn&view=quick-messages")
  })
})
