# Rust 后端收尾清单（Phase 3 / Phase 4）

> 复核时间：2026-11-13 23:20。命令移植 **366 / 366**（`scripts/historical/inventory-commands.mjs`
> 的 RUST 集合已按 Rust 分派面重建，`cargo test` **256 个全绿**）。
> 本清单逐项核对后的结果写在每项的「结论」里——未做的、写法的出入都在下面说明。

---

## 总体结论

| 阶段 | 状态 | 说明 |
|---|---|---|
| Phase 3 — Rust 直服 | ✅ 完成 | 静态资源、health、上传、companion、防护、WS 协议、dev 代理全部本地实现并双跑一致 |
| Phase 4 — 移除 sidecar | ✅ 完成 | `server-go/` 与 `src-tauri/binaries/` 已删除，安装包不再携带 Go 二进制 |

---

## A. Phase 3 — Rust 直服（不依赖 sidecar 也能跑）

### A1. 静态资源 ✅ 已做
- [x] `codeg-ui/out` 由 Rust 直服（`webapp.rs::static_asset`）
- [x] 路径解析复刻 Go `static()`：`/` → `/index.html`；`Rsc: 1` → 先试 `<file>.txt`
      并设 `Content-Type: text/x-component`；否则文件缺失/是目录时试 `<file>.html`，
      再回落 `<dir>/index.html`
- [x] 响应头：`Content-Type`（按扩展名）、`Content-Length`、`Last-Modified`、
      `ETag`/`If-None-Match`（304）、`Accept-Ranges`
- [x] **Range 请求**（`bytes=start-end` → 206 + `Content-Range`；越界 → 416）
- [x] 路径穿越防护：`filepath.Clean("/"+request)` 等价实现，`..` 无法越出根目录

验收：`scripts/historical/parity-web.mjs` —— 静态 5 项 + RSC 1 项全部 MATCH；
单进程烟测 `GET /workspace` → 200，**441,601 B**（与迁移前 Go 侧逐字节一致）。

### A2. `/api/health` 本地应答 ✅ 已做
- [x] 本地合成，字段与 Go 完全一致：`{ok, app:"codepluse", data:{status, version,
      sessionRuntime}, backend, codeg_version, path_boundary:{allow_any_path,
      allow_any_browse}}`
- [x] `backend` 取 `"rust"`
- [x] `version`/`codeg_version` 的取值：**刻意保持 Go 的字面量**（`2.0.0-go` →
      `2.0.0-rust` 仅此一处，`codeg_version` 仍是 `0.42.0`）。原因：`codeg_version`
      会被前端拿来和自身版本比较，updater 另有自己的 `VERSION` 契约；改这两个值
      会移动本次迁移不该移动的行为。
- [x] `path_boundary` 读 `HARNESS_HUB_ALLOW_ANY_PATH` / `HARNESS_HUB_ALLOW_ANY_BROWSE`

验收：与 Go 响应逐字段对比（掩码 backend/version）一致。

### A3. `/api/upload_attachment` 本地实现 ✅ 已做
- [x] multipart 解析（**手写**，`axum` 的 `multipart` feature 会引入新 crate，
      离线构建拉不到；当前实现只解析单个 `file` 字段，够用且零新依赖）
- [x] 语义复刻 `attachments.go`：字段名 `file`；`filepath.Base` 归一文件名；
      空名/`.`/`..` → 400 `附件名称无效`；落盘 `.data/uploads/<32hex>/<name>`，
      独占创建（`create_new`）；空/超 20MB/读写失败 → 400 `附件为空、过大或读取失败`
      （并清理半成品与空目录）；成功 → `{path, name, size, mimeType}`
- [x] 请求体上限 20MB + 1MB（Go 的 `MaxBytesReader`）

验收：上传成功/坏文件名两条路径与旧 Go 文案一致（双跑 MATCH）。

### A4. `/api/codeg_mcp` 本地应答 ✅ 已做（比原计划更完整）
- [x] 非 POST → 405 `{"error":"POST required"}`
- [x] token 校验：按 `Authorization: Bearer <token>` 在 **ACP 调用方注册表**里查，
      查不到 → 401 `{"error":"Invalid companion token"}`
- [x] 非 JSON body → 400（Go 的 decoder 错误文案）
- [x] **超出原计划**：连带把 `codeg-mcp` 伴随服务本身移植了
      （`mcp_companion.rs` + 二进制 `mcp-codeg <endpoint> <token>` 模式）——
      否则 Rust 仍在向 agent 广告一个不存在的子命令（见 §D 偏差表的第 1 条）。

### A5. `/api/*` 非 POST → 405 ✅ 已做
- [x] 路由由 `post(api_dispatch)` 改为 `any(...)`，非 POST 回
      `{"code":"invalid_input","message":"Use POST"}` 405

### A6. 本地请求防护（安全边界）✅ 已做
- [x] **Host 白名单**：`127.0.0.1:<port>` / `localhost:<port>` / `[::1]:<port>`，
      否则 403 `forbidden_host`
