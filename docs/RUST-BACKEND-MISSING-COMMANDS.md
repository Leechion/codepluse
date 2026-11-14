# 前端调用但未找到 Rust 命令所有者

- 审计日期：2026-11-15
- 审计结果：前端字面量调用：561；未找到 Rust 命令所有者：134
- 审计脚本：`scripts/audit-command-coverage.mjs`

## 判定口径

脚本扫描 `codeg-ui/src` 中的字面量 `.call("command")`，再检查 Rust 后端各模块 `owns(command)` 的声明。下列条目表示当前静态审计没有在这些声明中找到命令所有者；这不等同于逐项运行并确认接口报错。动态分发、前缀路由或其他显式分支可能造成误报，后续移植前需要逐项核实。

## ACP 与智能体（19）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 1 | `acp_answer_plan_approval` | `codeg-ui\src\lib\api.ts` |
| 2 | `acp_antigravity_login_cancel` | `codeg-ui\src\lib\api.ts` |
| 3 | `acp_antigravity_login_finish` | `codeg-ui\src\lib\api.ts` |
| 4 | `acp_antigravity_login_start` | `codeg-ui\src\lib\api.ts` |
| 5 | `acp_antigravity_sign_out` | `codeg-ui\src\lib\api.ts` |
| 6 | `acp_cursor_auth_status` | `codeg-ui\src\lib\api.ts` |
| 7 | `acp_cursor_list_models` | `codeg-ui\src\lib\api.ts` |
| 8 | `acp_fetch_agent_latest_release` | `codeg-ui\src\lib\api.ts` |
| 9 | `acp_install_uv_tool` | `codeg-ui\src\lib\api.ts` |
| 10 | `acp_list_pi_model_capabilities` | `codeg-ui\src\lib\api.ts` |
| 11 | `acp_load_deepseek_model_catalog` | `codeg-ui\src\lib\api.ts` |
| 12 | `acp_qoder_auth_status` | `codeg-ui\src\lib\api.ts` |
| 13 | `acp_reclaim_leaked_temp` | `codeg-ui\src\lib\api.ts` |
| 14 | `acp_scan_leaked_temp` | `codeg-ui\src\lib\api.ts` |
| 15 | `acp_sync_antigravity_settings` | `codeg-ui\src\lib\api.ts` |
| 16 | `acp_update_claude_model_picker` | `codeg-ui\src\lib\api.ts` |
| 17 | `acp_update_deepseek_model_catalog` | `codeg-ui\src\lib\api.ts` |
| 18 | `acp_update_kimi_code_config` | `codeg-ui\src\lib\api.ts` |
| 19 | `acp_update_system_agent` | `codeg-ui\src\lib\api.ts` |

## 备份与恢复（11）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 20 | `backup_active_agents` | `codeg-ui\src\lib\api.ts` |
| 21 | `backup_cancel` | `codeg-ui\src\lib\api.ts` |
| 22 | `backup_create` | `codeg-ui\src\lib\api.ts` |
| 23 | `backup_create_ticket` | `codeg-ui\src\lib\api.ts` |
| 24 | `backup_discard_pending` | `codeg-ui\src\lib\api.ts` |
| 25 | `backup_list_safety_snapshots` | `codeg-ui\src\lib\api.ts` |
| 26 | `backup_prepare_source` | `codeg-ui\src\lib\api.ts` |
| 27 | `backup_release_source` | `codeg-ui\src\lib\api.ts` |
| 28 | `backup_restore_stage` | `codeg-ui\src\lib\api.ts` |
| 29 | `backup_rollback` | `codeg-ui\src\lib\api.ts` |
| 30 | `backup_scan_external_conflicts` | `codeg-ui\src\lib\api.ts` |

## 浏览器（2）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 31 | `browser_bridge_open` | `codeg-ui\src\lib\browser\browser-bridge.ts` |
| 32 | `browser_bridge_status` | `codeg-ui\src\lib\browser\browser-bridge.ts` |

## CLI 与电脑操作（3）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 33 | `cli_access_set_enabled` | `codeg-ui\src\lib\api.ts` |
| 34 | `cli_access_status` | `codeg-ui\src\lib\api.ts` |
| 35 | `computer_driver_install` | `codeg-ui\src\lib\computer\computer-api.ts` |

