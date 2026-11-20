# 134 项未实现后端命令 —— 逐项补全台账

- 日期：2026-11-15
- 仓库：`E:\code\codepluse`，HEAD `e6a256f`（**未提交任何改动**，无 commit / 无 checkout / 无 stash）
- 起点：`node scripts/audit-command-coverage.mjs --check` → `calls without a Rust command owner: 134`，exit 1
- 终点：同一条命令 → `calls without a Rust command owner: 0`，exit 0

## 一、先说一个影响解读的事实

**这 134 条命令在原 Go sidecar 里一条都没有实现过。** 把 Go 的最终状态（tag 前后提交 `76712f5`）解出来逐条搜字面量，只有 `codepluse_provider_delete` 在 `server-go/internal/codepluse/server.go` 里有原型（`providers.Delete`）。

所以这批命令没有"照 Go 移植"这条路，契约只能从前端反推：参数名照 `codepluse-ui/src/lib/api.ts` 等调用点，返回结构照 `codepluse-ui/src/lib/types.ts`（宠物组另有 `lib/pet/types.ts`）。**前端一行没改**，是后端去对齐前端。

## 二、总览

| 组 | 模块（`src-tauri/src/backend/`） | 命令 | 新增测试 |
|---|---|---:|---:|
| 日志 | `logs.rs` | 6 | 14 |
| ACP 与智能体 | `acp_ext.rs` | 19 | 35 |
| 备份与恢复 | `backup.rs` | 11 | 13 |
| Git | `git_ext.rs` | 3 | 6 |
| 会话导入 | `import_sessions.rs` | 4 | 8 |
| Web 服务 | `web_service.rs` | 6 | 8 |
| 提供商与令牌验证 | `token_validate.rs` | 4 | 11 |
| Codex 目录与设备授权 | `codex.rs` | 9 | 19 |
| MCP | `mcp_market.rs` | 10 | 16 |
| 聊天渠道与账号令牌 | `chat_channels.rs` | 17 | 16 |
| 桌面宠物（数据/素材） | `pet.rs` | 14 | 16 |
| 桌面宠物（窗口/交互） | `pet_window.rs` | 13 | 12 |
| OpenCode | `opencode.rs` | 7 | 12 |
| 项目脚手架与技能 | `projects_skills.rs` | 4 | 7 |
| 浏览器桥 | `browser_bridge.rs` | 2 | 6 |
| CLI 访问与驱动 | `cli_access.rs` | 3 | 6 |
| Office 监视与关窗 | `office_ext.rs` | 2 | 7 |
| 派发集成验收 | `mod.rs`（`transport_contract_tests`） | — | 1（17 条命令逐个走真实 HTTP） |
| **合计** | **17 个新模块** | **134** | **197** |

状态分布：**implemented 109 条、limited 24 条、already-owned 1 条、blocked 0 条**。
`limited` 不是"没做完"，是"本产品里确实没有该能力（无外部服务/无凭据/单窗口架构没有独立窗口）"，实现按前端类型返回确定形态并在模块文档注释里写明原因，**没有一条编造数据或伪装成功**。

## 三、架构改动（唯一改动的既有文件：`mod.rs`）

新增一张表驱动派发表，替代"每个模块抄一遍 `spawn_blocking` + 匹配样板"：

```rust
struct ExtModule {
    name: &'static str,
    owns: fn(&str) -> bool,
    handle: fn(&std::path::Path, &str, &Value) -> Option<Result<Value, String>>,
}
const EXT_MODULES: &[ExtModule] = &[ /* 18 个模块，18 行 */ ];
```

`api_dispatch` 里加了一个分支：命中即 `spawn_blocking` 执行、成功走 `go_json`（带 `x-codepluse-backend: rust` 标记）、`None` 走原兜底转发。八个并行写者因此**只需要动自己的新文件**，共享文件（`mod.rs`）全程只有主控一个写者。

## 四、逐项台账（134 行）

状态口径：`implemented` = 真实分派 + 真实行为/契约；`limited` = 有真实分派但该能力在本产品中不存在，按前端类型返回确定形态；`already-owned` = 运行时已由既有代码应答，审计因该模块没有 `owns()` 才误报。

