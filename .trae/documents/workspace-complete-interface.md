# /workspace 补齐完整界面（最左侧会话侧栏）

## Context

用户截图（真实产品，Release v0.102.3101）显示完整工作区是**三栏**：

```
[会话侧栏 ~264-320px] [工作处理面板 306px] [对话区]
 搜索/新建会话/自动化     任务 tabs/搜索        欢迎页/对话
 待办任务/无限画布
 文件夹/聊天列表
 底部 N 个会话
```

而当前 `/workspace`（[ZCodeWorkspacePage.tsx](file:///e:/code/codepluse/codeg-ui/src/zcode/pages/ZCodeWorkspacePage.tsx)）只渲染两栏：ZCodeWorkPanel + ConversationDetailPanel，**缺少最左侧的会话侧栏**。上一次修复只在工作面板里加了「工作区」切换和「设置」按钮，没有解决"界面不完整"的本意。

仓库中已有完整版 codeg 经典侧栏组件 [sidebar.tsx](file:///e:/code/codepluse/codeg-ui/src/components/layout/sidebar.tsx)（导出 `Sidebar()`，无 props，当前未被任何页面挂载），其全部 Provider 依赖已在 [workspace/layout.tsx](file:///e:/code/codepluse/codeg-ui/src/app/workspace/layout.tsx) 挂好。点击侧栏会话 → tab-context → ConversationDetailPanel 切换对话的链路是现成的。

不采用 `src/zcode/app-shell/WorkspaceShellLayout.tsx`（ZCode 全套壳）：它依赖 ZCode 自己的 store 生态（TabStoreProvider 等），与当前混合页不兼容。

## 改动

### 1. [zcode-workspace.css](file:///e:/code/codepluse/codeg-ui/src/app/zcode-workspace.css)

- `.zw-shell`（L150-159）追加 `position: relative;`（供浮动 dock 定位）。
- 新增 `.zw-side`（左列容器：flex 列、`background: var(--sidebar)`、`border-right: 1px solid var(--sidebar-border)`，宽度由 inline style 注入）。
- 新增 `.zw-float-dock`（absolute 左上角，`top:9px; left:10px; z-index:6`，flex 横排，内部 `.zw-float-toggle` 改 `position:static`）+ `.zw-float-toggle--icon`（28px 纯图标变体）。
- **注意**：Sidebar 不能包进任何 `.zw-root` 容器（会重映射 shadcn token）；`.zw-shell` 本身无 zw-root 类，安全。

### 2. [ZCodeWorkspacePage.tsx](file:///e:/code/codepluse/codeg-ui/src/zcode/pages/ZCodeWorkspacePage.tsx)

- 导入：`Sidebar`（`@/components/layout/sidebar`）、`useSidebarContext`（`@/contexts/sidebar-context`）、lucide 图标 `PanelLeft / PanelLeftOpen`。
- 组件体解构：`const { isOpen: sidebarOpen, toggle: toggleSidebar, width } = useSidebarContext()`。
- JSX 结构：

```tsx
<div className="zw-shell">
  {sidebarOpen ? (
    <div className="zw-side" style={{ width }}>
      <Sidebar />
    </div>
  ) : null}
  {workOpen ? (<ZCodeWorkPanel ... /> + <div className="zw-divider" />) : null}
  <div className="zw-chat"><ConversationDetailPanel /></div>
  <div className="zw-float-dock">
    {/* 侧栏开：图标按钮收起；关：带文字按钮展开 */}
    {/* workOpen 关时：第二个按钮展开「工作处理」（原 zw-chat 内浮动按钮移入此处） */}
  </div>
</div>
```

- 折叠/展开按钮放 Sidebar **外部**（Sidebar 在 `!isOpen` 时自身 return null）。
- dock 常驻渲染，避免侧栏关 + 工作处理关时两个 absolute 按钮重叠。

### 3. 可选（P2，本次一并做）：侧栏拖拽调宽

侧栏右缘加 `.zw-divider` 拖拽条，pointerdown 后 `setWidth(startW + dx)`；min 224 / max 900 由 context 的 clamp 自动夹紧，localStorage 自动记忆。

## 关键事实（已验证）

- `useSidebarContext`：`{ isOpen, restored, width, minWidth, maxWidth, toggle, setWidth }`，默认宽 320，localStorage key `workspace:left-sidebar`，移动端 <768px 默认关闭。
- Sidebar 无 footer（截图的「N 个会话 + 设置」footer 属 ZCode 套件）；设置入口沿用上次加在 ZCodeWorkPanel 底部的按钮即可。
- ZCodeWorkPanel 与 Sidebar 无状态冲突（前者只读 app-workspace-store，后者写 tab store）。
- `data-tauri-drag-region` 在浏览器下是 inert 属性，无害。
- i18n：`Folder.sidebar` 命名空间 zh-CN 等语言完整。

## 验证

```powershell
pnpm --dir codeg-ui build
```

浏览器打开 http://127.0.0.1:8840/workspace（后台 serve-codepluse.exe 正在运行）：
1. 三栏布局：Sidebar(320px) | 工作处理(306px) | 对话区，无横向溢出。
2. 点击侧栏会话 → 右侧对话切换；点文件夹 → tab 过滤；「新建会话」→ 新 tab。
3. dock 按钮折叠/展开侧栏；刷新后宽度/开关状态保持。
4. 明暗主题下侧栏配色正常（--sidebar token）。
5. 「设置」按钮仍可用；console 无报错。
6. 桌面端（codepluse.exe）截图复核。

## 已知取舍

- 侧栏导航行「自动化/待办任务/仓库面板/无限画布」点击只切换 routeId 高亮，混合页暂不渲染对应内容区（无害空操作，后续可补）。
- 侧栏 header 无搜索框（上游将其放在未挂载的 LeftEdgeChrome）。