## Codex 目录与设备授权（9）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 36 | `codex_bundled_catalog` | `codeg-ui\src\lib\api.ts` |
| 37 | `codex_catalog_plan_preview` | `codeg-ui\src\lib\api.ts` |
| 38 | `codex_endpoint_catalog_cached` | `codeg-ui\src\lib\api.ts` |
| 39 | `codex_endpoint_catalog_published` | `codeg-ui\src\lib\api.ts` |
| 40 | `codex_endpoint_catalog_status` | `codeg-ui\src\lib\api.ts` |
| 41 | `codex_fetch_endpoint_catalog` | `codeg-ui\src\lib\api.ts` |
| 42 | `codex_poll_device_code` | `codeg-ui\src\lib\api.ts` |
| 43 | `codex_refresh_endpoint_catalog` | `codeg-ui\src\lib\api.ts` |
| 44 | `codex_request_device_code` | `codeg-ui\src\lib\api.ts` |

## 聊天渠道与账号令牌（17）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 45 | `connect_chat_channel` | `codeg-ui\src\lib\api.ts` |
| 46 | `create_chat_channel` | `codeg-ui\src\lib\api.ts` |
| 47 | `delete_account_token` | `codeg-ui\src\lib\api.ts` |
| 48 | `delete_chat_channel` | `codeg-ui\src\lib\api.ts` |
| 49 | `delete_chat_channel_token` | `codeg-ui\src\lib\api.ts` |
| 50 | `disconnect_chat_channel` | `codeg-ui\src\lib\api.ts` |
| 51 | `get_account_token` | `codeg-ui\src\lib\api.ts` |
| 52 | `get_chat_channel_has_token` | `codeg-ui\src\lib\api.ts` |
| 53 | `get_chat_channel_status` | `codeg-ui\src\lib\api.ts` |
| 54 | `list_chat_channel_messages` | `codeg-ui\src\lib\api.ts` |
| 55 | `list_chat_channels` | `codeg-ui\src\lib\api.ts` |
| 56 | `save_account_token` | `codeg-ui\src\lib\api.ts` |
| 57 | `save_chat_channel_token` | `codeg-ui\src\lib\api.ts` |
| 58 | `test_chat_channel` | `codeg-ui\src\lib\api.ts` |
| 59 | `update_chat_channel` | `codeg-ui\src\lib\api.ts` |
| 60 | `weixin_check_qrcode` | `codeg-ui\src\lib\api.ts` |
| 61 | `weixin_get_qrcode` | `codeg-ui\src\lib\api.ts` |

## 项目与技能（4）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 62 | `create_hyperframes_project` | `codeg-ui\src\lib\api.ts` |
| 63 | `create_shadcn_project` | `codeg-ui\src\lib\api.ts` |
| 64 | `detect_hyperframes_skills` | `codeg-ui\src\lib\api.ts` |
| 65 | `install_hyperframes_skills` | `codeg-ui\src\lib\api.ts` |

## 文件夹与会话导入（4）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 66 | `get_folder_conversation_outline` | `codeg-ui\src\lib\api.ts` |
| 67 | `import_local_conversations` | `codeg-ui\src\lib\api.ts` |
| 68 | `import_selected_sessions` | `codeg-ui\src\lib\api.ts` |
| 69 | `scan_importable_sessions` | `codeg-ui\src\lib\api.ts` |

## 日志（6）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 70 | `get_log_settings` | `codeg-ui\src\lib\api.ts` |
| 71 | `get_recent_logs` | `codeg-ui\src\lib\api.ts` |
| 72 | `list_log_files` | `codeg-ui\src\lib\api.ts` |
| 73 | `open_logs_dir` | `codeg-ui\src\lib\api.ts` |
| 74 | `read_log_file` | `codeg-ui\src\lib\api.ts` |
| 75 | `set_log_settings` | `codeg-ui\src\lib\api.ts` |

## Web 服务（6）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 76 | `get_web_server_status` | `codeg-ui\src\lib\api.ts` |
| 77 | `get_web_service_config` | `codeg-ui\src\lib\api.ts` |
| 78 | `probe_web_service_port` | `codeg-ui\src\lib\api.ts` |
| 79 | `start_web_server` | `codeg-ui\src\lib\api.ts` |
| 80 | `stop_web_server` | `codeg-ui\src\lib\api.ts` |
| 81 | `update_web_service_config` | `codeg-ui\src\lib\api.ts` |

## Git（3）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 82 | `git_generate_commit_message` | `codeg-ui\src\lib\api.ts` |
| 83 | `git_generate_commit_message_cancel` | `codeg-ui\src\lib\api.ts` |
| 84 | `git_remove_worktree` | `codeg-ui\src\lib\api.ts` |

