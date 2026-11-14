# 后端 Rust 化迁移方案

> 结论先说：这是一次**多阶段、以周计**的重写，不是一次提交能完成的改动。
> 本文件把规模、契约、分期与验收标准固定下来，避免"重构到一半、应用不可用"。

## 一、规模基线（实测）

| 指标 | 数值 | 说明 |
|---|---|---|
| Go 生产代码 | **26,443 行** | `server-go/`，11 个包 |
| Go 测试代码 | **5,883 行** | 这些测试就是现有行为的契约 |
| HTTP/WS 命令 | **334 个**（389 处 `case`） | 清单见 `rust-backend-command-inventory.tsv` |
| 前端 API 面 | **6,817 行** | `codeg-ui/src/lib/api.ts` |
| 后端二进制 | 22 MB | 含 ACP 运行时、SQLite、PTY、CDP |

命令分布：`internal/codeg` 360 处、`providers` 9、`acp` 8、`mcpserver` 4、`cmd` 8。

## 二、为什么不能一次性重写

1. **前端要求全量命令同时存在**。`tauri-transport.ts` 把命令名直接 `invoke()`；
   只要有一个命令没实现，对应功能立刻 500，而 334 个命令分散在 60+ 个界面入口。
2. **行为契约只在测试里**。并发、锁顺序、事件序号、幂等语义都没有文档，
   只能靠 5.9k 行 Go 测试反推——这是移植的验收依据，不能跳过。
3. **平台依赖没有等价物开箱即用**。ConPTY（终端）、CDP（内置浏览器）、
   SQLite（会话历史）、ACP（stdio JSON-RPC）在 Rust 侧都要重新选型并验证。

所以唯一安全的路径是**绞杀式迁移（strangler）**：新实现与旧实现并存，
逐个命令切换，任何中间态都保持应用可运行。

## 三、子系统映射

| Go 包 | 职责 | Rust 方案 | 风险 |
|---|---|---|---|
| `internal/acp` | ACP 协议运行时，stdio JSON-RPC，进程池 | `tokio` + 自写 codec；进程树用 `jobobject` | 中：协议细节多，但有 Go 实现可对照 |
| `internal/codeg` | 334 个命令、HTTP/WS、静态资源 | `axum` + `tower` + `tokio-tungstenite` | 高：体量最大，需按命令分批 |
| `internal/codeg/turnstore.go` | 会话历史（SQLite） | `rusqlite`（bundled），**复用现有 .db 文件与 schema** | 低：schema 不变即可原地读写 |
| `internal/codeg/terminal.go` | ConPTY 终端 | `portable-pty`（wezterm） | 中：Windows 下行为需逐项对齐 |
| `internal/codeg/browser.go` | CDP 内置浏览器 | `chromiumoxide` 或裸 CDP over `tokio-tungstenite` | 高：截图/注入/多标签语义多 |
| `internal/extra` | worktree、skills、通知、handoff | 调 `git` CLI（与现状一致）或 `git2` | 低 |
| `internal/orch` | 编排器、定时任务 | 纯逻辑移植 | 低 |
| `internal/hub` | Agent 注册表 | 纯逻辑移植 | 低 |
| `internal/mcpserver` | 内置 MCP 服务 | 自写（协议简单） | 低 |
| `internal/providers` | 供应商配置读写 | `serde` + 文件 | 低 |
| `cmd/desktop` | WebView2 外壳 | **已由 Tauri 取代** | 完成 |

## 四、分期与验收

### Phase 0 — 契约固化（已完成）
- 产出：`docs/rust-backend-command-inventory.tsv`（334 条命令 × 文件 × 行号）
- 验收：命令清单可由脚本重生成，作为后续"哪些已移植"的勾选表

### Phase 1 — Rust 服务骨架 + 代理（已完成）
- `src-tauri/src/backend/mod.rs`：`axum` 前门，监听 `CODEPLUSE_PORT + 1`（可用
  `CODEPLUSE_PROXY_PORT` 覆盖），窗口加载它；Go sidecar 仍在 `CODEPLUSE_PORT`。
- 已实现：
  - `GET /api/health` 本地应答，返回 `proxy:"rust"` 与 `upstream` 状态（`backend`
    仍为 `"go"`，兼容以它为准的探活）
  - `/api/*` 与全部静态资源转发；转发时移除 `Host`/`Origin`/`Connection`，否则
    Go 的本地安全网关会把代理请求判成外来来源
  - `/ws`、`/ws/events` 双向帧中继，并回显 `Sec-WebSocket-Protocol`（浏览器在
    服务端不选中子协议时会直接拒绝升级）
  - `CODEPLUSE_BACKEND=go` 绕过代理，窗口直连 sidecar（回滚开关）
