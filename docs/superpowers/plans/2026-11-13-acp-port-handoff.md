# C 组 ACP 移植 — 集成移交（handoff）

> 分支 `acp-port-c`（worktree `C:\Users\lee\.zcode\worktrees\d733\codepluse`）
> baseline：`2517c5a`（主检出 E:\code\codepluse 的 src-tauri 快照 @ 2026-11-13 10:17）
> HEAD：`70c9c9d` + 收尾提交。**158 个测试全绿**（143 既有 + 15 新增）。

## 交付内容

C 组 = internal/acp 运行时 + 28 条命令（清单见
`docs/superpowers/plans/2026-11-13-acp-runtime-port.md` Task 13）。Go 侧
**零改动**（B 会话的 forge/net/updater 原样保留）。

**新增文件**（`src-tauri/src/backend/acp/` 全目录 + 计划/移交文档）：
acp/{mod,commands,connections,runtime,agents,launch,providers_core,custom_install,
store_conv,question,splits,tool_identity,wire}.rs

**共享文件 hunk**（集成时需在主检出最新版上手工重打，基线会漂移）：

1. `src-tauri/Cargo.toml` — tokio features 补 `process/io-util/sync`；新增
   toml/serde_yaml/zip/tar/flate2/bzip2/rusqlite(bundled)/sha2/url。
2. `src-tauri/src/backend/mod.rs` —
   a. `pub mod acp;`（模块声明区首位）；
   b. ProxyState 增加 `pub upstream_port: u16` 字段 + serve() 里赋值；
   c. api_dispatch 里 providers 分支**之前**插入 acp 分支（async 派发 +
      `turn_in_progress` → 409 特判）——精确代码见本分支 mod.rs:207-226。
3. `src-tauri/src/backend/experts.rs` — owns() 增加两条 skills 命令 +
   run_with_home 开头加 `list_agent_skills` 分支（Go skills.go:294）。

## 集成步骤（等 B 组落盘后执行）

1. 在本 worktree 把主检出最新 src-tauri 覆盖进来（`robocopy /MIR` 排除
   target），`cargo test` 确认合并树在 worktree 里编译+全绿（**绝不在主检出
   跑 cargo**，与并行会话互锁）。
2. 只拷 `src-tauri/src/backend/acp/` 整目录 + `docs/superpowers/plans/` 两个
   文档到主检出。
3. 三个共享文件用 Edit 工具按上面 hunk 打到主检出最新版（先重读文件）。
4. 主检出 `git add src-tauri && git commit`（首次纳入版本控制）。
5. 更新 `docs/RUST-BACKEND-REMAINING.md`：C 组 28 条翻绿（B 组验收时它统计
   的 173 → 本批 +28）。

## 已知偏差（有意为之，见计划「自审记录」）

1. **协作域（delegation/companion MCP）未移植**：`acp_respond_task_split`
   的 approve 路径返回明确错误（该路径只在 companion 提案后可达，Rust 侧
   splits 注册表恒空）；codeg-mcp 伴生进程端点仍指 Go sidecar（其 caller
   注册表在 Go 侧，token 校验 401 → 模型侧协作工具失败，不影响对话主链）。
2. **driver 指纹哈希**：`providerVersion` 的哈希字节与 Go 不同（JSON 编码器
   不同）——只影响进程池 key 命名，acp 命令整体路由到一侧，无跨进程混用。
3. **DSH 补丁文件字节**：YAML 输出格式与 yaml.v3 不同（语义等价，文件名
   hash 不同）。
4. **sessions.json**：Rust 写入 null→`[]` 归一（turns/permissions 恒为数组），
   Go load 端两种都接受。
5. 双跑验收以 fixture 单测为准（Go 自己的 ACP 测试同样是 node fixture）；
   有状态命令不跑真实 harness 双跑。无状态命令（list_agents/preflight/
   detect_version/get_agent_status/list_connections/touch/snapshot-missing）
   可用 `scripts/parity-acp.mjs` 双跑——本批未写，集成后按需补。
6. `get_conversation` 族 4 条仍归 ④⑤ 批次（SQLite turn 库）。

## 三个运行期死锁坑（移植时踩过的，集成排查可复用）

1. **漏 `.await`**：async fn 返回 future 不 await = 静默不执行（通知永不应用）。
2. **wall-clock 混入 Instant**：`Instant::now() + (wall - wall)` 钳位会把
   deadline 每个 tick 顺延——超时永不触发；时间运算全程用毫秒整数。
3. **临时守卫跨 await**：`json!({...field.lock()...})` 的临时守卫活到整条语句
   结束（包括 .await）；`let turns = &guard.field;` 触发生命周期延长同理。
   根任务（root future）无 Send 约束，编译器不报，运行期死锁。
   排查工具：`std::backtrace::Backtrace::force_capture()` 写文件（libtest
   capture 会吞 eprintln）。
