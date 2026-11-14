//! Work-task execution engine, ported from
//! `server-go/internal/codeg/worktask_run.go`.
//!
//! The board itself is Rust-owned (`worktask.rs`); this module is the engine
//! that was deliberately left in Go until the ACP runtime arrived: claim →
//! worktree setup → conversation → prompt → settle, plus the per-folder
//! merge queue and worktree discard. Runtime-only fields Go keeps on its
//! in-memory `WorkTask` (live connection, cancel flag, checkout path) live in
//! a process-level registry here, because the board is reloaded from disk on
//! every call.
//!
//! Commands ported (4): `work_task_engine_claim`, `work_task_engine_merge`,
//! `work_task_engine_cancel_conn`, `work_task_engine_discard_worktree`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};

use crate::backend::acp::commands::layer_if_ready;
use crate::backend::acp::connections::Layer;
use crate::backend::acp::wire::text;
use crate::backend::goformat;

// ── runtime state (Go's unexported WorkTask fields) ─────────────────────────

#[derive(Default)]
struct EngineState {
    /// task id → live connection id.
    live: HashMap<i64, String>,
    /// task ids asked to stop.
    cancel: HashSet<i64>,
    /// task id → checkout path (worktree, or the shared folder).
    checkout: HashMap<i64, PathBuf>,
    /// folder id → merge mutex (Go mergeLock)。tokio 版：合并是 async，
    /// 跨 await 持锁需要异步互斥量。
    merge_locks: HashMap<i64, Arc<tokio::sync::Mutex<()>>>,
}

fn engine() -> &'static Mutex<EngineState> {
    static ENGINE: OnceLock<Mutex<EngineState>> = OnceLock::new();
    ENGINE.get_or_init(|| Mutex::new(EngineState::default()))
}

fn merge_lock(folder_id: i64) -> Arc<tokio::sync::Mutex<()>> {
    let mut state = engine().lock().unwrap();
    state
        .merge_locks
        .entry(folder_id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

// ── board helpers (worktask.rs' store is private; keep a small view here) ──

/// Go 的任务板文件名。
fn tasks_file(root: &Path) -> PathBuf {
    root.join(".data").join("codeg-worktasks.json")
}

fn load_board(root: &Path) -> Value {
    crate::backend::store_file::read_json(&tasks_file(root))
        .ok()
        .flatten()
        .unwrap_or_else(|| json!({"tasks": [], "events": [], "templates": [], "settings": {}}))
}

fn save_board(root: &Path, board: &Value) -> Result<(), String> {
    // 与 worktask.rs 相同的落盘口径（Go struct 序 + map 排序）。
    crate::backend::worktask::persist_board(root, board)
}

fn find_task<'a>(board: &'a Value, id: i64) -> Option<&'a Value> {
    board
        .get("tasks")
        .and_then(Value::as_array)
        .and_then(|rows| rows.iter().find(|row| number(row.get("id")) == id))
}

/// 在 board 上就地改任务（调用方负责持久化）。
fn update_task(board: &mut Value, id: i64, patch: impl FnOnce(&mut Value)) {
    if let Some(tasks) = board.get_mut("tasks").and_then(Value::as_array_mut) {
        for row in tasks.iter_mut() {
            if number(row.get("id")) == id {
                patch(row);
                return;
            }
        }
    }
}

fn set_status(board: &mut Value, id: i64, status: &str, actor: &str, payload: Value) {
    let now = goformat::timestamp();
    update_task(board, id, |task| {
        task["status"] = json!(status);
        task["updated_at"] = json!(now);
        if status == "running" || status == "preparing" || status == "merging" {
            if task.get("started_at").map(Value::is_null).unwrap_or(true) {
                task["started_at"] = json!(now);
            }
        }
        if status == "review" || status == "done" || status == "failed" || status == "canceled" {
            task["settled_at"] = json!(now);
        }
        if status == "done" || status == "failed" || status == "canceled" {
            task["finished_at"] = json!(now);
        }
    });
    push_event(board, id, "status", actor, payload_with_status(payload, status));
}