- 验收（2026-11-13 实测，Go 8899 / 前门 8900）：
  - `/api/health` → `{"backend":"go","ok":true,"proxy":"rust","upstream":true}`
  - `GET /workspace` → 200，441,601 B（与直连一致）
  - `POST /api/acp_current_platform` → 与直连返回完全相同
  - WebSocket → 协商 `codeg-events`，收到上游首帧 `__ready__`

> 这一阶段看起来"没重写业务"，但它是唯一能让后续 334 个命令逐个迁移、
> 且应用始终可运行的前提。没有它，只能等全部写完再切换——那是不可验收的。

### Phase 2 — 按子系统移植（进行中）

顺序按依赖与风险从低到高：`providers` → `hub` → `orch` → `extra` →
`turnstore` → `mcpserver` → `acp` → `terminal` → `browser` → `codeg` 剩余命令。

**基础设施：`store_file`（已完成）**

`src-tauri/src/backend/store_file.rs`，后续所有状态文件迁移都走这一层：

| 能力 | 实现 |
|---|---|
| 原子替换 | 同目录临时文件 → `sync_all` → `rename` 覆盖；读者只会看到旧文件或新文件 |
| 跨进程写锁 | `<file>.lock` 以 `create_new` 争用，5s 超时，30s 以上的陈旧锁可抢占 |
| 抗半截读取 | 解析失败重试 6 次（对方可能正在替换文件） |

压测结果（`cargo test` 内）：

- **8 线程 × 25 次读-改-写**：最终计数精确为 200，零丢失、零锁超时
- **读线程在 60 次大文件（4000 行）替换期间**：从未读到半截 JSON

两个已移植的 store（`model-providers.json`、`api-providers.json`）都已改走这层。
重构后重跑双跑，`exec.save` 仍 MATCH，但 `providers.create` 暴露出真实差异：

> Go 的 `time.RFC3339` 是**秒级**（`2026-11-13T13:33:24Z`），Rust `time` 的 Rfc3339
> 默认输出亚秒（`...24.8618128Z`）。这不仅让双跑失败，还会把不同精度的时间戳写进
> 落盘文件、传给前端。现已截断到秒，并加了固定长度/无小数点的回归测试。

**双跑工具：`scripts/parity.mjs`（已完成）**

每次手写 curl 比对既慢又容易出错（已踩过 MSYS 把 `/api` 转成盘符的坑）。现在统一为：

```
pnpm parity --command list_model_providers --go 8899 --rust 8900
pnpm parity --command codepluse_provider_delete --body '{"id":"nope"}'
pnpm parity --command health --method GET
```

行为：同时打 Go 与 Rust，比较状态码与 JSON；键排序后深比较，时间戳与 24 位十六进制 id
默认归一；不一致时**指出第一个差异路径**，例如：

```
❌ GET /api/health 响应 JSON
   $.codeg_version: 仅 Go 有
```

实测三类场景：已移植命令 MATCH、错误路径 MATCH（400 + 同文案）、故意不同的接口能准确
定位到字段。`--path` 形式在某些终端会被 MSYS 改写，建议用 `--command`。

> 顺带修掉一个真实漂移：`/api/health` 原本是 Rust 手拼响应，缺 `codeg_version`/`data`，
> 一旦有调用方读这些字段就会出问题。现在改为**透传上游响应体**，只在响应头加
> `x-codepluse-proxy: rust` 作为标记——body 与 Go 逐字节一致。

**试点：providers（4 个命令，已完成）**

`src-tauri/src/backend/providers.rs`，读写同一份 `.data/model-providers.json`：

| 命令 | 状态 |
|---|---|
| `list_model_providers` | ✅ Rust |
| `create_model_provider` | ✅ Rust |
| `update_model_provider` | ✅ Rust |
| `delete_model_provider` | ✅ Rust |

双跑比对（Go 8899 / Rust 前门 8900，同一份数据）：

| 比对项 | 结果 |
|---|---|
| `create` 响应（时间戳归一化） | **MATCH** |
| `create` 后落盘文件 | **MATCH**（字节一致） |
| `list`（含 1 条记录） | **MATCH** |
| 4 个 Rust 单测（掩码/校验/CRUD/读旧格式） | 全通过 |
| 真实数据文件 | 未改动（比对后 byte-identical） |

过程中对齐的一处**语义差异**：Go 的 `create` 返回原始 `Info`，`api_key_masked` 为空
（前端随后重拉列表拿掩码），Rust 最初回了掩码——已按 Go 对齐。

> 决策：**Go 里这 4 个 handler 暂时保留**。`CODEPLUSE_BACKEND=go` 是回滚开关，
> 删了它回滚就没有 providers。等 Phase 4 移除 sidecar 时统一删除。

**切片选择准则（实测得出）**

能否逐命令迁移，取决于该存储是否**每次调用都重读文件**：

