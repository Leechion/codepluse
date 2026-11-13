"use client"

import {
  useCallback,
  useEffect,
  useState,
  type ComponentType,
  type ReactNode,
} from "react"
import {
  ArrowLeft,
  Bot,
  BookOpenText,
  Bubbles,
  Compass,
  FileSpreadsheet,
  GitBranch,
  Keyboard,
  Menu,
  Palette,
  PlugZap,
  SlidersHorizontal,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { usePathname } from "next/navigation"
import { useRouter } from "next/navigation"
import { Button } from "@/components/ui/button"
import { ScrollArea } from "@/components/ui/scroll-area"
import { AppToaster } from "@/components/ui/app-toaster"
import { cn } from "@/lib/utils"
import {
  detectEnvironment,
  type TransportEnvironment,
} from "@/lib/transport/detect"
import { AppTitleBar } from "@/components/layout/app-title-bar"
import { useIsMobile } from "@/hooks/use-mobile"
import { Drawer, DrawerContent, DrawerTitle } from "@/components/ui/drawer"
import { useSettingsCardMotion } from "./use-settings-card-motion"

interface SettingsNavItem {
  href: string
  labelKey:
    | "general"
    | "appearance"
    | "agents"
    | "model_providers"
    | "mcp"
    | "skills"
    | "skill_packs"
    | "collaboration"
    | "browser"
    | "computer_use"
    | "quick_messages"
    | "shortcuts"
    | "version_control"
    | "chat_channels"
    | "system"
    | "web_service"
    | "logs"
  icon: ComponentType<{ className?: string }>
}

const SETTINGS_NAV_ITEMS: SettingsNavItem[] = [
  {
    href: "/settings/appearance",
    labelKey: "appearance",
    icon: Palette,
  },
  {
    href: "/settings/general",
    labelKey: "general",
    icon: SlidersHorizontal,
  },
  {
    href: "/settings/collaboration",
    labelKey: "collaboration",
    icon: Bubbles,
  },
  {
    href: "/settings/mcp",
    labelKey: "mcp",
    icon: PlugZap,
  },
  {
    href: "/settings/skills",
    labelKey: "skills",
    icon: BookOpenText,
  },
  {
    // 供应商设置已并入「智能体」页底部的折叠区，不再单设导航项。
    href: "/settings/agents",
    labelKey: "agents",
    icon: Bot,
  },
  {
    href: "/settings/browser",
    labelKey: "browser",
    icon: Compass,
  },
  {
    href: "/settings/shortcuts",
    labelKey: "shortcuts",
    icon: Keyboard,
  },
  {
    href: "/settings/version-control",
    labelKey: "version_control",
    icon: GitBranch,
  },
  {
    href: "/settings/logs",
    labelKey: "logs",
    icon: FileSpreadsheet,
  },
]

interface SettingsShellProps {
  children: ReactNode
  /**
   * 浮层用法：不读 URL，由调用方给出当前分区的 href（如 "/settings/mcp"）。
   * 独立设置窗口仍不传，继续以 pathname 为准。
   */
  activeHref?: string
  /** 浮层用法：给定时导航不再改 URL，而是回调给调用方。 */
  onNavigate?: (href: string) => void
  /**
   * 浮层用法：给定时在导航栏左上角渲染「← 回到工作区」，作为退出设置回到工作区
   * 的唯一入口。独立设置窗口不传（它有自己的关闭窗口的方式）。
   */
  onClose?: () => void
}

/** Go runtime navigation: merged pages and unavailable services stay off the rail. */
export function settingsNavItemsFor(
  _env: TransportEnvironment
): SettingsNavItem[] {
  void _env // The Go capability set is shared by browser and desktop windows.
  return SETTINGS_NAV_ITEMS
}

function normalizePath(path: string): string {
  const noSuffix = path.replace(/\/index\.html$/, "").replace(/\.html$/, "")
  const noTrailingSlash = noSuffix.replace(/\/+$/, "")
  return noTrailingSlash || "/"
}

function isWindowsRuntime(): boolean {
  if (typeof navigator === "undefined") return false
  const platform = navigator.platform.toLowerCase()
  const userAgent = navigator.userAgent.toLowerCase()
  return platform.includes("win") || userAgent.includes("windows")
}

export function SettingsShell({
  children,
  activeHref,
  onNavigate,
  onClose,
}: SettingsShellProps) {
  const t = useTranslations("SettingsShell")
  const navigation = useTranslations("SettingsNavigation")
  const pathname = usePathname()
  const router = useRouter()
  const normalizedPathname = normalizePath(activeHref ?? pathname)
  const isMobile = useIsMobile()
  const [navOpen, setNavOpen] = useState(false)
  const cardMotion = useSettingsCardMotion()

  useEffect(() => {
    // 浮层里不改主窗口的标题：那是工作区窗口的标题，关掉浮层后没人负责还原它。
    if (activeHref !== undefined) return
    document.title = `${t("title")} - codepluse`
  }, [activeHref, t])

  const navigateTo = useCallback(
    (href: string) => {
      if (typeof window === "undefined") return

      const target = normalizePath(href)

      if (onNavigate) {
        onNavigate(target)
        setNavOpen(false)
        return
      }

      const current = normalizePath(window.location.pathname)
      if (current === target) {
        setNavOpen(false)
        return
      }

      // Preserve current query string so the active remote workspace context
      // (`?remoteConnectionId=N`) carries over to sub-pages — without this,
      // navigating from /settings/appearance to /settings/mcp drops the
      // remote id and the next page falls back to the local Tauri backend.
      const search = window.location.search
      const fullTarget = search ? `${target}${search}` : target

      if (isWindowsRuntime()) {
        window.location.assign(fullTarget)
        return
      }

      router.push(fullTarget)
      setNavOpen(false)
    },
    [onNavigate, router, setNavOpen]
  )

  const filteredNavItems = settingsNavItemsFor(detectEnvironment())

  const navContent = (
    <div className="flex min-h-0 flex-1 flex-col">
      {onClose && (
        // 同窗口浮层的退出入口：放在导航栏左上角（设置界面内部），而不是浮层
        // 自己额外顶一条工具栏。文案是 codepluse 本地新增的单窗口 affordance，
        // 与 executor-providers-panel 一样先只给中文，未走 10 个语言包。
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="mb-1 w-full justify-start px-2 text-muted-foreground hover:text-foreground"
          onClick={() => {
            setNavOpen(false)
            onClose()
          }}
        >
          <span className="inline-flex items-center gap-1">
            <ArrowLeft className="h-3.5 w-3.5" />
            回到工作区
          </span>
        </Button>
      )}
      <div className="px-2 pb-2 text-2xs font-medium text-muted-foreground">
        {t("preferences")}
      </div>
      <ScrollArea className="min-h-0 flex-1">
        <nav className="space-y-1">
          {filteredNavItems.map((item) => {
            const Icon = item.icon
            const translationKey = `nav.${item.labelKey}` as const
            const active =
              normalizedPathname === item.href ||
              normalizedPathname.startsWith(`${item.href}/`)
            return (
              <Button
                key={item.href}
                variant={active ? "secondary" : "ghost"}
                size="sm"
                className={cn(
                  "relative w-full justify-start px-2 transition-colors",
                  // 激活项：左侧 2px 主色圆角指示条 + 中字重，扫一眼就能定位当前页。
                  active &&
                    "font-medium before:absolute before:bottom-1.5 before:left-0 before:top-1.5 before:w-0.5 before:rounded-full before:bg-primary"
                )}
                type="button"
                onClick={() => navigateTo(item.href)}
                aria-current={active ? "page" : undefined}
              >
                <span className="inline-flex items-center gap-1">
                  <Icon className="h-3.5 w-3.5" />
                  {item.labelKey === "skills"
                    ? navigation("skills")
                    : t(translationKey)}
                </span>
              </Button>
            )
          })}
        </nav>
      </ScrollArea>
    </div>
  )

  return (
    <div
      data-settings-shell=""
      className="h-screen flex flex-col overflow-hidden bg-background text-foreground"
    >
      {/* Only settings documents opt into same-origin shared-element navigation. */}
      <style>{"@view-transition { navigation: auto; }"}</style>
      {/*
        浮层里不再顶一条「设置」标题栏：工作区窗口本来就有自己的 chrome，这条
        栏只是重复的装饰。窄屏仍保留一条极窄的栏，因为抽屉触发器住在里面，但不
        再显示「设置」文字。独立设置窗口（没有 onClose）一切照旧。
      */}
      {(!onClose || isMobile) && (
        <AppTitleBar
          className="cp-settings-titlebar"
          left={
            isMobile ? (
              <Button
                variant="ghost"
                size="icon"
                className="h-8 w-8"
                onClick={() => setNavOpen(true)}
              >
                <Menu className="h-4 w-4" />
              </Button>
            ) : undefined
          }
          center={
            onClose ? undefined : (
              <div className="text-sm font-bold tracking-tight">{t("title")}</div>
            )
          }
        />
      )}

      <div className="cp-settings-layout flex-1 min-h-0 flex">
        {/* Desktop sidebar */}
        {!isMobile && (
          <aside className="cp-settings-sidebar flex min-h-0 w-56 shrink-0 flex-col border-r px-2 py-3">
            {navContent}
          </aside>
        )}

        {/* Mobile navigation Drawer. Opts back into press-outside-to-close,
            against the app-wide drawer default: it is navigation, and tapping
            the page it partially covers is how you put it away on a phone. */}
        {isMobile && (
          <Drawer
            open={navOpen}
            onOpenChange={setNavOpen}
            swipeDirection="left"
            disablePointerDismissal={false}
          >
            {/* rem so the nav grows with the zoom level like its own labels do,
                but capped against the viewport: 16.25rem at 150% is 390px, wider
                than the phone this drawer is for. */}
            <DrawerContent
              showCloseButton={false}
              className="w-[min(16.25rem,85vw)] p-3"
            >
              <DrawerTitle className="sr-only">{t("title")}</DrawerTitle>
              {navContent}
            </DrawerContent>
          </Drawer>
        )}

        <section
          className="cp-settings-content flex-1 min-w-0 min-h-0 overflow-hidden"
          {...cardMotion}
        >
          {children}
        </section>
      </div>
      <AppToaster position="bottom-right" closeButton duration={4000} />
    </div>
  )
}
