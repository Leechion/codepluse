"use client"

import { memo } from "react"

import { rowIdentity } from "@/lib/activity/build-activity"
import type { ActivityStep } from "@/lib/activity/types"
import { useDisclosure } from "../disclosure-store"
import {
  Reasoning,
  ReasoningContent,
  ReasoningTrigger,
} from "../../zcode/reasoning"

type ThoughtStep = Extract<ActivityStep, { type: "thought" }>

/** ZCode's thought block, connected to the existing session disclosure state. */
export const ThoughtRow = memo(function ThoughtRow({
  step,
}: {
  step: ThoughtStep
}) {
  const [open, setOpen] = useDisclosure(step.key, { defaultOpen: false })
  if (step.streaming && !step.part.content.trim()) return null

  return (
    <li
      className="activity-item zcode-thought-row"
      data-marker="ring"
      data-status={step.streaming ? "running" : "succeeded"}
      data-step-key={step.key}
      data-motion-key={rowIdentity(step.key)}
    >
      <Reasoning
        className="w-full"
        isStreaming={step.streaming}
        autoCollapseKey={step.streaming ? null : "complete"}
        open={open}
        onOpenChange={setOpen}
      >
        <ReasoningTrigger streamingText={step.part.content} />
        <div data-conversation-selectable="true">
          <ReasoningContent>{step.part.content}</ReasoningContent>
        </div>
      </Reasoning>
    </li>
  )
})
