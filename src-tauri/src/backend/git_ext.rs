//! Git 提交消息生成与工作树移除 —— 补全批次（3 条）。
//!
//! 前端（`codepluse-ui/src/lib/api.ts`）已经在调用这些命令，但原 Go sidecar 与
//! Rust 都没有实现过它们，契约只由调用点参数名与 `types.ts` 返回结构决定：
//!
//! * `git_generate_commit_message` `{ requestId, path, folderId, files, draft, fallbackAgent }`
//!   → `GeneratedCommitMessage { message, agent_type, auto_commit }`
//! * `git_generate_commit_message_cancel` `{ requestId }` → `null`
//! * `git_remove_worktree` `{ path, branchName, sourceFolderId, deleteBranch, force }`
//!   → `GitWorktreeRemoval { worktree_path, branch_deleted, folder_id, reparented }`
//!
//! ## 能力分级
//!
//! * L3（limited）：提交消息语义生成依赖模型服务，本后端未接入。这里不伪装成
//!   AI 写作：有用户草稿就回显草稿，否则按变更文件清单给一条本地确定性摘要；
//!   `auto_commit` 恒为 false。取消标志位是真实的内存集合，`cancel` 之后同
//!   `requestId` 的生成调用会被拒绝。
//! * L1：工作树移除复用既有 `git.rs` 的子进程封装。**红线**：只允许移除
//!   仓库登记在册、且 `git status` 干净的工作树；有未提交改动一律先查状态再
//!   拒绝，`force` 也不能绕过这条干净性检查。分支删除走安全的 `branch -d`，
//!   未合并分支不会被强删。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::backend::git;

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "git_generate_commit_message"
            | "git_generate_commit_message_cancel"
            | "git_remove_worktree"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "git_generate_commit_message" => generate_message(root, args),
        "git_generate_commit_message_cancel" => cancel_generation(args),
        "git_remove_worktree" => remove_worktree(root, args),
        _ => Err(format!("未知的 Git 扩展命令：{command}")),
    }
}

// ── 提交消息生成 ──────────────────────────────────────────────────────────

fn cancelled_requests() -> &'static Mutex<HashSet<String>> {
    static CANCELLED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CANCELLED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn take_cancelled(request_id: &str) -> bool {
    let Ok(mut guard) = cancelled_requests().lock() else {
        return false;
    };
    guard.remove(request_id)
}

#[cfg(test)]
fn is_cancelled(request_id: &str) -> bool {
    let Ok(guard) = cancelled_requests().lock() else {
        return false;
    };
    guard.contains(request_id)
}

fn arg_text(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// L3：未接入模型服务。回显草稿，或按文件清单生成一条确定性本地摘要；不做
/// 语义改写，也不自动提交。
fn generate_message(root: &Path, args: &Value) -> Result<Value, String> {
    let _ = root;
    let request_id = arg_text(args, "requestId");
    if request_id.is_empty() {
        return Err("缺少 requestId 参数".into());
    }
    let path = arg_text(args, "path");
    if path.is_empty() {
        return Err("缺少 path 参数".into());
    }
    // 取消标志位真实生效：被取消的请求直接拒绝，不再进入生成流程。
    if take_cancelled(&request_id) {
        return Err("提交消息生成已取消".into());
    }

    let files = match args.get("files") {
        Some(Value::Array(rows)) => rows
            .iter()
            .filter_map(|row| row.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string))
            .collect::<Vec<_>>(),
        Some(_) => return Err("files 必须是数组".into()),
        None => Vec::new(),
    };

    let draft = args.get("draft").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let message = if !draft.is_empty() {
        draft
    } else if files.is_empty() {
        "更新工作区".to_string()
    } else {
        let shown = files.iter().take(5).cloned().collect::<Vec<_>>();
        let suffix = if files.len() > shown.len() {
            format!(" 等 {} 个文件", files.len())
        } else {
            String::new()
        };
        format!("更新 {} 个文件：{}{}", files.len(), shown.join("、"), suffix)
    };

    let agent_type = args
        .get("fallbackAgent")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("local")
        .to_string();

    Ok(json!({
        "message": message,
        "agent_type": agent_type,
        "auto_commit": false,
    }))
}

/// 登记取消标志；已结束的请求再来一次也是无害空操作。
fn cancel_generation(args: &Value) -> Result<Value, String> {
    let request_id = arg_text(args, "requestId");
    if request_id.is_empty() {
        return Err("缺少 requestId 参数".into());
    }
    if let Ok(mut guard) = cancelled_requests().lock() {
        guard.insert(request_id);
    }
    Ok(Value::Null)
}

// ── 工作树移除 ────────────────────────────────────────────────────────────

/// 读取工作树状态：有任何未提交/未跟踪条目即视为脏。
fn worktree_is_clean(path: &Path) -> Result<bool, String> {
    let porcelain = git::git_string(path, &["status", "--porcelain"])?;
    Ok(porcelain.trim().is_empty())
}

fn worktree_is_registered(path: &Path) -> Result<bool, String> {
    let list = git::git_string(path, &["worktree", "list", "--porcelain"])?;
    let target = fs_canonical(path);
    Ok(list.lines().any(|line| {
        line.strip_prefix("worktree ")
            .map(PathBuf::from)
            .map(|p| fs_canonical(&p) == target)
            .unwrap_or(false)
    }))
}

