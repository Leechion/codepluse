# 挂载底部状态栏（未渲染功能 ②）

## Context

`StatusBar`（[status-bar.tsx](file:///e:/code/codepluse/codepluse-ui/src/components/layout/status-bar.tsx)）从未被渲染（无 props，h-8 底栏）。内容：左侧 **QuickActionsDropdown**（快捷操作菜单，内含「打开设置」→ `openSettingsWindow`，[quick-actions-dropdown.tsx](file:///e:/code/codepluse/codepluse-ui/src/components/layout/quick-actions-dropdown.tsx) L105-111）+ StatusBarStats（会话统计）；右侧 更新/任务/命令/计算机/MCP/告警 指示器。所需 Provider（Update/AppWorkspace/WorkbenchRoute 等）已在 workspace/layout.tsx 挂齐；样式复用 globals.css 的 `ws-surface-muted`/`ws-chrome-border`。

上游组合（.data/upstream-workspace-layout.tsx L1447）：`<StatusBar />` 位于整个 shell 之下、**窗口级全宽**（含侧栏下方）。

## 改动

### 1. [ZCodeWorkspacePage.tsx](file:///e:/code/codepluse/codepluse-ui/src/zcode/pages/ZCodeWorkspacePage.tsx)

- 导入 `StatusBar`（`@/components/layout/status-bar`）。
- 结构：`.zw-shell` 内现有全部行内容（侧栏/divider/工作处理/chat-col/route + dock）包进 `<div className="zw-body">`，其后追加 `<StatusBar />`：

```tsx
<div className="zw-shell">
  <div className="zw-body">
    {/* 现有：侧栏、workOpen、chat-col / route、float-dock */}
  </div>
  <StatusBar />
</div>
```

- dock 保持 absolute 于 `.zw-shell`（坐标不变）。

### 2. [zcode-workspace.css](file:///e:/code/codepluse/codepluse-ui/src/app/zcode-workspace.css)

- `.zw-shell` 改为纵向总容器：`flex-direction: column;`（其下 .zw-body 承担原横向行布局）。
- 新增 `.zw-body { display:flex; flex:1 1 0; min-width:0; min-height:0; width:100%; overflow:hidden; }`。
- StatusBar 自带 `h-8` 高度与 surface 样式，无需额外 CSS（最多加 `.zw-body + section { flex:0 0 auto }` 类的兜底，视渲染结果定）。

## 验证

```powershell
pnpm --dir codepluse-ui build
```

浏览器（Ctrl+F5）+ 桌面截图：
1. 窗口底部出现 h-8 状态栏：左侧快捷菜单 + 统计，右侧 更新/任务/命令/MCP 等指示器；侧栏、面板、对话区、终端均上移不重叠。
2. 点快捷菜单 → 菜单弹出，含「打开设置」项（向上弹起，不被底边裁切）。
3. 切「自动化」路由 → 状态栏仍在（窗口级）；终端开合不受影响。
4. 明暗主题下状态栏配色正常；console 无报错。
5. 回归：侧栏折叠、终端拖拽、路由切换。

## 已知范围

- 状态栏右侧部分指示器（计算机/MCP/告警）依赖后端状态，空数据时显示默认态——属正常。
