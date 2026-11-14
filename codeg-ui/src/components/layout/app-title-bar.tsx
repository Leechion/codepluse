"use client"

import type { ReactNode } from "react"
import { usePlatform } from "@/hooks/use-platform"
import { useIsMobile } from "@/hooks/use-mobile"
import { cn } from "@/lib/utils"
import { WindowControls } from "./window-controls"

interface AppTitleBarProps {
  left?: ReactNode
  center?: ReactNode
  right?: ReactNode
  className?: string
  rowClassName?: string
  showWindowControls?: boolean
}

export function AppTitleBar({
  left,
  center,
  right,
  className,
  rowClassName,
  showWindowControls = true,
}: AppTitleBarProps) {
  const { isMac, isWindows, isLinux } = usePlatform()
  const isMobile = useIsMobile()
  const isDesktopRuntime =
    typeof window !== "undefined" &&
    "__TAURI_INTERNALS__" in window &&
    // 只有自带窗口边框的外壳才画自己的标题栏：旧的 Go WebView2 外壳
    // （GoWindowTitlebar）自带一套，再叠一套 Tauri 控件就重复了。那个外壳
    // 随 Go 后台一起删除后，这里只剩 Tauri 外壳 —— 它必须显示这些控件。
    //
    // 不能用 __CODEG_TAURI_SHELL__ 判断：Tauri 外壳自己就置了这个标志
    // （界面仍走 HTTP/WebSocket 连本进程的后台），用它会把控件误藏起来。
    typeof (window as unknown as { __codegGoOpenWindow?: unknown })
      .__codegGoOpenWindow !== "function"
  const hasDesktopWindowChrome = showWindowControls && isDesktopRuntime
  // Windows and Linux both render custom controls on the right (macOS keeps
  // native traffic lights on the left via the overlay title bar).
  const usesRightControls = (isWindows || isLinux) && hasDesktopWindowChrome

  const rowPadding = cn(
    "px-3",
    isMac && hasDesktopWindowChrome && "pl-[92px]",
    usesRightControls && "pr-[138px]"
  )

  return (
    <div
      className={cn(
        "relative shrink-0 border-b bg-muted/70 select-none",
        isMobile ? "h-11" : "h-8",
        className
      )}
    >
      <div data-tauri-drag-region className="absolute inset-0" />

      <div
        data-tauri-drag-region
        className={cn(
          "relative z-10 flex h-full items-center",
          rowPadding,
          rowClassName
        )}
      >
        <div className="min-w-0 flex-1">{left}</div>
        {right ? (
          <div className={cn("ml-auto shrink-0", usesRightControls && "mr-4")}>
            {right}
          </div>
        ) : null}
      </div>

      {center ? (
        <div className="pointer-events-none absolute inset-0 z-20 flex items-center justify-center">
          <div>{center}</div>
        </div>
      ) : null}

      {usesRightControls ? (
        <div className="absolute inset-y-0 right-0 z-30 flex items-stretch">
          <WindowControls />
        </div>
      ) : null}
    </div>
  )
}
