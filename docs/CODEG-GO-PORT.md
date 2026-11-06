# codepluse 移植

基于本机 `I:\NEW\code\codeg` 的 0.42.0 源码，保留原 Next.js / React 界面和交互，将核心 Rust 服务接口迁移为 Go。没有调用上游 Rust 后端。Windows 桌面窗口由 Go + WebView2 创建。

## 启动

依赖：Go 1.27、Node.js 22+、pnpm 11，以及 Windows WebView2 Runtime。Codex / Claude Code 需配置可用的模型服务。模型选择来自各执行端实际返回的目录。

```powershell
npm ci
pnpm --dir codeg-ui install --frozen-lockfile
npm run codepluse:build
npm run codepluse:start
```

界面：<http://127.0.0.1:8840/workspace>。默认端口 8840，原项目 8787 服务可同时运行。直接运行 Go 后台时，工作目录必须是项目根目录，设置 `HARNESS_HUB_CODEG=1` 和 `PORT=8840`。

```powershell
# 构建 Go 桌面窗口，并创建 codepluse 桌面快捷方式
powershell -NoProfile -File scripts/build-codeg-desktop.ps1 -InstallShortcut
& '.\dist\codepluse\codepluse.exe'
```

桌面程序复用已有 codepluse 后台，未运行时自动启动。关闭窗口后后台继续运行。当前是依赖本机项目目录的构建，`dist/codepluse/workspace.json` 记录项目路径；移动项目后重新构建。WebView2 用户资料独立保存在系统缓存目录的 `Codeg Go/WebView2`（保留旧路径以延续已有偏好设置）。

## 已迁移的核心

### DSH 与 WorkBuddy

后台会检测本机 `@deepseek-ai/dsh`，以 `node …/lib/bin.js --profile acp` 启动，界面显示 DeepSeek。DSH 的模型配置、工具策略和凭据由它自己的 ACP profile 管理；不伪造只读模式，恢复会话使用其声明的 `session/resume`。只读负责人不能向 DSH 委派，需先切换工作区模式。

WorkBuddy 使用桌面安装包内置的 CodeBuddy 执行内核，界面显示 WorkBuddy。精简包以 `--serve --acp --host 127.0.0.1` 启动并设置 `CODEBUDDY_FORCE_LITE_WB_BUNDLE=1`，保留服务自身的默认认证。默认使用已有 `.workbuddy` 配置目录，也可沿用 `CODEBUDDY_CONFIG_DIR`。这不等于控制 WorkBuddy 桌面窗口。

非标准安装路径可在 `.data/harness-paths.json` 配置 `workbuddy` / `dsh` 的 Node 入口绝对路径，或使用 `HARNESS_HUB_WORKBUDDY_BIN` / `HARNESS_HUB_DSH_BIN`。文件只记录路径，不保存登录凭据。也支持 `HARNESS_HUB_WORKBUDDY_HOME` 指向桌面安装目录。

本机验证：两者均通过真实 ACP 初始化、会话创建、模型目录读取及 codepluse MCP 注入。DSH 已把桌面 profile 的 provider/default-model 配置复制到 ACP profile（原文件备份为 `cordis.patch.yml.codepluse-backup`），默认线路实际调用返回额度耗尽；WorkBuddy 内置 CLI 实际调用返回 Authentication required，其桌面登录未被 CLI 自动复用。完成 CLI 登录并恢复 DSH 线路额度后才能完成模型任务；这些失败会作为失败显示，不会冒充成功。

本次更新后台运行在 **8841**，桌面快捷方式使用 `--port 8841`；旧 8840 后台保留。桌面构建现在生成 `server-go/serve-codepluse.exe`，兼容旧 `serve-codeg.exe`。

