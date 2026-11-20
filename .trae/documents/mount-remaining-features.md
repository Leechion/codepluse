# 挂载剩余功能：文件工作区 + 搜索对话框 + 全局快捷键

## Context

未渲染清单收尾。三项一次做完：

1. **文件工作区（④）**：上游（.data/upstream-workspace-layout.tsx L572-591）在有文件 tab 时显示文件列：`FileWorkspaceTabBar`（h-10 标签条）+ `FileWorkspaceHeader` + `FileWorkspacePanel`。显示条件是派生的：`mode = visibleFileTabs.length > 0 ? "fusion" : "conversation"`（[workspace-context.tsx](file:///e:/code/codepluse/codeg-ui/src/contexts/workspace-context.tsx) L916-917），`useWorkspaceView()` 暴露 `mode`。默认无文件 tab → 不渲染，零影响。
2. **搜索对话框（⑤）+ 全局快捷键（⑥）**：两者是同一个组件——`WorkspaceChromeController`（[workspace-chrome-controller.tsx](file:///e:/code/codepluse/codeg-ui/src/components/layout/workspace-chrome-controller.tsx)）注册 toggle_search(Ctrl+K)/toggle_sidebar/toggle_terminal/toggle_aux_panel/new_conversation/open_folder 等全局快捷键（L210-247），并渲染 `SearchCommandDialog` + `WorkspaceFolderDialog`（L423-428）。上游在窗口级挂载（.data/upstream-workspace-layout.tsx L1434-1436，StatusBar 之后，"Mounted on both platforms"）。
3. **边缘悬浮条（⑦ LeftEdgeChrome/RightEdgeChrome）——跳过**：其功能已被现有 dock（侧栏/终端/辅助面板开关）+ 快捷菜单（设置）+ 本次快捷键控制器（搜索）覆盖，且其定位依赖已删除的 FolderLayoutShell，强行挂载会错位。

## 改动（仍是 2 个文件）

### 1. [ZCodeWorkspacePage.tsx](file:///e:/code/codepluse/codeg-ui/src/zcode/pages/ZCodeWorkspacePage.tsx)

- 导入：`useWorkspaceView`（`@/contexts/workspace-context`）、`FileWorkspaceTabBar`/`FileWorkspaceHeader`/`FileWorkspacePanel`（`@/components/files/*`）、`WorkspaceChromeController`（`@/components/layout/workspace-chrome-controller`）。
- `const { mode } = useWorkspaceView()`。
- `isConversations` 分支内、`.zw-chat-col` 之后、aux 之前插入文件列（fusion 模式才渲染）：

```tsx
{mode === "fusion" ? (
  <div className="zw-files-col">
    <div className="zw-files-strip"><FileWorkspaceTabBar /></div>
    <FileWorkspaceHeader />
    <div className="zw-files-body"><FileWorkspacePanel /></div>
  </div>
) : null}
```

- `<StatusBar />` 之后挂 `<WorkspaceChromeController />`。

### 2. [zcode-workspace.css](file:///e:/code/codepluse/codeg-ui/src/app/zcode-workspace.css)

- `.zw-files-col`：`display:flex; flex-direction:column; flex:0 0 44%; min-width:0; min-height:0; overflow:hidden; border-left:1px solid var(--sidebar-border); background:var(--background);`（对齐上游 fusion 默认 [56,44] 布局的文件列占比）。
- `.zw-files-strip`：`display:flex; height:2.5rem; flex:0 0 auto; align-items:stretch;`（上游同款 h-10 标签条容器）。
- `.zw-files-body`：`flex:1 1 0; min-height:0; overflow:hidden;`。

## 验证

```powershell
pnpm --dir codeg-ui build
```

浏览器（Ctrl+F5）：
1. 默认（无文件 tab）布局与之前完全一致（文件列不渲染）。
2. Ctrl+K → 搜索对话框弹出、可搜会话；Esc 关闭。
3. 桌面端：快捷菜单「打开文件夹」选一个本地文件夹 → 文件列出现在对话区右侧（标签条 + 文件面板）；点文件 tab 切换。
4. 回归：侧栏/终端/辅助面板/状态栏/路由切换不受影响；console 无报错。

## 已知范围

- 文件列宽固定 44%（上游 fusion 默认占比），暂不做拖拽调宽。
- 边缘悬浮条（⑦）跳过，理由见 Context。
- Ctrl+W 等关闭类快捷键沿用上游语义（关到最后一个 tab 关窗口）。
