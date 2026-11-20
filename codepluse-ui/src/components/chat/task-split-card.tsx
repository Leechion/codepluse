"use client"

/**
 * Task-split card: the main bot proposed a fan-out (`propose_task_split`), and
 * this IS the decision point — the proposal tool returned immediately, so
 * nothing parks the agent's turn; confirming HERE is what dispatches the
 * children. Each row is editable (title, prompt, target harness instance,
 * model), tasks can be removed (to 1) or added (to 8, the backend's validation
 * window), and confirming sends the edited plan. Targets come from the shared
 * agent registry (harness instances first — they pin their own model), and a
 * chip click adopts the instance's pinned `default_model` unless the user has
 * typed their own. Modeled on `PlanApprovalCard`'s in-flight/disable pattern:
 * on success the backend's `task_split_resolved` clears `pendingSplit` and
 * unmounts this card, so controls stay disabled rather than flashing back on.
 */

import { useCallback, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { Loader2, Plus, Sparkles, Trash2 } from "lucide-react"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { useAcpAgents } from "@/hooks/use-acp-agents"
import type { PendingTaskSplit } from "@/contexts/acp-connections-context"
import { getAgentLabel } from "@/lib/custom-agents"

/** Backend `proposeTaskSplit` validation window — stay inside it or the
 *  resolve call is rejected. */
const MIN_TASKS = 1
const MAX_TASKS = 8

interface TaskRow {
  title: string
  prompt: string
  agent_type: string
  model: string
  /** True while the row still carries the server's auto-assigned target. */
  auto: boolean
}

interface TaskSplitCardProps {
  /** The awaiting-decision split. Rendered only when one is pending. */
  split: PendingTaskSplit
  /** The conversation's own agent wire — offered as a target (a task may
   *  legitimately stay on the main bot) and used to break label ties. */
  agentType?: string | null
  /**
   * Answers the card. `approve` dispatches the (possibly edited) task list;
   * `reject` retires the proposal. Returns a promise so the card can hold an
   * in-flight state and surface a retryable error on failure.
   */
  onRespond: (
    splitId: string,
    action: "approve" | "reject",
    tasks?: Array<{
      title: string
      prompt: string
      agent_type: string
      model: string
    }>
  ) => void | Promise<void>
}

export function TaskSplitCard({
  split,
  agentType,
  onRespond,
}: TaskSplitCardProps) {
  const t = useTranslations("Folder.chat.taskSplitCard")
  const { agents } = useAcpAgents()

  // Candidates mirror the Go side's `splitAgentEnabled`: registered AND
  // available AND enabled. Instances sort first — they pin a provider+model,
  // which is the whole point of the fan-out.
  const candidates = useMemo(() => {
    const enabled = agents.filter((a) => a.available && a.enabled)
    const score = (a: (typeof enabled)[number]) =>
      a.instance ? 0 : a.agent_type === agentType ? 1 : 2
    return [...enabled].sort(
      (x, y) => score(x) - score(y) || x.name.localeCompare(y.name)
    )
  }, [agents, agentType])

  // Seed local rows from the wire card; the split_id key at the mount sites
  // remounts this component when the slot is reused for a new card, so
  // per-row state never needs syncing back from the prop. The initializer
  // runs once; a registry that lands later never rewrites edits in flight.
  // A row whose target the registry already knows about adopts that target's
  // pin when the wire left `model` blank — the server resolves pins too, so
  // this only matters when the card was raised before the registry loaded.
  const [rows, setRows] = useState<TaskRow[]>(() =>
    split.tasks.map((task) => {
      const agent = task.agent_type ?? ""
      const pin =
        candidates.find((a) => a.agent_type === agent)?.default_model?.trim() ??
        ""
      return {
        title: task.title ?? "",
        prompt: task.prompt ?? "",
        agent_type: agent,
        // The server always resolves a pin into `model` when the card is
        // raised with a loaded registry; this fallback only covers a card
        // raised before it. A genuinely unpinned target stays blank (=
        // target's own default).
        model: task.model ?? pin,
        auto: task.auto === true,
      }
    })
  )
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState(false)
  const inFlight = useRef(false)

  const defaultTarget = useCallback((): TaskRow => {
    const own = agentType
      ? candidates.find((a) => a.agent_type === agentType)
      : undefined
    const pick = own ?? candidates[0]
    return {
      title: "",
      prompt: "",
      agent_type: pick?.agent_type ?? agentType ?? "",
      model: pick?.default_model?.trim() || "",
      auto: false,
    }
  }, [agentType, candidates])

  const patch = (index: number, next: Partial<TaskRow>) =>
    setRows((cur) =>
      cur.map((row, i) => (i === index ? { ...row, ...next } : row))
    )

  // Choosing a target adopts its pinned default model — the row's model field
  // is trusted as exactly what the user sees, so a hand-typed value only
  // survives when it isn't the outgoing target's own pin.
  const chooseTarget = (index: number, agentTypeWanted: string) => {
    setRows((cur) =>
      cur.map((row, i) => {
        if (i !== index) return row
        const info = candidates.find((a) => a.agent_type === agentTypeWanted)
        const pinned = info?.default_model?.trim() || ""
        // Hand-typed = non-empty and not what the OLD target carried — the
        // row's model field is the only record of the old pin once the row
        // itself is gone, so it is compared before anything else changes.
        const oldPin =
          candidates.find((a) => a.agent_type === row.agent_type)
            ?.default_model?.trim() ?? ""
        const handTyped = row.model !== "" && row.model !== oldPin
        return {
          ...row,
          agent_type: agentTypeWanted,
          model: handTyped ? row.model : pinned,
          auto: false,
        }
      })
    )
  }

  const chooseModel = (index: number, model: string) =>
    setRows((cur) =>
      cur.map((row, i) => (i === index ? { ...row, model, auto: false } : row))
    )

  const canRemove = rows.length > MIN_TASKS

  const run = async (action: "approve" | "reject") => {
    if (inFlight.current) return
    inFlight.current = true
    setSubmitting(true)
    setError(false)
    try {
      await onRespond(
        split.split_id,
        action,
        action === "approve"
          ? rows.map((row) => ({
              // Backend rule: an empty title is derived from the prompt and
              // an empty model re-resolves at dispatch. Send the trimmed
              // field verbatim so both rules run server-side on exactly what
              // the card showed — including when the registry was cold at
              // seed time and the pin was left blank.
              title: row.title.trim(),
              prompt: row.prompt,
              agent_type: row.agent_type,
              model: row.model.trim(),
            }))
          : undefined
      )
      inFlight.current = false
      // Stay disabled on success: the resolved event unmounts us; re-enabling
      // would flash a live card that answers nothing.
    } catch {
      setError(true)
      setSubmitting(false)
      inFlight.current = false
    }
  }

  const approveDisabled =
    submitting || rows.length < MIN_TASKS || rows.some((r) => !r.prompt.trim())

  return (
    <div className="w-full space-y-3 rounded-lg border border-border/60 bg-card/60 p-4">
      <div className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-1.5 text-sm font-medium text-foreground">
          <Sparkles className="size-4 shrink-0 text-muted-foreground" />
          <span className="truncate">{t("title")}</span>
        </div>
        <span className="shrink-0 text-xs text-muted-foreground">
          {t("taskCount", { count: rows.length })}
        </span>
      </div>

      <p className="text-xs text-muted-foreground">{t("subtitle")}</p>

      <div className="max-h-[min(46vh,26rem)] space-y-2 overflow-y-auto pr-1">
        {rows.map((row, index) => (
          <div
            key={index}
            className="space-y-2 rounded-md border border-border/60 bg-muted/20 p-2.5"
          >
            <div className="flex items-center gap-2">
              <span className="shrink-0 text-xs font-medium text-muted-foreground tabular-nums">
                {index + 1}
              </span>
              <Input
                value={row.title}
                onChange={(e) => patch(index, { title: e.target.value })}
                placeholder={t("titlePlaceholder")}
                disabled={submitting}
                className="h-8 flex-1 text-sm"
              />
              {row.auto && (
                <span className="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-3xs text-muted-foreground">
                  {t("autoBadge")}
                </span>
              )}
              <Button
                variant="ghost"
                size="icon"
                className="size-7 shrink-0 text-muted-foreground"
                disabled={submitting || !canRemove}
                title={t("removeTask")}
                onClick={() =>
                  setRows((cur) => cur.filter((_, i) => i !== index))
                }
              >
                <Trash2 className="size-3.5" />
              </Button>
            </div>

            <Textarea
              value={row.prompt}
              onChange={(e) => patch(index, { prompt: e.target.value })}
              placeholder={t("promptPlaceholder")}
              disabled={submitting}
              rows={2}
              className="text-xs"
            />

            <div className="flex flex-wrap items-center gap-1">
              {candidates.map((cand) => {
                const selected = cand.agent_type === row.agent_type
                return (
                  <button
                    key={cand.agent_type}
                    type="button"
                    disabled={submitting}
                    onClick={() => chooseTarget(index, cand.agent_type)}
                    className={
                      "rounded-full border px-2 py-0.5 text-3xs transition-colors disabled:opacity-50 " +
                      (selected
                        ? "border-primary bg-primary/10 text-foreground"
                        : "border-border/60 text-muted-foreground hover:bg-muted/40")
                    }
                    title={
                      cand.default_model
                        ? `${cand.name} · ${cand.default_model}`
                        : cand.name
                    }
                  >
                    {/* Registry name wins (an instance's is its configured
                        display name, e.g. "claude 2号"); the label covers an
                        instance whose list row hasn't loaded yet. */}
                    {cand.name || getAgentLabel(cand.agent_type)}
                  </button>
                )
              })}
              {/* A target the registry doesn't (no longer) know about still
                  renders, so a stale proposal never loses its row. */}
              {row.agent_type &&
                !candidates.some((c) => c.agent_type === row.agent_type) && (
                  <span className="rounded-full border border-border/60 px-2 py-0.5 text-3xs text-muted-foreground">
                    {getAgentLabel(row.agent_type)}
                  </span>
                )}
            </div>

            <Input
              value={row.model}
              onChange={(e) => chooseModel(index, e.target.value)}
              placeholder={t("modelPlaceholder")}
              disabled={submitting}
              className="h-7 font-mono text-3xs"
            />
          </div>
        ))}
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="outline"
          size="sm"
          disabled={submitting || rows.length >= MAX_TASKS}
          onClick={() => setRows((cur) => [...cur, defaultTarget()])}
        >
          <Plus className="size-3.5" />
          {t("addTask")}
        </Button>
        <div className="flex-1" />
        <Button
          variant="destructive"
          size="sm"
          disabled={submitting}
          onClick={() => void run("reject")}
        >
          {t("reject")}
        </Button>
        <Button
          size="sm"
          disabled={approveDisabled}
          onClick={() => void run("approve")}
        >
          {submitting && <Loader2 className="size-3.5 animate-spin" />}
          {t("confirm")}
        </Button>
      </div>

      {error && <p className="text-xs text-destructive">{t("submitError")}</p>}
    </div>
  )
}