fn payload_with_status(mut payload: Value, status: &str) -> Value {
    if !payload.is_object() {
        payload = json!({});
    }
    if payload.get("status").is_none() {
        payload["status"] = json!(status);
    }
    payload
}

fn push_event(board: &mut Value, task_id: i64, kind: &str, actor: &str, payload: Value) {
    let next = number(board.get("nextEvent")).max(1);
    board["nextEvent"] = json!(next + 1);
    let event = json!({
        "id": next,
        "task_id": task_id,
        "kind": kind,
        "actor": actor,
        "payload": payload,
        "created_at": goformat::timestamp(),
    });
    // Go: Events map[string][]WorkTaskEvent（键是任务 id）。
    let events = board
        .as_object_mut()
        .unwrap()
        .entry("events")
        .or_insert_with(|| json!({}));
    if !events.is_object() {
        *events = json!({});
    }
    let key = task_id.to_string();
    let list = events
        .as_object_mut()
        .unwrap()
        .entry(key)
        .or_insert_with(|| json!([]));
    if !list.is_array() {
        *list = json!([]);
    }
    list.as_array_mut().unwrap().push(event);
}

fn number(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

fn folder_path(root: &Path, folder_id: i64) -> Option<String> {
    crate::backend::store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
        .ok()
        .flatten()
        .and_then(|doc| {
            doc.get("folders").and_then(Value::as_array).and_then(|rows| {
                rows.iter()
                    .find(|row| number(row.get("id")) == folder_id)
                    .and_then(|row| row.get("path"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        })
}

fn folder_row(root: &Path, folder_id: i64) -> Value {
    crate::backend::store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
        .ok()
        .flatten()
        .and_then(|doc| {
            doc.get("folders").and_then(Value::as_array).and_then(|rows| {
                rows.iter()
                    .find(|row| number(row.get("id")) == folder_id)
                    .cloned()
            })
        })
        .unwrap_or(Value::Null)
}

fn folder_settings(board: &Value, folder_id: i64) -> Value {
    board
        .get("settings")
        .and_then(|settings| settings.get(&folder_id.to_string()))
        .cloned()
        .unwrap_or(Value::Null)
}

fn resolved_agent(root: &Path, board: &Value, task: &Value) -> String {
    if let Some(agent) = task
        .pointer("/config/agent_type")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return agent.to_string();
    }
    if let Some(agent) = folder_settings(board, number(task.get("folder_id")))
        .get("default_agent_type")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return agent.to_string();
    }
    if let Some(agent) = folder_row(root, number(task.get("folder_id")))
        .get("default_agent_type")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return agent.to_string();
    }
    "codex".into()
}

fn task_worktree_name(id: i64) -> String {
    format!("task-{id}")
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("git {} 失败", args.join(" "))
        } else {
            stderr
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn git_ok(cwd: &Path, args: &[&str]) -> Option<String> {
    git(cwd, args).ok()
}

// ── entry: engine commands (sync, called from the work_task dispatcher) ────

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "work_task_engine_claim"
            | "work_task_engine_merge"
            | "work_task_engine_cancel_conn"
            | "work_task_engine_discard_worktree"
    )
}

/// 同步入口：claim/cancel 立即返回（引擎在后台跑），merge 返回是否排队，
/// discard 同步做完 git 清理。
pub fn handle(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let id = number(args.get("id"));
    match command {
        "work_task_engine_claim" => {
            let root = root.to_path_buf();
            spawn(async move {
                claim_task(&root, id).await;
            });
            Ok(Value::Null)
        }
        "work_task_engine_merge" => {
            let board = load_board(root);
            let Some(task) = find_task(&board, id) else {
                return Err("任务不存在".into());
            };
            let folder_id = number(task.get("folder_id"));
            let message = text(args.get("message"));
            let delete_worktree = args.get("deleteWorktree") == Some(&Value::Bool(true));
            Ok(json!(dispatch_merge(
                root,
                folder_id,
                id,
                &message,
                delete_worktree
            )?))
        }
        "work_task_engine_cancel_conn" => {
            let live = {
                let mut state = engine().lock().unwrap();
                let live = state.live.get(&id).cloned();
                state.cancel.insert(id);
                live
            };
            if let Some(cid) = live {
                if let Some(layer) = layer_if_ready() {
                    spawn(async move {
                        let _ = layer.cancel(&cid).await;
                    });
                }
            }
            Ok(Value::Null)
        }
        "work_task_engine_discard_worktree" => {
            discard_worktree(root, id)?;
            Ok(Value::Null)
        }
        other => Err(format!("未知的引擎命令：{other}")),
    }
}

fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(future: F) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
    } else if let Some(runtime) = SHARED_RUNTIME.get() {
        runtime.spawn(future);
    }
}