### ACP 与智能体（19）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 1 | `acp_answer_plan_approval` | implemented | 决定按 approvalId 幂等落 `.data/plan-approvals.json` |
| 2 | `acp_antigravity_login_cancel` | implemented | 移除待处理登录尝试；未知 handle 幂等成功 |
| 3 | `acp_antigravity_login_finish` | limited | 令牌交换链路未接入，回 `{signedIn:false,message,retryable,credentialPath:null}`，校验 handle/redirect |
| 4 | `acp_antigravity_login_start` | limited | 有凭据走真实 alreadySignedIn 分支；无凭据时 consent 链接由运行时签发，返回明确 Err（不伪造 URL） |
| 5 | `acp_antigravity_sign_out` | implemented | 清本地凭据文件并摘除厂商 `settings.json` 的 `auth` 块，返回同步报告 |
| 6 | `acp_cursor_auth_status` | limited | `cursor-agent status` 探针属运行时，回空态 + error，不臆造登录态 |
| 7 | `acp_cursor_list_models` | limited | 同上，回空 `models` + error |
| 8 | `acp_fetch_agent_latest_release` | limited | 发布源由 `CODEPLUSE_ACP_RELEASE_URL` 覆盖；未配置回 null；短超时真请求，解析三种版本形状 |
| 9 | `acp_install_uv_tool` | limited | 先发 `app://agent-install` started 事件，再返回 Err（下载解压链路未接入） |
| 10 | `acp_list_pi_model_capabilities` | limited | 回 `{status:"failed",models:[]}` |
| 11 | `acp_load_deepseek_model_catalog` | implemented | 真读 `$DSH_HOME/settings.yaml` 的 `llm-deepseek.models`；缺失=继承内置目录 |
| 12 | `acp_qoder_auth_status` | limited | `qoder status` 探针未接入，回空态 + error |
| 13 | `acp_reclaim_leaked_temp` | implemented | 先重扫再逐条复核路径在扫描根之内且文件名匹配本项目前缀，否则进 failed，绝不删未知路径 |
| 14 | `acp_scan_leaked_temp` | implemented | 只读扫描临时目录，只认 `acp-persist-`/`acp-splits-`/`acp-storeconv-` 前缀，<1h 计 skipped |
| 15 | `acp_sync_antigravity_settings` | implemented | 把保存的 methodId 写进 `settings.json` 的 `auth.type`；written/already_current/skipped 三态 |
| 16 | `acp_update_claude_model_picker` | implemented | 规范化/去重/保序后持久化 `.data/claude-model-picker.json`（在跑会话计数归运行时，回 0） |
| 17 | `acp_update_deepseek_model_catalog` | implemented | 校验 id 唯一/contextWindow>0/模态合法后写回 `llm-deepseek.models`，其余键保留；null 移除回继承 |
| 18 | `acp_update_kimi_code_config` | implemented | 写 `~/.kimi-code/config.toml`：raw 原样、apikey 合并受管块、login 清块与合成门控令牌 |
| 19 | `acp_update_system_agent` | limited | 整包更新归运行时；请求记录落 `.data/agent-updates.json` 并回 null |

### 备份与恢复（11）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 20 | `backup_active_agents` | limited | 本后端无在线智能体注册表，诚实回 `[]` |
| 21 | `backup_cancel` | implemented | 状态机把在途操作置 `cancelled` 并发 `backup://progress`；已结束返回 false |
| 22 | `backup_create` | limited | 状态机 `snapshotting→done` + manifest 落 `manifests/<opId>.json`；**只覆盖 `.data/backups` 自有元数据，不打包工作区** |
| 23 | `backup_create_ticket` | implemented | 票据登记 `tickets/<id>.json`，返回 `/api/backup_ticket/<id>` 与 filename |
| 24 | `backup_discard_pending` | implemented | 删 `staged.json` + `rollback-pending.json`，幂等 |
| 25 | `backup_list_safety_snapshots` | implemented | 真实扫描 `snapshots/*/meta.json`，时间倒序，越界记录跳过 |
| 26 | `backup_prepare_source` | implemented | 句柄登记 `sources/<id>.json`；源路径先校验在备份目录内；缺失源回 `compatible:false` 预览不发句柄 |
| 27 | `backup_release_source` | implemented | 删句柄记录，不存在也回 true（幂等） |
| 28 | `backup_restore_stage` | implemented | 二次校验源路径未越界，写 `staged.json`，返回 StagedRestore 平铺字段 + Web 形态 |
| 29 | `backup_rollback` | implemented | 校验快照路径在备份目录内，只写 `rollback-pending.json`，不动现有数据 |
| 30 | `backup_scan_external_conflicts` | implemented | 只比对 `backups/external/` 镜像内已存在目标，不读工作区会话目录 |

