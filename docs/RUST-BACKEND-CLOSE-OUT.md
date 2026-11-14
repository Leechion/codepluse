# Rust 后端收尾清单（Phase 3 / Phase 4）

> 快照：2026-11-13。命令移植 **366 / 366 已完成**（TSV 全标 rust，`cargo test` 239 绿，
> `parity-final.mjs` 31 步 EXIT=0）。本清单只列**命令之外**的收尾工作。
> 当前状态：Rust 前门仍把静态资源、`/api/health`、`/api/upload_attachment`、
> `/api/codeg_mcp`、`/ws*` 转发给 Go sidecar；sidecar 仍随包分发。

---

## A. Phase 3 — Rust 直服（不依赖 sidecar 也能跑）

### A1. 静态资源 ✅ 未做
- [ ] `codeg-ui/out` 由 Rust 直服（现转发给 Go）
- [ ] 路径解析复刻 Go `static()`：`/` → `/index.html`；`Rsc: 1` → 先试 `<file>.txt`
      并设 `Content-Type: text/x-component`；否则文件缺失/是目录时试 `<file>.html`，
      再回落 `<dir>/index.html`
- [ ] 响应头：`Content-Type`（按扩展名）、`Content-Length`、`Last-Modified`、
      `ETag`/`If-None-Match`（304）
- [ ] **Range 请求**（PDF 预览、视频拖动依赖；Go 的 `http.ServeFile` 自带）
- [ ] 路径穿越防护（`..`、绝对路径、符号链接逃逸）——Go 用 `filepath.Clean("/"+)` 约束

验收：断网、不启 sidecar，`GET /workspace`、JS/CSS/字体、`Rsc: 1`、
`Range: bytes=0-1023` 全部正常；`/../../etc/passwd` 类请求 404/403。

### A2. `/api/health` 本地应答 ✅ 未做
- [ ] 现在透传 Go；改为本地合成，字段与 Go 完全一致：
      `{ok, app:"codepluse", data:{status:"ok", version, sessionRuntime:"acp-v1"},
        backend, codeg_version, path_boundary:{allow_any_path, allow_any_browse}}`
- [ ] `backend` 取 `"rust"`（前端无逻辑依赖 `"go"`，已核查）；
      `version` 与 `codeg_version` 的取值来源要定（建议常量，与 `updater.rs` 的
      `VERSION` 统一）
- [ ] `path_boundary` 读 `HARNESS_HUB_ALLOW_ANY_PATH` / `HARNESS_HUB_ALLOW_ANY_BROWSE`

验收：与旧 Go 响应逐字段对比（掩码 backend/version），前端状态栏与探活正常。

### A3. `/api/upload_attachment` 本地实现 ✅ 未做
- [ ] multipart 解析（axum 需开 `multipart` feature）
- [ ] 语义复刻 `attachments.go`：字段名 `file`；`filepath.Base` 归一文件名；
      空名/`.`/`..` → 400 `附件名称无效`；落盘 `.data/uploads/<32hex>/<name>`，
      独占创建；空/超 20MB/读写失败 → 400 `附件为空、过大或读取失败`
      （并清理半成品）；成功 → `{path, name, size, mimeType}`
- [ ] 请求体上限 20MB + 1MB（Go 的 `MaxBytesReader`）

验收：上传成功/空文件/超限/坏文件名四条路径与旧 Go 文案一致。

### A4. `/api/codeg_mcp` 本地应答 ✅ 未做
- [ ] 非 POST → 405 `{"error":"POST required"}`
- [ ] token 校验：Rust 侧委托注册表恒空 → 一律 401
      `{"error":"Invalid companion token"}`（与 Go 无委托时一致）
- [ ] 非 JSON body → 400（Go 的 decoder 错误文案）

### A5. `/api/*` 非 POST → 405 ✅ 未做
- [ ] 现在路由只有 `post(api_dispatch)`，GET 会落到 fallback（转发静态）；
      改为 `any(...)`，非 POST 回 `{"code":"invalid_input","message":"Use POST"}` 405

### A6. 本地请求防护（安全边界）✅ 未做
> 代理阶段 Rust 删掉了 `Origin` 才能转发；直服后必须自己实现，否则任意网页可
> CSRF 驱动终端/git/文件系统接口。
- [ ] `Sec-Fetch-Site` ∈ {same-origin, none}
- [ ] `Origin` 若存在：必须 loopback（或显式 `HOST` 配置的地址）、端口等于监听端口、
      且与 `Host` 一致（防 DNS rebinding）
- [ ] 命令体 `Content-Type` 必须是 `application/json`
- [ ] 拒绝 → 403 `{"code":"forbidden","message":...}`
- 参考：`server-go/internal/codeg/guard.go`

### A7. WebSocket 协议本地实现（最大的一块）✅ 未做
> 现在 `/ws`、`/ws/events` 整条中继给 Go；sidecar 删除后必须自己讲这套协议。
- [ ] 首帧 `{"channel":"__ready__","payload":{}}`
- [ ] 客户端帧：`ping` → `{"type":"pong"}`；`attach{subscription_id, connection_id, since_seq}`；
      `detach{subscription_id}`
- [ ] 服务端帧：`snapshot{subscription_id, connection_id, snapshot, event_seq}`；
      `replay{…, events, high_water_seq}`；`event{subscription_id, envelope}`；
      `detached{reason}`（`connection_gone` / `lagged` / `too_many_subscriptions`）
