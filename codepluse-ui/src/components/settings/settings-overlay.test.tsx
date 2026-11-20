import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { beforeEach, describe, expect, it, vi } from "vitest"
import { SettingsOverlayProvider } from "./settings-overlay"
import {
  registerSettingsOverlayHandler,
  settingsOverlayIsAvailable,
} from "@/lib/settings-overlay-bridge"
import { openSettingsWindow } from "@/lib/api"

// The shell and every section page are stubbed: this file's job is the routing
// decision (overlay instead of a second window), not the sections' own tests.
vi.mock("./settings-shell", () => ({
  SettingsShell: ({
    children,
    activeHref,
    onNavigate,
    onClose,
  }: {
    children: React.ReactNode
    activeHref?: string
    onNavigate?: (href: string) => void
    onClose?: () => void
  }) => (
    <div data-testid="shell" data-href={activeHref}>
      <button type="button" onClick={() => onNavigate?.("/settings/logs")}>
        go-logs
      </button>
      {/* The back-to-workspace affordance lives inside the shell, so the
        overlay must hand it a close handler. */}
      {onClose && (
        <button type="button" onClick={onClose}>
          back-to-workspace
        </button>
      )}
      {children}
    </div>
  ),
}))

// Literal paths only: `vi.mock` is hoisted, so a loop variable would not exist
// yet when the mock registers.
vi.mock("@/app/settings/appearance/page", () => ({
  default: () => <div data-testid="page">appearance-page</div>,
}))
vi.mock("@/app/settings/general/page", () => ({
  default: () => <div data-testid="page">general-page</div>,
}))
vi.mock("@/app/settings/collaboration/page", () => ({
  default: () => <div data-testid="page">collaboration-page</div>,
}))
vi.mock("@/app/settings/mcp/page", () => ({
  default: () => <div data-testid="page">mcp-page</div>,
}))
vi.mock("@/app/settings/skills/page", () => ({
  default: () => <div data-testid="page">skills-page</div>,
}))
vi.mock("@/app/settings/agents/page", () => ({
  default: () => <div data-testid="page">agents-page</div>,
}))
vi.mock("@/app/settings/browser/page", () => ({
  default: () => <div data-testid="page">browser-page</div>,
}))
vi.mock("@/app/settings/shortcuts/page", () => ({
  default: () => <div data-testid="page">shortcuts-page</div>,
}))
vi.mock("@/app/settings/version-control/page", () => ({
  default: () => <div data-testid="page">version-control-page</div>,
}))
vi.mock("@/app/settings/logs/page", () => ({
  default: () => <div data-testid="page">logs-page</div>,
}))

function mount() {
  return render(
    <SettingsOverlayProvider>
      <div>workspace</div>
    </SettingsOverlayProvider>
  )
}

describe("SettingsOverlayProvider", () => {
  beforeEach(() => {
    vi.clearAllMocks()
    // Leave the bridge empty for every test: register a throwaway handler and
    // immediately restore, which puts the bridge back to whatever it was
    // before (null on the first run, and never a leaked provider's handler).
    registerSettingsOverlayHandler(() => undefined)()
  })

  it("takes over openSettingsWindow instead of a second window", async () => {
    const openSpy = vi.spyOn(window, "open").mockReturnValue(null)
    mount()
    expect(settingsOverlayIsAvailable()).toBe(true)

    await openSettingsWindow("mcp")

    const dialog = await screen.findByRole("dialog")
    expect(dialog).toHaveAttribute("data-settings-overlay", "")
    expect(screen.getByTestId("shell")).toHaveAttribute(
      "data-href",
      "/settings/mcp"
    )
    expect(screen.getByTestId("page")).toHaveTextContent("mcp-page")
    // No popup attempt: the overlay answered before any window opening.
    expect(openSpy).not.toHaveBeenCalled()
    openSpy.mockRestore()
  })

  it("navigates inside the overlay without touching the URL", async () => {
    mount()
    await openSettingsWindow("mcp")
    await screen.findByRole("dialog")

    await userEvent.click(screen.getByRole("button", { name: "go-logs" }))
    await waitFor(() =>
      expect(screen.getByTestId("shell")).toHaveAttribute(
        "data-href",
        "/settings/logs"
      )
    )
    expect(screen.getByTestId("page")).toHaveTextContent("logs-page")
    expect(window.location.pathname).toBe("/")
  })

  it("hands back to the window path once unmounted", async () => {
    const view = mount()
    await openSettingsWindow("general")
    await screen.findByRole("dialog")

    await userEvent.keyboard("{Escape}")
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument()
    )

    // Unregistering restores the previous handler rather than clearing it, so
    // the observable contract is: with no provider mounted, the call is no
    // longer answered by an overlay and falls back to opening a window.
    view.unmount()
    const openSpy = vi.spyOn(window, "open").mockReturnValue(null)
    // The stubbed popup is blocked, which is exactly the point: the call must
    // reach the window-opening path again.
    await openSettingsWindow("general").catch(() => undefined)
    await waitFor(() => expect(openSpy).toHaveBeenCalled())
    openSpy.mockRestore()
  })

  it("closes through the back affordance inside the shell", async () => {
    mount()
    await openSettingsWindow("general")
    await screen.findByRole("dialog")

    // No outer toolbar of its own: the only exit is the shell's back button.
    expect(
      screen.queryByRole("button", { name: "关闭设置" })
    ).not.toBeInTheDocument()

    await userEvent.click(screen.getByRole("button", { name: "back-to-workspace" }))
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument()
    )
  })

  it("forwards the deep-linked agent type to the agents section", async () => {
    mount()
    await openSettingsWindow("agents", { agentType: "codex" })
    await screen.findByRole("dialog")
    expect(screen.getByTestId("shell")).toHaveAttribute(
      "data-href",
      "/settings/agents"
    )
    expect(screen.getByTestId("page")).toHaveTextContent("agents-page")
  })
})
