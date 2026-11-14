# C 组 ACP 运行时移植 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement
> this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `docs/RUST-BACKEND-REMAINING.md` C 组（ACP 运行时 + 28 条命令）整流迁移到
`src-tauri/src/backend/acp/`，在隔离 worktree（分支 `acp-port-c`，baseline `2517c5a`）完成，
集成回主检出时只拷新文件 + 3 个共享文件的小 hunk。

**Architecture:** Rust 侧常驻 `Runtime`（tokio 进程池 + stdio NDJSON JSON-RPC + 会话记录 +
权限卡 + 可暂停回合时钟），连接层（warm pool / 事件流 / 快照）与 Go `codeg` 包逐行为对应。
Agent 注册表按 Go boot 装配逻辑从持久化文件**每次请求无状态重建**（内置探针 + Store.CustomAgents
+ harness-instances.json + acp-agents.json + Agents overlay），与未移植的 I 组命令保持一致。
对话/回合落盘沿用批次 5 约定：每次请求经 `store_file` 锁读-改-写 `codeg-workspace.json`，
`goformat::marshal_store` 落盘，Go 侧 `reloadIfChangedLocked` 兜底。WS 事件经 mod.rs 既有
`emit_events` 注入。

**Tech Stack:** tokio（补 `process`/`io-util`/`sync` features）、`toml`（codex config）、
`serde_yaml`（dsh patch）、`zip`/`flate2`/`tar`/`bzip2`（custom 归档解包）、既有
`net.rs`（binary 归档下载，**只调用不修改**）、`store_file`+`goformat`、serde_json
`preserve_order`。

**Spec:** `docs/RUST-BACKEND-REMAINING.md` C 组；Go 源码以本 worktree `server-go/`（eedc419）
为准：`internal/acp/runtime.go`、`internal/codeg/{server,connections,fork,preflight,
agentconfig,custom_launch,custom_archive,tool_identity,question,wire,store,history,
companion}.go`、`cmd/serve/{wiring,cli,provider_profiles,official_accounts,dsh_model,
anthropic_base,state}.go`。测试 fixture：`fixtures/acp-agent.cjs`（node）。

## Global Constraints

- 隔离：一切 cargo/go 构建只在 worktree；主检出 E: 在 B 会话结束前**不碰**。
- 共享文件只允许 3 处增量 hunk：`src-tauri/src/backend/mod.rs`、`src-tauri/Cargo.toml`、
  `src-tauri/src/backend/experts.rs`（新增 owns 两条）。forge/net/updater/providers/git/
  store_* 一律不改。
- 响应包络：成功 `go_json`（canon 后 marshal_preserve + `\n`，200）；失败 `fail()`（400
  `task_execution_failed`）；唯一例外 `turn_in_progress` → 409
  `{"code":"turn_in_progress","message":"A turn is already in progress"}`（Go server.go:275）。
- 状态文件：desktop 下 `CODEG_MODE=1`（`HARNESS_HUB_CODEG=1`）→ `sessions.json` 换名
  `.data/codeg-sessions.json`（main.go:513-516）；`:memory:` 分支仅测试用。
- 端口纪律：任何双跑/运行验证用非 8840 端口；不在主检出发 cargo。
- Go 侧源文件零改动（CRLF 陷阱 + B 会话并行）。
- 会话记录 JSON 需 Go wire 字段序（serde struct 声明序 + `preserve_order`），仅
  `sessions.json` 落盘与 ListJSON 用；HTTP 响应一律走 canon（Go 侧这些返回都是
  `map[string]any`）。

## Review Focus

1. **回合时钟无活动超时**：流式/工具更新必须续期 deadline，pending 权限卡存在时 deadline
   无限顺延；cancel 后 10s 强杀 → 期待 `ACP 会话超时`（runtime_test.go:169-185 对应两个测试）。
2. **权限卡不阻塞读循环**：`session/request_permission` 在独立任务应答，通知按 wire 序先于
   最终响应落库（TestPermissionRequestDoesNotBlockStreamReader）。
3. **只读会话越权**：`AllowInternalTool` 只放行 codeg-mcp 工具且有 caller 的连接；原生写请求
   在 readonly 模式一律 cancelled（TestOnlyOwnedInternalToolsMayBeAllowedInReadOnlySessions）。
4. **fork 事务原子性**：store 锁 + 连接锁双校验、失败回滚（ForgetFork + 内存快照还原），
   `turn_in_progress` 用 TryLock（fork.go 全文）。
