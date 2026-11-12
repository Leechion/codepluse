"use client"

/**
 * ZCodeWorkspacePage —— codepluse 的 /workspace 主界面。
 *
 * 布局：
 *   左：codeg 经典会话侧栏（Sidebar：新建会话/自动化/待办任务/文件夹/聊天列表），
 *       点击会话经 tab-context 驱动右侧对话区切换。
 *   右：codepluse 原生 ConversationDetailPanel（原输入框、原生模型选择、
 *       已验证的发送链路）+ 可选终端条 + fusion 模式文件列 + 可选辅助面板。
 *   底：StatusBar 状态栏；非会话路由时内容区整体切换为 WorkbenchRoutePage。
 *
 * 「工作处理」面板（ZCodeWorkPanel）已按需求下架，组件保留在仓库中。
 * ZCode 版对话页 ZCodeChatPage（vendored ZCode v4 组件 + GoConversationTransport）
 * 仍保留在仓库里，只是不再挂载，需要时可切回。
 * 旧 codeg 工作区的独立入口也仍保留在 /workspace-classic。
 */
import {
  useCallback,
  useEffect,
  useRef,
  type PointerEvent as ReactPointerEvent,
} from "react"
import { PanelLeft, PanelLeftOpen, PanelRight, SquareTerminal } from "lucide-react"
import { ConversationDetailPanel } from "@/components/conversations/conversation-detail-panel"
import { AuxPanel } from "@/components/layout/aux-panel"
import { Sidebar } from "@/components/layout/sidebar"
import { WorkspaceChromeController } from "@/components/layout/workspace-chrome-controller"
import { useAuxPanelContext } from "@/contexts/aux-panel-context"
import { useSidebarContext } from "@/contexts/sidebar-context"
import { useTerminalContext } from "@/contexts/terminal-context"
import { useWorkbenchRoute } from "@/contexts/workbench-route-context"
import { useWorkspaceView } from "@/contexts/workspace-context"
import { WorkbenchRoutePage } from "@/components/workbench/workbench-content"
import { StatusBar } from "@/components/layout/status-bar"
import { TerminalPanel } from "@/components/terminal/terminal-panel"
import { FileWorkspaceHeader } from "@/components/files/file-workspace-header"
import { FileWorkspacePanel } from "@/components/files/file-workspace-panel"
import { FileWorkspaceTabBar } from "@/components/files/file-workspace-tab-bar"

