import { act, render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { HarnessProfiles, ProviderModelPicker } from "./harness-profiles"
import { applyMainBotSettings, getCachedMainBot } from "@/lib/main-bot"

const mocks = vi.hoisted(() => ({
  models: vi.fn(),
  save: vi.fn(),
  create: vi.fn(),
  list: vi.fn(),
}))
vi.mock("@/lib/api", () => ({
  acpListAgents: async () => [
    {
      agent_type: "claude_code",
      real_tool: "claude",
      available: true,
      enabled: true,
    },
    {
      agent_type: "deepseek",
      real_tool: "dsh",
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
      {
        id: "subscription",
        agent: "claude",
        name: "Official subscription",
        hasKey: false,
        url: "",
      },
      {
        id: "dsh-native",
        agent: "dsh",
        name: "Native DSH",
        model: "dsh-small",
        hasKey: true,
        url: "https://dsh.test",
        source: "dsh",
      },
    ],
  }),
  getMainBotSettings: async () => ({ agent_type: "", model: "" }),
  setMainBotSettings: (...args: unknown[]) => mocks.save(...args),
  listHarnessProviderModels: (...args: unknown[]) => mocks.models(...args),
  createHarnessInstance: (...args: unknown[]) => mocks.create(...args),
  updateHarnessInstance: vi.fn(),
  deleteHarnessInstance: vi.fn(),
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
  mocks.models.mockResolvedValue({
    models: [{ id: "model-a" }, { id: "model-b" }],
  })
  mocks.save.mockImplementation(async (v) => ({
    ...v,
    agent_type: "inst_main_claude_api",
    base_agent_type: "claude_code",
  }))
  mocks.create.mockResolvedValue({})
})

describe("provider-bound harness profiles", () => {
  it("loads provider models, excludes subscription accounts and saves the main profile", async () => {
    const user = userEvent.setup()
    render(wrap(<HarnessProfiles mode="main" />))
    await waitFor(() =>
      expect(
        screen.getByRole("combobox", { name: "Harness" })
      ).toHaveTextContent("Claude Code")
    )
    await user.click(screen.getByRole("combobox", { name: "API provider" }))
    expect(
      screen.queryByRole("option", { name: "Official subscription" })
    ).not.toBeInTheDocument()
    await user.click(
      await screen.findByRole("option", { name: "Gateway · ccswitch" })
    )
    await waitFor(() => expect(mocks.models).toHaveBeenCalledWith("cc-api"))
    await user.click(screen.getByRole("combobox", { name: "Model" }))
    await user.click(await screen.findByRole("option", { name: "model-b" }))
    await user.click(screen.getByRole("button", { name: "Save main agent" }))
    await waitFor(() =>
      expect(mocks.save).toHaveBeenCalledWith({
        agent_type: "claude_code",
        model: "model-b",
        provider_id: "cc-api",
      })
    )
    await waitFor(() =>
      expect(getCachedMainBot()?.agentType).toBe("inst_main_claude_api")
    )
  })

  it("creates a DSH model profile with the provider read from native configuration", async () => {
    const user = userEvent.setup()
    render(wrap(<HarnessProfiles mode="instances" />))
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Add model configuration" })
      ).toBeEnabled()
    )
    await user.click(
      screen.getByRole("button", { name: "Add model configuration" })
    )
    await user.type(screen.getByLabelText("Configuration name"), "DSH small")
    await user.click(screen.getByRole("combobox", { name: "Harness" }))
    await user.click(
      await screen.findByRole("option", { name: "DeepSeek Harness" })
    )
    await user.click(screen.getByRole("combobox", { name: "API provider" }))
    await user.click(
      await screen.findByRole("option", { name: "Native DSH · dsh" })
    )
    await user.click(screen.getByRole("button", { name: "Save" }))
    await waitFor(() =>
      expect(mocks.create).toHaveBeenCalledWith({
        name: "DSH small",
        realTool: "dsh",
        providerId: "dsh-native",
        defaultModel: "dsh-small",
      })
    )
  })

  it("ignores a late model catalogue from the previously selected provider", async () => {
    let finishOld!: (v: unknown) => void
    mocks.models.mockImplementation((id) =>
      id === "old"
        ? new Promise((resolve) => {
            finishOld = resolve
          })
        : Promise.resolve({ models: [{ id: "new-model" }] })
    )
    const user = userEvent.setup()
    const view = render(
      wrap(<ProviderModelPicker provider="old" model="" onChange={vi.fn()} />)
    )
    await waitFor(() => expect(mocks.models).toHaveBeenCalledWith("old"))
    view.rerender(
      wrap(<ProviderModelPicker provider="new" model="" onChange={vi.fn()} />)
    )
    await waitFor(() => expect(mocks.models).toHaveBeenCalledWith("new"))
    await act(async () => finishOld({ models: [{ id: "old-model" }] }))
    await user.click(screen.getByRole("combobox", { name: "Model" }))
    expect(
      await screen.findByRole("option", { name: "new-model" })
    ).toBeInTheDocument()
    expect(
      screen.queryByRole("option", { name: "old-model" })
    ).not.toBeInTheDocument()
  })
})