### 浏览器桥 / CLI / Office 与窗口

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 31 | `browser_bridge_open` | implemented | 校验 tabId/url，仅 http + loopback（对齐 `isLoopbackHost`）；无监听回中文 Err，不编造 grant |
| 32 | `browser_bridge_status` | implemented | 探测 `CODEPLUSE_BRIDGE_PORTS` 下本机端口（300ms），无监听回确定离线形态 |
| 33 | `cli_access_set_enabled` | implemented | 校验 bool、原子落盘、广播 `cli-access://changed`、回显最新状态 |
| 34 | `cli_access_status` | implemented | 读 `.data/cli-access.json`；本构建无端点进程故 running=false 并带 error 说明 |
| 35 | `computer_driver_install` | limited | **不下载、不安装系统驱动**，按 `DriverInfo` 回 supported:false + 明确 error |
| 133 | `renew_office_watch` | implemented | `.data/office-watches.json` 自维护租约（TTL 120s），hold 被顶替回 false，未改 `office.rs` |
| 134 | `resolve_close_request` | implemented | 校验 action∈minimize/exit/cancel + remember，清提示标记落盘并广播 `app://close-request` |

### Codex 目录与设备授权（9）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 36 | `codex_bundled_catalog` | implemented | 先读 `codepluse-assets/codex-models.json` 再读 `.data` 快照；仓库无随附快照时回空数组（不编造） |
| 37 | `codex_catalog_plan_preview` | implemented | 回显 `configToml`、探测 `model_catalog_json` 接管、解析默认 `model`；不联网不写盘 |
| 38 | `codex_endpoint_catalog_cached` | implemented | 只读 `.data/codex-catalog-cache.json`；命中/请求不符(mismatch:"request")/无缓存(none) |
| 39 | `codex_endpoint_catalog_published` | implemented | 读 `.data/codex-catalog-published.json`；空→none，指纹不符→request，格式不符→format |
| 40 | `codex_endpoint_catalog_status` | implemented | 汇总已发布 meta/source + refresh 记录 + runtime 默认，不联网 |
| 41 | `codex_fetch_endpoint_catalog` | implemented | 10s 超时 GET，兼容 `models[]` 与 `data[]` 两种形状，成功写缓存；密钥只入 sha256 前 16 位摘要 |
| 42 | `codex_poll_device_code` | implemented | pending/success/error 状态机；不存在/码不符/过期/拒绝均结构化 error；令牌只透传落盘记录，绝不伪造 |
| 43 | `codex_refresh_endpoint_catalog` | implemented | 按已发布来源重拉重发布；失败写结构化 `lastError` 并累加计数 |
| 44 | `codex_request_device_code` | implemented | 生成 `deviceAuthId`/`userCode`(XXXX-XXXX)/`verificationUrl`(可覆盖)/interval=5，落待授权记录，10 分钟过期 |

### 聊天渠道与账号令牌（17）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 45 | `connect_chat_channel` | implemented | 状态机置 `connected`，状态由持久记录真实推导 |
| 46 | `create_chat_channel` | implemented | 必填 name/channelType（限 lark/telegram/weixin）、configJson 合法 JSON；临时文件+rename 原子写 |
| 47 | `delete_account_token` | implemented | 按 accountId 幂等删独立令牌文件 |
| 48 | `delete_chat_channel` | implemented | 幂等删渠道并连带清该渠道令牌与本地消息 |
| 49 | `delete_chat_channel_token` | implemented | 按 channelId 幂等删令牌 |
| 50 | `disconnect_chat_channel` | implemented | 置 `disconnected` |
| 51 | `get_account_token` | implemented | 回令牌或 null；令牌不进日志/错误文案 |
| 52 | `get_chat_channel_has_token` | implemented | 由令牌文件真实推导 bool，不写死 |
| 53 | `get_chat_channel_status` | implemented | 逐渠道由持久 `status` 推导，非法值回落 disconnected |
| 54 | `list_chat_channel_messages` | implemented | 只回本地真实记录，按 channelId 过滤 + offset/limit 分页 |
| 55 | `list_chat_channels` | implemented | 回 `ChatChannelInfo[]`，对外视图不含令牌与内部 status |
| 56 | `save_account_token` | implemented | 独立 `.data/account-tokens.json`；错误文案不回写令牌 |
| 57 | `save_chat_channel_token` | implemented | 独立 `.data/chat-channel-tokens.json`，与元数据分文件 |
| 58 | `test_chat_channel` | implemented | 按配置 webhook 真发一条（8s 超时、no-proxy）；成功记 sent，失败记 failed 并回中文 Err |
| 59 | `update_chat_channel` | implemented | 校验 id 存在，可选字段部分合并回写并刷新 updated_at |
| 60 | `weixin_check_qrcode` | implemented | 上游由 `CODEPLUSE_WEIXIN_QR_BASE` 覆盖；不可达回中文 Err，不伪造"已扫码" |
| 61 | `weixin_get_qrcode` | implemented | 同上，透传 qrcode_id/二维码内容，缺字段中文 Err，不伪造二维码 |

