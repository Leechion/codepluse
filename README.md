# codepluse · 多 Harness 工作台

codepluse 使用 Codeg 0.42.0 的 Next.js 界面、**Rust ACP 后台**和 Windows 桌面窗口（Tauri 2）。它支持真实 ACP 会话、执行端委派、文件和 Git 操作、终端与画布。

## 安装与启动

需要 Node.js 22+、pnpm 11；构建 Windows 桌面版还需要 Rust（x86_64-pc-windows-msvc）与 Visual Studio Build Tools。Windows WebView2 Runtime 由 Tauri 自带。执行端模型服务需单独安装并配置；ACP 适配器（claude/codex）作为**全局** npm 包安装，不再随仓库安装落本地 `node_modules`——后台按 `%APPDATA%\npm\node_modules` → 仓库 `node_modules` 的顺序查找适配器。

```powershell
npm i -g @agentclientprotocol/claude-agent-acp @agentclientprotocol/codex-acp
pnpm --dir codeg-ui install --frozen-lockfile
npm run codepluse:build
npm start
```

> 根目录没有运行时依赖，`npm ci` 不会安装任何东西，已从步骤中移除；前端依赖只装 `codeg-ui/`。安装会触发 `postinstall`，把 Monaco 与 PDF.js 资源复制到 `codeg-ui/public/`。

打开 http://127.0.0.1:8840/workspace。构建 Windows 桌面版使用 `npm run codepluse:desktop:build`（Tauri 2 窗口 + Rust 后端，需要 Rust/MSVC，见 [Tauri 外壳说明](docs/TAURI-SHELL.md)）。安装、功能边界及数据位置见 [移植说明](docs/CODEG-GO-PORT.md)。

## 代码结构

- `codeg-ui/`：Codeg 前端。
- `src-tauri/`：桌面外壳与**全部后台**——HTTP API、事件 WebSocket、静态资源、ACP 运行时、终端、浏览器、画布、任务与定时任务。
- `fixtures/`：ACP 协议测试使用的 fixture。
- `skills/`：可注入执行端的技能包。
- `scripts/historical/`：迁移期的双跑对拍工具（需要旧 Go sidecar，见其 README）。
- `docs/`：设计文档与迁移记录（`docs/RUST-BACKEND-MIGRATION.md`）。

验证：在 `src-tauri/` 执行 `cargo test`。旧的 Go 后台（`server-go/`）已于 Phase 4 删除，其最终状态留档在 tag `go-sidecar-final`；历史设计文档记录了 Node 版 → Go 重写 → Rust 移植的演进。
