import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { AgentConnections } from "./agent-connections"

const mocks = vi.hoisted(() => ({ save: vi.fn(), call: vi.fn() }))
vi.mock("next/navigation", () => ({
  useSearchParams: () => new URLSearchParams(),
}))
vi.mock("./add-custom-agent-dialog", () => ({
  AddCustomAgentDialog: () => null,
}))
vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), message: vi.fn() },
}))
vi.mock("@/lib/transport", () => ({
  getTransport: () => ({ call: mocks.call }),
}))
vi.mock("@/lib/api", () => ({
  acpListAgents: async () => [
    {
      agent_type: "codex",
      name: "Codex",
      real_tool: "codex",
      enabled: true,
      available: true,
    },
    ...[
      ["account_buddy", "WorkBuddy 国内版", "account:buddy"],
      ["account_workbuddy", "WorkBuddy 国际版", "account:workbuddy"],
      ["account_qoder", "Qoder", "account:qoder"],
      ["account_qodercn", "Qoder CN", "account:qodercn"],
    ].map(([agent_type, name, real_tool]) => ({
      agent_type,
      name,
      real_tool,
      enabled: true,
      available: true,
      // Older Go backends marked any non-empty wire ID as a profile. Fixed
      // account-agent IDs are stable identities, not model-specific profiles.
      instance: true,
    })),
    {
      agent_type: "inst_qoder_model_b",
      name: "Qoder model B",
      real_tool: "account:qoder",
      enabled: true,
      available: true,
      instance: true,
    },
  ],
  acpDeleteCustomAgent: vi.fn(),
  listHarnessProviders: async () => ({
    providers: [
      {
        id: "native:codex:gateway",
        name: "Native Codex",
        agent: "codex",
        source: "codex",
        url: "https://gateway.test",
        hasKey: true,
        model: "model-a",
        models: [{ id: "model-a" }],
        protocol: "openai",
      },
      {
        id: "ccswitch:claude",
        name: "Claude supplier",
        agent: "claude",
        hasKey: true,
        url: "https://claude.test",
        model: "claude-a",
      },
    ],
  }),
  listHarnessConnections: async () => [
    {
      agent_type: "codex",
      tool: "codex",
      account: {
        supported: true,
        configured: true,
        managed: false,
        source: "native",
        models: [{ id: "account-model" }],
      },
      defaults: {
        auth_mode: "api",
        provider_id: "native:codex:gateway",
        model: "model-a",
      },
    },
  ],
  listHarnessProviderModels: async () => ({ models: [{ id: "model-a" }] }),
  saveHarnessConnection: (...args: unknown[]) => mocks.save(...args),
}))
beforeEach(() => {
  vi.clearAllMocks()
  mocks.save.mockResolvedValue({})
  mocks.call.mockResolvedValue({ id: "copy" })
})
const view = () =>
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <AgentConnections />
    </NextIntlClientProvider>
  )

it("lets Codex use the native official account without an API supplier", async () => {
  const user = userEvent.setup()
  view()
  await screen.findByRole("button", { name: "Native Codex" })
  expect(screen.queryByText("Claude supplier")).not.toBeInTheDocument()
  await user.click(
    screen.getByRole("combobox", { name: messages.Connections.connectionType })
  )
  await user.click(
    await screen.findByRole("option", {
      name: messages.Connections.subscription,
    })
  )
  expect(screen.queryByLabelText("Base URL")).not.toBeInTheDocument()
  expect(
    screen.getByText(messages.Connections.credentialsFound)
  ).toBeInTheDocument()
  await user.click(
    screen.getByRole("button", { name: messages.Connections.useAccount })
  )
  await waitFor(() =>
    expect(mocks.save).toHaveBeenCalledWith({
      agentType: "codex",
      authMode: "subscription",
      model: "",
    })
  )
})

it("lists each independent account agent and hides its named model profiles", async () => {
  const user = userEvent.setup()
  view()

  await screen.findByRole("button", { name: "Native Codex" })
  await user.click(screen.getByRole("combobox", { name: messages.Connections.harness }))

  const labels = (await screen.findAllByRole("option")).map(
    (option) => option.textContent || ""
  )
  for (const name of ["WorkBuddy 国内版", "WorkBuddy 国际版", "Qoder", "Qoder CN"]) {
    expect(labels.some((label) => label.includes(name))).toBe(true)
  }
  expect(labels.some((label) => label.includes("Qoder model B"))).toBe(false)
})

it("saves edits to the existing API supplier without creating a copy or retyping its key", async () => {
  const user = userEvent.setup()
  view()
  await screen.findByRole("button", { name: "Native Codex" })
  const input = screen.getByLabelText("Base URL")
  await user.clear(input)
  await user.type(input, "https://edited.test")
  expect(
    screen.queryByRole("button", { name: messages.Connections.saveFirst })
  ).not.toBeInTheDocument()
  await user.click(
    screen.getByRole("button", { name: messages.Connections.saveChanges })
  )
  await waitFor(() =>
    expect(mocks.call).toHaveBeenCalledWith(
      "codepluse_provider_save",
      expect.objectContaining({
        id: "native:codex:gateway",
        url: "https://edited.test",
        key: "",
        agent: "codex",
        protocol: "openai-responses",
      })
    )
  )
})