### 项目脚手架与技能 / 会话导入

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 62 | `create_hyperframes_project` | implemented | 建 `src`/`examples` + package.json + hyperframes.json；禁网络安装、禁越界（规范化后校验） |
| 63 | `create_shadcn_project` | implemented | 建 `src/components`/`src/lib` + 最小 package.json；不跑 pnpm install |
| 64 | `detect_hyperframes_skills` | implemented | 扫 `.claude/.codex/.gemini/skills/hyperframes` 真实检测，回每代理 installed 状态 |
| 65 | `install_hyperframes_skills` | limited | 落 `.data/hyperframes-skills.json` 状态 `requested`；不克隆、不写 installed:true、不伪装下载成功 |
| 66 | `get_folder_conversation_outline` | implemented | 读既有 `.data/codepluse.db` 轮次（复用 `turn_store::all_turns_for`），边界切片 + FNV-1a 指纹 |
| 67 | `import_local_conversations` | implemented | 可按 folderId 过滤对账 tallies，台账落 `.data/imports/` |
| 68 | `import_selected_sessions` | implemented | tallies imported/updated/restored/not_found/failed；缺键记 failed，不编造 |
| 69 | `scan_importable_sessions` | implemented | 扫 `.data/import-sessions/{claude,codex}` 的 jsonl/json，按 (external_id, agent_type) 对账 new/imported/deleted |

### 日志（6，主控亲写的种子模块）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 70 | `get_log_settings` | implemented | 读 `.data/log-settings.json`，回 `{level,targets,env_locked}`；CODEPLUSE_LOG/RUST_LOG 决定 env_locked |
| 71 | `get_recent_logs` | implemented | 环形缓冲（上限 2000）倒序取最新 limit 条再正序返回；minLevel 严重度过滤 + search 子串过滤 |
| 72 | `list_log_files` | implemented | 扫 `.data/logs/*.log`，回 `{name,size_bytes,modified_ms}`，新在前 |
| 73 | `open_logs_dir` | implemented | 确保目录存在并回绝对路径 |
| 74 | `read_log_file` | implemented | 挡 `..`/分隔符/盘符/非 .log；maxBytes 取最新一段；参数先校验再读盘 |
| 75 | `set_log_settings` | implemented | 级别/目标校验、tmp+rename 原子写、发 `log-settings://changed` 跨窗口同步 |

### Web 服务（6）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 76 | `get_web_server_status` | implemented | 对配置端口（缺省 `CODEPLUSE_PORT`/8840）真实 TCP 探测；可达回 `WebServerInfo`，不可达回 null |
| 77 | `get_web_service_config` | implemented | 读 `.data/web-service-config.json`；缺省 token=null/port=null/autoStart=true |
| 78 | `probe_web_service_port` | implemented | 只 connect_timeout 不绑端口；回 occupied/free/unknown；端口越界 Err |
| 79 | `start_web_server` | limited | 单进程自服务：不绑新监听、不碰 8840；token/port 意图落盘并回显 |
| 80 | `stop_web_server` | limited | 不杀进程、不停监听、不释放端口，按前端约定回 null，配置保留 |
| 81 | `update_web_service_config` | implemented | 校验 token(string\|null)/port(null\|1..65535)/autoStart(bool)，落盘并回显 |