| 存储 | 读取方式 | 可否并行移植 |
|---|---|---|
| `model-providers.json` | 每次 `read()` | ✅ 已移植（4 命令） |
| `api-providers.json` | 每次 `read()` | ✅ 部分（delete 已移植） |
| `settings.json`（`Store`） | 内存缓存 + temp+rename 写盘 | ❌ 会丢更新（缓存覆盖，不是半截文件） |
| `Hub`（Agent 注册表） | 内存缓存 | ❌ 会丢更新 |
| `automations.json` | 内存缓存 | ❌ 会丢更新 |

这就是为什么 `hub` 不能像计划那样"按包直接搬"：`acp_list_agents` 一条命令同时依赖
Hub 注册表、`settings.json` overlay、`AgentAvailability` 二进制探测与 custom agent 合并。
这类**状态所有者**必须整体迁移，不能逐命令切；否则 Go 内存里的旧副本会在下一次写入时
覆盖 Rust 的修改。

因此剩余工作分两类：
- **可逐命令切**：仅 `api-providers.json` 的其余命令（`list`/`save`/`activate`/`models`，
  各自受 ccswitch 合并、外部 source、联网限制，需单独评估）
- **必须整流迁移**：`Store`（settings/conversations/turns）、`Hub`、`automations`、
  `ACP runtime`、`terminal`、`browser`

**第二批：`codepluse_provider_delete`（已完成）**

`src-tauri/src/backend/exec_providers.rs`，用 `serde_json::Value` 读写以保留未知字段。

| 比对项 | 结果 |
|---|---|
| 删除成功（200）与删除后落盘文件 | **MATCH** |
| 删除不存在的 id | **MATCH**（同为 400 + `供应商不存在`） |
| 4 个 Rust 单测（移除/活动指针/未知字段保留/缺文件） | 全通过 |
| 真实 `api-providers.json` | 未改动（byte-identical） |

同族命令**故意不移植**：`list` 合并 CC Switch、`save` 可能写入外部 source profile、
`activate` 接受 `ccswitch:` 前缀并读取 Claude 配置、`models` 联网取列表——任何一条先切
都会引入行为差异，留着转发更安全。

**第三批：`codepluse_provider_save`（已完成）**

`save` 的语义链比 delete 长得多，全部逐条对齐 Go：

协议默认值（`claude→anthropic`、`codex→openai-responses`、其余 `openai`）→ 协议/Harness
合法性 → URL 校验（`http(s)`、无 userinfo/query/fragment、host 非空）→ 名称/模型非空 →
外部 source 判定 → 更新（禁止改 agent、空 key 沿用旧值、空 models 沿用旧数组）或新建
（要求 key、12 字节随机 id）→ 响应回填 `hasKey`/`api` 并剔除明文 `key`。

| 比对项 | 结果 |
|---|---|
| `create` 响应 / 落盘文件 | **MATCH** / **MATCH** |
| `update` 响应 / 落盘文件 | **MATCH** / **MATCH** |
| 协议非法（400） | **MATCH**（同 code、同文案） |
| 6 个新单测（创建/更新/校验/外部转发） | 全通过 |
| 真实 `api-providers.json` | 未改动（byte-identical） |

外部 id（`native:` / `ccswitch:` / `store:`）在 `handle` 里直接返回 `None`，交由 Go 的
`saveExternal` 处理——Rust 只接管本文件的读写路径。

> 注意：`serde_json` 开启了 `preserve_order`，新建条目按 Go 结构体字段顺序插入，
> 所以落盘文件在规范化 id 后可与 Go 逐字节比较。

- 每个命令的验收：
  1. 同一请求分别打到 Go 与 Rust，响应 JSON 深度相等（忽略时间戳/随机 id）；
  2. Go 对应测试用例翻译为 Rust 测试并通过；
  3. 切走该命令后，前端 E2E 冒烟通过；
  4. 在 `rust-backend-command-inventory.tsv` 的 `ported` 列标为 `rust`。

### Phase 3 — 数据层与静态资源（2–3 人日）
- 直接读写现有 `codeg.db` / `settings.json`，保证新旧版本可互操作（降级回滚可行）
- Rust 直接服务 `codeg-ui/out`，去掉对 Go 静态服务的依赖

### Phase 4 — 移除 sidecar（1 人日）
- 删除 `server-go/`、`build-tauri.ps1` 中的 Go 构建步骤、`externalBin`
- 验收：安装包内不含任何 Go 产物；`cargo build` 即可完整出包

## 五、总工作量与风险

- 代码量：约 26k 行 Go → 预计 30–40k 行 Rust（含测试）
- 日历时间：单人全职约 **6–10 周**；风险主要集中在 `acp`、`browser`、`terminal` 三块
- 最大风险不是写不出来，而是**中间态不可用**与**行为差异难发现**——
  Phase 1 的代理层 + 双跑对比 harness 就是为这两点设计的

## 六、当前状态

