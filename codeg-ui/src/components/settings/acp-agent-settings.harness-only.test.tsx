/**
 * Settings → Agents is the harness registry: one row per executable the app
 * can drive. The collaboration profiles (the main bot and the named sub-agent
 * copies) live in the same registry so conversations and delegation can address
 * them, but they are only a harness plus a pinned provider/model and are
 * curated under Settings → Collaboration — listing them here would offer a
 * second place to edit the same harness, with half the form missing.
 */
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"

import enMessages from "@/i18n/messages/en.json"
import { acpListAgents, listModelProviders } from "@/lib/api"
import type { AcpAgentInfo } from "@/lib/types"

import { AcpAgentSettings } from "./acp-agent-settings"

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>()
  return Object.fromEntries(
    Object.entries(actual).map(([name, value]) => [
      name,
      typeof value === "function" ? vi.fn(() => new Promise(() => {})) : value,
    ])
  )
})
vi.mock("next/navigation", () => ({
  // Read per call: the mock factory runs at import time, long before a test
  // picks the deep link it wants.
  useSearchParams: () =>
    new URLSearchParams(
      globalThis.__settingsSearchAgent
        ? { agent: globalThis.__settingsSearchAgent }
        : {}
    ),
}))
vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}))

declare global {
  var __settingsSearchAgent: string | undefined
}

function harness(overrides: Partial<AcpAgentInfo>): AcpAgentInfo {
  return {
    agent_type: "claude_code",
    registry_id: "claude-acp",
    name: "Claude Code",
    description: "",
    available: true,
    enabled: true,
    instance: false,
    real_tool: "claude",
    skills_capable: true,
    registry_version: null,
    supports_custom_version: false,
    distribution_type: "system",
    is_acp_adapter: true,
    custom_source: null,
    sort_order: 0,
    installed_version: "local",
    host_tools_agent_mode: true,
    env: {},
    config_json: null,
    config_file_path: null,
    codex_auth_json: null,
    cline_secrets_json: null,
    codex_config_toml: null,
    codex_model_catalog: null,
    codex_sandbox_settings: null,
    grok_config_toml: null,
    grok_settings: null,
    hermes_config_yaml: null,
    cursor_cli_config_json: null,
    cursor_settings: null,
    model_provider_id: null,
    icon_url: null,
    ...overrides,
  }
}

// What the Go registry actually returns on a machine with a sub-agent profile
// and a main bot configured: the harnesses plus their collaboration copies.
const CLAUDE = harness({})
const CODEX = harness({
  agent_type: "codex",
  registry_id: "codex-acp",
  name: "Codex",
  real_tool: "codex",
  sort_order: 1,
})
const QODER = harness({
  agent_type: "custom:qoder",
  registry_id: "qoder",
  name: "Qoder CLI",
  real_tool: "",
  instance: false,
  custom_source: "registry",
  distribution_type: "npx",
  is_acp_adapter: false,
  sort_order: 2,
})
const SUB = harness({
  agent_type: "inst_claude2",
  name: "克小德",
  instance: true,
  provider_id: "ccswitch:default",
  default_model: "gemini-3.8-flash",
  sort_order: 3,
})
const MAIN = harness({
  agent_type: "inst_main_claude_caad2c9f3149",
  name: "主智能体 · claude · qwen3.8-flash",
  real_tool: "claude",
  instance: true,
  provider_id: "ccswitch:default",
  default_model: "qwen3.8-flash",
  sort_order: 4,
})

function rows() {
  return Array.from(document.querySelectorAll("[data-agent-type]"), (el) =>
    el.getAttribute("data-agent-type")
  )
}

function selectedRow(): string | null {
  const hit = Array.from(
    document.querySelectorAll<HTMLElement>("[data-agent-type]")
  ).find((el) => el.className.includes("border-primary"))
  return hit?.getAttribute("data-agent-type") ?? null
}

async function renderPanel(agentType?: string) {
  globalThis.__settingsSearchAgent = agentType
  vi.mocked(acpListAgents).mockResolvedValue([CLAUDE, CODEX, QODER, SUB, MAIN])
  vi.mocked(listModelProviders).mockResolvedValue([])
  render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <AcpAgentSettings />
    </NextIntlClientProvider>
  )
  await screen.findByText("Agent List")
  await waitFor(() => expect(rows()).not.toHaveLength(0))
}

describe("AcpAgentSettings — the list holds harnesses only", () => {
  beforeEach(() => {
    vi.resetAllMocks()
    globalThis.__settingsSearchAgent = undefined
  })

  it("lists every harness and neither collaboration profile", async () => {
    await renderPanel()
    expect(rows()).toEqual(["claude_code", "codex", "custom:qoder"])
    expect(screen.queryByText("克小德")).not.toBeInTheDocument()
    expect(screen.queryByText(/主智能体 · claude/)).not.toBeInTheDocument()
  })

  it("keeps a custom harness row — it is a harness, not a profile", async () => {
    await renderPanel()
    expect(screen.getByText("Qoder CLI")).toBeInTheDocument()
  })

  it("opens the base harness when a profile asks for its settings", async () => {
    await renderPanel("inst_claude2")
    await waitFor(() => expect(selectedRow()).toBe("claude_code"))
  })

  it("still selects the harness a deep link names directly", async () => {
    await renderPanel("codex")
    await waitFor(() => expect(selectedRow()).toBe("codex"))
    // A plain click keeps working the same way.
    await userEvent.setup().click(screen.getByText("Qoder CLI"))
    await waitFor(() => expect(selectedRow()).toBe("custom:qoder"))
  })
})
