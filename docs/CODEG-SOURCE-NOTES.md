# Codeg 源码研读笔记 → 已移植的改造

源码位置：`I:/NEW/code/codeg`（shallow snapshot，Next.js/TS 前端 + Rust/Tauri 后端，Apache-2.0）。
核心业务逻辑在 `src-tauri/src/acp/`（delegation broker 约 2.9 万行）。以下是研读结论与对应改造。

## 已移植

### 1. submit_result 按阶段 schema 校验（源：`cli_run/output_schema.rs` + `acp/result_tool.rs`）

**codeg 的设计**：schema 随连接的根会话存在；`submit_result` 的入参 schema 就是任务的 schema，
不合格的提交被**拒绝并附上"要改什么"**，模型当场重试（最多 `MAX_REFUSED=5` 次，之后放行防止死循环）；
MCP 不可达的 agent 才回退为"正文输出 JSON（整段或单个 ```json 块）"。

**移植到 harness-hub**：
- `core/orchestrator.js` 定义 6 个阶段 schema（plan / worker / report / decision / adjust / briefing），
  `call()` 按 stage 注入 `options.outputSchema`，经 `acp-runtime.mcpServers()` 以 argv 传给 `core/result-mcp.js`；
- result-mcp 内置轻量校验器（type/required/properties/items/enum，最多报 8 条错误），
  拒绝时 `isError` + 修正说明，模型同轮重试；同样 `MAX_REFUSED=5` 后放行；
- 文本 JSON 兜底保留（与 codeg 行为一致）。
- 配套改动：manager 各阶段 prompt 均自包含，故 manager 原生会话按 **schema 族**拆分
  （`manager:plan` / `manager:report` / `manager:decision`），保证一个会话内工具 schema 唯一。

### 2. worktree 失败回滚（源：`cli_run/worktree.rs`）

codeg 的规则："an attempt that fails takes away only what is provably its own"。
移植：`Worktrees.for()` 创建 worktree 失败时，删除自己刚建的分支（allowFail）再抛错；
mission id 按名称规则校验。未采纳：把 worktree 放仓库同级目录——我们的 `allowedRoot`
安全边界要求工作目录在项目根之内，同级目录会放宽边界，维持 `.worktrees/` 内置 + gitignore。

### 3. fork/委派链深度上限（源：`acp/delegation/depth.rs`）

codeg 用**饱和式遍历**（cap 截断，坏链不会导致无限查库）计算委派深度。
移植：`orchestrator.forkDepth()` 沿 `forkedFrom` 链饱和遍历，`fork()` 超过 5 层拒绝。

### 4. 提及路由语义确认（源：`acp/agent_mentions.rs`）

codeg 的 @ 路由判据是"提示词里的可见链接"（`[@Label](codeg://agent/<type>)`），无 out-of-band 声明，
且**有意让粘贴的旧链接与新鲜选择行为一致**（"That is the POINT"）；先去重、再对**去重后的** distinct
agent 数限量（16）。我们的纯文本 `@名` 解析语义与此一致（粘贴旧消息里的 @ 也会路由，属预期行为），
已按"先去重再限量"固化并写注释。

## 研读过但未移植（及原因）

- **路由帧字节身份**（agent_mentions.rs 的 RS 分隔符 + 逐字节重渲染 + 版本钉死）：
  用于从各 agent 私有转录中剥离注入的元数据。我们不注入转录帧，无剥离需求。
- **原生 session/fork**（fork.rs）：依赖 claude/codex/deepseek 适配器各自的 fork 点解析
  （message_id + fingerprint + occurrence），版本敏感、收益在"从历史任意轮分叉"。
  我们的 mission 级 fork 已覆盖主场景。
- **delegation broker**（broker.rs 1.1 万行）：多级子代理委派、每连接 companion MCP、
  事件总线。我们的宰相编排是另一种更窄的模型，整体照搬等于换架构。
- **assistant brief 帧**：等价于我们的 skills 注入，已有。

## 本地复刻注意

- 本仓库依赖 Rust 工具链（src-tauri）构建服务端与桌面壳，本机暂无 cargo；前端为 pnpm workspace。
- 参考阅读入口：`src-tauri/src/acp/agent_mentions.rs`、`src-tauri/src/acp/result_tool.rs`、
  `src-tauri/src/cli_run/output_schema.rs`、`src-tauri/src/cli_run/worktree.rs`、
  `src-tauri/src/acp/delegation/depth.rs`、`src-tauri/src/acp/fork.rs`。