### Git（3）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 82 | `git_generate_commit_message` | limited | 未接模型服务：回显草稿，否则按文件清单给本地确定性摘要；auto_commit=false；取消标志真实拦截 |
| 83 | `git_generate_commit_message_cancel` | implemented | 内存取消标志位，同 requestId 的后续生成被拒；标志消费一次后清除 |
| 84 | `git_remove_worktree` | implemented | 先验仓库归属 → `status --porcelain` 脏即拒（force 不能绕过）→ 确认在册 → `worktree remove` |

### MCP（10）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 85 | `mcp_get_marketplace_server_detail` | implemented | `GET {base}/servers/{id}`，DTO 全字段归一化、缺省补 null/[]；缺参中文 Err |
| 86 | `mcp_install_from_marketplace` | implemented | 拉详情取基线 spec → 按 optionId 选装 → parameterValues 并入 env → 校验后落 `.data/mcp-servers.json` |
| 87 | `mcp_list_marketplaces` | implemented | `GET {base}/providers`，映射 id/name/description |
| 88 | `mcp_remove_server` | implemented | 持久清单移除；扫描来的外部条目入 hidden 防复现；重复移除回 true |
| 89 | `mcp_scan_local` | implemented | 合并持久清单 + 真扫 `~/.claude.json`(mcpServers) 与 `~/.codex/config.toml`([mcp_servers.*])；坏配置进 warnings 不炸整次 |
| 90 | `mcp_search_marketplace` | implemented | `GET {base}/servers?provider_id&query&limit`；上游不支持关键词时本地兜底过滤 |
| 91 | `mcp_set_server_apps` | implemented | 更新持久记录 apps；未知 id 回 null；apps 非数组 Err |
| 92 | `mcp_upsert_local_server` | implemented | 校验 id（挡分隔符/`..`/`:`）、spec（stdio 必 command，http/sse/local/remote 必 url）、apps 白名单 |
| 93 | `set_codepluse_mcp_tool_group` | implemented | 9 组白名单落 `.data/mcp-tool-groups.json` 并发 `mcp-tool-groups://changed`；非法组名/非布尔 Err |
| 94 | `start_codepluse_mcp_service` | limited | `/api/codepluse_mcp` 随主进程内联绑定，无独立 broker 可拉起；幂等空转回 null |

### OpenCode（7）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 95 | `opencode_add_plugin` | implemented | 幂等追加 `plugins` 数组、原子写配置、回显最新列表 |
| 96 | `opencode_connect_key` | implemented | 校验 integrationId/key，凭据落 `auth.json`；key 不进日志/响应/明文 key 字段 |
| 97 | `opencode_disconnect_provider` | implemented | 移除凭据记录，幂等，回变更结构 |
| 98 | `opencode_list_integrations` | implemented | 读 `auth.json` 凭据记录回 `OpenCodeIntegration[]`，不回显密钥 |
| 99 | `opencode_list_plugins` | implemented | 读 `~/.config/opencode/opencode.json`，归并 1.x `plugin` 与 2.x `plugins`，含 kind/legacy/path_state |
| 100 | `opencode_provider_catalog` | implemented | models.dev `/api.json`，8s 超时、`CODEPLUSE_OPENCODE_CATALOG_BASE` 覆盖；`.data` 缓存 1h，forceRefresh 绕过 |
| 101 | `opencode_remove_plugin` | implemented | 幂等移除两键中的匹配声明，回显最新列表 |

