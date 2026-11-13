# 主智能体与子智能体供应商 / 模型配置

2026-11-12。运行服务为 `http://127.0.0.1:8841`。

## 使用

- 协作 → 主智能体配置：选择 Harness、API 供应商和模型，再保存。新对话优先使用此配置，工作区输入框上方不再显示多个 Harness 切换入口。
- 协作 → 子智能体配置：为同一个 Harness 添加多套具名配置。每套固定自己的供应商和模型，分别保存。主智能体通过 `list_agents` 得到具体配置的 `agent_type`、`provider_id` 与 `default_model`，可按任务选择同一 Harness 的不同模型。
- 原生供应商与模型可以刷新读取。模型目录不可用时保留原配置中的模型，并允许手动填写模型 ID；不会伪造网关返回的模型。

## 数据来源与运行

- Claude Code：只读 CC Switch 的 SQLite 数据库及 Claude 原生 API 设置。
- Codex：读取 CC Switch 的 API 配置和模型目录，或原生 `config.toml` / API-key auth 配置。
- DSH：读取 desktop / acp profile 中的供应商、模型和 API-key 引用。
- 同时支持 codepluse 已保存的模型供应商，以及在当前表单为这些 Harness 添加 API 供应商。
- 官方登录 / 订阅凭据不作为 API 供应商展示。WorkBuddy 等没有接入对应 API 配置的执行端不提供伪造的供应商选项。
- 前端只接收供应商 ID、名称、来源、模型和密钥是否存在，不接收密钥。模型目录请求的 URL 和凭据均在后端由同一个供应商 ID 解析。
- 主配置使用独立、不可变的实例 wire；更换主模型不会改写旧主实例，已有对话仍可恢复原供应商和模型。保存通知更新工作区的未持久化草稿，新对话优先级为主智能体 → 文件夹默认 → 继承 / 可用执行端。
- Claude 的供应商环境按进程注入；Codex 写入隔离的 CODEX_HOME；DSH 使用每套配置独立生成的 YAML patch 和进程环境。不会切换全局供应商，也不修改 CC Switch 或 DSH 的原配置。
- 配置 API 模式后，委派目录使用已保存的 API 子配置；同名 Harness 的多个目标通过不同 wire 区分。指定的模型与目标配置不一致时返回错误，不会回退到另一供应商或订阅。

## 验证

- `go test ./...`、`go vet ./...` 通过；覆盖原生配置解析、前端数据脱敏、主实例与历史实例隔离、同 Harness 多模型解析、供应商 URL / 凭据绑定，以及 Codex / DSH 启动配置隔离。
- TypeScript `tsc --noEmit` 通过；修复了供应商页两个原有的可空类型问题。
- 六个相关前端测试文件共 133 项测试通过，覆盖供应商 / 模型选择、订阅排除、迟到目录响应、主配置保存、跨窗口事件与新草稿、已有对话保留原执行端、Go 原生工具与协作能力共存。
- 修改的前端文件 ESLint 无错误；原 ACP 设置大文件仍有两条已有的 hook 依赖警告。
- 额外运行的旧 `conversation-detail-panel-layout.test.ts` 有 7 条静态断言失败，断言仍针对已替换的工作区结构；涉及的工作区布局和输入框源文件在本次功能改动前就不含这些断言要求的内容。本次未通过修改这些断言掩盖失败。
- 在本机实际读取到 CC Switch 的 Claude / Codex API 配置、Claude 原生 API 配置、DSH 的 desktop / acp 供应商与模型；服务返回的数据已验证不包含密钥。运行中的接口直接返回 DSH 模型目录，浏览器验证了实际模型联动。
- 本轮未对外发送模型推理任务；执行与委派协议验证使用 ACP fixture。配置读取和浏览器模型目录验证使用真实本机数据，不代表所有网关的额度、鉴权或推理可用性都已验收。

预览：`codepluse-main-agent-models.png`、`codepluse-sub-agent-profile.png`、`codepluse-workspace-main-agent.png`。

## 主智能体保存与草稿复用修复

- 修复 Claude / DSH 创建配置时直接以内部 tool 名查找执行端的问题，统一转换为 `claude_code` / `deepseek` wire；主配置、子配置保存与启动恢复使用同一映射。
- 新增使用生产形式 registry ID 的 Go 回归测试，覆盖 Claude、Codex、DSH 的主配置、子配置保存和重新加载。修复前 Claude / DSH 均复现“主智能体配置无效”。
- 修复“新建会话”复用聊天草稿仍沿用旧 Harness，以及冷启动恢复旧草稿未应用 API 主配置的问题。正式对话仍保留原执行端；草稿 ID 和输入内容保留。
- 本次 `go test ./...`、`go vet ./...`、TypeScript 检查、前端构建及 104 项相关前端测试通过。Go 全量首轮的工作树生命周期测试出现一次异步时序失败，单独复测及随后全量复测均通过。
- 已在运行界面实际保存用户原先选择的 `Claude Code / default（CC Switch）/ qwen3.8-flash`，并确认服务重启后恢复相同供应商和模型。本次未发送推理消息。