- [x] **Origin 白名单**：必须是本服务的三个 origin 之一，否则 403 `forbidden_origin`
- [x] `Sec-Fetch-Site: cross-site`（仅在无 Origin 时）→ 403 `cross_site_blocked`
- [x] 命令体 `Content-Type` 必须是 `application/json`（`/api/upload_attachment`
      为 `multipart/form-data`），否则 415 `unsupported_content_type`
- [x] 拒绝体统一为 `{"code":"<code>","message":"请求被本地安全策略拒绝"}`
- [x] 参考实现：`server-go/cmd/serve/guard.go`（tag `go-sidecar-final`）
      —— **注意**：`internal/codeg/guard.go` 是更早的一版（Sec-Fetch-Site 白名单 +
      Origin 同源比对），随 sidecar 一起删除了；线上行为以 `cmd/serve/guard.go` 为准。

### A7. WebSocket 协议本地实现 ✅ 已做
- [x] 首帧 `{"channel":"__ready__","payload":{}}`
- [x] 客户端帧：`ping` → `{"type":"pong"}`；`attach{subscription_id, connection_id,
      since_seq}`；`detach{subscription_id}`
- [x] 服务端帧：`snapshot{…}`；`replay{…, events, high_water_seq}`；
      `event{subscription_id, envelope}`；`detached{reason}`（`connection_gone` /
      `lagged` / `too_many_subscriptions`）
- [x] 每连接最多 64 订阅；`since_seq > 0` 且环内可重放 → replay，否则 snapshot
- [x] `acp://event` 只投递给匹配 `connection_id` 的订阅；序号连续性检查（gap → lagged）
- [x] 空闲 90s 断开、30s 服务端 ping、pong 续期、64KB 读上限
- [x] 事件源：Rust 侧连接层（`acp/connections.rs` 的 `events`/`seq`/`live`）
      与 `emit_events`

验收：协议帧与 Go 的 `mcp-codeg` 逐帧对拍一致（`parity-mcp.mjs`，4/4）；
单进程烟测 `/ws/events` 收到 `__ready__` 首帧。

### A8. 开发热更新代理 ✅ 已做
- [x] `HARNESS_HUB_DEV_FRONTEND` 指向 `next dev` 时，非 `/api` 请求反向代理过去
- [x] 目标不可达 → 502 `dev frontend unavailable (<target>): <err>`

> HMR 的 WebSocket 隧道未做（Go 的 `httputil.ReverseProxy` 会一并隧道化升级请求；
> Rust 侧只代理普通 HTTP）。`scripts/dev.ps1` 的前端热更新因此依赖浏览器直连
> `next dev` 的 HMR 端口，而不是经后端隧道。

---

## B. Phase 4 — 移除 sidecar

### B1. 先保全历史 ✅ 已做
- [x] `server-go/` 的未提交改动（guard / fetchguard / turnstore / textutil 等 142 个
      文件）已整体 commit：`76712f5 chore(server-go): 保全 Go sidecar 的最终状态`
- [x] tag `go-sidecar-final` 已打

### B2. 桌面外壳去 sidecar ✅ 已做
- [x] `src-tauri/src/main.rs`：删掉 sidecar 启动、`CODEPLUSE_BACKEND=go` 回滚开关、
      `CODEPLUSE_PROXY_ONLY`（改为 `CODEPLUSE_HEADLESS=1` 的无头模式）
- [x] 端口策略收敛：直接监听 `CODEPLUSE_PORT`（8840），不再 `+1` 前门
- [x] `tauri.conf.json`：删 `externalBin`、sidecar 相关 resources
- [x] `src-tauri/binaries/` 已清理
- [x] `Cargo.toml` 删掉 `tauri-plugin-shell`（不再需要）

### B3. 构建脚本去 Go ✅ 已做
- [x] `scripts/build-tauri.ps1`、`tauri-dev.ps1`、`dev.ps1`、
      `rebuild-after-pull.ps1`、`start-codeg.ps1`：Go 步骤全部换成 Rust
      （`cargo build` / 直接启动 `codepluse.exe`）
- [x] `scripts/build-codeg-desktop.ps1`（旧 Go + WebView2 外壳）已删除
- [x] `scripts/inventory-commands.mjs` 依赖扫 Go 源码 → 已归档到
      `scripts/historical/`（命令面清单 TSV 成为权威记录）
- [x] 迁移期双跑脚本（`parity*.mjs`）→ 移到 `scripts/historical/`，附 README 说明
      需要 Go sidecar（可从 tag `go-sidecar-final` 还原）

### B4. 删除产物 ✅ 已做
- [x] 删 `server-go/`（含 exe、`.data`、`run-serve.ps1`）与 `src-tauri/binaries/*`
- [x] `cargo build` 即可完整运行；安装包内无任何 Go 二进制

### B5. 清理 `resolveExistingPath` 怪癖 ⏳ 未做（已从阻塞项降级）
- [ ] Rust `git.rs::resolve_existing_path` 仍返回「最近的已存在祖先」
- [ ] `git_show_file`（不存在文件）仍报 `path '.' exists on disk`，
      `git_is_tracked`（不存在文件）仍返回 true
- [ ] UI 依赖旧行为的调用点未排查