5. **warm pool 不泄漏**：TTL 2min、占用即出池、`releaseWarm` 不杀活跃会话、连接列表跳过
   warm（connections.go:200-371）。
   每条都已落在对应任务的测试步骤里。

---

### Task 1: 依赖与骨架

**Files:**
- Modify: `src-tauri/Cargo.toml`（tokio features + 新 deps，全部追加行）
- Create: `src-tauri/src/backend/acp/mod.rs`（空 `owns()` 占位 + 模块声明）

**Interfaces:**
- Produces: `pub mod acp;`；`acp::owns(&command) -> bool`（先返回 false）。

- [ ] **Step 1:** Cargo.toml 追加：tokio features 加 `"process","io-util","sync"`；新增
  `toml = "0.8"`、`serde_yaml = "0.9"`、`zip = { version = "2", default-features = false,
  features = ["deflate"] }`、`tar = "0.4"`、`flate2 = "1"`、`bzip2 = "0.5"`、`sha1`? 不需要。
  等待 target 拷贝完成后再跑构建。
- [ ] **Step 2:** `cargo check` 通过（baseline 334 测试先跑一遍确认绿：`cargo test`）。
- [ ] **Step 3:** Commit `acp-port: task1 cargo deps + acp module skeleton`。

### Task 2: wire.rs — Row 助手与 wire 变换

**Files:**
- Create: `src-tauri/src/backend/acp/wire.rs`

**Interfaces:**
- Produces: `text(Value)->String`、`number(Value)->i64`、`bool_of(Value)->bool`、
  `nil_if_empty(Value)->Value`、`mode_wire(&Value)->Value`、`config_wire(&Value)->Vec<Value>`、
  `permission_options(&Value)->Vec<Value>`、`tool_state_wire(&[Value])->Vec<Value>`、
  `persisted_blocks(&Value,&[Value],&str)->Vec<Value>`、`tool_output_text(&str)->String`。
  语义逐条对照 `codeg/wire.go` + `history.go:61-135`。

- [ ] **Step 1:** 先写测试（每函数 ≥1 例，取 Go 行为构造期望 JSON；configWire 空 opts 走
  models 兜底分支、persistedBlocks 的 tool_call_ref→tool_use+tool_result 展开序）。
- [ ] **Step 2:** `cargo test acp::wire` 失败 → 实现 → 通过。
- [ ] **Step 3:** Commit。

### Task 3: agents.rs — 注册表无状态重建

**Files:**
- Create: `src-tauri/src/backend/acp/agents.rs`

**Interfaces:**
- Produces: `Agent`（镜像 hub.Agent 字段）、`registry(root) -> Vec<Arc<Agent>>`（装配序：
  内置 codex/claude/dsh/workbuddy（探针 acp_installed）→ acp-agents.json（registerAcp 语义）
  → Store.CustomAgents（customHubID）→ harness-instances.json（instances.go profileBase 克隆）；
  enabled overlay 最后套用）、`wire_agent(&Agent)->String`（server.go:130）、
  `pick(root,agent_type)->Option<Arc<Agent>>`、`agent_by_type`、`installed_version(root,&Agent)`、
  `pub fn agent_exists(root, &str) -> bool`（给 experts.rs 用）。
- 探针函数与 `cli.go` 完全同序：`acp_packages` 表（npmGlobal()+ROOT/node_modules 双候选）、
  `work_buddy_entry`（注册表 App Paths 探测含 `reg query`）、`configured_harness_path`
  （.data/harness-paths.json）。

- [ ] **Step 1:** 测试：fixture 目录造 Store.CustomAgents/instances/acp-agents.json/agents
  overlay，断言装配序、wire 命名（claude→claude_code、dsh→deepseek）、overlay enabled 覆盖、
  未知探针（stub HOME）返回不可用。
- [ ] **Step 2:** 实现 → `cargo test acp::agents` 通过。
- [ ] **Step 3:** Commit。

### Task 4: launch.rs — 启动链

**Files:**
- Create: `src-tauri/src/backend/acp/launch.rs`