- [x] Phase 0：命令清单（334 条）
- [x] 桌面外壳：已迁移到 Tauri（`src-tauri/`），Go 作为 sidecar
- [x] Phase 1：Rust 前门 + 代理（health / 静态 / 命令 / WS 四项已验证）
- [x] Phase 2 试点：`providers` 4 个命令 + `codepluse_provider_delete` / `_save`（双跑一致，6/389）
- [ ] Phase 2 其余：`hub` → `orch` → `extra` → `turnstore` → `mcpserver` → `acp` → `terminal` → `browser`

在 Phase 1 落地前，任何"后端改成 Rust"的进展都是不可运行的代码，
不建议直接往 `src-tauri/src/` 里堆业务实现。


## 七、剩余量量化快照（2026-11-13 实测）

### 7.1 总量

| 指标 | 数值 |
|---|---|
| 唯一命令 | 334 个（389 处 case） |
| 已切 Rust | **6 个**（providers 4 + exec_providers 2） |
| 待迁移 | **328 个（约 98%）** |
| Go 生产代码 | ~26k 行（internal/codeg 独占 20,247 行 / 89 文件） |

### 7.2 按难度分级（依据：对 Server 状态字段的引用数 + 切片准则）

**Tier A — 无状态 / 每次调用读文件，可逐命令切（约 230 处 case，约 190 条命令）**

| 文件 | 命令数 | 状态引用 | 备注 |
|---|---|---|---|
| git.go | 31 | 0 | 仅 Resolve + git CLI 外壳，最优先 |
| command_workspace.go | 27 | 0 | settings/工作区读写 |
| command_sidebar.go | 24 | 0 | 侧栏配置文件 |
| forge.go + forge_github.go | 23 | 0 | forgeStore 走文件 |
| computer.go | 17 | 0 | computerState 为会话级 |
| browser.go | 15 | 0 | browserManager 可后置 |
| providermodels.go | 13 | 0 | |
| canvas.go / delegation.go | 20 | 各 1 | |
| 其余小文件（office/custom_launch/updater/terminal/skills/split/history/files/feedback/turnstore/webfetch/token_usage/git_settings/preflight/companion 等） | 约 80 | 0-8 | 多为单文件小命令 |

**Tier B — 内存态 + 整流迁移（约 127 处 case + acp/mcpserver/cmd 三包，约 140 条命令）**

| 文件/包 | 命令数 | 状态字段 | 迁移单元 |
|---|---|---|---|
| server.go（ACP 会话核心） | 47 | connections/byRuntime/subscribers/warm pool | 与 internal/acp 一体迁 |
| worktask.go + worktask_run.go | 32 | tasks2 (taskStore) | 整流 |
| instances.go | 15 | 15 处引用 | 整流 |
| connections.go | 13 | 20 处引用 | 与 server.go 同批 |
| automations.go | 11 | autoStore | 整流（调度器 + 存储） |
| custom_agents.go | 9 | 34 处引用 | 整流 |
| internal/acp（Runtime） | 8 | 进程池/stdio | 最难，放最后 |
| internal/mcpserver | 4 | | 随 ACP |
| cmd/serve | 8 | | 装配层，收尾时消化 |

### 7.3 建议顺序（自下而上吃掉 Tier A）

1. git.go（31 条，0 状态）——纯 git CLI 外壳，store_file 地基已就绪，第一批含 git_settings/history 约 37 条
2. command_workspace / command_sidebar / office / custom_launch / skills（约 70 条，全部 0-1 状态引用）
3. forge / computer / browser / providermodels / turnstore（约 65 条）
4. Tier B 按上表自易到难，acp 运行时压轴

### 7.4 已迁移批次记录

#### 批次 1 — git（2026-11-13）

- 范围：`git.go` 48 条中 47 条 + `gitWindow` 4 条（`open_commit/push/stash/merge_window`）
- 留 Go：`git_commit`（需要向 Go 的内存订阅者总线发 `folder://git-commit-succeeded`）、
  `get/set/delete_commit_message_settings`（读写 Go 的 `Store.Settings` 内存缓存，属 Tier B）
- 实现：`src-tauri/src/backend/git.rs`，运行在 `spawn_blocking`（fetch/push 最长 45s，
  不能阻塞 WS 中继）
- 测试：10 个，覆盖引用校验、query 转义、窗口路由、真实仓库读写、stash、远端、
  冲突检测、工作区边界
- 双跑：`node scripts/parity-git.mjs`，双胞胎仓库 + 64 步序列，**64/64 一致**
- 期间修掉的真实 bug：`branch.<name>.remote` 查询漏传 `config` 子命令；
  merge/rebase 把命令 id 当 git 子命令传（`git git_merge`）

**已知 Go 侧怪癖（本批刻意对齐）**：`cmd/serve/state.go:resolveExistingPath` 在目标
不存在且路径上没有符号链接时返回**最近的已存在祖先目录**而不是候选路径。因此
`git_show_file` 取不存在的文件时，传给 git 的是 `HEAD:.`（报错 `path '.' exists on
disk`），`git_is_tracked` 取不存在的文件会返回 `true`。Rust 已逐字节复刻该行为；
迁移收尾后应两侧一起改成返回 `candidate`（各一行）。

