# codepluse · 多 Harness 工作台

新增 **codepluse 移植版**：保留 Codeg 0.42.0 原界面，使用 Go ACP 后台和 Go + WebView2 桌面窗口。运行 `npm run codepluse:start` 打开 <http://127.0.0.1:8840/workspace>；桌面构建入口为 `npm run codepluse:desktop:build`。安装、迁移范围及当前未完成部分见 [codepluse 移植说明](docs/CODEG-GO-PORT.md)。以下介绍原工作台。

主界面是一段你与「宰相」的持续对话。宰相由真实模型驱动，会讨论需求、自主拆解任务、选择执行 Harness、接收你的干预，并最终在对话里汇报。左侧显示各 Harness、分到的任务和实际状态。

网站没有自动生成的任务、消息、负载或演示实例。未连接服务时显示连接错误，不会回退到浏览器仿真。

## 启动

需要 Node.js 22+。首次安装固定版本的 ACP SDK 与 ACP 项目维护的 Codex / Claude 适配器：

```powershell
npm ci
npm start
```

### 桌面端

Windows 桌面端使用 Electron 独立窗口，连接 Go 后台，带中文菜单和系统托盘。开发运行 `npm run desktop`；构建本机桌面版运行 `npm run desktop:build`，然后双击 `dist/Harness Hub/Harness Hub.exe`。运行 `scripts/install-desktop.ps1` 可创建桌面快捷方式。

首次打开自动启动 Go 后台；已有 Go 服务时复用。关闭窗口收进托盘，双击托盘图标重新打开；从菜单退出界面后，后台仍继续执行任务和定时计划。桌面端与网页共用原有 `.data/` 数据。

当前构建是连接本机项目的桌面版，需要保留项目目录和整个 `dist/Harness Hub` 文件夹；项目路径记录在 `resources/workspace.json`，也可通过 `HARNESS_HUB_ROOT` 指定。`PORT` 可改服务端口。PWA 安装方式继续保留。

应用图标由 `node scripts/make-icons.js` 生成（纯 Node 手写 PNG 编码，改设计后重跑即可）。

打开 http://127.0.0.1:8787 。服务自动发现本机已安装的 Codex CLI 和 Claude Code，并注册真实执行实例。CLI 需要事先登录或配置可用的模型服务；安装检测通过不代表模型服务已经可达。

## 界面布局

两套界面共用一套设计系统（浅色主题、统一令牌与组件），均为「顶栏 + 三栏工作区 + 底部状态条」：

```
对话式主界面（默认）                 系统管理控制台
┌────────────────────────────┐   ┌────────────────────────────┐
│ 顶栏：品牌 · 会话标题 · 状态 · 操作 │   │ 顶栏：品牌 · 全局指标 · 操作      │
├───────┬───────────┬────────┤   ├──────┬────────────┬────────┤
│ 对话与 │ 对话流      │ 执行   │   │ 导航  │ 视图工作区   │ 常驻    │
│ 团队   │ （限宽 780）│ 面板   │   │      │            │ 实时   │
│       │             │ 任务/指标│   │      │            │ 面板   │
├───────┴───────────┴────────┤   ├──────┴────────────┴────────┤
│ 状态条：连接 · tick · 实例 · 审计链 │   │ 状态条（共用同一渲染）           │
└────────────────────────────┘   └────────────────────────────┘
```

