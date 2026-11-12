"use client"

import { Suspense } from "react"
import { useTranslations } from "next-intl"
import { AcpAgentSettings } from "@/components/settings/acp-agent-settings"

export default function SettingsAgentsPage({
  initialAgentType,
}: {
  /** 同窗口设置浮层的深链；独立设置窗口仍用 `?agent=` 查询参数。 */
  initialAgentType?: string | null
} = {}) {
  const t = useTranslations("SettingsPages")

  return (
    <Suspense
      fallback={
        <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
          {t("agentsLoading")}
        </div>
      }
    >
      <AcpAgentSettings initialAgentType={initialAgentType} />
    </Suspense>
  )
}