## MCP（10）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 85 | `mcp_get_marketplace_server_detail` | `codeg-ui\src\lib\api.ts` |
| 86 | `mcp_install_from_marketplace` | `codeg-ui\src\lib\api.ts` |
| 87 | `mcp_list_marketplaces` | `codeg-ui\src\lib\api.ts` |
| 88 | `mcp_remove_server` | `codeg-ui\src\lib\api.ts` |
| 89 | `mcp_scan_local` | `codeg-ui\src\lib\api.ts` |
| 90 | `mcp_search_marketplace` | `codeg-ui\src\lib\api.ts` |
| 91 | `mcp_set_server_apps` | `codeg-ui\src\lib\api.ts` |
| 92 | `mcp_upsert_local_server` | `codeg-ui\src\lib\api.ts` |
| 93 | `set_codeg_mcp_tool_group` | `codeg-ui\src\lib\api.ts` |
| 94 | `start_codeg_mcp_service` | `codeg-ui\src\lib\api.ts` |

## OpenCode（7）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 95 | `opencode_add_plugin` | `codeg-ui\src\lib\api.ts` |
| 96 | `opencode_connect_key` | `codeg-ui\src\lib\api.ts` |
| 97 | `opencode_disconnect_provider` | `codeg-ui\src\lib\api.ts` |
| 98 | `opencode_list_integrations` | `codeg-ui\src\lib\api.ts` |
| 99 | `opencode_list_plugins` | `codeg-ui\src\lib\api.ts` |
| 100 | `opencode_provider_catalog` | `codeg-ui\src\lib\api.ts` |
| 101 | `opencode_remove_plugin` | `codeg-ui\src\lib\api.ts` |

## 桌面宠物（27）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 102 | `close_pet_panel` | `codeg-ui\src\lib\pet\api.ts` |
| 103 | `close_pet_window` | `codeg-ui\src\lib\pet\api.ts` |
| 104 | `focus_conversation` | `codeg-ui\src\lib\pet\api.ts` |
| 105 | `open_pet_window` | `codeg-ui\src\lib\pet\api.ts` |
| 106 | `pet_add` | `codeg-ui\src\lib\pet\api.ts` |
| 107 | `pet_celebrate` | `codeg-ui\src\lib\pet\api.ts` |
| 108 | `pet_codex_import_available` | `codeg-ui\src\lib\pet\api.ts` |
| 109 | `pet_delete` | `codeg-ui\src\lib\pet\api.ts` |
| 110 | `pet_get` | `codeg-ui\src\lib\pet\api.ts` |
| 111 | `pet_get_current_state` | `codeg-ui\src\lib\pet\api.ts` |
| 112 | `pet_get_settings` | `codeg-ui\src\lib\pet\api.ts` |
| 113 | `pet_import_codex` | `codeg-ui\src\lib\pet\api.ts` |
| 114 | `pet_list` | `codeg-ui\src\lib\pet\api.ts` |
| 115 | `pet_list_active_sessions` | `codeg-ui\src\lib\pet\api.ts` |
| 116 | `pet_list_importable_codex` | `codeg-ui\src\lib\pet\api.ts` |
| 117 | `pet_marketplace_asset` | `codeg-ui\src\lib\pet\api.ts` |
| 118 | `pet_marketplace_install` | `codeg-ui\src\lib\pet\api.ts` |
| 119 | `pet_marketplace_list` | `codeg-ui\src\lib\pet\api.ts` |
| 120 | `pet_read_spritesheet` | `codeg-ui\src\lib\pet\api.ts` |
| 121 | `pet_replace_sprite` | `codeg-ui\src\lib\pet\api.ts` |
| 122 | `pet_save_window_state` | `codeg-ui\src\lib\pet\api.ts` |
| 123 | `pet_set_active` | `codeg-ui\src\lib\pet\api.ts` |
| 124 | `pet_show_context_menu` | `codeg-ui\src\lib\pet\api.ts` |
| 125 | `pet_update_meta` | `codeg-ui\src\lib\pet\api.ts` |
| 126 | `pet_window_record_position` | `codeg-ui\src\lib\pet\api.ts` |
| 127 | `resize_pet_panel` | `codeg-ui\src\lib\pet\api.ts` |
| 128 | `toggle_pet_panel` | `codeg-ui\src\lib\pet\api.ts` |

## 提供商与令牌验证（4）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 129 | `codepluse_provider_delete` | `codeg-ui\src\components\settings\agent-connections.tsx` |
| 130 | `validate_cnb_token` | `codeg-ui\src\lib\api.ts` |
| 131 | `validate_gitea_token` | `codeg-ui\src\lib\api.ts` |
| 132 | `validate_gitlab_token` | `codeg-ui\src\lib\api.ts` |

## Office 与窗口请求（2）

| # | 命令 | 前端调用位置 |
|---:|---|---|
| 133 | `renew_office_watch` | `codeg-ui\src\lib\api.ts` |
| 134 | `resolve_close_request` | `codeg-ui\src\lib\api.ts` |

总计：134 项。
