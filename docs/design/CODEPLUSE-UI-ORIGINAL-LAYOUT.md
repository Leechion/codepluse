# codepluse 原布局视觉升级（修订稿）

Figma：https://www.figma.com/design/eNjOk5vGAqsntf5eqmGKpH?node-id=9-612

以当前源码 `ZCodeWorkspacePage.tsx`、`Sidebar`、`ConversationDetailPanel`、`ConversationDetailHeader`、`AuxPanel`、`StatusBar` 为准。只升级配色、字体、边框、圆角与控件视觉，不调整功能布局、导航层级和操作入口。

- 保留 320px 会话侧栏及原新建会话、自动化、待办任务、仓库面板、无限画布入口。
- 保留文件夹、聊天、最近列表及父子会话层级，不另加 Harness 导航分区。
- 保留顶部标签和会话标题、原消息流与原生输入框、原模型与模式选择入口。
- 保留输入框下方目录、分支、连接状态和上下文信息。
- 保留对话下方终端、右侧可选辅助面板与全窗口底部状态栏。
- 不采用上一版新增的工作台、任务协作看板、独立 Harness 管理页和常驻右侧协作概览；上一版已移动到 Figma 归档页。

两张可编辑画板：默认状态 `9:612`，终端与辅助面板展开状态 `9:832`。参考图仅影响视觉风格。消息、工具结果和数量为设计示例。此次仅修订 Figma，未修改前端实现。

预览：`codepluse-original-layout.png`、`codepluse-original-layout-panels.png`。