- **执行面板**（主界面右栏，新增）：本轮状态徽章、各 Harness 的任务卡（状态进度条、执行端、调用次数、依赖）、协作轮次 / 任务总数 / 完成数 / 团队规模指标。worktree 隔离任务结束后在面板内「查看变更 / 合并到主分支 / 丢弃」（合并与丢弃需二次确认）。对话限宽 780px 居中，用户消息为右侧浅靛气泡，宰相回复为白卡，协作消息与成果回传为可折叠卡片。
- **输入框 @ 委派**：在消息里输入 `@` 会弹出执行端补全（键盘 ↑↓ 选择、Enter/Tab 确认、Esc 关闭），选中后直接委派。
- **定时任务面板**（顶栏「定时任务」）：列表展示每条任务的对话、cron 与中文预览、下次触发时间和上次结果；可停用/启用/删除，或在同一面板新建。
- **常驻实时面板**（控制台右栏，新增）：实时事件流（全部 / 告警 / 错误过滤）、待审批申请的快捷批准 / 拒绝、系统负载——切换任何视图都保持可见，不再丢失全局态势。
- **全局指标条**（控制台顶栏，新增）：在线实例、活跃任务、真实任务、待审批四项常驻指标。
- **底部状态条**（两套界面共用）：服务连接、引擎 tick、实例、任务进度、审计链完整性、真实执行状态。
- 响应式：≤1500px 自动收起右栏（执行面板 / 实时面板）与顶栏指标；≤900px 团队侧栏收窄并保留任务和会话信息，管理导航收为图标栏。

## 下达任务

1. 直接在底部输入目标，宰相会根据对话判断是继续讨论、开始执行还是调整当前任务。无需先填写任务表单。
2. 在消息里 `@执行端名`（如 `@Codex 检查登录模块`）可跳过拆解、直接把目标委派给该执行端；多个 `@` 会并行启动各自的任务，完成后仍由宰相验收汇报。也可通过 API 传 `mentions` 数组达到同样效果。
3. 默认允许宰相在已连接的真实 Harness 之间自主分工。通过右上角「模型与分工设置」可选择负责人、限制执行团队，并分别填写宰相和各执行端的模型名称。
4. 在左侧查看每个 Harness 的任务。点击正在执行的任务，可以指定新的执行端、模型和具体要求；人工指定会被保留，宰相不能覆盖。
5. 随时追加消息，例如「把审查交给 Codex」「先只检查登录模块」「alpha 使用 gpt-6-luna」。宰相会将调整应用于后续调用，不会把当前调用重新标成另一执行端。
6. 执行过程、成员回传、相互通信和最后汇报都会进入当前对话。默认只读；工作目录默认是项目根目录。

设置修改用于后续新任务。进行中的子任务可在侧边栏单独调整；自然语言指定模型时请写出完整模型名称。模型名称必须由对应 CLI 或兼容网关支持，服务会将名称传入实际的 `--model` 参数或 HTTP `payload.model`，无效模型的调用会如实报错。

对话历史保存在 `.data/chats.json`。原有智能体管理、接入配置和审计页面仍可通过「系统管理」进入。

结构化回传：编排类会话会自动注入 `submit_result` MCP 工具（`core/result-mcp.js`，经 ACP `session/new` 的 `mcpServers` 参数，携带该阶段的输出 schema）。执行端与宰相把约定的 JSON 字段作为工具参数提交；不符合 schema 的提交被拒绝并附上修正说明，模型在同轮重试（最多 5 次，之后放行由服务端最终校验）；服务端从 ACP `tool_call` 的 rawInput 取结构化结果，正文 JSON 仅作兼容兜底。自定义 ACP 适配器会话不注入该工具，继续使用文本约定。设计与实现对照见 `docs/CODEG-SOURCE-NOTES.md`。

工作树隔离：任务可指定 `isolate: 'worktree'`（对话设置或 mission 参数），在 `.worktrees/<missionId>` 的独立 git worktree 中执行；结束后 `GET /api/missions/:id/diff` 查看变更（含未跟踪新文件），审查后 `POST .../merge`（自动提交 worktree 内改动后 `--no-ff` 合并）或 `POST .../discard`。非 git 目录自动回退共享目录，不影响执行。

定时任务：`POST /api/automations` 保存「对话 + 提示词 + 5 字段 cron」，服务端每 30 秒检查触发，到点相当于代用户向该对话发一条消息；目标对话有未结束任务时标记 `busy` 跳过。任务会话可通过 `POST /api/sessions/:id/handoff` 移交给其他 ACP 执行端：宰相把已有纪要压缩为简报并在目标执行端开启新原生会话，原会话标记 `handedOff` 后不再接受调用。执行端可通过 `PATCH /api/agents/:id` 配置 `skills`（`skills/*.md`，最多 3 个），编排提示词会自动附带技能正文；任务结束后配置环境变量 `HARNESS_HUB_WEBHOOK_URL` 可把任务终态推送到 Webhook（失败静默重试一次）。已结束的任务可 `POST /api/missions/:id/fork` 分叉：任务板成果带入新任务、全部重新排队执行。

