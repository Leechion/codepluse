/** Registered native account harnesses also support collaboration profiles. */
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { HarnessProfiles } from "./harness-profiles"
import { applyMainBotSettings, getCachedMainBot } from "@/lib/main-bot"

const mocks = vi.hoisted(() => ({ save: vi.fn() }))
vi.mock("@/lib/api", () => ({
  acpListAgents: async () => [
    {
      agent_type: "claude_code",
      name: "Claude Code",
      real_tool: "claude",
      available: true,
      enabled: true,
    },
    {
      agent_type: "custom:qoder",
      name: "Qoder CLI",
      real_tool: "",
      available: true,
      enabled: true,
    },
  ],
  listHarnessConnections: async () => [
    {
      agent_type: "claude_code",
      tool: "claude",
      account: {
        supported: true,
        configured: true,
        managed: false,
        source: "fixture",
        models: [],
      },
      defaults: { auth_mode: "api", provider_id: "cc-api", model: "model-a" },
    },
    {
      agent_type: "custom:qoder",
      tool: "custom:qoder",
      account: {
        supported: true,
        configured: false,
        managed: true,
        source: "fixture",
        models: [],
      },
      defaults: { auth_mode: "", provider_id: "", model: "" },
    },
  ],
  listHarnessInstances: async () => [],
  listHarnessProviders: async () => ({
    providers: [
      {
        id: "cc-api",
        agent: "claude",
        name: "Gateway",
        model: "model-a",
        hasKey: true,
        url: "https://provider.test",
        source: "ccswitch",
      },
    ],
  }),
  getMainBotSettings: async () => ({ agent_type: "", model: "" }),
  setMainBotSettings: (...args: unknown[]) => mocks.save(...args),
  listHarnessProviderModels: async () => ({ models: [{ id: "model-a" }] }),
  createHarnessInstance: async () => ({}),
  updateHarnessInstance: async () => ({}),
  deleteHarnessInstance: async () => ({}),
}))
vi.mock("@/lib/transport", () => ({ getTransport: () => ({ call: vi.fn() }) }))
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }))

const wrap = (children: React.ReactNode) => (
  <NextIntlClientProvider locale="en" messages={messages}>
    {children}
  </NextIntlClientProvider>
)

beforeEach(() => {
  vi.clearAllMocks()
  applyMainBotSettings(null)
  mocks.save.mockImplementation(async (v) => v)
})

describe("harness registry sync on the collaboration card", () => {
  it("offers a newly added harness as the main agent and saves a subscription profile", async () => {
    const user = userEvent.setup()
    render(wrap(<HarnessProfiles mode="main" />))
    const harness = await screen.findByRole("combobox", { name: "Agent" })
    expect(harness).toHaveTextContent("Claude Code")
    await user.click(harness)
    await user.click(await screen.findByRole("option", { name: "Qoder CLI" }))
    await waitFor(() =>
      expect(
        screen.queryByRole("combobox", { name: "API provider" })
      ).not.toBeInTheDocument()
    )
    expect(
      screen.getByText(messages.Connections.accountManaged)
    ).toBeInTheDocument()
    await user.click(screen.getByRole("button", { name: "Save main agent" }))
    await waitFor(() =>
      expect(mocks.save).toHaveBeenCalledWith({
        agent_type: "custom:qoder",
        model: "",
        provider_id: "",
        auth_mode: "subscription",
      })
    )
    await waitFor(() =>
      expect(getCachedMainBot()?.agentType).toBe("custom:qoder")
    )
  })

  it("offers registered account harnesses in the sub-agent editor", async () => {
    render(wrap(<HarnessProfiles mode="instances" />))
    await screen.findByRole("button", { name: "Add model configuration" })
    const user = userEvent.setup()
    await user.click(
      screen.getByRole("button", { name: "Add model configuration" })
    )
    await user.click(screen.getByRole("combobox", { name: "Agent" }))
    expect(
      await screen.findByRole("option", { name: "Qoder CLI" })
    ).toBeInTheDocument()
  })
})
