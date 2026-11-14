# 桌面外壳（Tauri 窗口 + Rust 后端，单进程）

桌面端只有一种外壳：**Tauri 2 窗口 + 同一个二进制里的 Rust 后端**。

| 外壳 | 入口 | 构建命令 | 依赖 |
|---|---|---|---|
| **Tauri 2（唯一）** | `src-tauri/src/main.rs` | `pnpm codepluse:desktop:build` | Rust(MSVC) + VS Build Tools + pnpm |

旧的 Go + WebView2 外壳随 Phase 4 一起删除（Go 源码留档在 tag `go-sidecar-final`）。

## 架构

```
codepluse.exe（一个进程）
├─ Tauri 窗口          src/main.rs：窗口生命周期 + 5 个窗口命令
└─ Rust 后端           src/backend/：HTTP API + 事件 WebSocket + 静态资源
       监听 127.0.0.1:$CODEPLUSE_PORT（默认 8840）
```

没有 sidecar、没有第二个进程、没有 `+1` 前门。

## 为什么前端仍走 HTTP 而不是 `invoke()`

`codeg-ui/src/lib/transport/tauri-transport.ts` 把应用自己的命令名原样交给
`invoke()`（`acp_connect`、`workspace_*` 等）。上游 Codeg 的 Rust 后端提供这些命令，
本仓库没有这套实现——**后端是 HTTP 服务，窗口只是它的宿主**。

前端靠一个注入标记区分两者：

```
src-tauri/src/main.rs   →  window.__CODEG_TAURI_SHELL__ = true
codeg-ui/src/lib/transport/detect.ts
   isTauriSidecarShell() → true 时 detectEnvironment() 仍返回 "web"
```

少了这个标记，应用会切到 `TauriTransport` 并调用不存在的 Rust 命令，界面立刻失效。

## 组成

```
src-tauri/
├─ src/main.rs              窗口 + 后端进程内启动 + 5 个窗口命令
├─ src/backend/             全部后台能力（见下）
├─ tauri.conf.json          bundle、图标（不再有 externalBin）
├─ capabilities/main-shell.json
│                           允许 127.0.0.1:8840 页面调用窗口命令（remote.urls）
├─ frontend/index.html      占位：窗口加载的是后端页面，不是这里的产物
└─ icons/                   make-tauri-icons.ps1 生成
```

`src/backend/` 的分工：

| 模块 | 职责 |
|---|---|
| `mod.rs` | axum 路由、命令分派、本地请求防护、事件出口 |
| `webapp.rs` | 静态资源、health、附件上传、companion 端点、dev 代理 |
| `events_ws.rs` | 事件 WebSocket 协议（attach/detach/snapshot/replay/event） |
| `acp/` | ACP 运行时、连接层、协作（delegation）、分片、问答 |
| `store_reads.rs` / `store_writes.rs` | `codeg-workspace.json` 的读写 |
| `git.rs` / `files.rs` / `folder_commands.rs` | Git、文件、目录命令 |
| `terminal.rs` / `browser.rs` / `computer.rs` | ConPTY 终端、CDP 浏览器、桌面控制 |
| `worktask.rs` / `worktask_engine.rs` / `automations.rs` | 任务看板、执行引擎、定时任务 |
| `providers.rs` / `exec_providers.rs` / `provider_catalog.rs` / `provider_writeback.rs` | 供应商读写与外部写回 |
| `mcp_companion.rs` | `codeg-mcp` 伴随服务（stdio JSON-RPC） |

## 外壳做四件事

1. 解析项目根：`CODEPLUSE_ROOT` → 可执行文件旁的 `workspace.json` → 可执行文件目录。
2. 在进程内 `spawn` 后端服务（tokio 任务），监听 `CODEPLUSE_PORT`（默认 8840）。
3. 轮询端口直到就绪（最长 30s），再用 `WebviewUrl::External` 打开
   `http://127.0.0.1:<port>/workspace`，`decorations(false)` 供自绘标题栏使用。