static SHARED_RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();

/// serve() 启动时登记当前 runtime，阻塞上下文（worktask.rs 分发）也能发起
/// 后台引擎任务。
pub fn bind_runtime() {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let _ = SHARED_RUNTIME.set(handle);
    }
}

// ── claim / run ────────────────────────────────────────────────────────────

/// Go `claimTask`：并发上限内把 queued 推进 preparing 并开跑。
pub async fn claim_task(root: &Path, id: i64) {
    let mut board = load_board(root);
    let Some(task) = find_task(&board, id).cloned() else {
        return;
    };
    if task.get("status").and_then(Value::as_str) != Some("queued") {
        return;
    }
    let folder_id = number(task.get("folder_id"));
    let settings = folder_settings(&board, folder_id);
    let mut max = number(settings.get("max_concurrent"));
    if max <= 0 {
        max = 1;
    }
    let running = board
        .get("tasks")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter(|row| {
                    number(row.get("folder_id")) == folder_id
                        && matches!(
                            row.get("status").and_then(Value::as_str),
                            Some("preparing" | "running" | "awaiting_input" | "merging")
                        )
                })
                .count() as i64
        })
        .unwrap_or(0);
    if running >= max {
        return; // 等 tick 在空出槽位后重试
    }
    set_status(&mut board, id, "preparing", "engine", json!({}));
    if let Some(path) = folder_path(root, folder_id) {
        engine()
            .lock()
            .unwrap()
            .checkout
            .insert(id, PathBuf::from(path));
    }
    if save_board(root, &board).is_err() {
        return;
    }
    if let Err(err) = run_task(root, id).await {
        let mut board = load_board(root);
        update_task(&mut board, id, |task| {
            task["last_error"] = json!(err.to_string());
            task["failure_reason"] = json!("setup_error");
        });
        set_status(
            &mut board,
            id,
            "failed",
            "engine",
            json!({"error": err.to_string()}),
        );
        let _ = save_board(root, &board);
    }
}

