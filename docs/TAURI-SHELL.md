# Tauri 外壳（Tauri window + Go sidecar）

桌面端有两种外壳，共用同一个 Go 后端：

| 外壳 | 入口 | 构建命令 | 依赖 |
|---|---|---|---|
| **Tauri 2（默认）** | `src-tauri/src/main.rs` | `pnpm codepluse:desktop:build` | Go + Rust(MSVC) + VS Build Tools |
| WebView2（兼容保留） | `server-go/cmd/desktop` | `pnpm codepluse:desktop:build:webview2` | Go + 系统 WebView2 |

## 为什么不是"把后端改成 Rust"

`codeg-ui/src/lib/transport/tauri-transport.ts` 把应用自己的命令名原样交给
`invoke()`（`acp_connect`、`workspace_*` 等）。上游 Codeg 的 Rust 后端提供这些命令，
本仓库早已删除且 Git 历史里没有。**所以 Tauri 只能做外壳**：Rust 负责窗口生命周期
与 sidecar 启停，`codepluse-backend`（Go）继续在 127.0.0.1 上提供 HTTP/WebSocket。

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
├─ src/main.rs              窗口 + sidecar + 5 个窗口命令
├─ tauri.conf.json          bundle、externalBin、图标
├─ capabilities/main-shell.json
│                           允许 127.0.0.1:8840 页面调用窗口命令（remote.urls）
├─ frontend/index.html      占位：窗口加载的是后端页面，不是这里的产物
├─ binaries/                build 脚本生成 codepluse-backend-<triple>.exe
└─ icons/                   make-tauri-icons.ps1 生成
```

Rust 侧只做四件事：

1. 解析项目根：`CODEPLUSE_ROOT` → 可执行文件旁的 `workspace.json` → 可执行文件目录（与
   `cmd/desktop/main_windows.go` 一致）。
2. 用 `tauri-plugin-shell` 以 sidecar 方式启动 `codepluse-backend`，注入
   `HARNESS_HUB_CODEG=1`、`HOST=127.0.0.1`、`HARNESS_HUB_WARM=0`、`PORT`，
   工作目录设为项目根。
3. 轮询 TCP 8840 直到就绪（最长 30s），再用 `WebviewUrl::External` 打开
   `http://127.0.0.1:<port>/workspace`，`decorations(false)` 供自绘标题栏使用。
4. 进程退出时 `kill` sidecar；`window_open` 只接受站内绝对路径。

5 个命令与 WebView2 外壳的 `__codegGo*` 一一对应：
`window_open` / `window_close` / `window_minimize` / `window_toggle_maximize` /
`window_begin_drag`，由 `codeg-ui/src/lib/go-desktop.ts` 统一选择实现。

缩放（Tauri 独有）走官方 JS API 而不是自定义命令：Go 外壳保留了 `WS_THICKFRAME`，
原生边框即可缩放；Tauri 用 `decorations(false)` 换自绘标题栏后没有边框，于是由
`components/layout/window-resize-edges.tsx` 在窗口边缘铺 8 条不可见手柄，按下时调用
`getCurrentWindow().startResizeDragging()`。之所以不用 Rust 命令：`WebviewWindow` 没有
`start_resize_dragging`，方向枚举在 `tauri_runtime` 而非 facade crate；JS 侧已经封装好，
只需在 capabilities 里加 `core:window:allow-start-resize-dragging`。WebView2 外壳不会渲染这些手柄。

## 迁移前门（Rust 后端 Phase 1）

`src-tauri/src/backend/mod.rs` 在 sidecar 前面加了一层 Rust 服务：

```
WebView → 127.0.0.1:<port+1>   前门（health 本地应答，其余转发）
              ↓
        127.0.0.1:<port>        Go sidecar
```

- 端口：`CODEPLUSE_PORT` 是 sidecar（默认 8840），前门默认 `+1`（8841），可用
  `CODEPLUSE_PROXY_PORT` 覆盖；capabilities 已同时放行这两个端口
