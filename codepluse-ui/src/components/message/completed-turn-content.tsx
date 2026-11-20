"use client"

import { memo, useEffect, useMemo, useState } from "react"
import { ChevronRight } from "lucide-react"
import { useTranslations } from "next-intl"

import {
  splitTrailingAnswerParts,
  type AdaptedContentPart,
} from "@/lib/adapters/ai-elements-adapter"
import { buildActivity, hasVisibleAnswer } from "@/lib/activity/build-activity"
import { formatElapsedLabel } from "@/lib/format-elapsed"
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/instant-collapsible"
import { ActivityParts, ContentPartsRenderer } from "./content-parts-renderer"
import { useActivityLive } from "./activity/activity-live-context"
import { WorkingMark } from "./activity/rail-icon"

export interface SplitAssistantTurnParts {
  progress: AdaptedContentPart[]
  answer: AdaptedContentPart[]
}

export function splitAssistantTurnParts(
  parts: AdaptedContentPart[]
): SplitAssistantTurnParts {
  const { body, trailing } = splitTrailingAnswerParts(parts)
  return { progress: body, answer: trailing }
}

// Rows are virtualized: retain a reader's choice if a history row remounts.
const historyChoices = new WeakMap<AdaptedContentPart[], boolean>()

/** ZCode-style work history followed by the final answer. */
export const CompletedTurnContent = memo(function CompletedTurnContent({
  parts,
  durationMs,
  completed,
  currentRound = false,
}: {
  parts: AdaptedContentPart[]
  durationMs?: number | null
  completed: boolean
  currentRound?: boolean
  roundOpen?: boolean
  onRoundOpenChange?: (open: boolean) => void
  foldEpoch?: number
  pending?: boolean
}) {
  const isStreaming = !completed
  const split = useMemo(() => splitAssistantTurnParts(parts), [parts])
  const activity = useMemo(
    () => buildActivity(parts, { isStreaming, keepAnswer: isStreaming }),
    [parts, isStreaming]
  )
  const { awaitingUser } = useActivityLive()
  const [historyOpen, setHistoryOpen] = useState(
    () => historyChoices.get(parts) ?? false
  )
  // History may be re-adapted to a new array when usage or other metadata
  // arrives. Associate the current choice with the new array too, so virtual
  // scrolling doesn't silently close work the reader left open.
  useEffect(() => {
    historyChoices.set(parts, historyOpen)
  }, [parts, historyOpen])
  const t = useTranslations("Folder.chat.activity.row")
  const tElapsed = useTranslations("Folder.chat.liveTurnStats")
  // A stopped or failed turn without a final answer must remain readable.
  const foldable =
    completed &&
    activity.items.length > 0 &&
    hasVisibleAnswer(split.answer) &&
    !activity.summary.firstUnrecoveredKey &&
    !awaitingUser
  const duration =
    typeof durationMs === "number" && durationMs > 0
      ? formatElapsedLabel(durationMs, tElapsed)
      : null

  return (
    <div
      className="zcode-turn flex flex-col gap-5"
      data-reply-current={isStreaming || currentRound ? "" : undefined}
    >
      {foldable ? (
        <Collapsible
          open={historyOpen}
          onOpenChange={(next) => {
            historyChoices.set(parts, next)
            setHistoryOpen(next)
          }}
        >
          <div className="flex w-full items-center border-b border-border/50 pb-2">
            <CollapsibleTrigger className="group/history-message inline-flex max-w-full items-center gap-2 text-left text-sm text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
              <span>
                {duration
                  ? t("historyWorkedFor", { duration })
                  : t("historyWorked")}
              </span>
              <ChevronRight
                aria-hidden="true"
                className={`size-4 shrink-0 opacity-70 transition-transform ${historyOpen ? "rotate-90" : ""}`}
              />
            </CollapsibleTrigger>
          </div>
          {!historyOpen ? (
            <ActivityParts model={activity} isStreaming={false} pinnedOnly />
          ) : null}
          <CollapsibleContent>
            <div className="pt-5">
              <ActivityParts model={activity} isStreaming={false} />
            </div>
          </CollapsibleContent>
        </Collapsible>
      ) : (
        <ActivityParts model={activity} isStreaming={isStreaming} />
      )}
      {completed && split.answer.length > 0 ? (
        <ContentPartsRenderer
          parts={split.answer}
          role="assistant"
          isStreaming={false}
        />
      ) : null}
      {isStreaming && !awaitingUser ? <WorkingMark /> : null}
    </div>
  )
})
