import { describe, expect, it } from "vitest"
import type { AdaptedContentPart } from "@/lib/adapters/ai-elements-adapter"
import { resolveAssistantCopyText } from "./assistant-copy"

describe("ZCode assistant copy", () => {
  it("copies the visible answer and plan once, without command output", () => {
    const plan = "# Plan\n\n1. Inspect\n2. Fix"
    const parts: AdaptedContentPart[] = [
      { type: "text", text: "Here is the plan." },
      {
        type: "tool-call",
        toolCallId: "p1",
        toolName: "ExitPlanMode",
        input: JSON.stringify({ plan }),
        state: "output-available",
        output: "User approved",
      },
      { type: "proposed-plan", markdown: plan, isStreaming: false },
      {
        type: "tool-call",
        toolCallId: "c1",
        toolName: "Bash",
        input: "{}",
        state: "output-available",
        output: "private command output",
      },
    ]
    expect(resolveAssistantCopyText(parts)).toBe(`Here is the plan.\n\n${plan}`)
  })

  it("copies a plan-only reply and ignores incomplete tool input", () => {
    const parts: AdaptedContentPart[] = [
      {
        type: "tool-call",
        toolCallId: "p1",
        toolName: "ExitPlanMode",
        input: '{"plan":',
        state: "input-available",
      },
      { type: "proposed-plan", markdown: "# Ready", isStreaming: false },
    ]
    expect(resolveAssistantCopyText(parts)).toBe("# Ready")
  })
})