**Interfaces:**
- Produces: `Launch{command,args:Vec<String>,env:Option<Vec<String>>}`、
  `launch_fn(root, &Agent, permission, cwd) -> Result<Launch, String>`（wiring.go:218 全链：
  custom launch → customAcp → 内置适配器；buildEnv、overlay 写入 codex config/auth、
  INITIAL_AGENT_MODE、CLAUDE_CODE_EXECUTABLE、resolveProvider 环境注入、
  applyCodexProvider（TOML）、applyOfficialAccount、dsh patch（YAML）、workbuddy args）、
  `provider_version(root,&Agent)->String`（driver key 用）、`resolve_bin/acp_installed/
  adapter_version/codex_home/drop_env_key/anthropic_base_url`。
- 消费：Task 3 registry、providers.json 读取（providers.rs 只读其文件格式，不调其 API——
  resolveProvider 需 `providers.Find/Active/IsAPI/NativeHome/AccountVersion/ClearAPIEnvironment/
  Account`，这些在 providers.rs 没有全部暴露 → 在 launch.rs 内按 Go `internal/providers`
  语义实现所需读取，读 `.data/providers.json`/native home；若 providers.rs 已有可复用私函数
  则提到 pub，属于**本分支内**改动，不碰 E:)。

- [ ] **Step 1:** 纯函数测试：anthropic_base_url（/v1/messages//messages 后缀）、drop_env_key、
  npm_package_name、provider_version 分支（subscription/dsh/无 provider）。
- [ ] **Step 2:** 实现 → 测试通过。
- [ ] **Step 3:** Commit。

### Task 5: runtime.rs — 进程与 JSON-RPC 层

**Files:**
- Create: `src-tauri/src/backend/acp/runtime.rs`（本期只到 driver 层）

**Interfaces:**
- Produces: `Launch`（复用 Task 4）、`LaunchResolver` trait（`fn launch(&self,&Agent,
  permission,cwd)->Result<Launch,String>` + `fn version(&self,&Agent)->String`，生产实现包
  launch.rs，测试实现指 fixture）、`Driver::spawn(launch, cwd) -> Arc<Driver>`、
  `Driver::call(method,params,timeout) -> Result<Value,String>`（pending map +
  oneshot；`timeout<=0` 表示无期限）、`Driver::notify`、`Driver::closed()` watch、
  agent→client 请求分派（fs/read_text_file、fs/write_text_file、session/request_permission）
  与通知循环（`serve` 等价任务）。`within_dir`/`split_lines` 同文件。
- Windows：`CREATE_NO_WINDOW 0x08000000` creation_flags（对应 hideCommand）。

- [ ] **Step 1:** 测试：spawn `node fixtures/acp-agent.cjs`（跳过条件：无 node，同 Go skip），
  initialize 握手成功且 protocolVersion==1；坏路径 spawn 报 `适配器进程启动失败`。
- [ ] **Step 2:** 实现 → 通过。
- [ ] **Step 3:** Commit。

### Task 6: runtime.rs — 会话/回合/权限核心

**Files:**
- Modify: `src-tauri/src/backend/acp/runtime.rs`

**Interfaces:**
- Produces: `Runtime`（`drivers`/`records`/`bindings`/`approvals` 状态 + `RuntimeOptions{
  state_file, timeout_ms, resolver: Arc<dyn LaunchResolver>, emitter: Arc<dyn Emitter>,
  extra_mcp, allow_internal_tool}`）、`Options`/`Turn`/`SessionRecord`/`PermissionCard`
  （serde wire 结构，字段名与 Go json tag 一致，声明序 = Go struct 序）、
  `ensure/connect/prompt_blocks/turn/cancel/configure/set_mode/goal_control/stop_async_task/
  fork/forget_fork/forget_session/has_active_turn/supports_fork/supports_point_fork/
  supports_native_steering/steer/prompt_capabilities/list_json/export_transcript/
  save/load/shutdown/kill_drivers_at/records/records_for_chat`。Emitter trait 在此定义：
  `fn emit(&self, channel: &str, payload: &Value)`。
- `Runtime::new` 语义 = Go `NewRuntime` + load；`save` 截尾 100 条 + tmp+rename。

- [ ] **Step 1:** 移植 Go runtime_test.go 的 15 个测试（名字保留 Go 原名转 snake）：
  steering/wire-order-40-turns/fork-capability/fork-independence/streaming-renews-timeout/
  idle-timeout/full-access-survives-restart/resume-only/refusal/session-view-normalization/
  active-turn-cancel/persistence-100/permission-no-block/internal-tool-readonly/list-empty。
  fixture resolver：`node <root>/fixtures/acp-agent.cjs [args]`，root 用
  `CARGO_MANIFEST_DIR/../..`。
