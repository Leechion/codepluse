# 挂载右侧辅助面板（未渲染功能 ③）

## Context

`AuxPanel`（[aux-panel.tsx](file:///e:/code/codepluse/codeg-ui/src/components/layout/aux-panel.tsx)）从未被渲染。它提供 文件树/Git 变更/Git 历史/会话详情/任务/Forge 等 tab（L201-263），无打开文件夹时自动降级为仅「会话详情」tab（L85-105）。`AuxPanelProvider` 已在 workspace/layout.tsx 挂载，context（[aux-panel-context.tsx](file:///e:/code/codepluse/codeg-ui/src/contexts/aux-panel-context.tsx) L46-72）：`isOpen/restored/width(默认320, min200/max900)/toggle/setOpen/setWidth/activeTab/...`，localStorage key `workspace:right-sidebar`，**默认关闭**（不影响现有布局）。

挂载方式与侧栏/终端同模式：对话路由下，在 `.zw-chat-col` 右侧加一列可拖拽调宽的辅助面板；全屏路由（自动化/任务等）不渲染（对齐上游 isConversations 联动）；dock 加开关按钮。

## 改动

### 1. [ZCodeWorkspacePage.tsx](file:///e:/code/codepluse/codeg-ui/src/zcode/pages/ZCodeWorkspacePage.tsx)

- 导入：`AuxPanel`（`@/components/layout/aux-panel`）、lucide `PanelRight`。
- 解构：`const { isOpen: auxOpen, toggle: toggleAux, width: auxWidth, setWidth: setAuxWidth } = useAuxPanelContext()`。
- `startAuxResize(e)`：pointer 拖拽，`setAuxWidth(startW - (clientX - startX))`（向左拖=变宽），context 自动夹紧 200–900。
- JSX：`isConversations` 分支内、`.zw-chat-col` 之后追加：

```tsx
{auxOpen ? (
  <>
    <div className="zw-divider" role="separator" aria-orientation="vertical"
         aria-label="调整辅助面板宽度" onPointerDown={startAuxResize} />
    <div className="zw-aux" style={{ width: auxWidth }}>
      <AuxPanel />
    </div>
  </>
) : null}
```

- dock 追加辅助面板开关按钮（仅 `isConversations`）：`PanelRight` 图标，title 打开/收起辅助面板，onClick `toggleAux`。

### 2. [zcode-workspace.css](file:///e:/code/codepluse/codeg-ui/src/app/zcode-workspace.css)

- `.zw-aux`：右列容器，`display:flex; flex:0 0 auto; flex-direction:column; min-width:0; min-height:0; overflow:hidden; border-left:1px solid var(--sidebar-border); background:var(--background);`（宽度 inline 注入，同 `.zw-side` 镜像）。

## 验证

```powershell
pnpm --dir codeg-ui build
```

浏览器（Ctrl+F5）+ 桌面截图：
1. 点 dock 辅助面板按钮 → 右侧出现辅助面板（无文件夹时仅「会话详情」tab），对话区相应变窄。
2. 面板右缘 tab 栏可切换 tab；拖拽左缘分隔条 → 宽度 200–900 变化。
3. 再点按钮收起；刷新后开合/宽度保持。
4. 切「自动化」→ 面板随全屏路由隐藏；切回对话恢复。
5. 回归：侧栏/终端/工作处理/状态栏不受影响；console 无报错。

## 已知范围

- 无打开文件夹时只有「会话详情」tab 有内容（文件树/Git 类 tab 需要打开文件夹）——组件既有降级逻辑。
- 默认关闭，首次打开不影响现有布局习惯。