#### 进度

| 批次 | 命令数 | 状态 |
|---|---|---|
| providers / exec_providers | 6 | ✅ |
| git + git 窗口 | 51 | ✅ |
| 合计 | **57 / 351** | 余 294 |

#### 批次 2 — Store 只读 + 文件操作（2026-11-13）

**为什么只读可以先迁**：Go 每次改动都会 `saveLocked()` 整文件落盘，磁盘即 Go 内存的镜像。
Rust 每次调用重读文件，结果与 Go 内存一致；而 Rust **写**会被 Go 下一次整文件保存覆盖。
所以读命令可逐条切，写命令必须等「所有会写 codeg-workspace.json 的命令」一起翻。

- `store_reads.rs`（21 条）：`get_sidebar_data`、`get_stats`、`list_open_folder_details`、
  `load_folder_history`、`list_all_folder_details`、`list_folders`、`get_folder`、
  `open_folder_by_id`、`list_remote_connections`、`get_system_settings`、
  `load_system_settings`、`list_all_conversations`、`list_conversations`、
  `list_child_conversations`、`list_opened_tabs`、`list_folder_groups`、
  `list_conversation_tags`、`get_conversation_branch_tag`、`assistant_list`、
  `quick_messages_list`、`get_commit_message_settings`
  - 注意：`get_stats.by_agent` 在 Go 里是 map 迭代顺序（随机），Rust 按 `agent_type` 排序；
    对拍时该数组需顺序无关比较
- `files.rs`（12 条）：`get_home_directory`、两个目录列表、`get_file_tree`、
  `list_workspace_files`、四个读取、两个保存、`get_git_head`
  - 自带 SHA-256（ETag 契约），不引入新依赖
  - `save_file_content` 的 `expectedEtag` 冲突、`save_file_copy` 已存在，均与 Go 同文案
- 双跑：21 条 Store 读 + 11 条文件命令全部一致；save 冲突文案一致
- 测试：files 8 条 + store_reads 7 条，累计 **41 个通过**

#### 批次 3 — 独立小域（2026-11-13）

- `updater.rs`（5 条）：`app_update_status`、`app_update_state`、`perform_app_update`、
  `rollback_app`、`restart_app`
- 留 Go：`check_app_update` —— 读 `HARNESS_HUB_UPDATE_MANIFEST` 并拉 HTTPS，
  本构建 reqwest 未启用 TLS，迁过去会在设了该变量时行为不同
- 双跑 5/5 一致；累计测试 **44 个通过**

**当前进度：95 / 351（余 256）**

| 已完成 | 命令数 |
|---|---|
| providers + exec_providers | 6 |
| git + git 窗口 | 51 |
| Store 只读 | 21 |
| 文件操作 | 12 |
| updater | 5 |

#### 批次 4 — office + experts/skills（2026-11-13，已验收）

- `office.rs`（14 条）：`officecli_detect/install/uninstall`、技能列表/同步/读取/
  安装状态/关联/渲染 HTML、`start/stop_office_watch`
- `experts.rs`（10 条）：`experts_list/read_content/link_to_agent/unlink_from_agent/
  apply_links`、`science_list`；skills 链接用 `home` 目录参数贯穿
  （claude/codex/其它 → `.claude/.codex/.agents`），测试全部隔离在临时目录，
  不触碰真实 `%USERPROFILE%\.claude\skills`
- 测试：office/experts 合计 14 个

#### 批次 5 — Store 写盘整批翻转（2026-11-13，已验收）

**解锁前提**：Go 侧 `Store.reloadIfChangedLocked()`（`server-go/internal/codeg/store.go`）
—— 每个请求入口 stat 文件，mtime/size 变化则重读 JSON。双写场景下 Go 能看到 Rust
的写入、Rust 每次调用重读文件，旧约束「写命令必须一次切完否则丢更新」由
「Go 每请求先重读」兜底化解。

- `store_writes.rs`（48 条）：sidebar 写 19（folder groups / tags / assistants /
  quick messages）、workspace 写 15（conversation 生命周期、tabs（带 expectedVersion
  CAS）、system settings、folder 属性）、canvas 12（boards/nodes/region/detach）、
  git_settings 2；写盘走 `goformat.rs`——逐字节复刻 Go `json.Marshal`
  （顶层字段序、嵌套键排序、HTML 转义、RFC3339Nano、整 f64 塌缩）
- `folder_commands.rs`（6 条）：独立文件 `codeg-folder-commands.json`，id 计数独立
- WS 事件注入：`conversation://changed`、`canvas://changed`、
  `canvas-board://changed`、`audit://folder_open` 由 Rust 直接喂给前端；
  canvas 删除时 `terminal_kill` 转发回 Go（终端未移植）