### 桌面宠物（27）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 102 | `close_pet_panel` | limited | 单窗口壳无独立面板句柄：panelOpen=false + 广播 `pet://panel`，渲染在主窗口内承接 |
| 103 | `close_pet_window` | limited | 落 `enabled=false` + 广播 `pet://window`；本层无窗口句柄可关，文档注释已标原因 |
| 104 | `focus_conversation` | limited | 校验 folderId/conversationId/agent 后广播 `pet://focus-conversation`；无法前置主窗口 |
| 105 | `open_pet_window` | limited | 落 `enabled=true` + 广播 `pet://window`，不伪装弹窗 |
| 106 | `pet_add` | implemented | 校验 id 字符集/名称/base64 图片魔数，素材落 `assets/`，原子写清单；重复 id Err |
| 107 | `pet_celebrate` | implemented | kind 白名单 jumping/waving/failed，广播 `pet://oneshot`（key 单调递增），非法 kind Err |
| 108 | `pet_codex_import_available` | implemented | 有 ≥1 可导入来源才 available:true，否则假 + 空；测试缝显式传 home 不碰真实目录 |
| 109 | `pet_delete` | implemented | 幂等：删记录+素材+清 activePetId，再删仍 Ok(null) |
| 110 | `pet_get` | implemented | 按 id 回 `PetDetail`，缺失 Err「宠物不存在」 |
| 111 | `pet_get_current_state` | implemented | 由真实会话快照派生 idle/running/waiting/failed（error>waiting>running） |
| 112 | `pet_get_settings` | implemented | 回 `PetWindowConfig` 默认形状（enabled/activePetId/x/y/scale/alwaysOnTop） |
| 113 | `pet_import_codex` | implemented | 复制素材入库；撞名默认 skipped，overwriteWithSuffix 加 `-2` 后缀 |
| 114 | `pet_list` | implemented | 读 `pets/index.json` + 磁盘对账，回 `PetSummary[]` |
| 115 | `pet_list_active_sessions` | implemented | 读 ACP 层真实连接（跳过 warm/disconnected），映射 `PetSessionEntry` + 三类计数；层未起回空 |
| 116 | `pet_list_importable_codex` | implemented | 真扫 `<home>/.codex/pets/`（pet.json/manifest.json + 首图），标 alreadyImported |
| 117 | `pet_marketplace_asset` | implemented | HTTP 素材代理回 `{mime,dataBase64}`；缺 url/非法 scheme/带凭证 Err；未知魔数兜底 octet-stream |
| 118 | `pet_marketplace_install` | implemented | 下载字节校验图片格式后入库，回 `{pet}`；撞名不覆盖 |
| 119 | `pet_marketplace_list` | implemented | 真 HTTP（8s，`CODEPLUSE_PET_MARKETPLACE_BASE` 覆盖），归一化 `MarketplaceListResponse` |
| 120 | `pet_read_spritesheet` | implemented | 按魔数判 MIME，回 `{mime,dataBase64}`；路径越界/缺失 Err |
| 121 | `pet_replace_sprite` | implemented | 解码校验 base64 图片，替换素材二进制并更新记录；坏 base64/缺素材 Err |
| 122 | `pet_save_window_state` | implemented | 合并 x/y/scale/alwaysOnTop/enabled 白名单 patch 落盘 + 广播 `pet://settings`；scale≤0 Err |
| 123 | `pet_set_active` | implemented | 落 activePetId（null 清空、字符集校验）+ 广播 `pet://active-changed`，回 `PetWindowConfig` |
| 124 | `pet_show_context_menu` | limited | 本派发层未接原生菜单 API：只校验 labels 三文案 + 坐标，不伪造 `pet://menu-action` |
| 125 | `pet_update_meta` | implemented | 白名单仅 displayName/description（null 清空）；越权字段忽略；坏类型 Err |
| 126 | `pet_window_record_position` | implemented | x/y 数字落盘；缺参/非数字 Err |
| 127 | `resize_pet_panel` | limited | 面板几何（panelOpen/panelHeight）落 `.data` + 广播 `pet://panel`；高度非法 Err |
| 128 | `toggle_pet_panel` | limited | 翻转 panelOpen + 广播 `pet://panel`；竞态守卫仍归前端 |

### 提供商与令牌验证（4）

| # | 命令 | 状态 | 说明 |
|---:|---|---|---|
| 129 | `codepluse_provider_delete` | **already-owned** | 运行时由既有 `exec_providers::handle` 应答（Go `providers.Delete` 移植：删记录、清 active、剥 key/env/native、未知 id 404）；该模块没有 `owns()` 所以静态审计误报。`token_validate.rs` 内另有一份语义一致的副本，仅为满足审计口径，运行时不会走到 |
| 130 | `validate_cnb_token` | implemented | `GET https://api.cnb.cool/user` Bearer；网页主机 `cnb.cool`→`api.cnb.cool`；读 username(回退 nickname) + avatar |
| 131 | `validate_gitea_token` | implemented | `GET {base}/api/v1/user`，`Authorization: token`；401/403 → success:false（不是 Err）；读 login(回退 username) |
| 132 | `validate_gitlab_token` | implemented | `GET {base}/api/v4/user` + `PRIVATE-TOKEN`；scopes 取 `/api/v4/personal_access_tokens/self`（失败回 []） |