- [ ] **Step 2:** 实现 → 全部通过（`cargo test acp::runtime`）。
- [ ] **Step 3:** Commit。

### Task 7: store_conv.rs — 对话/回合写原语

**Files:**
- Create: `src-tauri/src/backend/acp/store_conv.rs`

**Interfaces:**
- Produces（全部一次锁内完成读-改-写，`store_file::lock(codeg-workspace.json)`）:
  `open_folder(doc,path,kind)`、`create_conversation(doc,folder,agent,title,parent)`、
  `append_turn(doc,conv_id,role,body)`、`conversation(doc,id)`、`save(doc)`、
  `prompt_side_write(root, conn, conv_args, user_blocks) -> Result<(conv_id,folder_id),String>`
  （connections.go:594-631 的 store 段：无 conv 则建、append user turn、blocks 写入、
  status=in_progress、external_id、changed 事件返回）、`assistant_side_write(...)`（run 尾段：
  persistedBlocks/usage/model/duration/external_id/status pending_review|cancelled）。
- 消费：store_file、goformat::marshal_store、wire.rs、goformat::{timestamp,hex_id}。
- 事件：`conversation://changed` 帧（kind=upsert+summary row clone）。

- [ ] **Step 1:** 测试：临时 workspace 先放一份真实结构 JSON，跑 prompt_side_write 断言
  folders/conversations/turns 变化 + marshal_store 字节可再解析 + 事件帧内容。
- [ ] **Step 2:** 实现 → 通过。Commit。

### Task 8: connections.rs — 连接层（事件/快照/warm pool）

**Files:**
- Create: `src-tauri/src/backend/acp/connections.rs`

**Interfaces:**
- Produces: `Connections`（全局 per-root 单例，`OnceLock`；内部 `Mutex<HashMap<String,
  Arc<Mutex<Connection>>>` 风格按 Go 锁序）+ `event()`（Seq/Events 256 截尾/Live/Tools
  合并 + `acp://event` 帧）、`native_update`、`runtime_view`、`publish_session`、
  `snapshot`、`connection_list`、`permissions_loop`（200ms ticker，spawn 一次）、
  warm pool（`warm_pool_key/take_warm/put_warm/release_warm/warm_sweeper/queue_warm/
  warm_all`，TTL 2min、30s 扫描）、Emitter 生产实现（包 Arc<ProxyState>，在 Task 12 接线，
  这里先定义类型别名）。
- 依赖 Go `tasks`（active_delegations）与 `s.callers`：Rust 侧先建空容器 + callers 注册
  （extra_mcp 时创建，Task 9）。

- [ ] **Step 1:** 测试：event 序列（content_delta 聚合 text、thinking 块、tool_call 合并、
  seq 递增、256 截尾）、native_update 各 sessionUpdate 分支的事件帧、snapshot 字段全表、
  warm take/put/release/TTL。
- [ ] **Step 2:** 实现 → 通过。Commit。

### Task 9: connections.rs — connect/prompt/cancel/disconnect + extraMCP/tool_identity

**Files:**
- Modify: `src-tauri/src/backend/acp/connections.rs`
- Create: `src-tauri/src/backend/acp/tool_identity.rs`（internal_tool_name + allow_internal_tool）

**Interfaces:**
- Produces: `connect(root,args)->Result<String,String>`（connections.go:65 全语义：pick→
  resolve workdir→permission/mode 推导→preferredConfigValues→sessionId 复用（ListJSON 匹配
  + 同 RuntimeID 连接复用）→warm 认领→注册→Runtime.Connect→事件）、
  `prompt(args)->Result<(),String>`（busy/grace 3s 自愈 + RunSeq、blocks 解析含 image/
  resource_link/resource（attachments.go 全语义、20MiB 上限、file:// 上传目录校验）、store 写、
  spawn run_with_content）、`run_with_content`（delegation 前缀常量全文照抄 + delegationContext
  （Rust tasks 空则空串）+ splitContext + PromptBlocks + 完成段）、`release_run/cancel/
  disconnect/connection_list`、`extra_mcp`（companion token 注册 + codeg-mcp server 条目，
  endpoint 取 Go `Base`，实测值见 wiring 装配——实现时从 main.go 找 codeg.New 调用点钉死）。
- turn_in_progress 错误以精确字符串返回（Task 12 转 409）。