/// Go `runTask`：工作树 → 会话 → prompt → 等对话落定。
async fn run_task(root: &Path, id: i64) -> Result<(), String> {
    let mut board = load_board(root);
    let Some(task) = find_task(&board, id).cloned() else {
        return Ok(());
    };
    let folder_id = number(task.get("folder_id"));
    let folder = folder_path(root, folder_id).ok_or_else(|| "项目文件夹不存在".to_string())?;
    let folder_dir = PathBuf::from(&folder);
    let name = task_worktree_name(id);
    let mut checkout = folder_dir.clone();

    // 工作树（非 git 仓库回退共享目录）。
    let worktrees = folder_dir.join(".worktrees");
    let is_repo = git_ok(&folder_dir, &["rev-parse", "--is-inside-work-tree"])
        .map(|out| out.trim() == "true")
        .unwrap_or(false);
    if is_repo {
        let target = worktrees.join(&name);
        if !target.is_dir() {
            let branch = format!("hh/{name}");
            git(
                &folder_dir,
                &[
                    "worktree",
                    "add",
                    "-B",
                    &branch,
                    target.to_string_lossy().as_ref(),
                    "HEAD",
                ],
            )
            .map_err(|e| format!("创建隔离工作树失败：{e}"))?;
        }
        checkout = target.clone();
        engine().lock().unwrap().checkout.insert(id, checkout.clone());
        if task.get("base_branch").map(Value::is_null).unwrap_or(true) {
            if let Some(branch) = git_ok(&folder_dir, &["rev-parse", "--abbrev-ref", "HEAD"]) {
                update_task(&mut board, id, |task| {
                    task["base_branch"] = json!(branch.trim());
                });
            }
        }
        if let Some(sha) = git_ok(&folder_dir, &["rev-parse", "HEAD"]) {
            update_task(&mut board, id, |task| {
                task["base_sha"] = json!(sha.trim());
            });
        }
        update_task(&mut board, id, |task| {
            task["work_branch"] = json!(format!("hh/{name}"));
        });
    } else {
        push_event(
            &mut board,
            id,
            "notice",
            "engine",
            json!({"text": "非 git 仓库，回退共享目录执行", "folder_path": folder}),
        );
    }
    if let Some(branch) = git_ok(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        if !branch.trim().is_empty() {
            update_task(&mut board, id, |task| {
                task["work_branch"] = json!(branch.trim());
            });
        }
    }

    let agent_type = resolved_agent(root, &board, &task);
    set_status(&mut board, id, "running", "engine", json!({}));
    save_board(root, &board)?;

    let Some(layer) = layer_if_ready() else {
        return Err("连接执行端失败：ACP 连接层未就绪".into());
    };
    let cid = layer
        .connect(&json!({"agentType": agent_type, "workingDir": checkout.to_string_lossy()}))
        .await
        .map_err(|e| format!("连接执行端失败：{e}"))?;
    engine().lock().unwrap().live.insert(id, cid.clone());
    update_task(&mut board, id, |task| {
        task["connection_id"] = json!(cid);
    });

    let title = truncate_runes(&text(task.get("title")), 80);
    let conversation_id =
        crate::backend::acp::store_conv::create_task_conversation(root, folder_id, &agent_type, &title)?;
    update_task(&mut board, id, |task| {
        task["conversation_id"] = json!(conversation_id);
    });
    save_board(root, &board)?;

    let first_text = first_task_text(task.get("config"));
    let blocks = json!([{"type": "text", "text": first_text}]);
    if let Err(err) = layer
        .prompt(&json!({
            "connectionId": cid,
            "conversationId": conversation_id,
            "folderId": folder_id,
            "blocks": blocks,
        }))
        .await
    {
        let _ = layer.disconnect(&cid).await;
        engine().lock().unwrap().live.remove(&id);
        return Err(format!("启动对话失败：{err}"));
    }

    // 轮询直到对话落定，期间把「等权限卡」映射成 awaiting_input。
    let mut awaiting = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        {
            let engine_state = engine().lock().unwrap();
            if engine_state.cancel.contains(&id) {
                return Ok(());
            }
        }
        let mut board = load_board(root);
        if find_task(&board, id).is_none() {
            return Ok(());
        }
        let waiting = layer
            .connection(&cid)
            .map(|conn| {
                let conn = conn.lock().unwrap();
                conn.busy && !conn.pending.is_null()
            })
            .unwrap_or(false);
        if waiting != awaiting {
            awaiting = waiting;
            let status = if waiting { "awaiting_input" } else { "running" };
            set_status(&mut board, id, status, "engine", json!({}));
            let _ = save_board(root, &board);
        }
        let status = crate::backend::turn_store::conversation_status(root, conversation_id);
        if status == "pending_review" {
            let _ = layer.disconnect(&cid).await;
            engine().lock().unwrap().live.remove(&id);
            settle_task(root, id, &folder, &name, conversation_id);
            return Ok(());
        }
        if status == "cancelled" || status == "failed" {
            let _ = layer.disconnect(&cid).await;
            engine().lock().unwrap().live.remove(&id);
            let mut board = load_board(root);
            update_task(&mut board, id, |task| {
                task["last_error"] = json!("会话执行失败或被取消");
                task["failure_reason"] = json!("agent_error");
            });
            set_status(&mut board, id, "failed", "engine", json!({}));
            let _ = save_board(root, &board);
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            let _ = layer.cancel(&cid).await;
            let _ = layer.disconnect(&cid).await;
            engine().lock().unwrap().live.remove(&id);
            let mut board = load_board(root);
            update_task(&mut board, id, |task| {
                task["failure_reason"] = json!("agent_error");
                task["last_error"] = json!("运行超过 24 小时上限");
            });
            set_status(&mut board, id, "failed", "engine", json!({}));
            let _ = save_board(root, &board);
            return Ok(());
        }
    }
}