- 原始会话、聊天、项目侧栏、多个标签及版本冲突保护，原 UI 的中文、模型和模式选择器。
- Go 直接管理 Codex / Claude ACP 原生进程、持续会话、真实流式输出、工具调用、取消与权限请求。
- 每个原生会话注入独立的 `codeg-mcp` 子进程。模型调用 `list_agents` / `delegate_to_agent` 自主选择执行端和模型，多次调用可并行派发；`get_delegation_status` 等待并收集真实结果。子任务有独立 ACP 上下文、父子会话关联、继承权限、最多四层委派。
- 同一任务树的执行端可通过 `send_agent_message` 发消息，并通过 `check_user_feedback` 获取消息、用户干预及团队分工。消息入箱与被模型读取是两个阶段。
- 原始 WebSocket attach / snapshot / event 协议；对话和工具历史持久化；真实文件列表、预览、带版本冲突检查的编辑保存。
- 原仓库附带的专家 / 科研技能内容与本机技能安装状态读取。
- Windows 原生 ConPTY 终端：交互输入、尺寸调整、滚动输出、退出码、重连快照、代际隔离；关闭标签清理进程，画布终端在切换页面时继续运行。
- 本地 Git 面板：状态、差异、历史、分支、选中文件的暂存与提交、冲突内容与解决、stash、remote、系统凭据下的 fetch / pull / push。路径作为独立参数传给真实 Git，文件名按字面匹配。提交不会带入未选中文件的既有暂存改动。
- 无限画布：多个画板，八类节点，便签、位置、尺寸、区域分组与成员拖出持久化；每次节点变更对应一个有序版本事件。删除终端卡片或画板时清理其终端进程。
- 文件 / Git 状态通过 1.5 秒轮询发送原工作区事件协议，编辑器和变更面板可恢复完整快照。Go 桌面壳支持原版提交等独立窗口。

只读模式仅自动放行本工作台注册的、名称严格匹配的协作 MCP 工具；不会据此批准普通文件编辑、命令或模式升级。

## 当前边界

这是一份可运行的移植，尚未完成 Codeg 全功能等价。自动化、待办、远端仓库的 Issue / PR 工作台、Office CLI、浏览器 / 电脑操作、上游更新器等仍需迁移。本地 Git 的自定义凭据账户、自动生成提交说明、部分 worktree / 分支更新组合操作还未接通；网络操作使用系统 Git 凭据。保留原 UI 入口，调用未实现接口会明确报错。终端当前面向 Windows；对话输入仅支持文本；原生历史导入和任意消息分叉尚未实现。

工作台数据保存在 `.data/codeg-workspace.json`，原生 ACP 会话元数据保存在 `.data/codeg-sessions.json`，与旧工作台数据分开。委派任务的运行状态、待收消息仍保存在当前服务进程中；重启后可查看已保存的父子会话历史，但不能继续等待重启前的委派 ID。

## 验证

```powershell
Set-Location server-go
go test ./...
go vet ./...
Set-Location ..
npm test
```

协议测试使用独立 ACP fixture 进程，覆盖真实进程通信和事件顺序；fixture 不代表模型验收。另已使用本机配置实际连接 Claude Code，模型经 MCP 委派 Codex 计算 19+23，取得子任务 `completed` 和结果 42，再由主会话汇报。所用模型为本机网关提供的 `qwen3.8-flash`；Harness 与模型名称是两件事。

本轮新增测试使用真实 ConPTY 和临时 Git 仓库，验证输入执行、退出码、输出保留、提交选择及已暂存删除、画布事务回滚与重启恢复、工作区文件变化。原界面的手动验收画板为“Go 迁移验收”，Git 验收仓库位于 `workspace/codeg-go-git-check`；项目根目录未初始化 Git。

## 代码位置

- `codeg-ui/`：原始 Codeg 前端，Apache-2.0。
- `codeg-assets/`：上游专家 / 科研技能和原许可通知。
- `server-go/internal/codeg/`：原 API 合约、会话桥接、WebSocket、MCP、委派及数据存储。
- `server-go/internal/acp/`：Go ACP 运行时。
- `server-go/cmd/desktop/`：Windows 原生桌面窗口与后台启动。

许可与出处见根目录 `THIRD_PARTY_NOTICES.md`，上游许可证保存在 `codeg-ui/LICENSE`。
