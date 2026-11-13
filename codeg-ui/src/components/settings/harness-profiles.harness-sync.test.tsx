/**
 * The collaboration side must know about a harness the moment Settings → Agents
 * registers one. A custom ACP agent manages its own provider and model, so it
 * can never carry a model profile — but it is still a harness: it has to be
 * selectable as the main agent (stored bare, without a provider) and it has to
 * be accounted for on the sub-agent card rather than silently missing.
 */
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
  it("offers a newly added harness as the main agent and saves it bare", async () => {
    const user = userEvent.setup()
    render(wrap(<HarnessProfiles mode="main" />))
    const harness = await screen.findByRole("combobox", { name: "Harness" })
    expect(harness).toHaveTextContent("Claude Code")
    await user.click(harness)
    await user.click(await screen.findByRole("option", { name: "Qoder CLI" }))
    // Nothing to pin: the provider and model pickers belong to the harness's
    // own configuration, so the card says so instead of offering a dead form.
    await waitFor(() =>
      expect(
        screen.queryByRole("combobox", { name: "API provider" })
      ).not.toBeInTheDocument()
    )
    expect(
      screen.getByText(/manages its own provider and model/)
    ).toBeInTheDocument()
    await user.click(screen.getByRole("button", { name: "Save main agent" }))
    await waitFor(() =>
      expect(mocks.save).toHaveBeenCalledWith({
        agent_type: "custom:qoder",
        model: "",
        provider_id: "",
      })
    )
    await waitFor(() =>
      expect(getCachedMainBot()?.agentType).toBe("custom:qoder")
    )
  })

  it("names the harness it cannot build a model profile for", async () => {
    render(wrap(<HarnessProfiles mode="instances" />))
    await screen.findByRole("button", { name: "Add model configuration" })
    expect(await screen.findByText(/Qoder CLI/)).toBeInTheDocument()
    // Still not offered as a profile target: the backend cannot pin a
    // provider for it, so an option there would only fail on save.
    await expect(
      screen.findByRole("button", { name: "Add model configuration" })
    ).resolves.toBeEnabled()
  })
})