/// Go `settleTask`：进评审、算 diff、可选自动合并。
fn settle_task(root: &Path, id: i64, folder: &str, name: &str, conversation_id: i64) {
    let folder_dir = Path::new(folder);
    let has_worktree = folder_dir.join(".worktrees").join(name).is_dir();
    let mut board = load_board(root);
    if has_worktree {
        if let Some(summary) = last_assistant_text(root, conversation_id) {
            let trimmed = if summary.len() > 2000 {
                String::from_utf8_lossy(&summary.as_bytes()[..2000]).to_string()
            } else {
                summary
            };
            update_task(&mut board, id, |task| {
                task["result_summary"] = json!(trimmed);
            });
        }
        let (files, additions, deletions) = diff_stats(folder, name);
        update_task(&mut board, id, |task| {
            task["files_changed"] = json!(files);
            task["additions"] = json!(additions);
            task["deletions"] = json!(deletions);
        });
    }
    set_status(&mut board, id, "review", "engine", json!({}));
    let _ = save_board(root, &board);
    let auto_merge = folder_settings(&board, number(find_task(&board, id).and_then(|t| t.get("folder_id"))))
        .get("auto_merge")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if auto_merge {
        let folder_id = number(find_task(&board, id).and_then(|t| t.get("folder_id")));
        let root_owned = root.to_path_buf();
        spawn(async move {
            let _ = dispatch_merge(&root_owned, folder_id, id, "", false);
        });
    }
}

/// Go `lastAssistantText`：最后一条 assistant 文本块。
fn last_assistant_text(root: &Path, conversation_id: i64) -> Option<String> {
    let turns = crate::backend::turn_store::all_turns_for(root, conversation_id).ok()?;
    for turn in turns.iter().rev() {
        if text(turn.get("role")) != "assistant" {
            continue;
        }
        for block in turn.get("blocks").and_then(Value::as_array).into_iter().flatten() {
            if text(block.get("type")) == "text" {
                return Some(text(block.get("text")));
            }
        }
    }
    None
}

/// Go `diffStats`：numstat 汇总（多文件累加）。
fn diff_stats(folder: &str, name: &str) -> (i64, i64, i64) {
    let Ok(output) = git(
        Path::new(folder),
        &["diff", "--numstat", "HEAD", &format!("hh/{name}")],
    ) else {
        return (0, 0, 0);
    };
    let mut files = 0;
    let mut additions = 0;
    let mut deletions = 0;
    for line in output.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            files += 1;
            if parts[0] != "-" {
                additions += parts[0].parse::<i64>().unwrap_or(0);
            }
            if parts[1] != "-" {
                deletions += parts[1].parse::<i64>().unwrap_or(0);
            }
        }
    }
    (files, additions, deletions)
}