- 双跑：`scripts/parity-store.mjs` —— 孪生 workspace（Go/Rust 各一份种子数据），
  89 步比对 HTTP 响应 + 落盘文件逐字节（掩码时间戳/随机 id/绝对路径），
  WS 记录器验证事件注入。**89/89 一致**
- 测试：store_writes 21 + store_file 4；全量 **85 个通过**

**期间修掉的一处真实漂移**：Go `respond()` 用 `json.NewEncoder().Encode()`，
响应体带尾随 `\n` 且默认 HTML 转义；Rust 的 `axum::Json` 输出裸紧凑 JSON。
对 `null` 响应（大量写命令的返回值）这是字节级差异。已新增 `go_json()`
（复用 `goformat::marshal_preserve` + 补 `\n` + Go 同款两个响应头），
所有已接管命令统一走它。

**进度：173 / 351（余 178）**

| 已完成 | 命令数 |
|---|---|
| providers + exec_providers | 6 |
| git + git 窗口 | 51 |
| Store 只读 | 21 |
| 文件操作 | 12 |
| updater | 5 |
| office | 14 |
| experts/skills | 10 |
| Store 写 + folder commands | 54 |

剩余 178 条分组与推进顺序见 `docs/RUST-BACKEND-REMAINING.md`：B 联网 31（TLS 依赖）、
C ACP 28、D worktask 32、E 浏览器 19、F 桌面 21、G 终端 8、H 自动化 11、I 小域 24。

#### 批次 5 — Store 写整批翻转 + office/experts（2026-11-13）

**范围（78 条，95 → 173）**

- office 14 + experts/skills 10（前一批已写好、本批编译验收 + 双跑）
- Store 写 54：sidebar 19、workspace 15、canvas 12、git_settings 2、folder_commands 6
- 留 Go：`delete_conversation`（turn 库 + ACP 连接清理）、`get_conversation*` 3 条
  （turn 库）→ 归 ④/⑤；`experts_list_all_install_statuses` 等依赖 agents() 的仍留 Go

**为什么写可以翻了**：Go 侧 `Store.reloadIfChangedLocked()`（每请求入口 stat + 按需重读）
让「每请求单写者」成为可维持约束；Rust 侧每个写命令「重读文件 → 内存改 → 一次性落盘」，
出错即放弃内存副本（等价 Go 的回滚），全程持 `store_file` 跨进程锁。

**Go 落盘格式的逐字节复刻（`goformat.rs`）**——不做到这一步，双跑落盘比对没有意义：

| 维度 | Go 行为 | Rust 复刻 |
|---|---|---|
| 顶层 | `json.Marshal(Store)` 结构体字段序；`turns`/`customAgents` omitempty | `STORE_FIELDS` 固定序 + 缺省值（canvasBoards/canvasNodes 为 null）|
| 嵌套 | 全是 map[string]any → 键字节序排序 | `canon()` 递归排序（customAgents 子树例外，透传结构体序）|
| 字符串 | 默认 HTML 转义 `<>&`、U+2028/9 | `GoFormatter` 重载 `write_string_fragment` |
| 数字 | 读入即 float64，`json.Marshal(float64(320))`→`320` | `canon()` 把整值 f64 塌缩为整数；>2^53 的表示差异已知且可忽略 |
| 时间 | `time.RFC3339Nano`（尾零裁剪） | 手拼 `[Y]-[M]-[D]T[h]:[m]:[s]` + 去尾零小数 + `Z` |
| 控制字符 | `\b`/`\f` 也走 `\u0008`/`\u000c` | 重载 `write_char_escape`（评审发现，已修） |

格式假设全部用真实 serve-codeg.exe 冒烟验证过（紧凑无换行、`1e1`→`48`、tab `1.0`→`1`、
`member_ids:[]`）。HTTP 响应与 WS 注入帧同样先 `canon()` 再序列化（`go_json`），
与 Go 的 `respond(json.Marshal(map))` 同形；响应体 2MB 上限（`MaxBytesReader` 语义）同步。
评审还发现并修掉：tags 去重偏差、tabs 标量数组语义、folder_commands 浮点 id 与重复 id
语义、store 写入改走 `spawn_blocking`、terminal_kill 转发改为有界等待（保持 Go 的
「先杀后答」顺序）。已知残余：Go 入口 reload 与其 handler 内 save 之间的窗口里，
Rust 写仍可能被整文件覆盖（正确修法是 Go 侧按 span 加跨进程锁，归后续批次）。

**已知残余竞态**：`command()` 入口 `reloadIfChangedLocked()` 之后、handler 落盘之前的
窗口里，Rust 的写会被 Go 的整文件保存覆盖。剩余 Go store 写者只有
`delete_conversation`、自定义 agent 保存、`update_main_bot_settings` 等低频动作，
窗口为单个 Go handler 的毫秒级时长；根治需要 Go 侧按 store 写 span 持跨进程锁
（入口全局锁会把 acp 安装类长任务也串进去，saveLocked 内重读会反噬 handler 的内存
变更），归后续批次处理。