- [ ] **Step 1:** 测试：fixture agent 全链 connect→prompt（echo）→快照出 turn→cancel 路径；
  busy 双 prompt 第二次 turn_in_progress；sessionId 复用分支；resource_link 展示块。
- [ ] **Step 2:** 实现 → 通过。Commit。

### Task 10: question.rs + splits.rs — 阻塞卡

**Files:**
- Create: `src-tauri/src/backend/acp/question.rs`、`src-tauri/src/backend/acp/splits.rs`

**Interfaces:**
- Produces: parked question 注册表 + `answer_question/settle_question/
  release_questions_for_connection`（question.go 全语义；park 的应答 channel 仅当 companion
  在场才有发送端——Rust 侧保留同样的 settle 语义与卡片事件）；split 注册表 +
  `respond_task_split/drop_connection_splits/clear_split`（split.go:446-563；approve 的
  `launch_delegations` 未移植 → 返回明确错误「协作派发尚未移植」，因 propose_task_split
  只能经未移植的 companion 调用产生，该路径在 Rust 侧暂不可达，行为=reject/校验路径全真）。
- 持久化：splits 走 split.go 的 `saveSplitsLocked` 文件（实现时读 split.go 对应文件名）。

- [ ] **Step 1:** 测试：answer 未知 id no-op、settle 清卡发事件、drop 前缀 expired、
  respond 未知 split 报错、tasks 1–8/prompt 长度校验。
- [ ] **Step 2:** 实现 → 通过。Commit。

### Task 11: custom_install.rs — 安装/卸载/清缓存

**Files:**
- Create: `src-tauri/src/backend/acp/custom_install.rs`

**Interfaces:**
- Produces: `custom_cache_dir/cached_custom_launch/prepare_custom/npm_cli/npm_package_name/
  python_command_name/custom_env/run_custom_installer/custom_not_installed/
  validate_custom_def/current_acp_platform/safe_custom_path/extract_custom_archive/
  install_custom_archive`（custom_launch.go + custom_archive.go 全语义；下载走 `net::client`
  5min 超时 + SHA-256 校验；zip/tar/tar.gz/tar.bz2 解包 + Win32 路径清洗）+ 3 条命令入口
  `prepare_npx_agent/uninstall_agent/clear_binary_cache`（customInstallCommand 语义：
  emit app://agent-install started/completed/failed + app://acp-agents-updated、busy 连接
  拒绝、版本固定校验）。

- [ ] **Step 1:** 测试：safe_custom_path 拒绝表（`..`、尾点尾空格、盘符）、extract zip+tar.gz
  到临时目录断言条目、npm_package_name/python_command_name、custom_env 排序、
  cached_custom_launch 缺失/存在分支（不联网，archive 用本地 file:// 之外路径跳过——下载
  路径不测，SHA 校验用手工构造文件绕过 net::get? 不可绕 → install_custom_archive 只测
  extract 层）。
- [ ] **Step 2:** 实现 → 通过。Commit。

### Task 12: mod.rs 接线 + 409 + experts 钩子

**Files:**
- Modify: `src-tauri/src/backend/mod.rs`（+`pub mod acp;`、acp 分支、409 特判）
- Modify: `src-tauri/src/backend/experts.rs`（owns 增加 2 条 + pick 校验）

**Interfaces:**
- `acp::owns(command)` 覆盖 28 条命令全集（Task 13 列表）；派发分支置于 experts 之前：
  `if acp::owns(&command) { return acp::dispatch(&st, &command, &args).await; }`
  —— dispatch 为 async；内部把 store 落盘与安装器放 `spawn_blocking`。
- 409：dispatch 的 Err(message) 若 == "turn_in_progress" → 409 + envelope（Go server.go:275）。
- experts.rs：`acp_list_agent_skills`、`acp_list_agent_runtime_skills` 移入 owns，实现里
  先 `acp::agents::agent_exists`，不存在报「执行端不存在」（skills.go:294 语义）。

- [ ] **Step 1:** 测试：mod 层 409（用 axum::test? 无现成 harness → 单元测 dispatch 错误映射
  函数）；experts 两条命令经 `experts::handle` 返回 Some。
- [ ] **Step 2:** 实现 → `cargo test` 全绿。Commit。

### Task 13: 命令全集 + agentconfig + preflight + fork + 其余 handler

**Files:**
- Create: `src-tauri/src/backend/acp/commands.rs`（28 条的最终 owns()+dispatch 映射表 +
  handler；fork.go、preflight.go、agentconfig.go 的移植落这里或其子模块）

