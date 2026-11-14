// Builds the authoritative command inventory by scanning the Go dispatchers.
//
// The previous regex (`case "x"`) only captured the first label of a
// multi-label case, which undercounted git.go alone by 17 commands. This
// scanner extracts every label from `case "a", "b":` and every string from
// `command == "x"` / `strings.HasPrefix(command, "x")` guards, so the total
// reflects what the frontend can actually call.
//
//   node scripts/inventory-commands.mjs [--write]
//
// Output: docs/rust-backend-command-inventory.tsv (command, file, line, ported)

import fs from "node:fs";
import path from "node:path";

const ROOT = path.resolve(import.meta.dirname, "..");
const GO = path.join(ROOT, "server-go", "internal");
const OUT = path.join(ROOT, "docs", "rust-backend-command-inventory.tsv");

/** Commands already answered by Rust (src-tauri/src/backend). */
const RUST = new Set([
  // Regenerated from the dispatch surface in src-tauri/src/backend.
  "acp_add_registry_agent",
  "acp_answer_question",
  "acp_cancel",
  "acp_clear_binary_cache",
  "acp_connect",
  "acp_current_platform",
  "acp_delete_custom_agent",
  "acp_describe_agent_options",
  "acp_detect_agent_local_version",
  "acp_disconnect",
  "acp_download_agent_binary",
  "acp_fetch_registry_catalog",
  "acp_find_connection_for_conversation",
  "acp_fork",
  "acp_get_agent_status",
  "acp_get_session_snapshot",
  "acp_get_session_snapshot_by_conversation",
  "acp_goal_control",
  "acp_list_agent_runtime_skills",
  "acp_list_agent_skills",
  "acp_list_agents",
  "acp_list_connections",
  "acp_list_custom_agents",
  "acp_preflight",
  "acp_prepare_npx_agent",
  "acp_prompt",
  "acp_respond_permission",
  "acp_respond_task_split",
  "acp_save_custom_agent",
  "acp_set_config_option",
  "acp_set_mode",
  "acp_stop_async_task",
  "acp_touch_connection",
  "acp_uninstall_agent",
  "acp_update_agent_config",
  "acp_update_agent_env",
  "app_update_state",
  "app_update_status",
  "apply_sidebar_layout",
  "assistant_create",
  "assistant_delete",
  "assistant_list",
  "assistant_reorder",
  "assistant_update",
  "automation_cancel_run",
  "automation_compute_next_run",
  "automation_create",
  "automation_delete",
  "automation_get",
  "automation_list",
  "automation_mark_seen",
  "automation_run_now",
  "automation_runs",
  "automation_set_enabled",
  "automation_update",
  "bootstrap_folder_commands_from_package_json",
  "browser_agent_act",
  "browser_agent_capture",
  "browser_agent_console",
  "browser_agent_grant",
  "browser_agent_snapshot",
  "browser_answer_open_request",
  "browser_capabilities",
  "browser_clear_data",
  "browser_close_tab",
  "browser_doc_open",
  "browser_doc_set_mode",
  "browser_doc_state",
  "browser_eval_decide",
  "browser_find",
  "browser_freeze_frame",
  "browser_go_back",
  "browser_list_tabs",
  "browser_navigate",
  "browser_open_devtools",
  "browser_open_tab",
  "browser_page_capture",
  "browser_pick_cancel",
  "browser_pick_element",
  "browser_reload",
  "browser_set_bounds",
  "browser_set_host_rules",
  "browser_stop",
  "browser_tabs",
  "canvas_create_board",
  "canvas_create_node",
  "canvas_delete_board",
  "canvas_delete_node",
  "canvas_delete_nodes",
  "canvas_detach_member",
  "canvas_group_into_region",
  "canvas_list_boards",
  "canvas_list_nodes",
  "canvas_move_nodes",
  "canvas_update_board",
  "canvas_update_node",
  "ccswitch_provider_config",
  "check_app_update",
  "codepluse_connection_catalog",
  "codepluse_connection_save",
  "codepluse_instance_create",
  "codepluse_instance_delete",
  "codepluse_instance_list",
  "codepluse_instance_update",
  "codepluse_provider_activate",
  "codepluse_provider_delete",
  "codepluse_provider_models",
  "codepluse_provider_save",
  "codepluse_providers_list",
  "computer_available",
  "computer_driver_info",
  "computer_driver_uninstall",
  "computer_indicator_fit",
  "computer_list_shareable_windows",
  "computer_open_permission_settings",
  "computer_request_permission",
  "computer_reveal_helper",
  "computer_revoke_all",
  "computer_share_app",
  "computer_share_screen",
  "computer_share_window",
  "computer_share_windows",
  "computer_shared_state",
  "computer_status",
  "computer_stop",
  "computer_stop_key_status",
  "computer_window_thumbnail",
  "create_chat_conversation",
  "create_chat_dir",
  "create_conversation",
  "create_conversation_tag",
  "create_folder_command",
  "create_folder_group",
  "create_model_provider",
  "delete_commit_message_settings",
  "delete_conversation",
  "delete_conversation_tag",
  "delete_folder_command",
  "delete_folder_group",
  "delete_model_provider",
  "experts_apply_links",
  "experts_get_install_status",
  "experts_link_to_agent",
  "experts_list",
  "experts_list_all_install_statuses",
  "experts_read_content",
  "experts_unlink_from_agent",
  "folder_forge_remote",
  "forge_auto_scan_now",
  "forge_auto_status",
  "forge_change_detail",
  "forge_change_files",
  "forge_create_comment",
  "forge_create_issue",
  "forge_identity",
  "forge_issue_detail",
  "forge_list_comments",
  "forge_list_issues",
  "forge_list_labels",
  "forge_merge_change",
  "forge_merge_options",
  "forge_set_item_state",
  "forge_settings_get",
  "forge_settings_set",
  "forge_tab_count",
  "get_available_terminal_shells",
  "get_codeg_mcp_service_status",
  "get_commit_message_settings",
  "get_computer_tools_settings",
  "get_conversation",
  "get_conversation_branch_tag",
  "get_file_tree",
  "get_folder",
  "get_folder_conversation",
  "get_folder_conversation_turns",
  "get_git_branch",
  "get_git_head",
  "get_github_accounts",
  "get_home_directory",
  "get_sidebar_data",
  "get_stats",
  "get_system_settings",
  "get_workspace_snapshot",
  "git_abort_operation",
  "git_add_files",
  "git_add_remote",
  "git_checkout",
  "git_commit",
  "git_commit_branches",
  "git_commit_files",
  "git_conflict_file_versions",
  "git_continue_operation",
  "git_current_user",
  "git_delete_branch",
  "git_diff",
  "git_diff_with_branch",
  "git_fetch",
  "git_fetch_remote",
  "git_has_merge_head",
  "git_init",
  "git_is_tracked",
  "git_list_all_branches",
  "git_list_branches",
  "git_list_conflicts",
  "git_list_remotes",
  "git_log",
  "git_merge",
  "git_new_branch",
  "git_pull",
  "git_push",
  "git_push_info",
  "git_rebase",
  "git_remove_remote",
  "git_reset",
  "git_resolve_conflict",
  "git_rollback_file",
  "git_search_authors",
  "git_set_remote_url",
  "git_show_diff",
  "git_show_file",
  "git_show_file_base64",
  "git_stash_apply",
  "git_stash_clear",
  "git_stash_drop",
  "git_stash_list",
  "git_stash_pop",
  "git_stash_push",
  "git_stash_show",
  "git_status",
  "git_worktree_add",
  "list_all_conversations",
  "list_all_folder_details",
  "list_child_conversations",
  "list_conversation_tags",
  "list_conversations",
  "list_directory_entries",
  "list_directory_with_files",
  "list_folder_commands",
  "list_folder_groups",
  "list_folders",
  "list_model_providers",
  "list_open_folder_details",
  "list_opened_tabs",
  "list_provider_models",
  "list_remote_connections",
  "list_workspace_files",
  "load_folder_history",
  "load_system_settings",
  "officecli_detect",
  "officecli_install",
  "officecli_list_skills",
  "officecli_render_html",
  "officecli_skill_apply_links",
  "officecli_skill_get_install_status",
  "officecli_skill_link_to_agent",
  "officecli_skill_list_all_install_statuses",
  "officecli_skill_read_content",
  "officecli_skill_unlink_from_agent",
  "officecli_sync_skills",
  "officecli_uninstall",
  "open_commit_window",
  "open_folder",
  "open_folder_by_id",
  "open_folder_in_workspace",
  "open_merge_window",
  "open_push_window",
  "open_stash_window",
  "perform_app_update",
  "pick_directory",
  "probe_terminal_shell_path",
  "quick_messages_create",
  "quick_messages_delete",
  "quick_messages_list",
  "quick_messages_reorder",
  "quick_messages_update",
  "read_file_base64",
  "read_file_for_edit",
  "read_file_preview",
  "read_workspace_file_base64",
  "remove_folder_from_workspace",
  "reorder_conversation_tags",
  "reorder_folder_commands",
  "restart_app",
  "rollback_app",
  "save_file_content",
  "save_file_copy",
  "save_opened_tabs",
  "save_system_settings",
  "science_apply_links",
  "science_get_install_status",
  "science_link_to_agent",
  "science_list",
  "science_list_all_install_statuses",
  "science_read_content",
  "science_unlink_from_agent",
  "set_commit_message_settings",
  "set_computer_tools_enabled",
  "set_computer_tools_preferences",
  "set_folder_group",
  "start_office_watch",
  "start_workspace_state_stream",
  "stop_office_watch",
  "stop_workspace_state_stream",
  "submit_session_feedback",
  "terminal_kill",
  "terminal_list",
  "terminal_resize",
  "terminal_snapshot",
  "terminal_spawn",
  "terminal_write",
  "token_usage_facets",
  "token_usage_report",
  "token_usage_status",
  "token_usage_sync",
  "update_conversation_branch_tag",
  "update_conversation_pinned",
  "update_conversation_status",
  "update_conversation_tag",
  "update_conversation_tags",
  "update_conversation_title",
  "update_folder_alias",
  "update_folder_color",
  "update_folder_command",
  "update_folder_default_agent",
  "update_folder_group",
  "update_github_accounts",
  "update_main_bot_settings",
  "update_model_provider",
  "update_system_settings",
  "validate_github_token",
  "work_task_archive",
  "work_task_cancel",
  "work_task_changed_files",
  "work_task_cleanup",
  "work_task_complete",
  "work_task_create",
  "work_task_create_from_forge",
  "work_task_defer",
  "work_task_delete",
  "work_task_deliver_pr",
  "work_task_diff",
  "work_task_engine_cancel_conn",
  "work_task_engine_claim",
  "work_task_engine_discard_worktree",
  "work_task_engine_merge",
  "work_task_events",
  "work_task_get",
  "work_task_list",
  "work_task_lookup_by_source",
  "work_task_merge",
  "work_task_merge_unqueue",
  "work_task_reorder",
  "work_task_requeue",
  "work_task_retry",
  "work_task_return",
  "work_task_schedule",
  "work_task_settings_delete",
  "work_task_settings_effective",
  "work_task_settings_get",
  "work_task_settings_get_own",
  "work_task_settings_set",
  "work_task_start",
  "work_task_template_delete",
  "work_task_template_list",
  "work_task_template_save",
  "work_task_update",
]);

