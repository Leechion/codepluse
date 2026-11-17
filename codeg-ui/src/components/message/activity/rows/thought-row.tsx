"use client"

import { memo } from "react"
import { BrainIcon } from "lucide-react"

import { rowIdentity } from "@/lib/activity/build-activity"
import type { ActivityStep } from "@/lib/activity/types"
import { RailIcon } from "../rail-icon"
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
      className="activity-item zcode-thought-row flex items-start gap-2"
      data-marker="ring"
      data-status={step.streaming ? "running" : "succeeded"}
      data-step-key={step.key}
      data-motion-key={rowIdentity(step.key)}
    >
      <span className="activity-marker relative mt-1 flex size-5 shrink-0 -translate-y-0.5 items-center justify-center">
        <RailIcon
          icon={BrainIcon}
          status={step.streaming ? "running" : "succeeded"}
        />
      </span>
      <Reasoning
        className="min-w-0 flex-1"
        isStreaming={step.streaming}
        autoCollapseKey={step.streaming ? null : "complete"}
        open={open}
        onOpenChange={setOpen}
      >
        <ReasoningTrigger
          className="h-6 leading-6"
          showIcon={false}
          streamingText={step.part.content}
        />
        <div data-conversation-selectable="true">
          <ReasoningContent>{step.part.content}</ReasoningContent>
        </div>
      </Reasoning>
    </li>
  )
})
