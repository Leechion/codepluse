"use client"

/**
 * ZCodeWorkspacePage —— codepluse 的 /workspace 主界面。
 *
 * 右侧对话区直接使用 codepluse 原生的 ConversationDetailPanel：原输入框、
 * 原生模型选择、已验证的发送链路都保持不变。
 * 左侧保留按 ZCode 源码复刻的「工作处理」面板（任务 / 工作流 / 产物）。
 *
 * ZCode 版对话页 ZCodeChatPage（vendored ZCode v4 组件 + GoConversationTransport）
 * 仍保留在仓库里，只是不再挂载，需要时可切回。
 * 旧 codeg 工作区的独立入口也仍保留在 /workspace-classic。
 */
import { useState } from "react"
import { PanelLeftOpen } from "lucide-react"
import { ConversationDetailPanel } from "@/components/conversations/conversation-detail-panel"
import { ZCodeWorkPanel } from "./ZCodeWorkPanel"

export function ZCodeWorkspacePage() {
  const [workOpen, setWorkOpen] = useState(true)

  return (
    <div className="zw-shell">
      {workOpen ? (
        <>
          <ZCodeWorkPanel onCollapse={() => setWorkOpen(false)} />
          <div className="zw-divider" aria-hidden="true" />
        </>
      ) : null}
      <div className="zw-chat">
        <ConversationDetailPanel />
        {!workOpen ? (
          <button
            type="button"
            className="zw-root zw-float-toggle"
            onClick={() => setWorkOpen(true)}
          >
            <PanelLeftOpen size={13} /> 工作处理
          </button>
        ) : null}
      </div>
    </div>
  )
}

export default ZCodeWorkspacePage