宰相会返回 1–8 个子任务及其依赖关系。子任务只能交给你选中的执行端。无效计划会由宰相修正一次，修正后仍无效则报错。只读任务可以并发，写入或命令任务顺序执行，避免同时改写共享目录。

所有执行端都能看到同一任务的分工表、已完成成果和各自收到的消息。执行端返回 `messages` 时，调度器将消息投递至目标子任务的收件箱；目标在下一轮真实调用时处理。已完成的子任务收到新消息后会重新进入处理队列。需要同僚回答的任务必须返回 `complete: false`。协作最多推进 6 轮，未完成或失败会显示「部分完成」，并纳入汇报。

原生连接使用 ACP v1。宰相在同一对话中复用同一个原生会话，每个子任务保留自己的原生会话，后续轮次使用同一 session ID；指定新模型时通过 ACP 配置切换并保留上下文。服务重启后，支持 `session/load` 的 Harness 会从原生历史恢复，而不会悄悄创建失去上下文的新会话。目前不接管你在其他窗口里已经打开的任意交互会话。

侧栏会显示各 Harness 的原生会话。点击会话或顶栏「原生会话」可查看原生 ID、实际模型、权限模式、调用轮次、工具活动、文件差异和工具输出，并停止正在执行的调用。流式内容和工具状态通过 SSE 更新；用于内部编排的 JSON 不会作为模型思考过程展示。

目录内读写任务使用 Harness 的人工授权模式；权限卡保留该 Harness 给出的选项，需要用户选择后才继续。只读会话拒绝提升写入权限；等待用户授权的时间不计入模型调用超时。模型输入框的建议项来自已经连接的原生会话返回的模型目录，仍可输入网关支持的自定义模型名称。