export function ZCodeWorkspacePage() {
  const { isConversations } = useWorkbenchRoute()
  const { mode } = useWorkspaceView()
  const {
    isOpen: sidebarOpen,
    toggle: toggleSidebar,
    width: sidebarWidth,
    setWidth: setSidebarWidth,
  } = useSidebarContext()
  const {
    isOpen: terminalOpen,
    toggle: toggleTerminal,
    height: terminalHeight,
    setHeight: setTerminalHeight,
  } = useTerminalContext()
  const {
    isOpen: auxOpen,
    toggle: toggleAux,
    width: auxWidth,
    setWidth: setAuxWidth,
  } = useAuxPanelContext()

  // 收起侧栏时对话列占据窗口左缘，头部条必须为浮动 dock 预留宽度，否则
  // 「聊天模式 / 标题」会被 dock 按钮盖住。dock 宽度随语言（「会话」标签
  // 长短）与按钮数量变化，用 ResizeObserver 实测写到 shell 的 --zw-dock-w，
  // 由 zcode-workspace.css 的预留规则消费。
  const shellRef = useRef<HTMLDivElement>(null)
  const dockRef = useRef<HTMLDivElement>(null)
  const measureDock = useCallback(() => {
    const dock = dockRef.current
    const shell = shellRef.current
    if (!dock || !shell) return
    shell.style.setProperty("--zw-dock-w", `${Math.round(dock.offsetWidth)}px`)
  }, [])
  useEffect(() => {
    measureDock()
    const dock = dockRef.current
    if (!dock || typeof ResizeObserver === "undefined") return
    const ro = new ResizeObserver(measureDock)
    ro.observe(dock)
    return () => ro.disconnect()
  }, [measureDock, sidebarOpen, isConversations])

  // 侧栏拖拽调宽：min/max 由 sidebar-context 的 clamp 自动夹紧。
  const startSidebarResize = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    const startX = e.clientX
    const startWidth = sidebarWidth
    const onMove = (ev: PointerEvent) => {
      setSidebarWidth(startWidth + ev.clientX - startX)
    }
    const onUp = () => {
      window.removeEventListener("pointermove", onMove)
      window.removeEventListener("pointerup", onUp)
    }
    window.addEventListener("pointermove", onMove)
    window.addEventListener("pointerup", onUp)
  }

  // 终端拖拽调高：向上拖 = 变高，terminal-context 自动夹紧 150–600。
  const startTerminalResize = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    const startY = e.clientY
    const startHeight = terminalHeight
    const onMove = (ev: PointerEvent) => {
      setTerminalHeight(startHeight - (ev.clientY - startY))
    }
    const onUp = () => {
      window.removeEventListener("pointermove", onMove)
      window.removeEventListener("pointerup", onUp)
    }
    window.addEventListener("pointermove", onMove)
    window.addEventListener("pointerup", onUp)
  }

  // 辅助面板拖拽调宽：向左拖 = 变宽，aux-panel-context 自动夹紧 200–900。
  const startAuxResize = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    const startX = e.clientX
    const startWidth = auxWidth
    const onMove = (ev: PointerEvent) => {
      setAuxWidth(startWidth - (ev.clientX - startX))
    }
    const onUp = () => {
      window.removeEventListener("pointermove", onMove)
      window.removeEventListener("pointerup", onUp)
    }
    window.addEventListener("pointermove", onMove)
    window.addEventListener("pointerup", onUp)
  }

  return (
    <div
      ref={shellRef}
      className="zw-shell"
      data-sidebar={sidebarOpen ? "open" : "closed"}
    >
      <div className="zw-body">
      {sidebarOpen ? (
        <>
          <div className="zw-side" style={{ width: sidebarWidth }}>
            <Sidebar />
          </div>
          <div
            className="zw-divider"
            role="separator"
            aria-orientation="vertical"
            aria-label="调整会话侧栏宽度"
            onPointerDown={startSidebarResize}
          />
        </>
      ) : null}
      {isConversations ? (
        <>
          <div className="zw-chat-col">
            <div className="zw-chat">
              <ConversationDetailPanel />
            </div>
            {terminalOpen ? (
              <>
                <div
                  className="zw-term-resize"
                  role="separator"
                  aria-orientation="horizontal"
                  aria-label="调整终端高度"
                  onPointerDown={startTerminalResize}
                />
                <div className="zw-term" style={{ height: terminalHeight }}>
                  <TerminalPanel />
                </div>
              </>
            ) : null}
          </div>
          {mode === "fusion" ? (
            <div className="zw-files-col">
              <div className="zw-files-strip">
                <FileWorkspaceTabBar />
              </div>
              <FileWorkspaceHeader />
              <div className="zw-files-body">
                <FileWorkspacePanel />
              </div>
            </div>
          ) : null}
          {auxOpen ? (
            <>
              <div
                className="zw-divider"
                role="separator"
                aria-orientation="vertical"
                aria-label="调整辅助面板宽度"
                onPointerDown={startAuxResize}
              />
              <div className="zw-aux" style={{ width: auxWidth }}>
                <AuxPanel />
              </div>
            </>
          ) : null}
        </>
      ) : (
        <div className="zw-route">
          <WorkbenchRoutePage />
        </div>
      )}
      {/* 常驻左上角 dock：侧栏展开时按钮落在 Sidebar header 左侧的预留空白上，
          折叠时浮在最左侧。避免两个浮动按钮争抢同一坐标。 */}
      <div className="zw-float-dock" ref={dockRef}>
        {sidebarOpen ? (
          <button
            type="button"
            className="zw-root zw-float-toggle zw-float-toggle--icon"
            onClick={toggleSidebar}
            title="收起会话侧栏"
            aria-label="收起会话侧栏"
          >
            <PanelLeft size={13} />
          </button>
        ) : (
          <button
            type="button"
            className="zw-root zw-float-toggle"
            onClick={toggleSidebar}
            title="展开会话侧栏"
          >
            <PanelLeftOpen size={13} /> 会话
          </button>
        )}
        {isConversations ? (
          <button
            type="button"
            className="zw-root zw-float-toggle zw-float-toggle--icon"
            onClick={toggleTerminal}
            title={terminalOpen ? "收起终端" : "打开终端"}
            aria-label={terminalOpen ? "收起终端" : "打开终端"}
          >
            <SquareTerminal size={13} />
          </button>
        ) : null}
        {isConversations ? (
          <button
            type="button"
            className="zw-root zw-float-toggle zw-float-toggle--icon"
            onClick={toggleAux}
            title={auxOpen ? "收起辅助面板" : "打开辅助面板"}
            aria-label={auxOpen ? "收起辅助面板" : "打开辅助面板"}
          >
            <PanelRight size={13} />
          </button>
        ) : null}
      </div>
      </div>
      <StatusBar />
      {/* 全局快捷键（Ctrl+K 搜索等）+ 搜索/文件夹对话框宿主 */}
      <WorkspaceChromeController />
    </div>
  )
}

export default ZCodeWorkspacePage