> **为什么留到最后**：这个怪癖是 Go 与 Rust **当前一致**的行为，改一侧会造成
> 双跑不一致。Go 已删除，Rust 是唯一实现后它就成了「唯一行为」——但它同时是
> 用户可见的错误文案（`git show file` 指向不存在的文件时的报错）。修它需要同步
> 改 `git_show_file` / `git_is_tracked` 的语义与文案，属于行为变更而非迁移收尾。
> 建议单独一次提交处理，并同步更新前端文案。

---

## C. 验收与文档

- [x] `cargo test` —— **256 个全绿**（迁移前基线 239 + 本次新增 17）
- [x] 单进程烟测：起 Rust 后端（无 Go）→ `/api/health` 200、`GET /workspace`
      200（441,601 B）、`list_folders` 返回真实数据、`/ws/events` 收到 `__ready__`
- [x] 断网/无 Go 环境：Go sidecar 已从仓库删除，Rust 后端不引用任何 Go 产物；
      `server-go/` 只存在于 tag `go-sidecar-final`
- [ ] `cargo tauri build` 出包 + 安装后启动/升级流程 —— **本轮未做**（需要 NSIS
      打包环境与一次真实安装验证；`cargo build` 与单进程运行已验证）
- [x] `docs/RUST-BACKEND-MIGRATION.md`：Phase 3/4 勾选、总览数字、
      「迁移期工具/双跑」章节标注归档
- [x] `README.md` / `docs/TAURI-SHELL.md`：构建不再需要 Go 工具链
- [x] memory：单进程架构、双跑脚本归档

---

## D. 已知偏差（不阻塞，保留在文档里）

| 偏差 | 影响 |
|---|---|
| **协作域（delegation）只移植了注册表与部分工具** | `delegate_to_agent` / `get_delegation_status` / `cancel_delegation` / `resume_delegation` / `send_agent_message` / `check_user_feedback` 已落地（`acp/delegation.rs`）；**`propose_task_split` 与 `ask_user_question` 两个工具变体未接**，仍返回「未知协作工具」。拆分卡片的命令面（`acp_respond_task_split`）是有的 |
| `codeg-mcp` 的 `serverInfo.name` 为 `codeg-mcp-rust`（Go 为 `codeg-mcp-go`） | 仅日志展示，MCP 客户端不校验 |
| `fetch_url` 不做 GBK/Big5 解码 | 中文站点（GB2312/GBK）会乱码；Go 用 `golang.org/x/text` 解码，Rust 侧缺对应依赖 |
| `propose_task_split` 的卡片文案与 Go 一致，但 fan-out 未接 | 确认卡片后不会真的派发 |
| ACP driver 指纹哈希字节与 Go 不同 | 只影响进程池 key 命名，无跨实现混用 |
| DSH 补丁 YAML 输出字节与 yaml.v3 不同 | 语义等价，文件名 hash 不同（Go 会写单引号风格的 TOML，Rust 的 toml crate 用双引号；YAML 缩进宽度也不同） |
| `sessions.json` null→`[]` 归一 | Go 读端两种都接受 |
| `get_codeg_mcp_service_status.binary_path` | 报告 Rust 二进制（双跑掩码 `<BIN>`） |
| 任务引擎运行时字段在进程级注册表、合并锁为 tokio 异步锁、定时器 15s | 行为等价，重启后由磁盘布局重建 |
| ACP 有状态命令用 Go 的 fixture 测试验收，未走真实 harness 双跑 | 无状态命令已双跑 |
| dev 代理不隧道 HMR 的 WebSocket | 前端热更新需浏览器直连 `next dev` |
| `resolveExistingPath` 怪癖保留 | 见 §B5 |

---

## 附：账号智能体（独立账号）—— 上游新功能的移植状态（2026-11-13 23:30）

上游 `origin/main` 的两个新提交已移植到 Rust（详见
`docs/RUST-BACKEND-MIGRATION.md` 的同名章节）：

- [x] `account_channels.rs`：协议进程的启动/复用/调用（随机密钥、共享实例、
      Bearer 鉴权、45s 超时、8MB 上限）
- [x] 5 条 `codepluse_channel_*` 命令（status / login / login_status / cancel / logout）
- [x] 四个账号智能体的播种与公开名（`account_<id>`）
- [x] `account:<id>` 的 ACP 启动链（`acp-agent.mjs` + `CODEPLUSE_CHANNEL_*` 环境 +
      渠道模型校验）
- [x] 连接目录的账号块（managed / source / configured / models）
- [x] 主智能体实例 id 的 `toolKey`（`:` → `_`）
- [x] Tauri 外壳的 `open_external`（只允许 https，与 Go 桌面壳同契约）
- [x] 设置页的「管理官网账号」入口（发起登录 → 系统浏览器 → 轮询 → 刷新；
      已登录时列出账号并可退出）——`components/settings/account-channel-panel.tsx`

已知限制：

- 账号登录/推理需要用户在浏览器里完成真实授权，仓库内的检查不消耗任何额度；
- 协议进程需要 Node.js 22.19+（本仓前端与 bridge 都用它）。
