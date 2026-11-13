import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { beforeEach, expect, it, vi } from "vitest"
import { AccountChannels } from "./account-channels"

const mocks = vi.hoisted(() => ({ call: vi.fn(), open: vi.fn() }))
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call: mocks.call }),
}))
vi.mock("@/lib/go-desktop", () => ({
  goDesktopBridge: () => ({ openExternal: mocks.open }),
}))
vi.mock("sonner", () => ({ toast: { success: vi.fn() } }))
beforeEach(() => {
  vi.clearAllMocks()
  mocks.open.mockResolvedValue(undefined)
  mocks.call.mockImplementation(async (command: string) => {
    if (command === "codepluse_channel_status")
      return [
        {
          id: "qoder",
          name: "Qoder",
          configured: false,
          accounts: [],
          models: [],
        },
        {
          id: "qodercn",
          name: "Qoder CN",
          configured: false,
          accounts: [],
          models: [],
        },
      ]
    if (command === "codepluse_channel_login")
      return {
        id: "login-cn",
        provider: "qodercn",
        state: "pending",
        url: "https://qoder.cn/device/test",
      }
    if (command === "codepluse_channel_login_status")
      return {
        id: "login-cn",
        provider: "qodercn",
        state: "complete",
        url: "https://qoder.cn/device/test",
      }
    return {}
  })
})

it("logs into the selected independent product and refreshes its account/model metadata", async () => {
  const user = userEvent.setup(),
    changed = vi.fn()
  render(<AccountChannels provider="qodercn" onChanged={changed} />)
  await user.click(screen.getByRole("button", { name: "管理官网账号" }))
  expect(
    await screen.findByRole("heading", { name: "Qoder CN" })
  ).toBeInTheDocument()
  expect(
    screen.queryByRole("heading", { name: "Qoder", exact: true })
  ).not.toBeInTheDocument()
  await user.click(screen.getByRole("button", { name: "网页登录" }))
  expect(mocks.call).toHaveBeenCalledWith("codepluse_channel_login", {
    provider: "qodercn",
  })
  expect(mocks.open).toHaveBeenCalledWith("https://qoder.cn/device/test")
  await waitFor(() => expect(changed).toHaveBeenCalled(), { timeout: 3000 })
})

it("cancels the actual pending login when its dialog is dismissed", async () => {
  const user = userEvent.setup()
  render(<AccountChannels provider="qodercn" onChanged={() => {}} />)
  await user.click(screen.getByRole("button", { name: "管理官网账号" }))
  await user.click(await screen.findByRole("button", { name: "网页登录" }))
  await user.keyboard("{Escape}")
  expect(mocks.call).toHaveBeenCalledWith("codepluse_channel_cancel", {
    id: "login-cn",
  })
})
