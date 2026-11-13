import { beforeEach, expect, it, vi } from "vitest"
const mocks = vi.hoisted(() => ({ get: vi.fn(), save: vi.fn() }))
vi.mock("@/lib/api", () => ({
  getMainBotSettings: () => mocks.get(),
  setMainBotSettings: (...args: unknown[]) => mocks.save(...args),
}))
import {
  applyMainBotSettings,
  loadMainBot,
  getCachedMainBot,
  mainBotAgentFor,
  withMainBotModel,
} from "./main-bot"
import { resolveDefaultAgent } from "./resolve-default-agent"
import { instanceBaseAgent } from "./types"

beforeEach(() => {
  applyMainBotSettings(null)
  vi.clearAllMocks()
})
it("uses the saved main agent ahead of folder defaults and inherited harnesses", () => {
  applyMainBotSettings({
    agent_type: "inst_main_claude_api",
    provider_id: "api",
    model: "chosen",
  })
  const main = mainBotAgentFor(["codex"])
  expect(main).toBe("inst_main_claude_api")
  expect(instanceBaseAgent(main!)).toBe("claude_code")
  expect(instanceBaseAgent("inst_main_dsh_api")).toBe("deepseek")
  expect(instanceBaseAgent("inst_main_codex_api")).toBe("codex")
  expect(
    resolveDefaultAgent({
      folderDefault: "codex",
      mainBot: main,
      inherit: "deepseek",
      sortedTypes: ["codex"],
      fresh: true,
    }).agentType
  ).toBe(main)
  expect(
    withMainBotModel(main!, {
      modeId: null,
      configValues: { model: "previous-model" },
    }).configValues?.model
  ).toBe("chosen")
})
it("does not let an older initial fetch overwrite a saved main-agent change", async () => {
  let resolve!: (v: unknown) => void
  mocks.get.mockReturnValue(
    new Promise((done) => {
      resolve = done
    })
  )
  const pending = loadMainBot(true)
  applyMainBotSettings({
    agent_type: "inst_main_dsh_api",
    provider_id: "native:dsh:api",
    model: "chosen",
  })
  resolve({ agent_type: "codex", model: "old" })
  await pending
  expect(getCachedMainBot()?.agentType).toBe("inst_main_dsh_api")
})
