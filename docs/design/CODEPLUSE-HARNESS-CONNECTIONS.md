# Harness 连接与模型配置

智能体设置以「智能体 → 连接方式」组织。API 供应商与官网订阅分别保存，协作中的主、子智能体复用相同的选择方式。执行参数、环境变量、安装状态等高级配置不再占据智能体页的主界面；自定义 ACP 注册入口保留。

## 配置来源

| Harness | API 来源 | 官网账号 |
| --- | --- | --- |
| Claude Code | CC Switch 的 Claude 条目，以及 Codepluse 独立配置 | 本机 Claude Code 登录凭据 |
| Codex | `CODEX_HOME/config.toml`、对应环境变量及 `auth.json` API Key，以及独立配置 | 本机 `auth.json` 的账号登录凭据 |
| DSH | `DSH_HOME` 中 desktop/acp profiles、凭据引用及环境变量，以及独立配置 | 未接入，选项禁用 |
| WorkBuddy / 已注册的 Qoder CLI | 沿用原生客户端管理，尚无独立 API 编辑适配 | 由原生 Harness 管理登录 |
| 其他自定义 ACP | 保留原生配置 | 未适配的账号方式禁用 |

仅 Claude Code 从 CC Switch 导入。历史 `ccswitch:codex:*` 引用需重新选择 Codex 原生供应商，避免继续使用错误来源。

## 保存与启动

- 外部来源按需读取。编辑现有供应商后点击「保存修改」，Claude Code 更新 CC Switch 的原始行，Codex / DSH 写回各自原生配置，保持供应商 ID 不变；密钥留空沿用现有值，前端只接收是否已配置密钥。原生格式不支持的模型显示/启用信息以同一 ID 存入无密钥的 UI 元数据，不新建供应商。
- 每个供应商保存模型列表、默认模型、启用状态。同步模型列表保留手动编辑和停用的条目。
- `codepluse_connection_save` 保存每个基础 Harness 的默认连接引用。主智能体与子智能体仍各自保存准确的供应商、模型或官网订阅方式。
- 官网订阅以 `auth_mode: subscription` 保存。主智能体使用独立实例，新建对话跟随该实例；已有会话保留原实例。
- Codex 与 Claude 的官网模式使用应用内隔离目录，移除 API 覆盖后载入本机登录凭据。Codex API 模式也移除隔离目录中的账号令牌。缺少真实账号凭据时明确报错，不回退到 API。
- 本机凭据存在只表示已读取文件，不代表已在线验证有效性。登录由原生 `codex login` / `claude auth login` 完成。

## 验证

Go 回归覆盖账号/API 隔离、订阅模式保留原始账号配置、供应商编辑写回来源、订阅配置重载、委派到已保存的账号实例、供应商模型保存及密钥不返回前端。前端回归覆盖官网模式切换、原供应商保存、主智能体默认模型恢复及异步模型列表切换。
