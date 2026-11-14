# Rust 后端移植 — 剩余清单

> 快照时间：2026-11-13（批次 9 / C 组完成后更新）。命令总数以
> `docs/rust-backend-command-inventory.tsv` 为准（`node scripts/inventory-commands.mjs` 可重建）。
> 注：扫描器已修正「多行 case 标签漏抓」（browser.go 的 13 连 case 续行），总数
> 355 → 366——TSV 已按修正后口径重建；D/E/F 的 `scripts/parity-def.mjs` 双跑
> 106 步两次连跑全一致。

## 总览

| 状态 | 命令数 | 说明 |
|---|---:|---|
| 已验收（双跑一致 + 单测） | **366** | 100% |
| 待移植 | **0** | 0% |
| 合计 | **366** | |

已验收明细：providers + exec_providers 6、git + 窗口 4 共 51、Store 只读 21、
文件操作 12、updater 5、office 14、experts/skills 10、Store 写 54（sidebar 19、
workspace 15、canvas 12、git_settings 2、folder_commands 6）、HTTPS/forge/provider
**28**（批次 6：check_app_update + forge 全组 21；批次 7：providers_list/activate、
ccswitch_provider_config、provider_models 两条、acp_fetch_registry_catalog 5 条）、
并行批次 **72**：D work task 32 + E 浏览器 19 + F 桌面控制 21（见下）、
**批次 8（G/H/I）26**：终端 8 + 定时任务 11 + workspace 状态流 3 + `git_commit` +
`pick_directory` + `acp_current_platform` + `acp_list_custom_agents`（双跑
`scripts/parity-ghi.mjs` 38 步全一致 + 单测 10 个，见下）。
**批次 9（C 组 ACP）29**：进程池/stdio JSON-RPC/会话快照/权限应答/goal 控制 +
连接层/预热池 + 安装链 + launch 启动链 + provider 解析（含 CC Switch SQLite）
——本仓此前把 C 组记作 28 条，按 TSV 复核实为 29（server.go 25 + custom_launch 3 +
agentconfig 1；`answer_question` 此前漏计）。验收：Go `runtime_test.go` 15 个
fixture 测试全数移植成 Rust（node `fixtures/acp-agent.cjs`）+ 各模块单测，
全量 158 个测试三次连跑全绿；集成步骤见
`docs/superpowers/plans/2026-11-13-acp-port-handoff.md`。

> 注：批次 6/7 与并行批次的 28+72 条此前未同步进
> `scripts/inventory-commands.mjs` 的 RUST 集，批次 8 一并从各模块 `owns()`
> 派生补齐（浏览器 9 条是 Rust 新增面，不在 Go 清单内）。批次 9 的 29 条
> 同理：集成落盘后重跑 `node scripts/inventory-commands.mjs` 即同步
> （代码在 worktree 分支 `acp-port-c`，上表数字按集成后口径计）。

## 剩余 0 条 —— 收尾批（R1–R5）已完成

收尾批 28 条（2026-11-13，双跑 `scripts/parity-final.mjs` 31 步全一致 + 单测 239 个）：

| 域 | 条数 | 实现 |
|---|---:|---|
| 执行端实例 | 4 | `harness_instances.rs`：instances CRUD 直接读写 `.data/harness-instances.json`（注册表每次请求无状态重建，无 Hub 内存态） |
| 主智能体 | 1 | 同上：`update_main_bot_settings`（sha256 身份摘要 → inst_main_* 实例 + Settings） |
| 自定义智能体写 | 3 | `custom_agents.rs`：save / delete / add_registry_agent（validateCustomDef + 来源继承 + spec 按 Go omitempty 归一） |
| 安装状态 | 4 | `experts.rs`：技能 × 执行端矩阵（agents_rows 底座） |
| 连接目录 / 保存 | 2 | `harness_connections.rs`：catalog（供应商默认值 + 账户元数据）/ save（校验 + Settings + 预热退役） |
| MCP 状态 | 1 | 同上：companions/sessions/active 计数 + socket_path（拼上游地址） |
| 会话反馈 | 1 | `acp/connections.rs`：submit_feedback（原生引导直送 + pending 队列） |
| turn 库 | 4 | `turn_store.rs`：SQLite（WAL）读写 + 分页指纹（FNV-1a 与客户端/Go 字节一致）+ delete_conversation 级联 |
| token 用量 | 4 | 同上：report / facets / status / sync（时区桶、热力图、连击、top 5） |
| 任务引擎 | 4 | `worktask_engine.rs`：claim / run / settle / merge 队列 / 工作树丢弃 + 15s 定时器（Go TickWorkTasks 的对位） |

