import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { expect, it, vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { AddCustomAgentDialog } from "./add-custom-agent-dialog"

const mocks = vi.hoisted(() => ({ catalog: vi.fn() }))
vi.mock("@/lib/api", () => ({
  acpFetchRegistryCatalog: () => mocks.catalog(),
  acpCurrentPlatform: async () => "windows-x86_64",
  acpAddRegistryAgent: vi.fn(),
  acpListCustomAgents: async () => [],
  acpSaveCustomAgent: vi.fn(),
}))

it("shows the backend error message and can reload the real catalog after retry", async () => {
  mocks.catalog
    .mockRejectedValueOnce({
      code: "network_error",
      message: "ACP registry returned HTTP 503",
    })
    .mockResolvedValueOnce([
      {
        registryId: "fixture-agent",
        name: "Fixture ACP Agent",
        description: "Protocol test agent",
        distributionKinds: ["npx"],
        builtin: false,
        installed: false,
        supportedOnPlatform: true,
        spec: { npx: { package: "fixture-agent" } },
      },
    ])
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      <AddCustomAgentDialog open onOpenChange={vi.fn()} onAdded={vi.fn()} />
    </NextIntlClientProvider>
  )
  expect(
    await screen.findByText("ACP registry returned HTTP 503")
  ).toBeInTheDocument()
  expect(screen.queryByText("[object Object]")).not.toBeInTheDocument()
  await userEvent.click(
    screen.getByRole("button", {
      name: messages.AcpAgentSettings.customAgentRetry,
    })
  )
  expect(await screen.findByText("Fixture ACP Agent")).toBeInTheDocument()
})
