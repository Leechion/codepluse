# 自定义 ACP 智能体的 Go 移植

2026-11-13。当前桌面服务：`http://127.0.0.1:8840`。

## 对照来源

同级 `I:\NEW\code\codeg` 的 `src-tauri/src/acp/remote_registry.rs`、`acp/custom_registry.rs`、`commands/custom_agents.rs`。保留原前端的 camelCase 数据契约、`custom:<registryId>` 标识及原有界面。

## 已接通

- 读取真实 ACP 公共注册表、查询服务端平台、标识已添加和平台兼容的条目。内置标记以 Go 版实际支持的执行端为准。
- 从注册表添加，或手动填写发行版 JSON；支持编辑名称、图标、来源、技能声明、环境配置和 MCP 声明。定义保存在 codepluse 工作区数据文件中，重启后恢复到运行时。
- `npx` 包安装至 `.data/acp-custom/`，从实际安装的 `package.json` 解析 console-script，兼容包名与命令名不同的情况；Windows 使用 Node/npm 的实际入口。
- `uvx` 发行版通过 uv 独立工具目录安装；二进制发行版下载、校验 SHA-256、解压并启动。支持 ZIP、tar、tar.gz、tar.bz2；解压路径验证也覆盖 Windows 的点和空格规范化。
- 首次连接可准备发行版，设置页的准备/下载、清缓存、卸载接口使用同一路径；进度与结果发往原有安装事件通道。现有内置 Harness 启动分支保留。
- 启动进程继承该发行版参数及环境变量，再合并 codepluse 的单智能体环境配置。关闭 MCP 声明后不注入协作伴生服务。
- 移除自定义定义时保留既有对话；不再以 `[object Object]` 显示结构化错误。

## 验证

- Go 全量测试、`go vet`、前端构建和 TypeScript 检查通过。
- 11 项相关前端测试通过，覆盖原有表单解析和结构化错误后的重试。
- 本地测试完成真实 npm 包安装，解析与包名不同的 bin，启动安装后的 ACP 测试进程；另验证 JSON-RPC 初始化、提示和响应流、定义保存与恢复、供应商环境隔离、归档校验和路径拒绝。
- 运行中的服务读取到 45 个公共注册表条目；界面已确认列表可加载，不再出现 `[object Object]`。未由本轮操作安装或运行任何新的第三方智能体，未发送付费模型推理任务。
- 侧栏会话定位层改为透明，连接线使用低对比度颜色；在已有父/子会话上确认无方形白底，圆角选中态保留。截图：`codepluse-sidebar-glass-fixed.png`、`codepluse-acp-registry-fixed.png`。

## 当前边界

- Node.js/npm 或 uv 运行环境需已安装。本机已实际验证 npm 安装路径，uv 分支验证了缓存和进程参数，未实机下载外部 Python 智能体。
- 原版的自定义版本选择、上游版本升级，以及删除定义时同时清除历史记录尚未迁移；后者会明确报错，不会假装删除成功。
- 本版归档解压不接受符号链接或特殊设备文件；遇到这类发行版会返回具体错误。

安装目录按工具的隔离方式组织，参考 [uv 工具目录](https://docs.astral.sh/uv/reference/cli/#uv-tool-install)；npm 启动入口根据安装后的包元数据解析，参考 [npm exec 的入口规则](https://docs.npmjs.com/cli/v11/commands/npm-exec/)。
