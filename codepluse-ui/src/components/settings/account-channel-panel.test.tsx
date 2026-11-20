import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { AccountChannelPanel } from "./account-channel-panel"

const mocks = vi.hoisted(() => ({
  list: vi.fn(),
  start: vi.fn(),
  status: vi.fn(),
  cancel: vi.fn(),
  logout: vi.fn(),
  openExternal: vi.fn(),
}))

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), message: vi.fn() },
}))
vi.mock("@/lib/api", () => ({
  listAccountChannels: mocks.list,
  startAccountChannelLogin: mocks.start,
  getAccountChannelLoginStatus: mocks.status,
  cancelAccountChannelLogin: mocks.cancel,
  logoutAccountChannel: mocks.logout,
}))
vi.mock("@/lib/open-external", () => ({
  openExternalUrl: mocks.openExternal,
}))

const channel = {
  id: "workbuddy",
  name: "WorkBuddy 国际版",
  configured: false,
  accounts: [],
  models: [],
}

function renderPanel(onChanged?: () => void) {
  return render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <AccountChannelPanel channelId="workbuddy" onChanged={onChanged} />
    </NextIntlClientProvider>
  )
}

beforeEach(() => {
  vi.clearAllMocks()
  mocks.list.mockResolvedValue([channel])
  mocks.start.mockResolvedValue({
    id: "login-1",
    provider: "workbuddy",
    state: "pending",
    url: "https://login.test/authorize",
  })
  mocks.status.mockResolvedValue({
    id: "login-1",
    provider: "workbuddy",
    state: "pending",
  })
})

it("shows the channel state and the manage-account entry", async () => {
  renderPanel()
  await waitFor(() => expect(mocks.list).toHaveBeenCalled())
  expect(screen.getByRole("button", { name: /manage account/i })).toBeTruthy()
})

it("starts the login and hands the url to the browser", async () => {
  const user = userEvent.setup()
  renderPanel()
  await waitFor(() => expect(mocks.list).toHaveBeenCalled())
  await user.click(screen.getByRole("button", { name: /manage account/i }))
  await waitFor(() => expect(mocks.start).toHaveBeenCalledWith("workbuddy"))
  await waitFor(() =>
    expect(mocks.openExternal).toHaveBeenCalledWith("https://login.test/authorize")
  )
})

it("signs the account out through the channel command", async () => {
  const user = userEvent.setup()
  mocks.list.mockResolvedValue([
    {
      ...channel,
      configured: true,
      accounts: [{ id: "acct-1", nickname: "Lee" }],
    },
  ])
  mocks.logout.mockResolvedValue({})
  renderPanel()
  await waitFor(() => expect(screen.getByText("Lee")).toBeTruthy())
  await user.click(screen.getByRole("button", { name: /sign out/i }))
  await waitFor(() =>
    expect(mocks.logout).toHaveBeenCalledWith("workbuddy", "acct-1")
  )
})