/** Command ids are lowercase snake_case; this filters out error strings. */
const COMMAND = /^[a-z][a-z0-9_]*$/;

const CASE_LABELS = /\bcase((?:\s*"[^"]*"\s*,?)+)/g;
const GUARD = /command\s*==\s*"([^"]+)"/g;
const PREFIX = /strings\.HasPrefix\(\s*command\s*,\s*"([^"]+)"\s*\)/g;

function walk(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(full));
    else if (entry.name.endsWith(".go") && !entry.name.endsWith("_test.go")) out.push(full);
  }
  return out;
}

const rows = new Map(); // command -> {file, line}

/** Depth-aware brace scan so `case` labels are only collected from switches
 *  whose subject is the dispatch variable (`switch command {`, `switch op {`
 *  derived from command, ...), not from data switches like
 *  `switch block["type"] {`. */
function scanFile(rel, source) {
  const lines = source.split("\n");
  const stack = []; // { depth, isCommand }
  let depth = 0;
  let pendingSwitch = null;
  // 多行 case 的续行收集：`case "a", "b",` 没在本行以冒号收尾时，后续行
  // （直到以 ":" 结尾）全是标签。browser.go 的 13 连 case 就长这样。
  let inCaseContinuation = false;

  lines.forEach((line, index) => {
    const push = (name) => {
      if (!COMMAND.test(name)) return;
      if (!rows.has(name)) rows.set(name, { file: rel, line: index + 1 });
    };

    const trimmed = line.trim();
    if (pendingSwitch !== null) {
      // Decide whether this switch dispatches on the command name. `switch op`
      // where `op` came from the command is covered by the explicit op list.
      const subject = pendingSwitch;
      const isCommand = subject !== "";
      stack.push({ depth, isCommand });
      pendingSwitch = null;
    }

    const lineDepth = stack.length ? stack[stack.length - 1].depth + 1 : 0;
    const active = !stack.length || stack[stack.length - 1].isCommand;
    if (active) {
      if (inCaseContinuation) {
        for (const label of trimmed.matchAll(/"([^"]+)"/g)) push(label[1]);
        if (trimmed.endsWith(":")) inCaseContinuation = false;
      } else {
        for (const match of line.matchAll(CASE_LABELS)) {
          for (const label of match[1].matchAll(/"([^"]+)"/g)) push(label[1]);
        }
        if (/^case\s/.test(trimmed) && !trimmed.endsWith(":")) {
          inCaseContinuation = true;
        }
      }
    }
    void lineDepth;

    for (const match of line.matchAll(GUARD)) push(match[1]);
    for (const match of line.matchAll(PREFIX)) push(match[1] + "*");

    // Track braces (Go sources here are gofmt-clean; string literals with
    // braces are escaped JSON payloads and are rare on switch lines).
    const opens = (line.match(/{/g) || []).length;
    const closes = (line.match(/}/g) || []).length;
    for (let i = 0; i < opens; i += 1) {
      if (trimmed.startsWith("switch ") && i === opens - 1) {
        const header = trimmed.slice(0, trimmed.lastIndexOf("{"));
        pendingSwitch = /command|strings\.TrimPrefix/.test(header) ? header : "";
      } else {
        if (stack.length) stack[stack.length - 1].depth += 1;
      }
    }
    for (let i = 0; i < closes; i += 1) {
      if (stack.length) {
        if (stack[stack.length - 1].depth === 0) stack.pop();
        else stack[stack.length - 1].depth -= 1;
      }
    }
  });
}

for (const file of walk(GO)) {
  const rel = path.relative(path.join(ROOT, "server-go"), file).replace(/\\/g, "/");
  scanFile(rel, fs.readFileSync(file, "utf8"));
}

const commands = [...rows.entries()].sort((a, b) => a[0].localeCompare(b[0]));
if (process.argv.includes("--write")) {
  const tsv = ["command\tfile\tline\tported"]
    .concat(
      commands.map(
        ([name, meta]) =>
          `${name}\t${meta.file}\t${meta.line}\t${RUST.has(name) ? "rust" : ""}`,
      ),
    )
    .join("\n");
  fs.writeFileSync(OUT, tsv + "\n");
}

const ported = commands.filter(([name]) => RUST.has(name)).length;
console.log(
  `commands: ${commands.length}  ported: ${ported}  remaining: ${commands.length - ported}`,
);
if (process.argv.includes("--write")) console.log(`wrote ${OUT}`);

// Anything listed as ported but not found in Go means the Go side was already
// deleted (or the name is stale) - both worth surfacing.
const missing = [...RUST].filter((name) => !rows.has(name) && !name.endsWith("*"));
if (missing.length) console.log(`ported-but-absent-in-go: ${missing.join(", ")}`);
