# Harness Hub × Codeg 架构对齐设计

参照 [Codeg](https://github.com/spacering-net/codeg) 的多 Agent Hub 设计，规划本项目的演进路线。
原则与 codeg 一致：**本 Hub 不做模型推理，价值在于统一会话管理、任务编排与跨 Agent 协作**；
本地优先、无外部服务依赖、每项能力都挂在已有的真实 ACP 执行端上。

---

## 一、现状对照

| 能力 | Codeg 的做法 | Harness Hub 现状 | 差距 |
| --- | --- | --- | --- |
| 统一 Agent 接入 | ACP 协议 + 适配器，20+ 内置 agent | ACP v1 + HTTP 桥接，Codex / Claude + 可注册任意 ACP stdio | ✅ 已对齐 |
| 编排调度 | `@` 委派、多 agent 并行、流式回传 | 宰相规划 → 1–8 子任务 → 依赖波次调度 | ⚠️ 缺 `@` 直接委派 |
| 结构化回传 | MCP `submit_result` 工具 + `--output-schema` | 约定 JSON + 文本容错解析 | ❌ 需补 MCP 工具回传 |
| 无人值守任务 | 独立 git worktree 隔离 + 审查后合并 | 共享 workdir，写任务串行 | ❌ 需补 worktree |
| 定时任务 | Automations：保存的 composer 按 cron 运行 | 无 | ❌ 需补 |
| 会话恢复 | `session/load` + 各 agent 磁盘历史导入 | `session/load`（同 agent） | ⚠️ 缺跨 agent 移交 |
| Fork & Steer | 从任意回复分叉；运行中插入消息 | steer ✅；fork ❌ | ⚠️ 补 fork |
| 通知 | Webhook / 聊天渠道，只推主会话完成与错误 | SSE 页面内事件流 | ❌ 需补 Webhook |
| 技能包 | Skill Packs 按 agent 矩阵分配 | 无 | ❌ 需补 |
| 权限审批 | 授权模型（none/read/control） | 权限卡 + 只读拒绝提升 | ✅ 已对齐 |
| 审计 | token 认证、本地优先 | 审计链 + `/api/audit/verify` | ✅ 已对齐 |
| 仓库面板 | Issue/PR 自动处理 | 无 | P2 可选，依赖外部连接器 |

**不照搬的部分**：桌面壳（Tauri）、浏览器 agent、移动端、20+ 内置 agent 清单。本项目保持"轻服务端 + Web UI"形态。

---

## 二、目标架构

```
                         ┌────────────────────────────────────────┐
                         │              public/ (Web UI)          │
                         │   对话 · 侧栏任务 · 审批卡 · 定时任务面板   │
                         └───────────────┬────────────────────────┘
                                         │ REST + SSE（不变）
┌────────────────────────┴──────────────────────────────────────────────┐
│ server.js  ·  API 路由 + 并发队列 + CLI 自动注册                          │
│                                                                       │
│  chat-controller   ←→  orchestrator   ←→  engine（审计/ACL/任务记录）     │
│       │                     │                                          │
│  ┌────┴─────┐   ┌──────────┴───────────────────────────────────┐      │
│  │ delegate │   │ result-mcp（新）· worktree（新）· automations（新）│      │
│  │ （新）    │   │ skills（新）· notifier（新）· handoff（新）      │      │
│  └────┬─────┘   └──────────┬───────────────────────────────────┘      │
│       │                    │                                          │
│  acp-runtime ──────────────┘   （长连接驱动 · 原生会话 · 权限 · 超时）      │
└────────────────────────┬──────────────────────────────────────────────┘
                         │ ACP stdio（spawn 子进程）
              ┌──────────┴──────────┐
              │ Codex ACP  │ Claude Agent ACP │ 任意已注册 ACP 适配器
              └─────────────────────┘
```

核心不变式（延续现有代码约定）：

- 所有模型交互仍走 `acp-runtime.turn`，单会话串行（`turnQueues`）。
- 只读并发、写串行的波次调度不变；worktree 是"新工作目录"，不改变并发规则。
- 结构化 JSON 约定逐步被 `submit_result` 工具替代，文本解析降级为兜底。
- 新模块零运行时依赖（不引 node-cron 等），cron 用自写 5 字段解析器。

---

## 三、模块设计

### 1. `core/result-mcp.js` — 结构化回传（P0，最优先）

**问题**：现在 worker 必须在正文里输出约定 JSON，`orchestrator.json()` 靠正则剥离 Markdown 兜底，是整条链路最脆的一环。

**方案**（对标 codeg 的 `codeg-mcp` sidecar + `submit_result`）：

- 复用 `core/readonly-mcp.js` 的 stdio MCP 骨架，新增 `submit_result` 工具：

```jsonc
// 工具入参 schema —— 与现有 worker 约定字段一一对应
{ "output": "string", "complete": "boolean", "blocked": "boolean",
  "messages": [{ "to": "子任务ID", "text": "string" }],
  "reads": [{ "path": "string", "startLine": 1, "lineCount": 200 }] }
```

- 启动子任务时把该 MCP server 以 `session/new` 的 `mcpServers: []` 参数注入（acp-runtime `ensure()` 目前固定传空数组，改为接受 `options.mcpServers`）。
- `acp-runtime.update()` 已记录 `tool_call` 事件到 `turn.tools`；`turn()` 结束时扫描本轮 `submit_result` 调用，命中则直接取其结构化参数作为结果，**文本输出只作展示**。
- 兜底链：`submit_result` 工具结果 → 正文 JSON（现状逻辑）→ 报错。worker 提示词改为"优先调用 submit_result 工具"。
- 顺带消灭一类失败：模型把 JSON 写进思考块或代码块注释的情况。

**改动点**：`core/readonly-mcp.js`（或拆新文件）、`acp-runtime.js` 的 `ensure/turn/update`、`orchestrator.call/work` 的结果读取、worker 提示词。

### 2. `@` 直接委派（P0）

**对标**：codeg 在对话里 `@agent` 即把任务交给该 agent 并排执行、进度流式回主对话。

**方案**：

- `chat-controller.send()` 在进入宰相决策前解析 `@<agent名>`（前端输入框提供补全，服务端按 `hub.agents` 的 name/id 双匹配）。
- 命中时走新路径 `delegate(chat, agentId, text)`：不经宰相拆解，直接 `orchestrator.create({ objective: text, managerId: 宰相, workerIds: [agentId], skipPlanning: true })`。
- `skipPlanning` 时 `orchestrator.run()` 跳过"宰相拆解任务"，直接生成单任务 `t1{agentId, instructions: objective}`，保留汇报与验收环节（codeg 同样保留主对话的完成回报）。
- 多个 `@` 各建一个独立 mission，**并行运行**；各任务流式状态仍经 SSE 汇入当前对话（复用 `onMission`）。
- API：`POST /api/chats/:id/messages` 增加 `mentions: ["agt_codex_01"]` 字段；无 `@` 时行为完全不变。

### 3. `core/worktree.js` — worktree 隔离的无人值守任务（P1）

**对标**：codeg 的无人值守 To-dos 在独立 git worktree 中隔离运行，完成后用户审查 diff 再批准。

**方案**：

```js
// core/worktree.js
class Worktrees {
  async for(missionId)   // git worktree add .worktrees/<missionId> -b hh/<missionId>
                         //   已存在则复用；非 git 仓库时抛错并回退共享 workdir
  diff(missionId)        // git diff <base>...hh/<missionId>，截断到 200KB
  merge(missionId)       // 校验工作区干净 → merge --no-ff → 删除 worktree 与分支
  discard(missionId)     // worktree remove --force + branch -D
}
```

- 触发方式：对话设置或 mission spec 增加 `isolate: 'worktree' | 'shared'`（默认 shared，行为不变）。
- acp-runtime 的 `cwd` 本就按 `[agent, permission, cwd]` 分 driver，worktree 路径天然获得独立进程与独立原生会话，**零改动**。
- 任务完成后右栏任务卡展示 `worktrees.diff()` 摘要 + 「合并 / 丢弃」按钮；对应新 API：

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| GET | `/api/missions/:id/diff` | worktree 相对基线的 diff |
| POST | `/api/missions/:id/merge` | 审查后合并（body: `{ message }`） |
| POST | `/api/missions/:id/discard` | 丢弃 worktree |

- 审计：merge/discard 均经 `hub.sendMessage` 落审计链。

### 4. `core/automations.js` — 定时任务（P1）

**对标**：codeg Automations——保存已配置的对话输入，按 cron 或按需无头运行。

```js
// 存储 .data/automations.json
{ id, chatId, prompt, cron: '0 9 * * 1-5', enabled, lastRunAt, lastStatus, createdAt }
```

- 运行即调用现成的 `chatController.send(chatId, prompt)`——定时任务不是新执行通道，只是"代用户发一条消息"，天然复用宰相决策、权限、审计与 SSE。
- 自写 5 字段 cron 解析（`分 时 日 月 周`，支持 `* , - /`），server.js 主循环每分钟 tick 一次；启动时补算 `nextRunAt`。
- API：`GET/POST /api/automations`、`PATCH /api/automations/:id`（启停/改）、`DELETE`。
- UI：对话页顶栏加「定时任务」入口，列表 + cron 预览（如"每个工作日 09:00"）。
- 限制：同一 chat 已有未终态 mission 时本轮跳过并记 `lastStatus: 'busy'`，不排队。

### 5. `core/handoff.js` — 跨 Agent 会话移交（P2）

**对标**：codeg 的会话可移植性——"把会话交给另一个 agent 继续"。

- `acp-runtime.exportTranscript(runtimeSessionId)`：聚合该 runtime session 的 `turns[].output` 与工具摘要为纯文本纪要（现成数据，`turns` 已保留最近 20 轮）。
- `POST /api/sessions/:id/handoff { toAgentId }`：宰相（只读）把纪要压缩为移交简报 → 在目标 agent 上以 `sessionKey: 'handoff:' + 原ID` 开新原生会话，注入简报；原会话标记 `handedOff`，不再接受 prompt。
- 与 codeg 直接解析各 agent 磁盘历史的差异：不做磁盘导入（各家格式私有且易碎），只做 Hub 内原生会话互移——能覆盖"这任务换 Codex 接着干"的主场景，成本和风险都小一个量级。

### 6. `core/notifier.js` — Webhook 通知（P0，成本最低）

**对标**：codeg 只推主会话的完成与错误，附会话/agent/父会话标识。

```js
// 环境变量 HARNESS_HUB_WEBHOOK_URL 存在即启用
hub.on('mission', m => { if (terminal(m)) POST webhook });
// payload: { missionId, chatId, status, reply, error, finishedAt, workerIds }
```

- 只推终态（completed / partial / failed），与 codeg "减少子 agent 干扰"的取舍一致。失败静默重试 1 次，不影响主流程。

### 7. `core/skills.js` — 技能包（P2）

**对标**：codeg Skill Packs 按 agent 矩阵分配。

- 仓库根 `skills/<name>.md`（名称 + 指令正文），agent 注册项增加 `skills: ["review-style", ...]`。
- `orchestrator.call()` 组 prompt 时按 `agentId` 追加命中的技能正文（截断 4000 字符/技能，最多 3 个）。
- API：`GET /api/skills`；注册/修改 agent 时可配置矩阵。
- 用途：把"Codex 负责实现、Claude 负责按某评审清单复核"这类稳定分工固化下来。

### 8. `orchestrator.fork()` — 从成果分叉（P2）

- steer 已覆盖"运行中插话"；补齐 codeg 的另一半：从**已结束** mission 分叉。
- `POST /api/missions/:id/fork { taskId? }`：克隆任务板（outputs/inbox/files 原样带入，状态全部重置为 queued，dependsOn 保留），新 mission 挂同一 chat；用户在对话里说"基于刚才 xxx 再来一版"时由宰相决策调用。

---

## 四、存储与 API 增量汇总

新增存储：`.data/automations.json`；`.worktrees/`（git 管理，不入库）。
`sessions.json`、`missions.json`、`chats.json` 结构向后兼容，只加可选字段。

| 方法 | 路径 | 所属模块 |
| --- | --- | --- |
| GET/POST/PATCH/DELETE | `/api/automations` | automations |
| GET | `/api/missions/:id/diff` | worktree |
| POST | `/api/missions/:id/merge` / `discard` | worktree |
| POST | `/api/missions/:id/fork` | orchestrator.fork |
| POST | `/api/sessions/:id/handoff` | handoff |
| GET | `/api/skills` | skills |
| （扩展） | `POST /api/chats/:id/messages` 增加 `mentions` | delegate |
| （扩展） | agent 注册项增加 `skills` | skills |

---

## 五、实施顺序与理由

| 阶段 | 内容 | 理由 |
| --- | --- | --- |
| P0 | ① result-mcp 结构化回传 ② `@` 委派 ③ notifier | ①修最脆的解析链路，②③是纯增量、互不依赖 |
| P1 | ④ worktree 隔离 + 审查合并 ⑤ automations | ④依赖 git 环境，先在对话设置加开关灰度；⑤等 P0 稳定后借同一 tick 循环 |
| P2 | ⑥ 会话移交 ⑦ 技能包 ⑧ fork | 都是好上加好；⑥⑦依赖前两阶段打磨出的会话/提示词基础设施 |

每阶段补对应 `test/*.test.js`（用现有 fixtures 测试执行端，不产生模型费用）：

- result-mcp：工具回传命中/兜底解析/双通道冲突
- delegate：`@` 解析、skipPlanning 计划生成、并行 mission 回写对话
- worktree：非 git 回退、diff/merge/discard、worktree 内 cwd 隔离
- automations：cron 解析边界、busy 跳过、启停持久化

## 六、风险与决策点

1. **`mcpServers` 注入**：Codex / Claude ACP 适配器对 `session/new.mcpServers` 的支持程度需实测；不支持时 result-mcp 降级为"仅 Claude/Codex 各自可用的场景"，文本 JSON 兜底永久保留。
2. **worktree 与权限**：worktree 内 `workspace` 任务仍走人工授权，不变；但要防止 merge 时覆盖用户未提交的本地改动——merge 前强制要求主工作区干净，否则拒绝并提示。
3. **cron 精度**：自写解析器只做分钟级；不上 Quartz 式表达式。
4. **宰相权威**：`@` 委派绕过拆解，但汇报验收仍归宰相——保持"只有一个对用户负责的声音"的现有设计，避免 codeg 并排多 agent 各说各话的碎片感。
