# 挂载自动化/待办任务内容页（workbench 路由）

## Context

侧栏「自动化」「待办任务」等导航行点击后只改 `routeId` 高亮，内容区无反应。**调查发现源码里集成已存在**（[ZCodeWorkspacePage.tsx](file:///e:/code/codepluse/codepluse-ui/src/zcode/pages/ZCodeWorkspacePage.tsx) L68-84 已有 `isConversations` 分支：会话路由渲染 工作处理面板+对话区，非会话路由渲染 `<WorkbenchRoutePage />`；[zcode-workspace.css](file:///e:/code/codepluse/codepluse-ui/src/app/zcode-workspace.css) L220 已有 `.zw-route` 容器样式）——但 **out/ 构建产物里没有 zw-route（构建于该改动之前）**，所以运行中的应用点侧栏无反应。

`WorkbenchRoutePage`（[workbench-content.tsx](file:///e:/code/codepluse/codepluse-ui/src/components/workbench/workbench-content.tsx)）按 `routeId` 从注册表渲染：`automations → AutomationsPage`、`tasks → TasksPage`、`forge → ForgePage`、`tokenUsage`、`canvas`。所需 Provider（AutomationsViewProvider/TasksViewProvider/WorkbenchRouteProvider）已在 `app/workspace/layout.tsx` 挂载。

返回会话的链路已完备：侧栏聊天项点击（sidebar-conversation-list.tsx L2180+）和「新建会话」行（sidebar.tsx L323）都会调 `openConversations()` 切回对话视图。

## 改动

**无需改代码**——现有源码集成完整且自洽。仅执行构建与验证。

## 验证

```powershell
pnpm --dir codepluse-ui build
```

浏览器（127.0.0.1:8840/workspace，Ctrl+F5 强刷）+ 桌面端：
1. 点侧栏「自动化」→ 中右侧整体切换为 AutomationsPage，侧栏保持可见，行高亮。
2. 点「待办任务」→ TasksPage 渲染。
3. 点侧栏任一聊天项或「新建会话」→ 切回三栏对话视图。
4. 回归：侧栏折叠/展开、工作处理面板、设置按钮不受影响。
5. console 无报错。

## 已知范围

- 仓库面板(forge)/无限画布/用量页 同样经此注册表生效，属顺带修复。
- WorkbenchRouteStrip 标题条未挂载（页面自带标题，先不加）。