**Interfaces:**
- 28 条全集（inventory TSV 为准）：acp_connect/acp_disconnect/acp_prompt/acp_cancel/acp_fork/
  acp_get_session_snapshot/acp_get_session_snapshot_by_conversation/acp_find_connection_for_
  conversation/acp_respond_permission/acp_respond_task_split/acp_set_mode/acp_set_config_option/
  acp_stop_async_task/acp_touch_connection/acp_preflight/acp_list_connections/acp_list_agents/
  acp_get_agent_status/acp_goal_control/acp_describe_agent_options/acp_detect_agent_local_version/
  acp_update_agent_config/acp_update_agent_env/acp_list_agent_skills/acp_list_agent_runtime_skills/
  acp_answer_question/acp_prepare_npx_agent/acp_uninstall_agent/acp_clear_binary_cache。
- `fork_conversation`（fork.go 全文：busy/depth/tasks 校验、store 锁内外双段、
  TryLock turn、callers 身份检查（Rust callers 同源 extra_mcp）、sibling 连接 +
  oldCaller 改绑、回滚路径）。
- `update_agent_settings`（agentconfig.go：Store.Agents overlay 写、enabled → hub 无进程态，
  注册表无状态重建天然生效；返回 live 连接数）。
- `preflight`（preflight.go 全语义 + adapterIdentity/adapterInfo/nativeConfigDir/adapterLookup）。
- `describe_agent_options`/`detect_agent_local_version`/`get_agent_status`/`list_agents`（agents()
  行构造全文：installed_version、overlay merge、customDefForAgent 定制行）。

- [ ] **Step 1:** 测试：update_agent_env 写 overlay + live 计数；preflight 未注册 agent 的
  fail 卡；describe_agent_options 用 fixture agent 出 modes/config_options；fork 事务成功 +
  原生 id 缺失失败 + turn_in_progress；list_agents 行含 host_tools_supports_delegation=true。
- [ ] **Step 2:** 实现 → `cargo test` 全绿。Commit。

### Task 14: 验收 + 双跑 + 移交说明

**Files:**
- Create: `scripts/parity-acp.mjs`（worktree 内，集成时可带去 E:）
- Create: `docs/superpowers/plans/2026-11-13-acp-port-handoff.md`（集成协议）

- [ ] **Step 1:** `cargo test` 全量绿（含既有 334+ 新增）。
- [ ] **Step 2:** 双跑：worktree 里起 Go sidecar（非默认端口）+ 起 Rust（tauri dev 或直跑
  axum，非默认端口），`parity-acp.mjs` 对无状态命令逐条比对响应字节：
  acp_list_agents/acp_preflight/acp_get_agent_status/acp_detect_agent_local_version/
  acp_list_connections/acp_touch_connection（不存在 id）/acp_get_session_snapshot（不存在）/
  acp_describe_agent_options（需真实适配器环境——本机已装则跑，未装降级比较错误文案）。
  有状态命令以 Task 5-13 的 fixture 测试为准（双跑会产生真实 harness 会话，不做）。
- [ ] **Step 3:** handoff 文档：baseline hash、新文件清单、3 个 hunk 的精确位置与内容、
  merge 步骤（等 B 落盘 → 在 worktree 覆盖验证主树 → 只拷新文件 + Edit hunk → E: 不跑
  cargo，把 E: src 覆盖回 worktree 编译验证）、文档更新点（REMAINING.md C 组翻绿）。
- [ ] **Step 4:** Commit + 汇报。

## 自审记录

- 28 条命令与 inventory TSV 一一对应（Task 13 列表 = TSV 交集，排除 B 组的
  acp_fetch_registry_catalog/acp_download_agent_binary 与 I 组的 5 条 custom agent 命令）。
- 类型一致性：Emitter/Resolver/Launch/Outcome 命名在 Task 5/6/7/8/12 间一致。
- Review Focus 5 条各自落到 Task 6（时钟/权限/只读）、Task 9（busy 自愈）、Task 8（warm）。
- 已知偏差（写进 handoff）：①delegation/companion 未移植 → acp_respond_task_split approve
  与 codeg-mcp 工具调用在 Rust 侧不可用（前者不可达、后者 401，模型侧表现为协作工具失败，
  不影响对话主链）；②`get_conversation` 族仍归 ④⑤；③acp_update_agent_config 与 env 共用
  updateAgentSettings。
