"use client"

import { Suspense } from "react"
import { useTranslations } from "next-intl"
import { AgentConnections } from "@/components/settings/agent-connections"
import { ScrollArea } from "@/components/ui/scroll-area"

export default function SettingsAgentsPage({
  initialAgentType,
}: {
  /** 同窗口设置浮层的深链；独立设置窗口仍用 `?agent=` 查询参数。 */
  initialAgentType?: string | null
} = {}) {
  const t = useTranslations("SettingsPages")

  return (
    <ScrollArea className="h-full">
      <div className="cp-settings-page w-full">
        <Suspense
          fallback={
            <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
              {t("agentsLoading")}
            </div>
          }
        >
          <AgentConnections initialAgentType={initialAgentType} />
        </Suspense>
      </div>
    </ScrollArea>
  )
}