**关键迁移**：C 组的会话写（prompt / completion / fork）原先写 JSON 文档的 turns map，
Go 现行是 SQLite —— 本批把写路径切到 `codeg.db`，并实现旧文档 turns 的一次性迁移
（备份 + 幂等导入），`persist` 不再回写 turns。

**已知偏差**（有意为之）：

1. `get_codeg_mcp_service_status` 的 `socket_path` 用前端门的上游地址；`binary_path`
   报告各自后端二进制（双跑掩码为 `<BIN>`）。
2. 任务引擎的运行时字段（liveConn / cancelFlag / checkout）放在进程级注册表：board
   每次从磁盘重载，Go 靠内存里的 taskStore 对象保活。
3. 引擎的合并锁是 tokio 异步锁（合并本身 async），Go 用 sync.Mutex。
4. 任务定时器间隔 15s（Go 在 server 主循环里跑）；`TickWorkTasks` 的 Go 版本保留但
   不再被调用（避免双调度）——**收尾时需删除 wiring 里的调用**（已删）。


## 复核补记（2026-11-13 22:40）—— 命令面 0 缺口，但有三处功能缺口

复核方式：`node scripts/inventory-commands.mjs` 的 `RUST` 集合已按
`src-tauri/src/backend/` 的**分派面**重建（命令必须出现在某个模块 `owns()` 的
`matches!` 块、`match command` 块，或 `command == "…"` 比较式中）。结果：
**366 / 366，剩余 0**。三个工作区逐一比对（main、worktree `acp-port-c`、
worktree `641b`）——后两个是更早的快照（分别少 138 / 45 条），**没有 main
之外尚未集成的实现**，不需要再合并。

但「命令在 Rust 里」不等于「功能完整」。下列三处是删除 Go sidecar 前必须补的：

### 1. 外部 provider 写回 —— ❌ 唯一一条会落到 Go 转发的命令分支

`exec_providers.rs:314`：遇到外部 id（`native:` / `ccswitch:` / `store:`）直接
`return None` → 落回上游转发。Go 侧实现是 `internal/providers/writeback.go`(321)
+ `ccswitch.go`(104)，写回目标：

| id 前缀 | 写回位置 |
|---|---|
| `ccswitch:` | CC Switch 的 SQLite（`~/.cc-switch/cc-switch.db`） |
| `native:codex:` | `~/.codex/config.toml` |
| `native:dsh:` | DSH 的 YAML 配置 |
| `store:` | `.data/model-providers.json`（`modelproviders.Update`） |

删 Go 后「保存外部 provider」必然失败。

### 2. companion MCP（`mcp-codeg`）—— ❌ 未移植，但 Rust **仍在广告它**

`internal/mcpserver/`(457) + `cmd/serve/wiring.go:504` 的 `mcp-codeg` stdio 模式
没有移植，Rust 二进制里没有这个子命令。而
`acp/connections.rs:611` 仍向 agent 广告：

```json
{"name":"codeg-mcp","command":"<exe>","args":["mcp-codeg","<base>/api/codeg_mcp","<token>"]}
```