协议与适配器参考：[Codeg 接入方式](https://docs.codeg.app/guide/supported-agents)、[ACP TypeScript SDK](https://github.com/agentclientprotocol/typescript-sdk)、[Codex ACP](https://github.com/agentclientprotocol/codex-acp)、[Claude Agent ACP](https://github.com/agentclientprotocol/claude-agent-acp)。

## 权限和真实执行

| 模式 | Codex | Claude Code |
| --- | --- | --- |
| readonly | read-only | plan（只读） |
| workspace | workspace-write（人工授权） | default（人工授权） |
| shell | read-only | plan（命令仍受只读权限约束） |

执行端可返回 `reads: [{"path":"package.json"}]`，服务端真实读取文件并在下一轮 `files` 中返回带行号的内容，不依赖各工具的 shell。路径穿越和指向工作目录外的链接会被拒绝。每次最多读取 4 个文件，最多保留 12 份文件内容，每份返回不超过 30000 字符；大型文件可指定 `startLine` 和 `lineCount` 读取前 256 KiB 中的片段。该只读文件模块也附带可选的 [MCP stdio](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports) 入口。

宰相的规划和汇报使用只读权限；执行端使用任务指定的权限。最后由宰相逐项验收，只有执行状态和验收结论都通过才标记全部完成。真实调用会消耗所选工具的模型额度。原始输出、退出码、耗时以及工具返回的模型/费用信息可在「任务流」查看。

## 接入其他 Harness

优先通过侧栏「接入 ACP Harness」注册已安装的 ACP stdio 适配器：填写执行文件、JSON 参数数组，以及该适配器的只读和工作区模式 ID。支持 `plan`、`read-only`、`readonly` 三种只读模式命名；适配器必须实际提供选定的模式。命令不会经 shell 执行，Windows 的 `.cmd` / `.bat` 入口应改为 `node` 加对应 JS 文件的参数形式。

例如 API 注册一个本地适配器（示例路径需要替换为实际安装路径）：

```json
{"name":"My ACP Agent","type":"my-agent","connection":"acp","command":"node","args":["C:/tools/my-agent/index.js","--acp"],"readOnlyMode":"plan","workspaceMode":"default"}
```

ACP 接入定义保存在 `.data/acp-agents.json`，登录和凭据由各 Harness 自身管理。还保留以下 HTTP 请求/回复兼容桥接，HTTP 桥接是否保留原生上下文由桥接服务自行实现。

提供一个 HTTP 桥接服务，实现 `POST /messages`：请求体是标准信封，返回 `{ "ok": true, "output": "模型输出" }`。在「接入新工具」中填写 HTTP(S) 地址、名称和可选 Bearer 令牌，就可以将它选作宰相或执行端。

可运行的本机 CLI 桥接：

```powershell
$env:BRIDGE_TOOL = 'claude' # 或 codex
$env:BRIDGE_PORT = '9001'
$env:BRIDGE_WORKDIR = (Get-Location).Path
$env:BRIDGE_TOKEN = 'your-bridge-token'
npm run bridge
```

这个示例桥接固定只读。接入其他工具时可复用 HTTP 接收端：

```js
const { createBridge } = require('./adapters/cli-bridge');
const server = createBridge(async envelope => {
  const output = await yourHarness.send(envelope.payload.text);
  return { ok: true, output };
}, { token: process.env.BRIDGE_TOKEN });
server.listen(9002, '127.0.0.1');
```

`yourHarness.send` 需要替换为对应工具的真实接口。HTTP 实例默认只声明只读能力；桥接实际支持其他权限时，注册时可显式声明 `permissions: ["readonly", "workspace"]`，并在桥接端执行相应限制。HTTP 请求还会传入 `payload.permission` 和 `payload.workdir`。

## API

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| GET / POST | `/api/chats` | 对话列表 / 新建对话 |
| GET | `/api/chats/:id` | 对话、配置、关联任务与执行状态 |
| POST | `/api/chats/:id/messages` | 发送消息，让宰相决定回复、执行或调整；可携带人工 `overrides` |
| PATCH | `/api/chats/:id/config` | 宰相、允许团队、模型、执行权限和目录 |
| GET | `/api/sessions?chatId=...` | 原生会话、模型目录、工具活动和权限请求 |
| POST | `/api/sessions/:id/cancel` | 向原生 Harness 发送取消调用请求 |
| POST | `/api/session-permissions/:id` | `{ "optionId": "原生选项ID" }` 回答权限卡；null 表示取消 |
| GET / POST | `/api/missions` | 任务列表 / 下达目标并启动编排 |
| GET | `/api/missions/:id` | 状态、分工、消息、成果与汇报 |
| GET | `/api/missions/:id/diff` | worktree 隔离任务的变更摘要 |
| POST | `/api/missions/:id/merge` / `discard` | 审查后合并或丢弃 worktree |
| POST | `/api/missions/:id/fork` | 从已结束任务板分叉新任务 |
| GET / POST | `/api/automations` | 定时任务列表 / 新建（chatId + prompt + cron） |
| PATCH / DELETE | `/api/automations/:id` | 启停、修改、删除定时任务 |
| POST | `/api/sessions/:id/handoff` | 把原生会话移交给其他 ACP 执行端 |
| GET | `/api/skills` | 可用技能包列表 |
| GET / POST | `/api/agents` | 执行端列表 / 注册真实连接 |
| PATCH | `/api/agents/:id` | 启用或禁用 |
| POST | `/api/messages` | 手动向真实 Harness 发送消息并收取回复 |
| GET | `/api/snapshot` | 智能体、真实调用任务、消息、授权和审计 |
| GET | `/api/events` | SSE 实时事件流 |
| GET / POST | `/api/tasks` | 查看调用任务 / 单次真实 CLI 任务 |
| GET | `/api/real/status` | CLI 安装情况和配置 |
| POST | `/api/real/config` | 执行目录、权限、超时、并发配置 |
| GET | `/api/audit/verify` | 校验审计链 |

下达目标示例：

```powershell
$body = @{
  objective = '分析项目架构与关键缺陷，让两位执行端核对结论，给出有文件依据的报告'
  managerId = 'agt_claude_01'
  workerIds = @('agt_codex_01', 'agt_claude_01')
  permission = 'readonly'
} | ConvertTo-Json
Invoke-RestMethod http://127.0.0.1:8787/api/missions -Method Post -ContentType 'application/json; charset=utf-8' -Body ([Text.Encoding]::UTF8.GetBytes($body))
```

执行端 ID 可从 `GET /api/agents` 获取，只有安装成功的本机 CLI 才会注册上述默认 ID。

## 配置

| 环境变量 | 默认值 | 用途 |
| --- | --- | --- |
| HOST / PORT | 127.0.0.1 / 8787 | 监听地址 |
| HARNESS_HUB_REAL | 1 | 设为 0 禁用本机 CLI，仍可使用真实 HTTP 桥接 |
| HARNESS_HUB_WORKDIR | 项目根目录 | 默认执行目录 |
| HARNESS_HUB_ALLOWED_ROOT | 项目根目录 | 执行目录允许范围 |
| HARNESS_HUB_TIMEOUT | 180000 | 单次 CLI 超时，毫秒 |
| HARNESS_HUB_CONCURRENCY | 2 | 真实调用的并发上限 |
| HARNESS_HUB_CODEX_BIN / HARNESS_HUB_CLAUDE_BIN | 自动检测 | 指定 CLI 路径 |
| HARNESS_HUB_CODEX_ISOLATE | 1 | 使用隔离 CODEX_HOME，复用本机登录态 |
| HARNESS_HUB_CODEX_GATEWAY | 1 | 存在兼容网关配置时使用该网关 |
| HARNESS_HUB_CODEX_MODEL | 网关默认 | 指定 Codex 模型 |
| HARNESS_HUB_WEBHOOK_URL | 未启用 | 任务终态（完成/部分/失败）推送的 Webhook 地址 |

最近 20 项目标的分工、通信和汇报保存在 `.data/missions.json`；原生会话映射和最近轮次保存在 `.data/sessions.json`，支持恢复时在下一条消息到达后重新连接。重启中断的编排任务会标为失败，不会自动重复执行。旧的单次调用列表与 HTTP 令牌仍为内存状态。Hub API 作为可信本机控制面使用，尚无调用方身份认证；凭据不会写入会话映射或任务归档。

## 代码与检查

- `core/orchestrator.js`：规划、依赖调度、任务收件箱、协作轮次、汇报；`@` 直接委派、fork 分叉、worktree 隔离与技能注入。
- `core/chat-controller.js`：持续对话、模型驱动决策、人工干预、mention 委派与汇报回传。
- `core/acp-runtime.js`：长连接进程、原生会话复用与恢复、模型目录、工具事件、权限选择和取消；`submit_result` 结构化结果捕获与会话纪要导出。
- `core/result-mcp.js`：编排会话注入的 `submit_result` MCP 工具（结构化回传）。
- `core/worktree.js`：git worktree 隔离、diff 审查、审查后合并/丢弃。
- `core/automations.js`：定时任务（自写 5 字段 cron，触发即代发对话消息）。
- `core/skills.js`：技能包矩阵（`skills/*.md` → 按执行端注入提示词）。
- `core/handoff.js`：跨执行端原生会话移交（简报 + 纪要）。
- `core/notifier.js`：任务终态 Webhook 通知。
- `core/engine.js`：执行记录、消息鉴权、上下文 ACL、审计。
- `core/real-adapters.js`：真实 Codex / Claude 子进程调用。
- `core/http-adapter.js`：HTTP 真实连接。
- `server.js`：API、调用并发队列、CLI 自动注册。
- `public/chat.js`：主对话、Harness 任务侧栏、模型和分工设置。

```powershell
npm run check
npm test
```

自动测试覆盖依赖分工、双向协作、失败和无效计划、并发限制、HTTP 往返和 REST 注册；使用测试执行端，不产生模型费用。真实 CLI 验收通过指挥台另行提交。