**事件注入（`mod.rs`）**：端口命令以前靠 Go `emit` 推 `conversation://changed`、
`canvas://changed`、`canvas-board://changed`、`audit://folder_open`。前端门现在维护
WS 客户端 sink 注册表，把同格式帧直接注入客户端套接字（Go 总线收不到，但其订阅者只有
经前端门进来的 UI）。canvas 删除节点连带 `terminal_kill` 转发回 Go（终端属后续批次）。

**验收（`scripts/parity-store.mjs`）**：孪生 workspace（Go/Rust 各一份相同种子），100+ 步
逐步比对响应 JSON 与落盘文件（掩码时间戳/随机 uid/绝对路径后逐字节一致），WS 录帧断言
四类 channel 全部到达 —— **全部一致**。单测 85/85（含 goformat 形状、sidebar 写、
canvas 校验/成员、folder_commands 引导、settings 合并、tabs 版本冲突）。

**当前进度：173 / 351（余 178）**

| 已完成 | 命令数 |
|---|---|
| providers + exec_providers | 6 |
| git + git 窗口 | 51 |
| Store 只读 | 21 |
| 文件操作 | 12 |
| updater | 5 |
| office + experts/skills | 24 |
| Store 写 | 54 |

#### 批次 6 — TLS 解锁 + forge 全组（2026-11-13，173 → 195）

**TLS**：`reqwest` features `["native-tls", "blocking"]`。选 native-tls 而非 rustls：
Windows 上是纯 schannel FFI，不需要 ring 的 NASM/C 工具链。crates.io 直连可达，
一次 fetch + build 74s。

**net.rs**：共享 blocking 客户端 —— 系统代理（同 Go DefaultTransport）、无全局超时、
**UA 显式 `Go-http-client/1.1`**（reqwest 默认不发 UA；双跑实测：GitHub 对无 UA 请求
回 403，sidecar 得 401，所有错误文案会漂）。HTTPS 命令走 `spawn_blocking`。

**forge 全组 21 条一次翻转**（forge.go + forge_github.go + validate_github_token）：
- accounts（token）**只在进程内存**，与 Go 同寿命（重启要求重输）；settings 落盘
  `.data/codeg-forge.json`（无 token，canon 格式与 Go `atomicWrite` 同字节）。
- `validate_github_token` 在 Go 是**死路径**（forgeCommand 无 case，永远 fall-through
  到仓库解析报错）——Rust 逐字节复刻错误链，单测钉死，禁止"修好"。
- `github_request`：30s 超时、8MB 限读、状态码 ≥300 → `GitHub %s %s 返回 %d：%s`
  （message 截 300 字符）；请求体走 `canon + marshal_preserve` 与 Go 同字节。
- workbench 12 条 REST：`forgeRow`（body 2000 字节截断、merged_at → merged、
  labels # 色）、`query_escape`（Go url.QueryEscape 逐字节：空格 → +）、
  merge 后回读失败不算失败、check-runs 拉取失败不算失败。

**单测**：105/105（forge 13 个新用例：路由式 mock 按 method+path 前缀应答并记录
请求行，query 串逐字符断言）。

**双跑 `scripts/parity-net.mjs`**：孪生 workspace（git 仓库种子，origin 指向
github.com）+ 本地 manifest HTTP 服务器 + **mock GitHub**——Go 侧用
`go build -ldflags "-X harnesshub/internal/codeg.githubAPI=<mock>"` 重定向测试专用
var，Rust 侧读 `CODEPLUSE_GITHUB_API` env；26 步（含 401/200 两分支、12 条 REST、
settings 文件字节）**EXIT=0 全一致**。

**进度：195 / 351（余 156）**。B 组剩 9 条（provider/模型 7 —— 又一个整流单元；
ACP 下载 2）。

| 已完成 | 命令数 |
|---|---|
| providers + exec_providers | 6 |
| git + git 窗口 | 51 |
| Store 只读 | 21 |
| 文件操作 | 12 |
| updater | 5 |
| office + experts/skills | 24 |
| Store 写 | 54 |
| HTTPS/forge（批次 6） | 22 |

#### 批次 7 — provider 目录 + ACP 注册表（2026-11-13，195 → 201）

**新依赖**：`rusqlite`（bundled；CC Switch 数据库 `~/.cc-switch/cc-switch.db` 只读，
同时为批次 ④ turn 库铺路）、`serde_yaml`（DSH 配置）、`toml`（Codex 配置）。