- [ ] 每连接最多 64 订阅；`since_seq > 0` 且环内可重放 → replay，否则 snapshot
- [ ] `acp://event` 只投递给匹配 `connection_id` 的订阅；序号连续性检查（gap → lagged）
- [ ] 空闲 90s 断开、30s 服务端 ping、pong 续期、64KB 读上限
- [ ] 事件源换成 Rust 侧：`connections.rs` 的 `events`/`seq`/`live` 与 `emit_events`
- 参考：`server-go/internal/codeg/websocket.go`

验收：真 harness 起一个会话，attach→snapshot、断线 since_seq 重放、lagged 重挂全绿。

### A8. 开发热更新代理 ✅ 未做
- [ ] `HARNESS_HUB_DEV_FRONTEND` 指向 `next dev` 时，非 `/api` 请求反向代理过去
      （含 HMR WebSocket 隧道），否则 `scripts/dev.ps1` 在删 Go 后失效
- [ ] 目标不可达 → 502 `dev frontend unavailable (...)`

---

## B. Phase 4 — 移除 sidecar

### B1. 先保全历史 ✅ 未做
- [ ] `server-go/` 现有 ~80 个未提交改动先 commit（否则删除后无法追溯）
- [ ] 打 tag（如 `go-sidecar-final`）便于回滚

### B2. 桌面外壳去 sidecar ✅ 未做
- [ ] `src-tauri/src/main.rs`：删 sidecar 启动（`start_backend` / `wait_port` /
      `CommandChild` / `ensure_frontend` 的 Go 相关分支）、`CODEPLUSE_BACKEND=go`
      回滚开关、`CODEPLUSE_PROXY_ONLY`（改为默认单进程）
- [ ] 端口策略收敛：直接监听 `CODEPLUSE_PORT`（8840），不再 `+1` 前门
- [ ] `tauri.conf.json`：删 `externalBin`、sidecar 相关 resources
- [ ] `src-tauri/binaries/` 清理（`codepluse-backend-*.exe`）

### B3. 构建脚本去 Go ✅ 未做
- [ ] `scripts/build-tauri.ps1`：删 `go build`、`serve-codepluse.exe` 拷贝
- [ ] `scripts/tauri-dev.ps1`、`scripts/dev.ps1`、`scripts/rebuild-after-pull.ps1`、
      `scripts/start-codeg.ps1`：同样的 Go 步骤与 `serve-codepluse` 进程名
- [ ] `scripts/inventory-commands.mjs` 依赖扫 Go 源码 → 改为读现有 TSV（或标记归档）
- [ ] 迁移期双跑脚本（`parity*.mjs`）依赖 Go 孪生 → 移到 `scripts/historical/`
      并在 README 标注「迁移期工具，需 Go sidecar」

### B4. 删除产物 ✅ 未做
- [ ] 删 `server-go/`（含 exe、`.data`、`run-serve.ps1`）
- [ ] 删 `src-tauri/binaries/*`、`dist/` 里的旧 Go 产物
- [ ] 验收：`cargo build` 即可完整运行；安装包内无任何 Go 二进制

### B5. 清理 `resolveExistingPath` 怪癖 ✅ 未做
- [ ] Rust `git.rs::resolve_existing_path` 不再返回「最近的已存在祖先」，
      改为返回 `candidate`（目标不存在时给原路径）
- [ ] 同步修正依赖该怪癖的测试与行为：
      `git_show_file`（不存在的文件应报 path 不存在，而不是 `HEAD:.`）、
      `git_is_tracked`（应返回 false）
- [ ] 检查 UI 是否有依赖旧行为的调用点（git 面板、canvas file 节点、terminal cwd）

---

## C. 验收与文档

- [ ] `cargo test`（预计 240+）全绿
- [ ] 单进程烟测：启动 → 打开工作区 → 会话（ACP connect/prompt/cancel/fork）→
      终端 → 内置浏览器 → 画布 → 任务看板 → 定时任务 → 附件上传 → PDF 预览
- [ ] 断网/无 Go 环境下重复上述冒烟（证明无隐藏依赖）
- [ ] `cargo tauri build` 出包，安装后启动、升级流程（updater）可用
- [ ] 更新 `docs/RUST-BACKEND-MIGRATION.md`：Phase 3/4 勾选、总览数字、
      「迁移期工具/双跑」章节标注归档
- [ ] 更新 `README.md` / `docs/TAURI-SHELL.md`：构建不再需要 Go 工具链
- [ ] 更新 memory：单进程架构、`resolveExistingPath` 已修正、双跑脚本归档

---

## D. 已知偏差（不阻塞，需在文档里保留）

| 偏差 | 影响 |
|---|---|
| 协作域（delegation / companion MCP）未移植 | `acp_respond_task_split` 的 approve 返回明确错误；`/api/codeg_mcp` 恒 401；模型侧协作工具不可用，对话主链正常 |
| ACP driver 指纹哈希字节与 Go 不同 | 只影响进程池 key 命名，无跨实现混用 |
| DSH 补丁 YAML 输出字节与 yaml.v3 不同 | 语义等价，文件名 hash 不同 |
| `sessions.json` null→`[]` 归一 | Go 读端两种都接受 |
| `get_codeg_mcp_service_status.binary_path` | 报告 Rust 二进制（双跑掩码 `<BIN>`） |
| 任务引擎运行时字段在进程级注册表、合并锁为 tokio 异步锁、定时器 15s | 行为等价，重启后由磁盘布局重建 |
| ACP 有状态命令用 Go 的 fixture 测试验收，未走真实 harness 双跑 | 无状态命令已双跑 |
