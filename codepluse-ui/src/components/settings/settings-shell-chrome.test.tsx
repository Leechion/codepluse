import { render, screen } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { describe, expect, it, vi } from "vitest"
import { SettingsShell } from "./settings-shell"
import enMessages from "@/i18n/messages/en.json"

// The shell reads the route and the platform through these two; stubbing them
// keeps the test about the chrome the shell renders, not about Next internals.
let pathname = "/settings/general"
vi.mock("next/navigation", () => ({
  usePathname: () => pathname,
  useRouter: () => ({ push: vi.fn() }),
}))

let mobile = false
vi.mock("@/hooks/use-mobile", () => ({
  useIsMobile: () => mobile,
}))

vi.mock("@/lib/transport/detect", () => ({
  detectEnvironment: () => "web",
}))

vi.mock("@/components/layout/app-title-bar", () => ({
  AppTitleBar: ({ left, center }: { left?: any; center?: any }) => (
    <div data-testid="titlebar">
      {left}
      {center}
    </div>
  ),
}))

vi.mock("@/components/ui/app-toaster", () => ({ AppToaster: () => null }))

function mount(props: { onClose?: () => void; activeHref?: string } = {}) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <SettingsShell {...props}>
        <div>section</div>
      </SettingsShell>
    </NextIntlClientProvider>
  )
}

describe("SettingsShell chrome", () => {
  it("keeps the title bar and its label for the standalone settings window", () => {
    mobile = false
    mount()
    expect(screen.getByTestId("titlebar")).toHaveTextContent("Settings")
    expect(
      screen.queryByRole("button", { name: "回到工作区" })
    ).not.toBeInTheDocument()
  })

  it("drops the title bar entirely inside the overlay on desktop", () => {
    mobile = false
    mount({ onClose: () => undefined, activeHref: "/settings/general" })
    expect(screen.queryByTestId("titlebar")).not.toBeInTheDocument()
    // The exit lives in the rail instead, as the first item.
    const back = screen.getByRole("button", { name: "回到工作区" })
    expect(back).toBeInTheDocument()
    expect(screen.getByText("Preferences")).toBeInTheDocument()
  })

  it("keeps a label-free bar on narrow screens so the drawer trigger survives", () => {
    mobile = true
    mount({ onClose: () => undefined, activeHref: "/settings/general" })
    const bar = screen.getByTestId("titlebar")
    expect(bar).toBeInTheDocument()
    expect(bar).not.toHaveTextContent("Settings")
    expect(bar.querySelector("button")).toBeInTheDocument()
  })
})