- 回滚：`CODEPLUSE_BACKEND=go` 让窗口直连 sidecar，不经前门
- 为什么现在只做转发：前端要求 334 个命令同时存在，逐个移植必须有一层能兜底的门；
  移植一个命令 = 在 `fallback` 之前加一条 axum 路由，然后删掉 Go 里对应的 handler
- 已移植（由 Rust 直接应答，不再转发）：`list_model_providers`、
  `create_model_provider`、`update_model_provider`、`delete_model_provider`
  （`src-tauri/src/backend/providers.rs`，与 Go 双跑比对一致）
- 详细分期与验收标准见 [后端 Rust 化迁移方案](RUST-BACKEND-MIGRATION.md)

## 前置条件

本机当前布局：Rust 1.100.0 装在 `E:\rust`（`CARGO_HOME=E:\rust\.cargo`、
`RUSTUP_HOME=E:\rust\.rustup`，两个变量已写入用户环境变量）。`build-tauri.ps1` 与
`tauri-dev.ps1` 会自己定位 `cargo`，不依赖当前会话的 PATH。

1. **rustup**（https://win.rustup.rs），默认工具链 `x86_64-pc-windows-msvc`。
2. **Visual Studio Build Tools**，勾选"使用 C++ 的桌面开发"。只装 rustup 会在链接阶段
   报 `linker not found`。
3. `pnpm --dir codeg-ui install`（提供 `@tauri-apps/cli`）。

验证：`rustc -vV` 的 `host:` 必须是 `x86_64-pc-windows-msvc`。

## 命令

```powershell
pnpm tauri:dev      # 构建前端 + Go + sidecar，然后 tauri dev
pnpm tauri:build    # 产出 NSIS 安装包：src-tauri/target/release/bundle/nsis
pnpm tauri:build -SkipFrontend   # 只重建 Rust 外壳
```

冷启动第一次 `cargo build` 需要下载数百个 crate，耗时取决于网络。

## 回退到 WebView2 外壳

Tauri 已是默认桌面端：`codepluse:desktop:build` 与自动更新（`rebuild-after-pull.ps1`）
都优先调用 `scripts/build-tauri.ps1`。检测不到 `cargo` 时自动回退到旧外壳，也可以手动：

```powershell
pnpm codepluse:desktop:build:webview2
```

## 安装包

```powershell
pnpm codepluse:desktop:build
# 产物：src-tauri/target/release/bundle/nsis/codepluse_0.42.0_x64-setup.exe
```

NSIS 按 `currentUser` 安装（无需管理员），`codepluse-backend-<triple>.exe` 作为
sidecar 一起打包。安装后外壳从 `resources/workspace.json` 解析仓库根
（`build-tauri.ps1` 在打包前按当前仓库路径生成），因此**仍依赖本机仓库存在**——
这是"仓库模式"，不是自包含发行版。

## 已知限制

- **端口写死在 capabilities 里**：`remote.urls` 只列了 `127.0.0.1:8840` 与
  `localhost:8840`。改 `CODEPLUSE_PORT` 后页面仍能用（HTTP），但自绘标题栏会失效
  （IPC 被 ACL 拒绝），需要同步改 `capabilities/main-shell.json`。
- **仍是"仓库模式"**：`resources/workspace.json` 把 Root 指向打包机器的仓库；
  换机器或移动仓库后需要重新打包，或设置 `CODEPLUSE_ROOT`。真正自包含需要把
  `codeg-ui/out` 与可写数据目录（`.data` → `%APPDATA%`）一起规划，尚未做。
- **启动失败无 GUI 提示**：release 版是 `windows_subsystem="windows"`，`setup` 失败会
  直接退出；排查时用 `pnpm tauri:dev` 或 `-DebugBuild` 看控制台。
- 与 WebView2 外壳一样，两者都复用同一个 8840 端口；先关掉旧实例再启动。
- **release 用 thin LTO**：`lto = true` + `codegen-units = 1` 在本机出现过 rustc 空转
  （12 核只起 1 个进程、28 分钟零产物）；改成 `lto = "thin"` + `codegen-units = 16`
  后稳定在 1 分钟内完成，产物 5.1 MB。