fn truncate_runes(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

/// Go `firstTaskText`：任务的初始指令（prompt + notes）。
fn first_task_text(config: Option<&Value>) -> String {
    let Some(config) = config else {
        return String::new();
    };
    let mut out = text(config.get("prompt"));
    if let Some(notes) = config.get("notes").and_then(Value::as_array) {
        for note in notes {
            let line = text(note.get("text"));
            if !line.is_empty() {
                out.push_str("\n\n");
                out.push_str(&line);
            }
        }
    }
    out
}

// ── merge ──────────────────────────────────────────────────────────────────

/// Go `dispatchTaskMerge`：拿不到该文件夹的合并锁就排队。
fn dispatch_merge(
    root: &Path,
    folder_id: i64,
    id: i64,
    message: &str,
    delete_worktree: bool,
) -> Result<bool, String> {
    let lock = merge_lock(folder_id);
    let Ok(guard) = lock.try_lock() else {
        // 排队（调用方已持 board 锁）。
        let mut board = load_board(root);
        update_task(&mut board, id, |task| {
            task["merge_queued"] = json!({
                "message": message,
                "delete_worktree": delete_worktree,
                "queued_at": goformat::timestamp(),
            });
        });
        save_board(root, &board)?;
        return Ok(true);
    };
    drop(guard);
    let root_owned = root.to_path_buf();
    let message = message.to_string();
    spawn(async move {
        // 后台合并自己拿锁（与 drain 队列同一条路径）。
        let lock = merge_lock(folder_id);
        let _guard = lock.lock().await;
        process_merge(&root_owned, folder_id, id, &message, delete_worktree).await;
    });
    Ok(false)
}

/// Go `processTaskMerge`：合并分支（squash 或 worktree merge）并收尾。
async fn process_merge(
    root: &Path,
    folder_id: i64,
    id: i64,
    message: &str,
    delete_worktree: bool,
) {
    let mut board = load_board(root);
    set_status(&mut board, id, "merging", "engine", json!({}));
    let _ = save_board(root, &board);

    let folder = folder_path(root, folder_id)
        .or_else(|| {
            engine()
                .lock()
                .unwrap()
                .checkout
                .get(&id)
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_default();
    let folder_dir = PathBuf::from(&folder);
    let name = task_worktree_name(id);

    // 先断开活连接与驱动，别让 agent 进程卡住工作树目录。
    let (live, checkout) = {
        let mut state = engine().lock().unwrap();
        (state.live.remove(&id), state.checkout.get(&id).cloned())
    };
    if let Some(cid) = live {
        if let Some(layer) = layer_if_ready() {
            let _ = layer.disconnect(&cid).await;
        }
    }
    if let Some(worktree) = checkout {
        if let Some(layer) = layer_if_ready() {
            layer.kill_drivers_at(&worktree.to_string_lossy());
        }
    }

    let strategy = folder_settings(&board, folder_id)
        .get("merge_strategy")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let result = if strategy == "squash" {
        squash_merge(&folder_dir, &format!("hh/{name}"), message)
    } else {
        merge_worktree(&folder_dir, &name, message)
    };
    if let Err(err) = result {
        let mut board = load_board(root);
        update_task(&mut board, id, |task| {
            task["last_error"] = json!(err);
        });
        set_status(&mut board, id, "review", "engine", json!({"error": err}));
        let _ = save_board(root, &board);
        drain_merge_queue(root, folder_id);
        return;
    }
    let mut board = load_board(root);
    if let Some(sha) = git_ok(&folder_dir, &["rev-parse", "HEAD"]) {
        update_task(&mut board, id, |task| {
            task["merge_commit"] = json!(sha.trim());
        });
    }
    update_task(&mut board, id, |task| {
        task["completion_kind"] = json!("merged");
    });
    set_status(&mut board, id, "done", "engine", json!({}));
    let _ = save_board(root, &board);
    if delete_worktree {
        let _ = discard_worktree(root, id);
    }
    drain_merge_queue(root, folder_id);
}

/// Go `drainMergeQueue`：合并结束后启动同文件夹下一个排队的合并。
fn drain_merge_queue(root: &Path, folder_id: i64) {
    let mut board = load_board(root);
    let mut next: Option<(i64, String, bool)> = None;
    if let Some(tasks) = board.get("tasks").and_then(Value::as_array) {
        for task in tasks {
            if number(task.get("folder_id")) != folder_id {
                continue;
            }
            let queued = task.get("merge_queued");
            let Some(queued) = queued.filter(|v| v.is_object()) else {
                continue;
            };
            next = Some((
                number(task.get("id")),
                text(queued.get("message")),
                queued.get("delete_worktree") == Some(&Value::Bool(true)),
            ));
            break;
        }
    }
    if let Some((id, _, _)) = &next {
        update_task(&mut board, *id, |task| {
            task["merge_queued"] = Value::Null;
        });
        if save_board(root, &board).is_err() {
            return;
        }
    } else {
        return;
    }
    let (id, message, delete_worktree) = next.unwrap();
    let root_owned = root.to_path_buf();
    spawn(async move {
        let lock = merge_lock(folder_id);
        let _guard = lock.lock().await;
        process_merge(&root_owned, folder_id, id, &message, delete_worktree).await;
    });
}

/// Go `squashMerge`：一个提交落到基分支，不产生合并提交。
fn squash_merge(folder: &Path, branch: &str, message: &str) -> Result<(), String> {
    let status = git(
        folder,
        &["status", "--porcelain", "--", ".", ":!.worktrees"],
    )?;
    for line in status.lines() {
        if !line.trim().is_empty() && !line.contains(".worktrees") {
            return Err("主工作区有未提交改动，请先提交或暂存后再合并".into());
        }
    }
    let target = folder
        .join(".worktrees")
        .join(branch.trim_start_matches("hh/"));
    git(&target, &["add", "-A"])?;
    let _ = git(
        &target,
        &["commit", "-m", "Agent work (auto-committed before review merge)"],
    );
    let message = if message.is_empty() {
        format!("Squash merge {branch}")
    } else {
        message.to_string()
    };
    if git(folder, &["merge", "--squash", branch]).is_err() {
        return Err("squash 合并失败（可能需要先处理冲突）".into());
    }
    if git(folder, &["commit", "-m", &message]).is_err() {
        return Err("提交合并结果失败".into());
    }
    if git(folder, &["worktree", "remove", "--force", target.to_string_lossy().as_ref()]).is_ok() {
        let _ = git(folder, &["branch", "-D", branch]);
    }
    Ok(())
}

/// Go `extra.Worktrees.Merge`：分支合并回基目录并移除工作树。
fn merge_worktree(folder: &Path, name: &str, message: &str) -> Result<(), String> {
    let branch = format!("hh/{name}");
    if !message.is_empty() {
        if git(folder, &["merge", "--no-ff", "-m", message, &branch]).is_err() {
            return Err("合并失败（可能需要先处理冲突）".into());
        }
    } else if git(folder, &["merge", "--no-ff", &branch]).is_err() {
        return Err("合并失败（可能需要先处理冲突）".into());
    }
    let target = folder.join(".worktrees").join(name);
    if git(folder, &["worktree", "remove", "--force", target.to_string_lossy().as_ref()]).is_ok() {
        let _ = git(folder, &["branch", "-D", &branch]);
    }
    Ok(())
}

// ── discard ────────────────────────────────────────────────────────────────

/// Go `discardTaskWorktree`：断开连接、清驱动、删工作树与分支（尽力而为）。
fn discard_worktree(root: &Path, id: i64) -> Result<(), String> {
    let (live, checkout) = {
        let mut state = engine().lock().unwrap();
        (state.live.remove(&id), state.checkout.get(&id).cloned())
    };
    if let Some(cid) = live {
        if let Some(layer) = layer_if_ready() {
            layer.cancel_and_disconnect_later(&cid);
        }
    }
    if let Some(worktree) = checkout {
        if let Some(layer) = layer_if_ready() {
            layer.kill_drivers_at(&worktree.to_string_lossy());
        }
    }
    let board = load_board(root);
    let folder_id = find_task(&board, id)
        .map(|task| number(task.get("folder_id")))
        .unwrap_or(0);
    let folder = folder_path(root, folder_id).unwrap_or_default();
    let folder_dir = PathBuf::from(&folder);
    let name = task_worktree_name(id);
    let target = folder_dir.join(".worktrees").join(&name);
    let mut result = Ok(());
    if target.is_dir() {
        result = git(
            &folder_dir,
            &["worktree", "remove", "--force", target.to_string_lossy().as_ref()],
        )
        .map(|_| ());
        if result.is_ok() {
            let _ = git(&folder_dir, &["branch", "-D", &format!("hh/{name}")]);
        }
    } else {
        // Go extra.Worktrees.Discard：目标不存在时报这个文案。
        result = Err("该任务没有隔离工作树".into());
    }
    engine().lock().unwrap().checkout.remove(&id);
    result
}

/// 15 分钟一轮：把到期/排队的任务交给 claim（Go `TickWorkTasks`）。
pub fn tick(root: &Path) {
    let board = load_board(root);
    let now = goformat::timestamp();
    let mut due = Vec::new();
    for task in board.get("tasks").and_then(Value::as_array).into_iter().flatten() {
        let status = text(task.get("status"));
        let id = number(task.get("id"));
        if status == "todo" {
            if let Some(scheduled) = task.get("scheduled_at").and_then(Value::as_str) {
                if scheduled <= now.as_str() && !scheduled.is_empty() {
                    due.push(id);
                }
            }
        } else if status == "queued" {
            due.push(id);
        }
    }
    for id in due {
        let root_owned = root.to_path_buf();
        spawn(async move { claim_task(&root_owned, id).await });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-engine-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn seed(root: &Path, tasks: Value) {
        let board = json!({
            "next": 10, "nextEvent": 1, "nextTemplate": 1,
            "tasks": tasks, "events": {}, "templates": [], "settings": {}
        });
        std::fs::write(
            tasks_file(root),
            serde_json::to_vec_pretty(&board).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn merge_reports_missing_task() {
        let root = temp_root("merge-missing");
        seed(&root, json!([]));
        assert_eq!(
            handle(&root, "work_task_engine_merge", &json!({"id": 99})).unwrap_err(),
            "任务不存在"
        );
    }

    #[test]
    fn merge_queues_when_lock_is_held() {
        let root = temp_root("merge-queued");
        seed(
            &root,
            json!([{"id": 1, "folder_id": 5, "title": "t", "status": "review"}]),
        );
        // 先占住该文件夹的合并锁。
        let lock = merge_lock(5);
        let guard = lock.try_lock().expect("首次拿锁");
        let queued = handle(
            &root,
            "work_task_engine_merge",
            &json!({"id": 1, "message": "m", "deleteWorktree": true}),
        )
        .unwrap();
        assert_eq!(queued, json!(true), "锁被占用 → 排队");
        let board = load_board(&root);
        let task = find_task(&board, 1).unwrap();
        assert_eq!(task["merge_queued"]["message"], "m");
        assert_eq!(task["merge_queued"]["delete_worktree"], true);
        drop(guard);
    }

    #[test]
    fn cancel_marks_flag_and_is_idempotent() {
        let root = temp_root("cancel");
        seed(&root, json!([{"id": 2, "folder_id": 1, "title": "t", "status": "running"}]));
        handle(&root, "work_task_engine_cancel_conn", &json!({"id": 2})).unwrap();
        assert!(engine().lock().unwrap().cancel.contains(&2));
        handle(&root, "work_task_engine_cancel_conn", &json!({"id": 2})).unwrap();
    }

    #[test]
    fn squash_merge_refuses_dirty_main_tree() {
        let root = temp_root("squash-dirty");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "init"]);
        std::fs::write(repo.join("dirty.txt"), "x").unwrap();
        let err = squash_merge(&repo, "hh/task-1", "").unwrap_err();
        assert_eq!(err, "主工作区有未提交改动，请先提交或暂存后再合并");
    }

    #[test]
    fn diff_stats_accumulates_across_files() {
        let root = temp_root("diff");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "1\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "init"]);
        run(&["checkout", "-qb", "hh/task-1"]);
        std::fs::write(repo.join("a.txt"), "1\n2\n3\n").unwrap();
        std::fs::write(repo.join("b.txt"), "x\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "work"]);
        // 基分支上是 HEAD，任务分支另有提交（与引擎真实布局一致）。
        run(&["checkout", "-q", "-"]);
        let (files, additions, _deletions) = diff_stats(&repo.to_string_lossy(), "task-1");
        assert_eq!(files, 2, "两个文件都应计入");
        assert_eq!(additions, 3, "多文件新增行数累加（a:2 + b:1）");
    }
}