fn fs_canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn remove_worktree(root: &Path, args: &Value) -> Result<Value, String> {
    let raw_path = arg_text(args, "path");
    if raw_path.is_empty() {
        return Err("缺少 path 参数".into());
    }
    let branch_name = arg_text(args, "branchName");
    if branch_name.is_empty() {
        return Err("缺少 branchName 参数".into());
    }
    let delete_branch = args
        .get("deleteBranch")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // force 仅记录：干净性检查是红线，不能被 force 绕过。
    let _force = args.get("force").and_then(Value::as_bool).unwrap_or(false);

    let target = git::resolve_existing_path(&raw_path, root);

    // 先确认这是一个有效仓库内的工作树。
    let toplevel = git::git_string(&target, &["rev-parse", "--show-toplevel"])
        .map_err(|_| "不是有效的 Git 工作树".to_string())?;
    let toplevel = PathBuf::from(toplevel.trim());
    if !target.starts_with(&toplevel) && fs_canonical(&target) != fs_canonical(&toplevel) {
        return Err("目标路径不在仓库工作树之内".into());
    }

    // 红线：先查状态，有未提交改动就拒绝，绝不为了 force 而丢弃工作。
    if !worktree_is_clean(&target)? {
        return Err("工作树存在未提交改动，拒绝移除".into());
    }
    if !worktree_is_registered(&target)? {
        return Err("该路径不在仓库的工作树列表中".into());
    }

    git::git_run(&target, &["worktree", "remove", "--", &target.to_string_lossy()])
        .map_err(|e| format!("移除工作树失败：{e}"))?;

    let mut branch_deleted = false;
    if delete_branch {
        // 安全删除：未合并分支保留，由调用方重试。
        if git::git_run(&toplevel, &["branch", "-d", &branch_name]).is_ok() {
            branch_deleted = true;
        }
    }

    Ok(json!({
        "worktree_path": target.to_string_lossy(),
        "branch_deleted": branch_deleted,
        "folder_id": Value::Null,
        "reparented": 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-git-ext-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    #[test]
    fn owns_covers_the_three_documented_commands_only() {
        for command in [
            "git_generate_commit_message",
            "git_generate_commit_message_cancel",
            "git_remove_worktree",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("git_commit"));
        assert!(!owns("git_worktree_add"));
        assert!(!owns("Git_Remove_Worktree"));
    }

    #[test]
    fn generate_echoes_draft_or_summarizes_files_without_model() {
        let root = temp_root("gen");
        let with_draft = call(
            &root,
            "git_generate_commit_message",
            json!({
                "requestId": "req-1",
                "path": root.to_string_lossy(),
                "folderId": null,
                "files": ["src/a.rs", "src/b.rs"],
                "draft": "修复登录超时",
                "fallbackAgent": null,
            }),
        )
        .unwrap();
        assert_eq!(with_draft["message"], "修复登录超时");
        assert_eq!(with_draft["agent_type"], "local");
        assert_eq!(with_draft["auto_commit"], false);

        let summarized = call(
            &root,
            "git_generate_commit_message",
            json!({
                "requestId": "req-2",
                "path": root.to_string_lossy(),
                "folderId": null,
                "files": ["src/a.rs", "src/b.rs"],
                "draft": null,
                "fallbackAgent": "claude",
            }),
        )
        .unwrap();
        assert!(summarized["message"].as_str().unwrap().contains("2 个文件"));
        assert_eq!(summarized["agent_type"], "claude");
    }

    #[test]
    fn generate_rejects_missing_or_malformed_args() {
        let root = temp_root("gen-bad");
        assert!(call(&root, "git_generate_commit_message", json!({}))
            .unwrap_err()
            .contains("缺少 requestId"));
        assert!(call(
            &root,
            "git_generate_commit_message",
            json!({ "requestId": "r", "path": "" })
        )
        .unwrap_err()
        .contains("缺少 path"));
        assert!(call(
            &root,
            "git_generate_commit_message",
            json!({ "requestId": "r", "path": "x", "files": "not-an-array" })
        )
        .unwrap_err()
        .contains("files 必须是数组"));
    }

    #[test]
    fn cancel_flag_is_real_and_blocks_a_later_generation() {
        let root = temp_root("cancel");
        call(
            &root,
            "git_generate_commit_message_cancel",
            json!({ "requestId": "req-cancel" }),
        )
        .unwrap();
        assert!(is_cancelled("req-cancel"));
        let blocked = call(
            &root,
            "git_generate_commit_message",
            json!({ "requestId": "req-cancel", "path": root.to_string_lossy(), "files": [] }),
        )
        .unwrap_err();
        assert!(blocked.contains("已取消"));
        // 标志被消费一次后清除，不影响其它请求。
        assert!(!is_cancelled("req-cancel"));
        assert!(call(
            &root,
            "git_generate_commit_message",
            json!({ "requestId": "req-other", "path": root.to_string_lossy(), "files": [] })
        )
        .is_ok());

        assert!(call(&root, "git_generate_commit_message_cancel", json!({}))
            .unwrap_err()
            .contains("缺少 requestId"));
    }

    #[test]
    fn remove_worktree_rejects_bad_args_and_non_repo_paths_without_touching_disk() {
        let root = temp_root("remove-bad");
        assert!(call(&root, "git_remove_worktree", json!({}))
            .unwrap_err()
            .contains("缺少 path"));
        assert!(call(
            &root,
            "git_remove_worktree",
            json!({ "path": root.to_string_lossy(), "branchName": "" })
        )
        .unwrap_err()
        .contains("缺少 branchName"));
        // 临时目录不是 Git 仓库：状态/解析阶段就拒绝，不会执行任何 worktree remove。
        let isolated = temp_root("remove-nonrepo");
        let rejected = call(
            &root,
            "git_remove_worktree",
            json!({
                "path": isolated.to_string_lossy(),
                "branchName": "feature/x",
                "sourceFolderId": 0,
                "deleteBranch": false,
                "force": false,
            }),
        );
        assert!(rejected.is_err());
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "git_status", &json!({})).is_none());
    }
}
