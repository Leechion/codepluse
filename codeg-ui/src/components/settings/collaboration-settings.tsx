"use client"

/**
 * Collaboration settings: every panel that decides how agents are staffed and
 * how far an agent may reach beyond its own conversation.
 *
 *   * "Multi-Agent Collaboration" (`delegation-settings.tsx`) — whether an
 *     agent may hand a sub-task to another agent at all and how deep the chain
 *     may go; its "子智能体配置" tab hosts the main agent's default harness +
 *     model and the named harness instances (`harness-instance-settings.tsx`),
 *     each pinned to its own provider/model so every sub-agent can run its own
 *     model.
 *   * "In-conversation tools" — the tool groups codeg-mcp injects when an
 *     agent starts: feedback, ask-user-question, session info, the built-in
 *     browser, computer use and the create-from-chat writers
 *     (`agent-tools-settings.tsx`). Computer use's own settings — the driver,
 *     permissions, sharing — have a page of their own
 *     (`computer-use-settings.tsx`).
 *
 * The harness section used to be its own `/settings/harness-instances` page;
 * merging it here keeps all agent staffing on one page. One ScrollArea wraps
 * the whole page.
 */

import { useTranslations } from "next-intl"

import { ScrollArea } from "@/components/ui/scroll-area"
import { DelegationSettingsSection } from "@/components/settings/delegation-settings"
import { AgentToolsSettingsSection } from "@/components/settings/agent-tools-settings"

export function CollaborationSettings() {
  const t = useTranslations("CollaborationSettings")

  return (
    <ScrollArea className="h-full">
      <div className="w-full space-y-4 p-3 md:p-4">
        <section className="space-y-1">
          <h1 className="text-sm font-semibold">{t("sectionTitle")}</h1>
          <p className="text-xs text-muted-foreground">
            {t("sectionDescription")}
          </p>
        </section>

        <DelegationSettingsSection />

        <AgentToolsSettingsSection showComputerUse={false} />
      </div>
    </ScrollArea>
  )
}