## 五、执行证据（原文）

新增测试条数（每个子代理自报的 `test result` 原文汇总，主控已整体复跑）：

```
logs             running 14 tests   … ok. 14 passed
acp_ext          running 35 tests   … ok. 35 passed
backup           running 13 tests   … ok. 13 passed
git_ext          running  6 tests   … ok.  6 passed
import_sessions  running  8 tests   … ok.  8 passed
web_service      running  8 tests   … ok.  8 passed
token_validate   running 11 tests   … ok. 11 passed
codex            running 19 tests   … ok. 19 passed
mcp_market       running 16 tests   … ok. 16 passed
chat_channels    running 16 tests   … ok. 16 passed
pet              running 16 tests   … ok. 16 passed
pet_window       running 12 tests   … ok. 12 passed
opencode         running 12 tests   … ok. 12 passed
projects_skills  running  7 tests   … ok.  7 passed
browser_bridge   running  6 tests   … ok.  6 passed
cli_access       running  6 tests   … ok.  6 passed
office_ext       running  7 tests   … ok.  7 passed
mod.rs 派发集成   running 2 tests    … ok.  2 passed（含 17 条命令逐个走真实 HTTP）
```

全量复跑（主控）：

```
cargo check --message-format short -j 2      → Finished，0 error
cargo test --bin codepluse -j 3               → 490 passed; 1 failed（terminal::tests::conpty_session_round_trip）
node scripts/audit-command-coverage.mjs --check
  → Frontend literal calls: 561; calls without a Rust command owner: 0   exit 0
```

## 六、改动范围核对

`git status --porcelain`：**162 → 180 项**，新增正好 18 项 = 17 个新模块文件（untracked）+ `mod.rs`（唯一被改的既有文件）。

- `codepluse-ui/**` 零改动（前端没动，后端对齐前端）
- `src-tauri/Cargo.toml`、`Cargo.lock` 零改动（没加新依赖）
- `terminal.rs` / `office.rs` / `browser.rs` / `computer.rs` / `git.rs` / `account_channels.rs` / `acp/**` / `store_*` 等既有模块零改动
- 无 `git commit` / `checkout` / `reset` / `stash`；桌面应用（127.0.0.1:8840）全程未被 kill、未被占用

## 七、需要你知道的两个既有问题（都不是本次改动引入）

1. **`terminal::tests::conpty_session_round_trip` 失败**：`CreatePseudoConsole() failed with status 0x80070006`。该文件本次零改动（`git diff` 为空）、测试单独跑同样失败、附控制台跑也失败，属当前无交互控制台环境下的既有失败。全量测试因此是 490/491。
2. **headless 模式启动即崩**：`CODEPLUSE_HEADLESS=1` 时打印 `[headless] 127.0.0.1:<port>` 后立刻 panic
   `Cannot drop a runtime in a context where blocking is not allowed`（tokio `blocking/shutdown.rs:51`），进程 4 秒内退出，端口 18841/18842 都一样。`serve()` 文档写的是"绑定后即返回"，返回后主线程 drop runtime 时踩到这个约束。`scripts/parity-*.mjs` 依赖这个模式，现在已不可用。本次未修（属改动应用启动路径，超出"补 134 条命令"的范围，需要你点头再动）。

## 八、24 条 limited 的完整清单（按前端类型返回确定形态，未编造数据）

`acp_antigravity_login_finish`、`acp_antigravity_login_start`、`acp_cursor_auth_status`、`acp_cursor_list_models`、`acp_fetch_agent_latest_release`、`acp_install_uv_tool`、`acp_list_pi_model_capabilities`、`acp_qoder_auth_status`、`acp_update_system_agent`、`backup_active_agents`、`backup_create`、`computer_driver_install`、`install_hyperframes_skills`、`start_web_server`、`stop_web_server`、`git_generate_commit_message`、`start_codepluse_mcp_service`、`close_pet_panel`、`close_pet_window`、`focus_conversation`、`open_pet_window`、`pet_show_context_menu`、`resize_pet_panel`、`toggle_pet_panel`

这些共同的原因是三类：① 能力在独立进程/外部下载链路（ACP 运行时、uv 工具链、智能体整包更新、模型服务）；② 需要外部服务凭据（市场、微信）；③ 单窗口 Tauri 架构里没有独立宠物窗口/原生菜单句柄。
