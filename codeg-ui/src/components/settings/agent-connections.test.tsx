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
