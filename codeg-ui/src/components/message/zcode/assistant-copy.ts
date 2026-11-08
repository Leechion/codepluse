// Adapted from ZCode's resolveAssistantCopyText, Apache-2.0.
// See THIRD_PARTY_NOTICES.md for provenance.
import type { AdaptedContentPart } from "@/lib/adapters/ai-elements-adapter"
import {
  asRecord,
  extractPlanMarkdown,
  isPlanModeToolName,
} from "@/lib/plan-parse"

/** Copy the assistant's text and visible plan, never ordinary tool output. */
export function resolveAssistantCopyText(
  parts: readonly AdaptedContentPart[]
): string {
  const text: string[] = []
  const plans: string[] = []
  const visit = (items: readonly AdaptedContentPart[]) => {
    for (const part of items) {
      if (part.type === "text" && part.text.trim()) text.push(part.text)
      else if (part.type === "proposed-plan" && part.markdown.trim())
        plans.push(part.markdown)
      else if (part.type === "goal-run") visit(part.items)
      else if (
        part.type === "tool-call" &&
        isPlanModeToolName(part.toolName) &&
        part.input
      ) {
        try {
          const input = asRecord(JSON.parse(part.input))
          const plan = input ? extractPlanMarkdown(input) : null
          if (plan) plans.push(plan)
        } catch {
          // An incomplete tool payload has no copyable plan yet.
        }
      }
    }
  }
  visit(parts)
  const included = new Set(text)
  for (const plan of plans) {
    if (!included.has(plan)) {
      text.push(plan)
      included.add(plan)
    }
  }
  return text.join("\n\n")
}
