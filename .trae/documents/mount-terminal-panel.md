# 挂载终端面板（未渲染功能 ①）

## Context

`TerminalPanel`（[terminal-panel.tsx](file:///e:/code/codepluse/codeg-ui/src/components/terminal/terminal-panel.tsx)）从未被任何页面渲染（仅测试引用）。`TerminalProvider` 已在 [workspace/layout.tsx](file:///e:/code/codepluse/codeg-ui/src/app/workspace/layout.tsx) 挂载，提供 `isOpen / height(默认300, clamp 150–600) / toggle / setHeight / tabs` 等（[terminal-context.tsx](file:///e:/code/codepluse/codeg-ui/src/contexts/terminal-context.tsx) L42-44, L522-523, L656-667）。目标：在对话区下方挂一条可拖拽调高、可收起的终端条，开关按钮加入现有左上角 dock。

行为对齐上游（.data/upstream-workspace-layout.tsx L1346-1352）：
- 终端在对话路由（`isConversations`）时才渲染；切到自动化/任务等全屏路由自动让位（context 状态保留）。
- `toggle()` 在无活动文件夹时不会创建终端 tab（context 已处理，L436-462），面板只显示空标签栏——用户打开文件夹后即可用。

## 改动

### 1. [ZCodeWorkspacePage.tsx](file:///e:/code/codepluse/codeg-ui/src/zcode/pages/ZCodeWorkspacePage.tsx)

- 导入：`TerminalPanel`、`useTerminalContext`、lucide `SquareTerminal`。
- 解构：`const { isOpen: terminalOpen, toggle: toggleTerminal, height: terminalHeight, setHeight: setTerminalHeight } = useTerminalContext()`。
- `startTerminalResize(e)`：pointer 拖拽，`setTerminalHeight(startH - (clientY - startY))`（向上拖=变高），context 自动夹紧 150–600。
- JSX：`isConversations` 分支内，把现有 `.zw-chat` 包进新的列容器 `.zw-chat-col`，终端条件渲染在其下：

```tsx
<div className="zw-chat-col">
  <div className="zw-chat"><ConversationDetailPanel /></div>
  {terminalOpen ? (
    <>
      <div className="zw-term-resize" role="separator" aria-orientation="horizontal"
           aria-label="调整终端高度" onPointerDown={startTerminalResize} />
      <div className="zw-term" style={{ height: terminalHeight }}>
        <TerminalPanel />
      </div>
    </>
  ) : null}
</div>
```

- dock 增加终端开关按钮（仅 `isConversations` 时显示）：`SquareTerminal` 图标，title 打开/收起终端，onClick `toggleTerminal`。

### 2. [zcode-workspace.css](file:///e:/code/codepluse/codeg-ui/src/app/zcode-workspace.css)

- `.zw-chat-col`：`display:flex; flex-direction:column; flex:1 1 0; min-width:0; min-height:0;`（原 `.zw-chat` 的 flex:1 保留，在列内生效）。
- `.zw-term`：`flex:0 0 auto; overflow:hidden; border-top:1px solid var(--zw-border); background:var(--background);`
- `.zw-term-resize`：水平版分隔条，`flex:0 0 4px; cursor:row-resize;`（复用 `.zw-divider::after` 的 hover 视觉，改为水平线）。

## 验证

```powershell
pnpm --dir codeg-ui build
```

浏览器（Ctrl+F5）+ 桌面截图：
1. 点 dock 终端按钮 → 对话区下方出现终端条（标签栏 + 空白视图区），聊天区相应变矮。
2. 拖拽分隔条 → 高度在 150–600 间变化。
3. 再点按钮 → 收起；刷新后开合/高度状态保持（context 持久化）。
4. 切到「自动化」→ 终端随路由隐藏；切回对话 → 恢复。
5. 回归：侧栏、工作处理、设置、路由切换不受影响；console 无报错。

## 已知范围

- 无活动文件夹时终端为空壳（只有标签栏）——打开文件夹后 `toggle`/新建按钮才会创建 shell 会话（context 既有逻辑）。
- 终端开合状态经 `readTerminalSession` 持久化（localStorage），默认关闭，不影响首次打开的现有布局。
