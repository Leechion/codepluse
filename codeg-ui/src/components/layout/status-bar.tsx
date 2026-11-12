"use client"

import { Settings } from "lucide-react"
import { useTranslations } from "next-intl"
import { openSettingsWindow } from "@/lib/api"
import { StatusBarStats } from "@/components/layout/status-bar-stats"
import { StatusBarTasks } from "@/components/layout/status-bar-tasks"
import { StatusBarAlerts } from "@/components/layout/status-bar-alerts"
import { StatusBarMcp } from "@/components/layout/status-bar-mcp"
import { StatusBarComputer } from "@/components/layout/status-bar-computer"
import { StatusBarUpdate } from "@/components/layout/status-bar-update"
import { CommandDropdown } from "@/components/layout/command-dropdown"
import { useIsMobile } from "@/hooks/use-mobile"

function StatusBarSettings() {
  const t = useTranslations("Folder.statusBar.quickActions")

  return (
    <button
      type="button"
      title={t("openSettings")}
      aria-label={t("openSettings")}
      className="inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-md border-0 bg-transparent p-0 text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-1 focus-visible:ring-ring/50"
      onClick={() => {
        void openSettingsWindow().catch((error) => {
          console.error("[StatusBar] failed to open settings:", error)
        })
      }}
    >
      <Settings aria-hidden="true" className="size-3.5" />
    </button>
  )
}

export function StatusBar() {
  const isMobile = useIsMobile()

  if (isMobile) {
    // Mobile mirrors the desktop bar: workspace stats on the left, the command
    // launcher + alerts on the right. `h-8` (matching desktop) gives the h-6
    // command control room. The branch selector and context-window circle live
    // in the below-composer row, so those are the bar's only two clusters.
    return (
      <div className="h-8 shrink-0 border-t border-border ws-chrome-border ws-surface-muted px-3 flex items-center justify-between text-xs text-muted-foreground">
        <div className="flex items-center gap-3">
          <StatusBarSettings />
          <StatusBarStats />
        </div>
        <div className="flex items-center gap-3">
          <StatusBarUpdate />
          <CommandDropdown />
          <StatusBarMcp />
          <StatusBarAlerts />
        </div>
      </div>
    )
  }

  return (
    <div className="h-8 shrink-0 border-t border-border ws-chrome-border ws-surface-muted pl-2 pr-4 flex items-center justify-between text-xs text-muted-foreground">
      {/* Settings stays directly accessible at the bottom-left, even when the
          sidebar is collapsed. The remaining status-bar controls keep their slots. */}
      <div className="flex items-center gap-3">
        <StatusBarSettings />
        <StatusBarStats />
      </div>
      <div className="flex items-center gap-4">
        <StatusBarUpdate />
        <StatusBarTasks />
        {/* Command launcher (moved from the aux "session details" tab), taking
            the slot the old static branch label (StatusBarSessionInfo) held. */}
        <CommandDropdown />
        {/* Computer use: which windows agents may see right now. Renders
            nothing unless the desktop runtime has it switched on. */}
        <StatusBarComputer />
        {/* codeg-mcp service health. Sits next to the alerts bell because the
            two answer adjacent questions — "is something wrong right now?" —
            and both open a top-anchored popover from this corner. */}
        <StatusBarMcp />
        <StatusBarAlerts />
      </div>
    </div>
  )
}