**provider 目录 5 条**（`provider_catalog.rs`，~1500 行含测试）：
- `codepluse_providers_list` = providers.Public 全链：api-providers.json（执行端）→
  CC Switch（SQLite 只读，缺库=空列表）→ model-providers（store:）→
  native（Claude settings.json / Codex config.toml+auth.json / DSH yaml，**非 claude
  才入列**）+ provider-metadata.json 的 UI 属性覆盖；IsAPI 判定、URL 含 userinfo/
  query/fragment 时清空、key/env 出响应前清空。
- `codepluse_provider_activate`：执行端校验、ccswitch: 前缀的 HasKey/CCCurrent 校验、
  active 表回写（MarshalIndent 两空格，BTreeMap 键序）。
- `ccswitch_provider_config`：掩码回显（secrets.go maskSecret：≤8 位全掩码，
  否则前 3 + … + 后 4）。
- `codepluse_provider_models` / `list_provider_models`：三协议（openai/anthropic/
  gemini）目录拉取；keyHeader 覆盖语义；**凭据只发往存储主机**（sameHost，scheme
  忽略）；inline 密钥走 fetchguard 式 SSRF 分类（解析+分类后再请求）；
  loopback 目标不走代理，非 loopback 优先注册表代理（`reg query` 探测）回落环境代理；
  harness_provider_models 本地/远程合并（禁用模型过滤、dsh 原生短路、失败降级 warning）。

**ACP 注册表 1 条**（`acp_registry.rs`）：`acp_fetch_registry_catalog` —— 真 CDN
（UA codepluse/2.0-go、35s、8MB 限）+ customAgents 已安装标记 + builtin 表 +
平台适配判定（GOOS-GOARCH 形态）；**spec 按 Go customAgentSpec 重建**（未知字段丢弃、
空 args/cmd/sha256/env 被 omitempty 丢弃——双跑抓到的真实漂移）。

**双跑**：parity-net.mjs 扩至 35 步——本地模型网关 mock（openai/anthropic 形状）、
api-providers 种子、真 ACP 注册表 CDN；**EXIT=0 全一致**。单测 120+。

**留 B 组外的 3 条**（阻塞是状态不是 TLS）：connection 2 条依赖 Hub `s.agents()`
（⑥）；`acp_download_agent_binary` 依赖 ACP 连接注册表与 prepareCustom 下载器（⑤）。

**进度：201 / 351（余 150）**。

#### 批次 10（收尾）— 剩余 28 条全部翻转（2026-11-13，→ 366/366）

范围：instances 4、main bot 1、custom-agents 写 3、安装状态 4、connection 2、
mcp 状态 1、feedback 1、turn 库 4、token 用量 4、任务引擎 4。

**关键结构变化：会话历史正式落到 SQLite。** C 组移植时 prompt/completion/fork
写的是 JSON 文档里的 turns map，而 Go 现行实现（turnstore.go）早已迁到
`.data/codeg.db`（WAL、每行携带累积前缀指纹）。本批：

- 新增 `turn_store.rs`：schema 与 Go 完全一致（conversation_id/seq/data/role/
  timestamp/hash_after/assistant_upto/user_upto/max_ts），FNV-1a 前缀指纹
  （role tag 字节 + 时间戳毫秒 8 字节 LE）与客户端 `extendPrefixFingerprint`
  及 Go 的 `fnvTurn` 逐字节一致；
- 会话写路径（prompt / completion / fork / feedback）全部改为 SQLite 追加，
  JSON 文档只保留会话元数据，`persist` 明确不再回写 `turns`；
- 旧文档的一次性迁移（Go `importLegacyTurnsLocked`）：首次打开库时备份文件、
  幂等导入、重写文档去掉 turns；
- 连接按库路径缓存（单进程单根 = Go 的 SetMaxOpenConns(1)），测试/多根互不串库。

**任务引擎**（`worktask_engine.rs`）：claim（并发上限）→ runTask（worktree 建立、
会话创建、prompt、2s 轮询落定 / 权限卡映射 awaiting_input / 24h 上限）→ settle
（diff 统计 + 可选自动合并）→ 合并队列（per-folder 锁；失败也 drain 队列）→
工作树丢弃。运行时字段（liveConn / cancelFlag / checkout）放进程级注册表，因为
board 每次从磁盘重载；15s 定时器接管 Go 的 `TickWorkTasks`。ACP 连接层改为
**启动期初始化**，引擎在请求路径之外直接驱动它。

**验收**：`cargo test` 239 个全绿；`node scripts/parity-final.mjs` 31 步
**EXIT=0 全一致**（含 SQLite 直插的孪生 turns、token 用量聚合、引擎守卫文案、
落盘文档字节）。

**已知偏差**：mcp 状态的 binary_path 报告各自二进制（双跑掩码）；引擎合并锁是
tokio 异步锁；定时器 15s（Go 在主循环内）。

**进度：366 / 366（100%）**。剩余工作只剩 Phase 3/4：Rust 直服静态资源、
删 sidecar / `cmd/serve` 装配层、清掉两侧 `resolveExistingPath` 怪癖。