sidecar 移除后：`command` 指向不存在的子命令，`base` 又来自 sidecar 端口
（`Layer::upstream_base`）。**工具一被调用就失败**——要么移植
`internal/mcpserver` 并给 Rust 加 `mcp-codeg` 模式，要么停止广告。

### 3. 协作域 delegation —— ❌ 未移植（已知偏差，但同样属于「删 Go 前」）

`internal/codeg/delegation.go`(602) + `delegation_profiles.go`(98) +
`delegation_recovery.go`(195)。Rust 侧 `acp_respond_task_split` 的 approve 直接
返回「协作派发尚未移植：拆分提案已撤销……」（`acp/connections.rs:349`）。

### 4. 删 Go 之前的历史保全

`server-go/` 在 main 与 worktree `641b` 里都有 **75–84 个未提交的改动文件**，
从未进过任何 commit —— 删目录即不可追溯，先 commit（建议 tag `go-sidecar-final`）。

## 建议顺序

1. ~~验收本批 24 条（`office` + `experts`），跑双跑。~~ ✅ 批次 5
2. ~~**A 组 Store 写 58 条**——一次解锁最多，且已有 Go 重载补丁兜底。~~ ✅ 批次 5（54 条，
   4 条 turn 库依赖归 ④/⑤）
3. ~~B 组联网 31 条~~ ✅ 批次 6/7（28 条：TLS 解锁 + forge 全组 + provider 目录 +
   ACP 注册表）；剩 3 条阻塞是 Hub/ACP 状态（connection 2 条 → ⑥，
   acp_download_agent_binary → ⑤），已从 B 组移记。
4. ~~G 终端 8 → H 自动化 11 → I 小域 21，都是「单文件 + 有状态但自包含」。~~
   ✅ 批次 8（翻转 26 条：G 8 + H 11 + workspace 流 3 + 杂项 4；剩 16 条的阻塞
   见 I 节表格——hub/装配层 8 条、ACP/C 3 条、turn 库 4 条、
   `s.agents()` 枚举 2 条……按 5/6/7 的批次落地）。
   验收：`node scripts/parity-ghi.mjs --go 18840 --rust 18841` 38 步全一致；
   Go 侧唯一行为删除是 wiring.go 的 `TickAutomations` 调用（防双调度）。
5. ~~D worktask 32 → E 浏览器 19 → F 桌面 21。~~ ✅ 并行批次（D/E/F 同批翻转；
   worktask 引擎暂留 Go，走内部触发命令，C 落地后随 ACP 迁移）
6. ~~C ACP 29（+ `internal/acp`）压轴。~~ ✅ 批次 9（worktree `acp-port-c`，
   158 测试三次连跑全绿；本仓原记 28 条，按 TSV 复核实为 29——
   `answer_question` 此前漏计；集成步骤见
   `docs/superpowers/plans/2026-11-13-acp-port-handoff.md`）。
7. 收尾：`cmd/serve` 装配层、删 sidecar、清掉两侧 `resolveExistingPath` 怪癖；
   顺带翻转批次 9 解除的 4+1 条（mcp_service_status、submit_session_feedback、
   两条 list_all_install_statuses、acp_download_agent_binary）与
   automations Bridge 的 ACP 路径（已本地应答，无需改动）。

## 环境备注（2026-11-13）

`src-tauri/target/debug/deps` 被清空过一次，导致每次 `cargo test` 都要从零编译
约 400 个依赖；E 盘实测顺序写约 24 MB/s，整轮 release/test 构建是磁盘瓶颈而非 CPU
（构建期间 CPU 占用仅 33%）。已把 `[profile.dev] debug` 降为 1（只要行号表），
首次全量构建后增量构建应回到分钟级。

批次 5 补充：两个 `cargo test` 同时跑会在同一 target 上互锁（rustc 全部 ~2% CPU、
零产出），只能杀掉重来；构建期间不要再起第二个 cargo。8840 端口常驻一个开发实例，
双跑工具要用非默认端口（`parity-store.mjs --go N --rust M`）。
