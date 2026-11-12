"use client"

// 同窗口设置浮层。
//
// 桌面版（Go 壳）里点「设置」原本会开出第二个 WebView2 窗口：`openSettingsWindow`
// 走 web 分支 → `openAppWindow` → 壳的 `__codegGoOpenWindow` 重新 exec 自己并传
// `--page`。这里改为在当前窗口内以浮层渲染同一套设置分区，12 个入口不用各写一份。
//
// 分区组件直接复用 `src/app/settings/*/page.tsx` 的默认导出：它们本来就与 URL 无关
// （只有 SettingsShell 的导航高亮读 pathname），所以浮层和独立设置窗口用的是同一份
// 实现，不会出现两套行为。
import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type ComponentType,
  type ReactNode,
} from "react"
import { X } from "lucide-react"
import { Button } from "@/components/ui/button"
import { SettingsShell } from "./settings-shell"
import {
  registerSettingsOverlayHandler,
  type SettingsOverlayHandler,
} from "@/lib/settings-overlay-bridge"

import SettingsAppearancePage from "@/app/settings/appearance/page"
import SettingsBrowserPage from "@/app/settings/browser/page"
import SettingsCollaborationPage from "@/app/settings/collaboration/page"
import SettingsGeneralPage from "@/app/settings/general/page"
import SettingsLogsPage from "@/app/settings/logs/page"
import SettingsMcpPage from "@/app/settings/mcp/page"
import SettingsAgentsPage from "@/app/settings/agents/page"
import SettingsShortcutsPage from "@/app/settings/shortcuts/page"
import SettingsSkillsPage from "@/app/settings/skills/page"
import SettingsVersionControlPage from "@/app/settings/version-control/page"

/** 导航 href 尾段 → 分区组件。与 SettingsShell 的 SETTINGS_NAV_ITEMS 一一对应。 */
const SECTION_PAGES: Record<string, ComponentType> = {
  appearance: SettingsAppearancePage,
  general: SettingsGeneralPage,
  collaboration: SettingsCollaborationPage,
  mcp: SettingsMcpPage,
  skills: SettingsSkillsPage,
  agents: SettingsAgentsPage,
  browser: SettingsBrowserPage,
  shortcuts: SettingsShortcutsPage,
  "version-control": SettingsVersionControlPage,
  logs: SettingsLogsPage,
}

const DEFAULT_SECTION = "general"

function sectionIdOf(section: string | null | undefined): string {
  if (!section) return DEFAULT_SECTION
  const trimmed = section.replace(/^\/?settings\//, "").replace(/\/+$/, "")
  return SECTION_PAGES[trimmed] ? trimmed : DEFAULT_SECTION
}

interface OverlayState {
  open: boolean
  section: string
  agentType: string | null
}

export function SettingsOverlayProvider({
  children,
}: {
  children: ReactNode
}) {
  const [state, setState] = useState<OverlayState>({
    open: false,
    section: DEFAULT_SECTION,
    agentType: null,
  })

  // 登记桥：`openSettingsWindow` 在同一个模块图里，先问这里能不能就地打开。
  useEffect(() => {
    const handler: SettingsOverlayHandler = (section, agentType) => {
      setState({
        open: true,
        section: sectionIdOf(section),
        agentType: agentType ?? null,
      })
    }
    return registerSettingsOverlayHandler(handler)
  }, [])

  const close = useCallback(() => {
    setState((current) => ({ ...current, open: false }))
  }, [])

  const navigate = useCallback((href: string) => {
    setState((current) => ({
      ...current,
      open: true,
      section: sectionIdOf(href),
    }))
  }, [])

  // Esc 关闭：与浮层出现期间绑定，避免在设置窗口里抢走其它对话框的按键。
  useEffect(() => {
    if (!state.open) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation()
        close()
      }
    }
    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [close, state.open])

  const Section = useMemo(
    () => SECTION_PAGES[state.section] ?? SECTION_PAGES[DEFAULT_SECTION],
    [state.section]
  )

  return (
    <>
      {children}
      {state.open && (
        <div
          role="dialog"
          aria-modal="true"
          aria-label="设置"
          data-settings-overlay=""
          className="fixed inset-0 z-50 flex flex-col bg-background"
        >
          <div className="flex items-center justify-end gap-2 border-b px-3 py-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={close}
              aria-label="关闭设置"
            >
              <X className="h-3.5 w-3.5" />
              关闭
            </Button>
          </div>
          <div className="min-h-0 flex-1 overflow-hidden">
            <SettingsShell
              activeHref={`/settings/${state.section}`}
              onNavigate={navigate}
            >
              {/* 分区页里需要深链到某个 agent 时（会话详情「在设置中打开此
                agent」）由 props 传入；独立设置窗口仍走 URL 查询参数。 */}
              <SectionWithAgentType Section={Section} agentType={state.agentType} />
            </SettingsShell>
          </div>
        </div>
      )}
    </>
  )
}

function SectionWithAgentType({
  Section,
  agentType,
}: {
  Section: ComponentType<{ initialAgentType?: string | null }>
  agentType: string | null
}) {
  return <Section initialAgentType={agentType} />
}