4. `window_open` 只接受站内绝对路径。

无头模式（`CODEPLUSE_HEADLESS=1`）只起后端不开窗口——烟测与迁移期对拍脚本用它。

5 个窗口命令：`window_open` / `window_close` / `window_minimize` /
`window_toggle_maximize` / `window_begin_drag`，由 `codeg-ui/src/lib/go-desktop.ts`
统一选择实现。

缩放（Tauri 独有）走官方 JS API 而不是自定义命令：`WebviewWindow` 没有
`start_resize_dragging`，方向枚举在 `tauri_runtime` 而非 facade crate；JS 侧已封装好
（`components/layout/window-resize-edges.tsx` + capabilities 里的
`core:window:allow-start-resize-dragging`）。

## 前置条件

本机当前布局：Rust 1.100.0 装在 `E:\rust`（`CARGO_HOME=E:\rust\.cargo`、
`RUSTUP_HOME=E:\rust\.rustup`）。`build-tauri.ps1` 与 `tauri-dev.ps1` 会自己定位
`cargo`，不依赖当前会话的 PATH。

1. **rustup**（https://win.rustup.rs），默认工具链 `x86_64-pc-windows-msvc`。
2. **Visual Studio Build Tools**，勾选「使用 C++ 的桌面开发」。只装 rustup 会在链接阶段
   报 `linker not found`。
3. `pnpm --dir codeg-ui install`（提供 `@tauri-apps/cli`）。

验证：`rustc -vV` 的 `host:` 必须是 `x86_64-pc-windows-msvc`。

## 命令

```powershell
pnpm tauri:dev      # 构建前端 + tauri dev（后台在同一进程内起）
pnpm tauri:build    # 产出 NSIS 安装包：src-tauri/target/release/bundle/nsis
pnpm tauri:build -SkipFrontend   # 只重建 Rust 外壳
```

冷启动第一次 `cargo build` 需要下载数百个 crate，耗时取决于网络。

## 安装包

```powershell
pnpm codepluse:desktop:build
# 产物：src-tauri/target/release/bundle/nsis/codepluse_0.42.0_x64-setup.exe
```

NSIS 按 `currentUser` 安装（无需管理员）。安装后外壳从 `resources/workspace.json`
解析仓库根（`build-tauri.ps1` 在打包前按当前仓库路径生成），因此**仍依赖本机仓库存在**
——这是「仓库模式」，不是自包含发行版。

## 已知限制

- **端口写死在 capabilities 里**：`remote.urls` 只列了 `127.0.0.1:8840` 与
  `localhost:8840`。改 `CODEPLUSE_PORT` 后页面仍能用（HTTP），但自绘标题栏会失效
  （IPC 被 ACL 拒绝），需要同步改 `capabilities/main-shell.json`。
- **仍是「仓库模式」**：`resources/workspace.json` 把 Root 指向打包机器的仓库；
  换机器或移动仓库后需要重新打包，或设置 `CODEPLUSE_ROOT`。真正自包含需要把
  `codeg-ui/out` 与可写数据目录（`.data` → `%APPDATA%`）一起规划，尚未做。
- **启动失败无 GUI 提示**：release 版是 `windows_subsystem="windows"`，`setup` 失败会
  直接退出；排查时用 `pnpm tauri:dev` 或 `-DebugBuild` 看控制台。
- release 用 thin LTO（`lto = "thin"` + `codegen-units = 16`）：`lto = true` +
  `codegen-units = 1` 在本机出现过 rustc 空转（12 核只起 1 个进程、28 分钟零产物）。

## 迁移记录

- 后端从 Go 迁到 Rust 的分期与验收：`RUST-BACKEND-MIGRATION.md`
- 收尾清单与已知偏差：`RUST-BACKEND-CLOSE-OUT.md`
- 迁移期双跑工具（需要旧 Go sidecar）：`../scripts/historical/README.md`
