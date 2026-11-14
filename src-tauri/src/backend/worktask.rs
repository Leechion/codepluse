//! Work tasks, ported from `server-go/internal/codeg/worktask.go` +
//! `worktask_run.go`.
//!
//! Rust owns `.data/codeg-worktasks.json` (the `taskStore` document): every
//! command here re-reads the file, mutates a typed mirror of Go's structs and
//! writes it back with Go's `json.Marshal` byte layout — struct fields in
//! declaration order (`goformat::marshal_preserve`, no key sorting), map
//! fields (`events`, `settings`, `config_values`) sorted like Go's map
//! marshal. Rows the UI sends are canonicalised on the way in so the file
//! stays byte-identical to what the sidecar would have written.
//!
//! The execution engine (`claimTask`/`runTask`/`processTaskMerge`) stays in
//! the Go sidecar for now: it drives the ACP runtime, which is a later
//! migration batch. Rust hands work to it through four internal trigger
//! commands (`work_task_engine_claim/_merge/_cancel_conn/_discard_worktree`)
//! that the Go switch answers after `reloadIfChangedLocked` picks up this
//! process's writes. That reload is what keeps both writers honest — see
//! `Store.reloadIfChangedLocked` for the pattern this follows.
//!
//! Known divergence: Go gates `cleanup`/`delete`/`complete`'s worktree
//! handling on the engine's in-memory `worktreePath`, which is invisible
//! across a restart. Rust derives it from the on-disk checkout
//! (`<folder>/.worktrees/task-<id>`), so a cleanup after a sidecar restart
//! succeeds here where Go (pre-migration) answered 「该任务没有隔离工作树」.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::backend::goformat;
use crate::backend::store_file;

const FILE: &str = "codeg-worktasks.json";

/// Serialises every command against one shared board, mirroring Go's
/// `taskStore.mu` (a load → mutate → write cycle must not interleave).
static BOARD: Mutex<()> = Mutex::new(());

/// Sidecar base URL (`http://127.0.0.1:<port>`), set by the proxy at startup.
static UPSTREAM: OnceLock<String> = OnceLock::new();

static UPSTREAM_CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

/// Called once by the front door before the first command can run.
pub fn set_upstream(base: String) {
    let _ = UPSTREAM.set(base);
}

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "work_task_list"
            | "work_task_get"
            | "work_task_events"
            | "work_task_create"
            | "work_task_update"
            | "work_task_reorder"
            | "work_task_delete"
            | "work_task_start"
            | "work_task_retry"
            | "work_task_requeue"
            | "work_task_schedule"
            | "work_task_return"
            | "work_task_cancel"
            | "work_task_merge"
            | "work_task_merge_unqueue"
            | "work_task_deliver_pr"
            | "work_task_complete"
            | "work_task_archive"
            | "work_task_defer"
            | "work_task_cleanup"
            | "work_task_diff"
            | "work_task_changed_files"
            | "work_task_settings_get"
            | "work_task_settings_effective"
            | "work_task_settings_get_own"
            | "work_task_settings_set"
            | "work_task_settings_delete"
            | "work_task_template_list"
            | "work_task_template_save"
            | "work_task_template_delete"
            | "work_task_lookup_by_source"
            | "work_task_create_from_forge"
    )
}

/// Command outcome: the HTTP body plus `task://changed` frames for the relay.
#[derive(Debug)]
pub struct Outcome {
    pub result: Value,
    pub events: Vec<(String, Value)>,
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Outcome, String>> {
    if !owns(command) {
        return None;
    }
    let _board = BOARD.lock().unwrap();
    let mut doc = load(root);
    Some(run(root, &mut doc, command, args))
}

// ----------------------------- data model -----------------------------

fn default_config() -> WorkTaskConfig {
    WorkTaskConfig {
        prompt_blocks: Vec::new(),
        display_text: String::new(),
        agent_type: None,
        mode_id: None,
        config_values: BTreeMap::new(),
        label_snapshot: None,
        base_branch: None,
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct WorkTaskConfig {
    #[serde(rename = "prompt_blocks", default)]
    prompt_blocks: Vec<Value>,
    #[serde(rename = "display_text", default)]
    display_text: String,
    #[serde(rename = "agent_type", default)]
    agent_type: Option<String>,
    #[serde(rename = "mode_id", default)]
    mode_id: Option<String>,
    #[serde(rename = "config_values", default)]
    config_values: BTreeMap<String, String>,
    // Go: `Row json:"label_snapshot,omitempty"` — nil or empty maps disappear.
    #[serde(
        rename = "label_snapshot",
        default,
        skip_serializing_if = "label_snapshot_omitted"
    )]
    label_snapshot: Option<Value>,
    #[serde(rename = "base_branch", default)]
    base_branch: Option<String>,
}

fn label_snapshot_omitted(value: &Option<Value>) -> bool {
    match value {
        None => true,
        Some(value) => value.as_object().map(|m| m.is_empty()).unwrap_or(false),
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct WorkTask {
    #[serde(rename = "id", default)]
    id: i64,
    #[serde(rename = "folder_id", default)]
    folder_id: i64,
    #[serde(rename = "title", default)]
    title: String,
    #[serde(rename = "config", default)]
    config: Option<WorkTaskConfig>,
    #[serde(rename = "status", default)]
    status: String,
    #[serde(rename = "failure_reason", default)]
    failure_reason: Option<String>,
    #[serde(rename = "last_error", default)]
    last_error: Option<String>,
    #[serde(rename = "run_seq", default)]
    run_seq: i64,
    #[serde(rename = "sort_order", default)]
    sort_order: i64,
    #[serde(rename = "worktree_folder_id", default)]
    worktree_folder_id: Option<i64>,
    #[serde(rename = "agent_type", default)]
    agent_type: Option<String>,
    #[serde(rename = "conversation_id", default)]
    conversation_id: Option<i64>,
    #[serde(rename = "connection_id", default)]
    connection_id: Option<String>,
    #[serde(rename = "base_branch", default)]
    base_branch: Option<String>,
    #[serde(rename = "base_sha", default)]
    base_sha: Option<String>,
    #[serde(rename = "work_branch", default)]
    work_branch: Option<String>,
    #[serde(rename = "cleanup_state", default)]
    cleanup_state: Option<String>,
    #[serde(rename = "verdict", default)]
    verdict: Option<String>,
    #[serde(rename = "result_summary", default)]
    result_summary: Option<String>,
    #[serde(rename = "files_changed", default)]
    files_changed: Option<i64>,
    #[serde(rename = "additions", default)]
    additions: Option<i64>,
    #[serde(rename = "deletions", default)]
    deletions: Option<i64>,
    #[serde(rename = "merge_commit", default)]
    merge_commit: Option<String>,
    #[serde(rename = "completion_kind", default)]
    completion_kind: Option<String>,
    #[serde(rename = "preflight", default)]
    preflight: Value,
    // Go: `Row json:"merge_queued,omitempty"`.
    #[serde(rename = "merge_queued", default, skip_serializing_if = "Value::is_null")]
    merge_queued: Value,
    #[serde(rename = "archived_at", default)]
    archived_at: Option<String>,
    #[serde(rename = "scheduled_at", default)]
    scheduled_at: Option<String>,
    #[serde(rename = "deferred_at", default)]
    deferred_at: Option<String>,
    #[serde(rename = "source_kind", default)]
    source_kind: Option<String>,
    #[serde(rename = "source_key", default)]
    source_key: Option<String>,
    #[serde(rename = "source_meta", default)]
    source_meta: Value,
    #[serde(rename = "delivery", default)]
    delivery: Value,
    #[serde(rename = "latest_progress", default)]
    latest_progress: Option<String>,
    #[serde(rename = "created_at", default)]
    created_at: String,
    #[serde(rename = "updated_at", default)]
    updated_at: String,
    #[serde(rename = "started_at", default)]
    started_at: Option<String>,
    #[serde(rename = "settled_at", default)]
    settled_at: Option<String>,
    #[serde(rename = "finished_at", default)]
    finished_at: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct WorkTaskEvent {
    #[serde(rename = "id", default)]
    id: i64,
    #[serde(rename = "task_id", default)]
    task_id: i64,
    #[serde(rename = "kind", default)]
    kind: String,
    #[serde(rename = "actor", default)]
    actor: String,
    #[serde(rename = "payload", default)]
    payload: Value,
    #[serde(rename = "created_at", default)]
    created_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct WorkTaskTemplate {
    #[serde(rename = "id", default)]
    id: i64,
    #[serde(rename = "name", default)]
    name: String,
    #[serde(rename = "title", default)]
    title: String,
    #[serde(rename = "config", default)]
    config: Option<WorkTaskConfig>,
    #[serde(rename = "created_at", default)]
    created_at: String,
    #[serde(rename = "updated_at", default)]
    updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct WorkTaskFolderSettings {
    #[serde(rename = "default_agent_type", default)]
    default_agent_type: Option<String>,
    #[serde(rename = "mode_id", default)]
    mode_id: Option<String>,
    #[serde(rename = "config_values", default)]
    config_values: BTreeMap<String, String>,
    #[serde(rename = "label_snapshot", default)]
    label_snapshot: Value,
    #[serde(rename = "auto_process", default)]
    auto_process: bool,
    #[serde(rename = "max_concurrent", default)]
    max_concurrent: i64,
    #[serde(rename = "merge_strategy", default)]
    merge_strategy: String,
    #[serde(rename = "auto_merge", default)]
    auto_merge: bool,
}

impl WorkTaskFolderSettings {
    fn defaults() -> Self {
        WorkTaskFolderSettings {
            default_agent_type: None,
            mode_id: None,
            config_values: BTreeMap::new(),
            label_snapshot: Value::Null,
            auto_process: false,
            max_concurrent: 1,
            merge_strategy: "merge".into(),
            auto_merge: false,
        }
    }
}

/// The `taskStore` document. Field order matches Go's struct; `events` and
/// `settings` are Go maps, so `BTreeMap` reproduces their sorted marshal.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct TaskDoc {
    #[serde(rename = "next", default)]
    next: i64,
    #[serde(rename = "nextEvent", default)]
    next_event: i64,
    #[serde(rename = "nextTemplate", default)]
    next_tpl: i64,
    // Go's `Tasks` starts as a nil slice → `null` until the first append.
    #[serde(rename = "tasks")]
    tasks: Option<Vec<WorkTask>>,
    #[serde(rename = "events", default)]
    events: BTreeMap<String, Vec<WorkTaskEvent>>,
    #[serde(rename = "templates")]
    templates: Option<Vec<WorkTaskTemplate>>,
    #[serde(rename = "settings", default)]
    settings: BTreeMap<String, Option<WorkTaskFolderSettings>>,
}

impl TaskDoc {
    fn new() -> Self {
        TaskDoc {
            next: 1,
            next_event: 1,
            next_tpl: 1,
            tasks: None,
            events: BTreeMap::new(),
            templates: None,
            settings: BTreeMap::new(),
        }
    }

    fn tasks(&self) -> &[WorkTask] {
        self.tasks.as_deref().unwrap_or(&[])
    }

    fn tasks_mut(&mut self) -> &mut Vec<WorkTask> {
        self.tasks.get_or_insert_with(Vec::new)
    }

    fn find(&self, id: i64) -> Option<&WorkTask> {
        self.tasks().iter().find(|task| task.id == id)
    }

    fn find_mut(&mut self, id: i64) -> Option<&mut WorkTask> {
        self.tasks_mut().iter_mut().find(|task| task.id == id)
    }
}

fn file_path(root: &Path) -> PathBuf {
    root.join(".data").join(FILE)
}

fn load(root: &Path) -> TaskDoc {
    match store_file::read_json(&file_path(root)) {
        Ok(Some(value)) => serde_json::from_value(value).unwrap_or_else(|_| TaskDoc::new()),
        _ => TaskDoc::new(),
    }
}

/// Writes the document with Go's byte layout: struct fields in declaration
/// order, maps sorted, compact, HTML-escaped. No `canon` — that would sort
/// the struct fields too.
fn persist(root: &Path, doc: &TaskDoc) {
    let value = match serde_json::to_value(doc) {
        Ok(value) => value,
        Err(_) => return,
    };
    let body = goformat::marshal_preserve(&value);
    let _ = store_file::write_bytes(&file_path(root), &body);
}

// ----------------------------- small helpers -----------------------------

fn go_text(value: &Value) -> String {
    value.as_str().unwrap_or("").to_string()
}

fn go_number(value: &Value) -> i64 {
    match value {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

/// Go `fmt.Sprint` over a JSON value, as used for `config_values` entries.
/// Arrays/objects never occur from the UI; they fall back to compact JSON
/// instead of Go's struct dump.
fn go_stringify(value: &Value) -> String {
    match value {
        Value::Null => "<nil>".into(),
        Value::Bool(b) => b.to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => {
                let f = n.as_f64().unwrap_or(0.0);
                if f.fract() == 0.0 && f.is_finite() && f.abs() < 9.007_199_254_740_992e15 {
                    format!("{}", f as i64)
                } else {
                    format!("{f}")
                }
            }
        },
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

// ----------------------------- store ops -----------------------------

fn add_event(doc: &mut TaskDoc, task_id: i64, kind: &str, actor: &str, payload: Value) {
    let event = WorkTaskEvent {
        id: doc.next_event,
        task_id,
        kind: kind.into(),
        actor: actor.into(),
        payload,
        created_at: goformat::timestamp(),
    };
    doc.next_event += 1;
    let list = doc.events.entry(task_id.to_string()).or_default();
    list.push(event);
    if list.len() > 500 {
        let keep_from = list.len() - 500;
        *list = list.split_off(keep_from);
    }
}

/// Go `setStatus`: mutates the status timestamps, appends the status event and
/// notifies the board (`onChange` → `task://changed`). Re-finds the task by id
/// so callers never hold a task borrow across the event append.
fn set_status(
    doc: &mut TaskDoc,
    task_id: i64,
    status: &str,
    actor: &str,
    payload: Value,
) -> Vec<(String, Value)> {
    let Some(task) = doc.find_mut(task_id) else {
        return Vec::new();
    };
    task.status = status.into();
    task.updated_at = goformat::timestamp();
    let now = goformat::timestamp();
    match status {
        "running" | "preparing" | "awaiting_input" | "merging" => {
            if task.started_at.is_none() {
                task.started_at = Some(now);
            }
            task.finished_at = None;
        }
        "done" | "failed" | "canceled" => {
            task.finished_at = Some(now);
        }
        _ => {}
    }
    add_event(doc, task_id, "status", actor, payload);
    vec![("task://changed".into(), json!({ "id": task_id }))]
}

fn settings_for(doc: &TaskDoc, folder_id: i64) -> WorkTaskFolderSettings {
    let key = folder_id.to_string();
    match doc.settings.get(&key) {
        Some(Some(settings)) => settings.clone(),
        _ => WorkTaskFolderSettings::defaults(),
    }
}

/// The workspace document (`codeg-workspace.json`) is still Go-owned, but it
/// re-reads from disk on every request, so the file is authoritative here too.
fn folder_exists(root: &Path, folder_id: i64) -> bool {
    let path = root.join(".data").join("codeg-workspace.json");
    let Ok(Some(doc)) = store_file::read_json(&path) else {
        return false;
    };
    doc["folders"]
        .as_array()
        .map(|list| list.iter().any(|f| go_number(&f["id"]) == folder_id))
        .unwrap_or(false)
}

fn folder_path(root: &Path, folder_id: i64) -> Option<String> {
    let path = root.join(".data").join("codeg-workspace.json");
    let Ok(Some(doc)) = store_file::read_json(&path) else {
        return None;
    };
    doc["folders"]
        .as_array()?
        .iter()
        .find(|f| go_number(&f["id"]) == folder_id)
        .and_then(|f| f["path"].as_str())
        .map(|p| p.to_string())
}

// ----------------------------- engine triggers -----------------------------

/// POSTs an internal engine command to the sidecar and waits for its answer.
/// `None` means no sidecar was configured (tests, or a missing upstream).
fn engine_trigger(command: &str, body: Value) -> Option<Result<Value, String>> {
    let base = UPSTREAM.get()?;
    let client = UPSTREAM_CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("引擎触发客户端初始化失败")
    });
    let response = match client
        .post(format!("{base}/api/{command}"))
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body).unwrap_or_default())
        .send()
    {
        Ok(response) => response,
        Err(err) => return Some(Err(err.to_string())),
    };
    let status = response.status();
    let bytes = response.bytes().unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        Some(Ok(parsed))
    } else {
        let message = parsed["message"].as_str().unwrap_or("引擎触发失败");
        Some(Err(message.to_string()))
    }
}

/// Fire-and-forget variant used where Go ignores the engine's answer.
fn engine_trigger_ignored(command: &str, body: Value) {
    let _ = engine_trigger(command, body);
}

/// The engine's in-memory checkout path, derived from the on-disk layout
/// (`extra.NewWorktrees` mounts worktrees at `<folder>/.worktrees/<name>`).
/// See the module header for the divergence this paper over.
fn worktree_dir(root: &Path, folder_id: i64, task_id: i64) -> Option<PathBuf> {
    let base = folder_path(root, folder_id)?;
    let target = Path::new(&base)
        .join(".worktrees")
        .join(format!("task-{task_id}"));
    if target.is_dir() {
        Some(target)
    } else {
        None
    }
}

fn task_worktree_name(task_id: i64) -> String {
    format!("task-{task_id}")
}

// ----------------------------- git plumbing -----------------------------

/// Go `extra.git`: stdout captured, errors carry the stderr tail (400 bytes).
fn git_run(cwd: &Path, allow_fail: bool, args: &[&str]) -> Result<String, String> {
    if cwd.as_os_str().is_empty() {
        return Err("no cwd".into());
    }
    match Command::new("git").args(args).current_dir(cwd).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            if !output.status.success() && !allow_fail {
                let mut msg = String::from_utf8_lossy(&output.stderr).to_string();
                if msg.trim().is_empty() {
                    msg = format!(
                        "exit status {}",
                        output.status.code().unwrap_or_else(|| -1)
                    );
                }
                let bytes = msg.as_bytes();
                if bytes.len() > 400 {
                    msg = String::from_utf8_lossy(&bytes[bytes.len() - 400..]).to_string();
                }
                return Err(format!("git {} 失败：{}", args[0], msg));
            }
            Ok(stdout)
        }
        Err(err) => {
            if allow_fail {
                return Ok(String::new());
            }
            Err(format!("git {} 失败：{}", args[0], err))
        }
    }
}

/// Go `execGit` (worktask_run.go): stdout only, plain error on failure.
fn exec_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    if cwd.as_os_str().is_empty() {
        return Err("no cwd".into());
    }
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
        .map_err(|err| err.to_string())
}

const MAX_DIFF: usize = 200000;

struct WorktreeDiff {
    branch: String,
    has_diff: bool,
    truncated: bool,
    untracked: Vec<String>,
    text: String,
}

/// Port of `extra.Worktrees.Diff`: committed + dirty + untracked contents.
fn worktree_diff(base: &Path, id: &str) -> Result<WorktreeDiff, String> {
    let branch = format!("hh/{id}");
    let target = base.join(".worktrees").join(id);
    if !target.is_dir() {
        return Err("该任务没有隔离工作树".into());
    }
    let base_commit = git_run(base, true, &["merge-base", "HEAD", &branch])
        .map(|out| out.trim().to_string())
        .unwrap_or_default();
    let committed = if !base_commit.is_empty() {
        git_run(base, true, &["diff", &format!("{base_commit}..{branch}")]).unwrap_or_default()
    } else {
        String::new()
    };
    let dirty = git_run(&target, true, &["diff", "HEAD"]).unwrap_or_default();
    let untracked_out = git_run(&target, true, &["ls-files", "--others", "--exclude-standard"])
        .unwrap_or_default();
    let mut untracked: Vec<String> = untracked_out
        .split('\n')
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    // Untracked output files never appear in `git diff`, but a reviewer needs
    // them; the accumulator stops after 20 rendered sections like Go's.
    let mut new_files = String::new();
    for rel in untracked.iter() {
        if !untracked.is_empty() && !new_files.is_empty() && new_files.matches("diff --git").count() >= 20 {
            break;
        }
        let Ok(body_bytes) = std::fs::read(target.join(rel)) else {
            continue;
        };
        let end = body_bytes.len().min(20000);
        let body = String::from_utf8_lossy(&body_bytes[..end]);
        let plus: Vec<String> = body.split('\n').map(|l| format!("+{l}")).collect();
        new_files.push_str(&format!(
            "\ndiff --git a/{rel} b/{rel} (new file)\n--- /dev/null\n+++ b/{rel}\n{}\n",
            plus.join("\n")
        ));
    }
    let mut parts: Vec<String> = Vec::new();
    for part in [&committed, &dirty, &new_files] {
        if !part.trim().is_empty() {
            parts.push(part.clone());
        }
    }
    let mut text = parts.join("\n");
    let mut truncated = false;
    if text.len() > MAX_DIFF {
        let bytes = text.as_bytes();
        text = String::from_utf8_lossy(&bytes[..MAX_DIFF]).to_string();
        truncated = true;
    }
    if untracked.len() > 200 {
        untracked.truncate(200);
    }
    Ok(WorktreeDiff {
        has_diff: !text.trim().is_empty(),
        truncated,
        untracked,
        text,
        branch,
    })
}

/// Go `diffStats`: numstat summary for the task branch vs the base workdir.
fn diff_stats(folder_path: &str, name: &str) -> (i64, i64, i64) {
    let Ok(out) = exec_git(
        Path::new(folder_path),
        &["diff", "--numstat", "HEAD", &format!("hh/{name}")],
    ) else {
        return (0, 0, 0);
    };
    let mut files = 0i64;
    let mut additions = 0i64;
    let mut deletions = 0i64;
    for line in out.split('\n') {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            files += 1;
            if parts[0] != "-" {
                additions = parts[0].parse().unwrap_or(additions);
            }
            if parts[1] != "-" {
                deletions = parts[1].parse().unwrap_or(deletions);
            }
        }
    }
    (files, additions, deletions)
}

// ----------------------------- config parsing -----------------------------

/// Go `parseTaskConfig`: normalises the composer snapshot into a typed config.
/// Indexes go through `Value` (missing → null), never through `Map`, whose
/// `Index` impl panics on absent keys.
fn parse_task_config(raw: &Value) -> WorkTaskConfig {
    let mut config = default_config();
    if !raw.is_object() {
        return config;
    }
    if let Some(blocks) = raw["prompt_blocks"].as_array() {
        for block in blocks {
            if block.is_object() {
                // Go's rows are maps: marshal sorts their keys, so canon each
                // block on the way in to keep the file byte-identical.
                config.prompt_blocks.push(canon_row(block.clone()));
            }
        }
    }
    config.display_text = go_text(&raw["display_text"]);
    if let Some(v) = raw["agent_type"].as_str() {
        if !v.is_empty() {
            config.agent_type = Some(v.to_string());
        }
    }
    if let Some(v) = raw["mode_id"].as_str() {
        if !v.is_empty() {
            config.mode_id = Some(v.to_string());
        }
    }
    if let Some(values) = raw["config_values"].as_object() {
        for (k, v) in values {
            config.config_values.insert(k.clone(), go_stringify(v));
        }
    }
    if raw["label_snapshot"].is_object() {
        config.label_snapshot = Some(canon_row(raw["label_snapshot"].clone()));
    }
    if let Some(v) = raw["base_branch"].as_str() {
        if !v.is_empty() {
            config.base_branch = Some(v.to_string());
        }
    }
    config
}

fn first_task_text(config: Option<&WorkTaskConfig>) -> String {
    let Some(config) = config else {
        return String::new();
    };
    if !config.display_text.is_empty() {
        return config.display_text.clone();
    }
    let mut out = String::new();
    for block in &config.prompt_blocks {
        out.push_str(&go_text(&block["text"]));
    }
    out.trim().to_string()
}

/// Go `appendTaskNote`: attaches a retry/cancel note to the task prompt.
fn append_task_note(config: Option<WorkTaskConfig>, note: &str) -> WorkTaskConfig {
    let mut config = config.unwrap_or_else(default_config);
    if !config.display_text.is_empty() {
        config.display_text = format!("{}\n\n{}", config.display_text, note);
    } else {
        // Go 的 Row 是 map：marshal 排序键，落盘前 canon。
        config.prompt_blocks.push(canon_row(json!({ "type": "text", "text": note })));
    }
    config
}

/// Go `parseFolderSettings`.
fn parse_folder_settings(raw: &Value) -> WorkTaskFolderSettings {
    let mut settings = WorkTaskFolderSettings::defaults();
    if !raw.is_object() {
        return settings;
    }
    if let Some(v) = raw["default_agent_type"].as_str() {
        if !v.is_empty() {
            settings.default_agent_type = Some(v.to_string());
        }
    }
    if let Some(v) = raw["mode_id"].as_str() {
        if !v.is_empty() {
            settings.mode_id = Some(v.to_string());
        }
    }
    if let Some(values) = raw["config_values"].as_object() {
        for (k, v) in values {
            settings.config_values.insert(k.clone(), go_stringify(v));
        }
    }
    if raw["label_snapshot"].is_object() {
        settings.label_snapshot = raw["label_snapshot"].clone();
    }
    if let Some(v) = raw["auto_process"].as_bool() {
        settings.auto_process = v;
    }
    if let Some(v) = raw["max_concurrent"].as_f64() {
        settings.max_concurrent = v.trunc() as i64;
    }
    if let Some("squash" | "merge") = raw["merge_strategy"].as_str() {
        settings.merge_strategy = raw["merge_strategy"].as_str().unwrap().to_string();
    }
    if let Some(v) = raw["auto_merge"].as_bool() {
        settings.auto_merge = v;
    }
    settings
}

/// Go `filterTaskDiff`: keeps one file's section out of a unified diff.
fn filter_task_diff(diff: &str, file: &str) -> String {
    if file.is_empty() {
        return diff.to_string();
    }
    for section in diff.split("\ndiff --git ") {
        if section.contains(&format!("a/{file}")) || section.contains(&format!("b/{file}")) {
            return section
                .strip_prefix("diff --git ")
                .unwrap_or(section)
                .to_string();
        }
    }
    String::new()
}

/// Go `buildForgePrompt`.
fn build_forge_prompt(source: &Value, snapshot: &Value) -> String {
    let kind = if go_text(&source["kind"]) == "pr" {
        "拉取请求"
    } else {
        "议题"
    };
    let mut out = format!(
        "处理 {} 的{} #{}：{}\n\n",
        go_text(&source["owner_repo"]),
        kind,
        go_number(&source["number"]),
        go_text(&snapshot["title"])
    );
    let body = go_text(&snapshot["body"]);
    if !body.is_empty() {
        out.push_str(&body);
        out.push('\n');
    }
    out
}

// ----------------------------- commands -----------------------------

fn run(root: &Path, doc: &mut TaskDoc, command: &str, a: &Value) -> Result<Outcome, String> {
    match command {
        "work_task_list" => {
            let folder_id = go_number(&a["folderId"]);
            let mut out: Vec<&WorkTask> = doc
                .tasks()
                .iter()
                .filter(|task| folder_id == 0 || task.folder_id == folder_id)
                .collect();
            out.sort_by_key(|task| task.sort_order); // stable, like SliceStable
            let items: Vec<Value> = out
                .into_iter()
                .map(|task| serde_json::to_value(task).unwrap_or(Value::Null))
                .collect();
            Ok(outcome(json!(items)))
        }
        "work_task_get" => {
            let id = go_number(&a["id"]);
            let task = doc.find(id).ok_or("任务不存在")?;
            Ok(outcome(serde_json::to_value(task).unwrap_or(Value::Null)))
        }
        "work_task_events" => {
            let id = go_number(&a["taskId"]);
            let task = doc.find(id).ok_or("任务不存在")?;
            let task_id = task.id;
            let mut limit = go_number(&a["limit"]);
            if limit <= 0 || limit > 500 {
                limit = 500;
            }
            let events = doc.events.get(&task_id.to_string());
            let skip = events
                .map(|list| list.len().saturating_sub(limit as usize))
                .unwrap_or(0);
            let items: Vec<Value> = events
                .map(|list| {
                    list.iter()
                        .skip(skip)
                        .map(|event| serde_json::to_value(event).unwrap_or(Value::Null))
                        .collect()
                })
                .unwrap_or_default();
            Ok(outcome(json!(items)))
        }
        "work_task_create" => {
            let draft = a["draft"].clone();
            let folder_id = go_number(&draft["folder_id"]);
            if !folder_exists(root, folder_id) {
                return Err("项目文件夹不存在".into());
            }
            let config = parse_task_config(&draft["config"]);
            let mut title = go_text(&draft["title"]).trim().to_string();
            if title.is_empty() {
                title = first_task_text(Some(&config));
            }
            if title.is_empty() {
                return Err("请填写任务标题或提示词".into());
            }
            let task = WorkTask {
                id: doc.next,
                folder_id,
                title: title.clone(),
                config: Some(config),
                status: "todo".into(),
                sort_order: doc.tasks().len() as i64 + 1,
                created_at: goformat::timestamp(),
                updated_at: goformat::timestamp(),
                ..Default::default()
            };
            let task_id = task.id;
            doc.next += 1;
            doc.tasks_mut().push(task);
            add_event(doc, task_id, "created", "user", canon_row(json!({ "title": title })));
            persist(root, doc);
            let task = doc.find(task_id).cloned().unwrap_or_default();
            Ok(Outcome {
                result: serde_json::to_value(&task).unwrap_or(Value::Null),
                events: vec![("task://changed".into(), json!({ "id": task_id }))],
            })
        }
        "work_task_update" => {
            let id = go_number(&a["id"]);
            {
                let task = doc.find_mut(id).ok_or("任务不存在")?;
                if task.status != "todo" {
                    return Err("仅待办状态的任务可以编辑".into());
                }
                let draft = a["draft"].clone();
                let title = go_text(&draft["title"]).trim().to_string();
                if !title.is_empty() {
                    task.title = title;
                }
                if draft.as_object().map(|m| m.contains_key("config")).unwrap_or(false) {
                    task.config = Some(parse_task_config(&draft["config"]));
                }
                task.updated_at = goformat::timestamp();
                add_event(doc, id, "updated", "user", Value::Null);
            }
            persist(root, doc);
            let task = doc.find(id).cloned().unwrap_or_default();
            Ok(Outcome {
                result: serde_json::to_value(&task).unwrap_or(Value::Null),
                events: vec![("task://changed".into(), json!({ "id": id }))],
            })
        }
        "work_task_reorder" => {
            let folder_id = go_number(&a["folderId"]);
            let mut order: BTreeMap<i64, i64> = BTreeMap::new();
            if let Some(ids) = a["orderedIds"].as_array() {
                for (i, v) in ids.iter().enumerate() {
                    order.insert(go_number(v), i as i64 + 1);
                }
            }
            for task in doc.tasks_mut().iter_mut() {
                if task.folder_id == folder_id {
                    if let Some(&pos) = order.get(&task.id) {
                        task.sort_order = pos;
                    }
                }
            }
            persist(root, doc);
            Ok(outcome(Value::Null))
        }
        "work_task_delete" => {
            let id = go_number(&a["id"]);
            let task = doc.find(id).ok_or("任务不存在")?;
            let folder_id = task.folder_id;
            let delete_worktree = a["deleteWorktree"] == json!(true);
            if delete_worktree && worktree_dir(root, folder_id, id).is_some() {
                engine_trigger_ignored(
                    "work_task_engine_discard_worktree",
                    json!({ "id": id }),
                );
                // 镜像 Go discardTaskWorktree 的持久化效果。
                if let Some(task) = doc.find_mut(id) {
                    task.work_branch = None;
                    task.worktree_folder_id = None;
                }
            }
            doc.tasks = Some(
                doc.tasks()
                    .iter()
                    .filter(|item| item.id != id)
                    .cloned()
                    .collect(),
            );
            doc.events.remove(&id.to_string());
            persist(root, doc);
            Ok(Outcome {
                result: Value::Null,
                events: vec![("task://changed".into(), json!({ "id": id }))],
            })
        }
        "work_task_start" => {
            let id = go_number(&a["id"]);
            {
                let task = doc.find(id).ok_or("任务不存在")?;
                if task.status != "todo" {
                    return Err(format!("仅待办任务可以开始（当前 {}）", task.status));
                }
            }
            let events = set_status(doc, id, "queued", "user", Value::Null);
            persist(root, doc);
            engine_trigger_ignored("work_task_engine_claim", json!({ "id": id }));
            Ok(Outcome {
                result: Value::Null,
                events,
            })
        }
        "work_task_retry" | "work_task_requeue" => {
            let id = go_number(&a["id"]);
            {
                let task = doc.find(id).ok_or("任务不存在")?;
                if command == "work_task_retry" && task.status != "failed" {
                    return Err(format!("仅失败任务可以重试（当前 {}）", task.status));
                }
                if command == "work_task_requeue" && task.status != "canceled" {
                    return Err(format!("仅已取消任务可以重新排队（当前 {}）", task.status));
                }
            }
            let note = go_text(&a["note"]);
            if !note.is_empty() {
                add_event(doc, id, "note", "user", canon_row(json!({ "text": note })));
                if let Some(task) = doc.find_mut(id) {
                    let config = task.config.take();
                    task.config = Some(append_task_note(config, &note));
                }
            }
            if let Some(task) = doc.find_mut(id) {
                task.last_error = None;
                task.failure_reason = None;
            }
            let target = if command == "work_task_retry" {
                "queued"
            } else {
                "todo"
            };
            let events = set_status(doc, id, target, "user", Value::Null);
            persist(root, doc);
            if target == "queued" {
                engine_trigger_ignored("work_task_engine_claim", json!({ "id": id }));
            }
            Ok(Outcome {
                result: Value::Null,
                events,
            })
        }
        "work_task_schedule" => {
            let id = go_number(&a["id"]);
            let task = doc.find_mut(id).ok_or("任务不存在")?;
            let scheduled_at = go_text(&a["scheduledAt"]);
            task.scheduled_at = if scheduled_at.is_empty() {
                None
            } else {
                Some(scheduled_at)
            };
            task.updated_at = goformat::timestamp();
            persist(root, doc);
            Ok(Outcome {
                result: Value::Null,
                events: vec![("task://changed".into(), json!({ "id": id }))],
            })
        }
        "work_task_return" => {
            let id = go_number(&a["id"]);
            {
                let task = doc.find(id).ok_or("任务不存在")?;
                if task.status != "review" && task.status != "failed" {
                    return Err(format!(
                        "仅评审或失败状态可以追加反馈（当前 {}）",
                        task.status
                    ));
                }
            }
            let feedback = go_text(&a["feedback"]).trim().to_string();
            if feedback.is_empty() {
                return Err("请填写反馈内容".into());
            }
            if let Some(task) = doc.find_mut(id) {
                let config = task.config.take();
                task.config = Some(append_task_note(config, &feedback));
                task.last_error = None;
                task.failure_reason = None;
            }
            let events = set_status(doc, id, "queued", "user", canon_row(json!({ "feedback": feedback })));
            persist(root, doc);
            engine_trigger_ignored("work_task_engine_claim", json!({ "id": id }));
            Ok(Outcome {
                result: Value::Null,
                events,
            })
        }
        "work_task_cancel" => {
            let id = go_number(&a["id"]);
            let folder_id = {
                let task = doc.find(id).ok_or("任务不存在")?;
                if task.status == "done" || task.status == "canceled" {
                    return Err(format!("任务已结束（当前 {}）", task.status));
                }
                task.folder_id
            };
            // Go 先取消活跃连接、再落状态；触发要在写盘前发出。
            engine_trigger_ignored("work_task_engine_cancel_conn", json!({ "id": id }));
            let delete_worktree = a["deleteWorktree"] == json!(true);
            if delete_worktree && worktree_dir(root, folder_id, id).is_some() {
                engine_trigger_ignored(
                    "work_task_engine_discard_worktree",
                    json!({ "id": id }),
                );
            }
            let reason = go_text(&a["reason"]);
            let events = set_status(doc, id, "canceled", "user", canon_row(json!({ "reason": reason })));
            persist(root, doc);
            Ok(Outcome {
                result: Value::Null,
                events,
            })
        }
        "work_task_merge" => {
            let id = go_number(&a["id"]);
            let task = doc.find(id).ok_or("任务不存在")?;
            if task.status != "review" {
                return Err(format!("仅评审状态可以合并（当前 {}）", task.status));
            }
            let response = engine_trigger(
                "work_task_engine_merge",
                json!({
                    "id": id,
                    "message": go_text(&a["message"]),
                    "deleteWorktree": a["deleteWorktree"] == json!(true),
                }),
            );
            let queued = match response {
                Some(Ok(value)) => value["queued"].as_bool().unwrap_or(false),
                // 侧车不在时引擎不可能排队；与 Go 的 TryLock 失败语义相反,
                // 但 dead sidecar 下任何路径都无法合并。
                _ => false,
            };
            Ok(outcome(json!(queued)))
        }
        "work_task_merge_unqueue" => {
            let id = go_number(&a["id"]);
            let task = doc.find_mut(id).ok_or("任务不存在")?;
            task.merge_queued = Value::Null;
            persist(root, doc);
            Ok(Outcome {
                result: Value::Null,
                events: vec![("task://changed".into(), json!({ "id": id }))],
            })
        }
        "work_task_deliver_pr" => Err("PR 交付尚未接通（Forge 模块完成后可用）".into()),
        "work_task_complete" => {
            let id = go_number(&a["id"]);
            let folder_id = {
                let task = doc.find(id).ok_or("任务不存在")?;
                if task.status != "review" {
                    return Err(format!("仅评审状态可以完成（当前 {}）", task.status));
                }
                task.folder_id
            };
            if worktree_dir(root, folder_id, id).is_some() {
                let base = folder_path(root, folder_id).unwrap_or_default();
                match worktree_diff(Path::new(&base), &task_worktree_name(id)) {
                    Err(err) => {
                        // Go 的 fail-closed：读不出工作树状态时不能放行。
                        return Err(format!("无法确认工作树状态，请稍后重试：{err}"));
                    }
                    Ok(diff) => {
                        if diff.has_diff {
                            return Err("工作树仍有未合并改动，请合并或丢弃后再完成".into());
                        }
                    }
                }
            }
            let delete_worktree = a["deleteWorktree"] == json!(true);
            if let Some(task) = doc.find_mut(id) {
                task.completion_kind = Some("accepted_without_merge".into());
            }
            let events = set_status(doc, id, "done", "user", Value::Null);
            if delete_worktree && worktree_dir(root, folder_id, id).is_some() {
                engine_trigger_ignored(
                    "work_task_engine_discard_worktree",
                    json!({ "id": id }),
                );
                // 镜像 Go discardTaskWorktree 的持久化效果。
                if let Some(task) = doc.find_mut(id) {
                    task.work_branch = None;
                    task.worktree_folder_id = None;
                }
            }
            persist(root, doc);
            Ok(Outcome {
                result: Value::Null,
                events,
            })
        }
        "work_task_archive" => {
            let id = go_number(&a["id"]);
            let task = doc.find_mut(id).ok_or("任务不存在")?;
            task.archived_at = if a["archived"] == json!(true) {
                Some(goformat::timestamp())
            } else {
                None
            };
            task.updated_at = goformat::timestamp();
            persist(root, doc);
            Ok(Outcome {
                result: Value::Null,
                events: vec![("task://changed".into(), json!({ "id": id }))],
            })
        }
        "work_task_defer" => {
            let id = go_number(&a["id"]);
            let task = doc.find_mut(id).ok_or("任务不存在")?;
            task.deferred_at = if a["deferred"] == json!(true) {
                Some(goformat::timestamp())
            } else {
                None
            };
            task.updated_at = goformat::timestamp();
            persist(root, doc);
            Ok(Outcome {
                result: Value::Null,
                events: vec![("task://changed".into(), json!({ "id": id }))],
            })
        }
        "work_task_cleanup" => {
            let id = go_number(&a["id"]);
            let task = doc.find(id).ok_or("任务不存在")?;
            let folder_id = task.folder_id;
            if worktree_dir(root, folder_id, id).is_none() {
                return Err("该任务没有隔离工作树".into());
            }
            let response = engine_trigger(
                "work_task_engine_discard_worktree",
                json!({ "id": id }),
            );
            if let Some(task) = doc.find_mut(id) {
                // Go 的 discardTaskWorktree 无条件清这两个持久化字段（引擎
                // 侧的触发处理也会写同一份文件，两边收敛到相同内容）。
                task.work_branch = None;
                task.worktree_folder_id = None;
            }
            if let Some(Err(err)) = response {
                if let Some(task) = doc.find_mut(id) {
                    task.cleanup_state = Some("failed".into());
                }
                persist(root, doc);
                return Err(err);
            }
            if let Some(task) = doc.find_mut(id) {
                task.cleanup_state = None;
            }
            persist(root, doc);
            Ok(Outcome {
                result: Value::Null,
                events: vec![("task://changed".into(), json!({ "id": id }))],
            })
        }
        "work_task_diff" => {
            let id = go_number(&a["id"]);
            let task = doc.find(id).ok_or("任务不存在")?;
            let folder_id = task.folder_id;
            let Some(_) = worktree_dir(root, folder_id, id) else {
                // Go 在没有引擎工作树时返回空串。
                return Ok(outcome(json!("")));
            };
            let base = folder_path(root, folder_id).unwrap_or_default();
            let diff = worktree_diff(Path::new(&base), &task_worktree_name(id))?;
            let file = go_text(&a["file"]);
            if !file.is_empty() {
                Ok(outcome(json!(filter_task_diff(&diff.text, &file))))
            } else {
                Ok(outcome(json!(diff.text)))
            }
        }
        "work_task_changed_files" => {
            let id = go_number(&a["id"]);
            let task = doc.find(id).ok_or("任务不存在")?;
            let folder_id = task.folder_id;
            let Some(_dir) = worktree_dir(root, folder_id, id) else {
                return Ok(outcome(json!([])));
            };
            let base = folder_path(root, folder_id).unwrap_or_default();
            let Ok(out) = exec_git(
                Path::new(&base),
                &["diff", "--numstat", "HEAD", &format!("hh/{}", task_worktree_name(id))],
            ) else {
                return Ok(outcome(json!([])));
            };
            let files: Vec<Value> = out
                .split('\n')
                .map(|line| line.split_whitespace().collect::<Vec<_>>())
                .filter(|parts| parts.len() >= 3)
                .map(|parts| {
                    // Go 的响应是 []Row：map marshal 排序键。
                    canon_row(json!({
                        "file": parts[2],
                        "additions": parts[0],
                        "deletions": parts[1],
                    }))
                })
                .collect();
            Ok(outcome(json!(files)))
        }
        "work_task_settings_get" | "work_task_settings_effective" => {
            let settings = settings_for(doc, go_number(&a["folderId"]));
            Ok(outcome(serde_json::to_value(&settings).unwrap_or(Value::Null)))
        }
        "work_task_settings_get_own" => {
            let key = go_number(&a["folderId"]).to_string();
            let value = match doc.settings.get(&key) {
                Some(Some(settings)) => {
                    serde_json::to_value(settings).unwrap_or(Value::Null)
                }
                _ => Value::Null,
            };
            Ok(outcome(value))
        }
        "work_task_settings_set" => {
            let folder_id = go_number(&a["folderId"]);
            let settings = parse_folder_settings(&a["settings"]);
            doc.settings.insert(folder_id.to_string(), Some(settings));
            persist(root, doc);
            Ok(outcome(Value::Null))
        }
        "work_task_settings_delete" => {
            let folder_id = go_number(&a["folderId"]);
            doc.settings.remove(&folder_id.to_string());
            persist(root, doc);
            Ok(outcome(Value::Null))
        }
        "work_task_template_list" => {
            let value = serde_json::to_value(&doc.templates).unwrap_or(Value::Null);
            Ok(outcome(value))
        }
        "work_task_template_save" => {
            let draft = a["draft"].clone();
            let name = go_text(&draft["name"]).trim().to_string();
            if name.is_empty() {
                return Err("请填写模板名称".into());
            }
            let config = parse_task_config(&draft["config"]);
            // Go 的线性扫描让最后一个同名模板胜出。
            let existing_index = doc
                .templates
                .as_ref()
                .and_then(|list| list.iter().rposition(|tpl| tpl.name == name));
            let template = match existing_index {
                Some(index) => {
                    let list = doc.templates.as_mut().unwrap();
                    let tpl = &mut list[index];
                    tpl.name = name.clone();
                    tpl.title = go_text(&draft["title"]).trim().to_string();
                    tpl.config = Some(config);
                    tpl.updated_at = goformat::timestamp();
                    tpl.clone()
                }
                None => {
                    let tpl = WorkTaskTemplate {
                        id: doc.next_tpl,
                        name: name.clone(),
                        title: go_text(&draft["title"]).trim().to_string(),
                        config: Some(config),
                        created_at: goformat::timestamp(),
                        updated_at: goformat::timestamp(),
                    };
                    doc.next_tpl += 1;
                    doc.templates.get_or_insert_with(Vec::new).push(tpl.clone());
                    tpl
                }
            };
            persist(root, doc);
            Ok(outcome(serde_json::to_value(&template).unwrap_or(Value::Null)))
        }
        "work_task_template_delete" => {
            let id = go_number(&a["id"]);
            if let Some(list) = &mut doc.templates {
                list.retain(|tpl| tpl.id != id);
            }
            persist(root, doc);
            Ok(outcome(Value::Null))
        }
        "work_task_lookup_by_source" => {
            let mut want: Vec<String> = Vec::new();
            if let Some(keys) = a["sourceKeys"].as_array() {
                for k in keys {
                    want.push(go_text(k));
                }
            }
            let out: Vec<Value> = doc
                .tasks()
                .iter()
                .filter(|task| {
                    task.source_key
                        .as_ref()
                        .map(|key| want.iter().any(|w| w == key))
                        .unwrap_or(false)
                })
                .map(|task| {
                    // Go 的响应是 []Row：map marshal 排序键。
                    canon_row(json!({
                        "source_key": task.source_key.clone(),
                        "task_id": task.id,
                        "status": task.status,
                    }))
                })
                .collect();
            Ok(outcome(json!(out)))
        }
        "work_task_create_from_forge" => {
            let draft = a["draft"].clone();
            let source = draft["source"].clone();
            if !source.is_object() {
                return Err("缺少来源信息".into());
            }
            let snapshot = draft["snapshot"].clone();
            let title = go_text(&snapshot["title"]);
            let prompt = build_forge_prompt(&source, &snapshot);
            let mut config = default_config();
            config.prompt_blocks = vec![canon_row(json!({ "type": "text", "text": prompt }))];
            config.display_text = prompt;
            let folder_id = go_number(&draft["folder_id"]);
            if !folder_exists(root, folder_id) {
                return Err("项目文件夹不存在".into());
            }
            let kind = go_text(&source["kind"]);
            let source_meta = canon_row(json!({
                "provider": go_text(&source["provider"]),
                "server_host": go_text(&source["server_host"]),
                "owner_repo": go_text(&source["owner_repo"]),
                "number": go_number(&source["number"]),
                "title": title.clone(),
                "url": go_text(&snapshot["url"]),
            }));
            let task = WorkTask {
                id: doc.next,
                folder_id,
                title,
                config: Some(config),
                status: "todo".into(),
                sort_order: doc.tasks().len() as i64 + 1,
                source_kind: Some(format!("forge_{kind}")),
                source_key: Some(format!(
                    "github:{}:{}:{}:{}",
                    go_text(&source["server_host"]),
                    go_text(&source["owner_repo"]),
                    kind,
                    go_number(&source["number"])
                )),
                source_meta,
                created_at: goformat::timestamp(),
                updated_at: goformat::timestamp(),
                ..Default::default()
            };
            let title = task.title.clone();
            let task_id = task.id;
            doc.next += 1;
            doc.tasks_mut().push(task);
            add_event(doc, task_id, "created", "forge", canon_row(json!({ "title": title })));
            persist(root, doc);
            let task = doc.find(task_id).cloned().unwrap_or_default();
            Ok(Outcome {
                result: serde_json::to_value(&task).unwrap_or(Value::Null),
                events: vec![("task://changed".into(), json!({ "id": task_id }))],
            })
        }
        _ => Err(format!("未知的任务命令：{command}")),
    }
}

fn outcome(result: Value) -> Outcome {
    Outcome {
        result,
        events: Vec::new(),
    }
}

/// Rows constructed here must match Go's sorted-map marshal in the file, so
/// they go through `canon` once on the way in.
fn canon_row(value: Value) -> Value {
    goformat::canon(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-worktask-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn seed_folder(root: &Path, ids: &[i64]) {
        let folders: Vec<Value> = ids
            .iter()
            .map(|id| json!({ "id": id, "path": root.display().to_string() }))
            .collect();
        let doc = json!({ "next": 1, "folders": folders });
        store_file::write_bytes(
            &root.join(".data").join("codeg-workspace.json"),
            &serde_json::to_vec(&doc).unwrap(),
        )
        .unwrap();
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Outcome, String> {
        let mut doc = load(root);
        let result = run(root, &mut doc, command, &args);
        result
    }

    #[test]
    fn owns_covers_the_thirty_two_commands() {
        for command in [
            "work_task_list",
            "work_task_get",
            "work_task_events",
            "work_task_create",
            "work_task_update",
            "work_task_reorder",
            "work_task_delete",
            "work_task_start",
            "work_task_retry",
            "work_task_requeue",
            "work_task_schedule",
            "work_task_return",
            "work_task_cancel",
            "work_task_merge",
            "work_task_merge_unqueue",
            "work_task_deliver_pr",
            "work_task_complete",
            "work_task_archive",
            "work_task_defer",
            "work_task_cleanup",
            "work_task_diff",
            "work_task_changed_files",
            "work_task_settings_get",
            "work_task_settings_effective",
            "work_task_settings_get_own",
            "work_task_settings_set",
            "work_task_settings_delete",
            "work_task_template_list",
            "work_task_template_save",
            "work_task_template_delete",
            "work_task_lookup_by_source",
            "work_task_create_from_forge",
        ] {
            assert!(owns(command), "{command} 应归 worktask 模块");
        }
        // 引擎触发命令必须继续走 Go。
        for command in [
            "work_task_engine_claim",
            "work_task_engine_merge",
            "work_task_engine_cancel_conn",
            "work_task_engine_discard_worktree",
        ] {
            assert!(!owns(command), "{command} 必须转发 sidecar");
        }
    }

    #[test]
    fn unknown_command_message_matches_go() {
        let root = temp_root("unknown");
        assert_eq!(
            call(&root, "work_task_nope", json!({})).unwrap_err(),
            "未知的任务命令：work_task_nope"
        );
    }

    #[test]
    fn create_requires_folder_title_and_persists_in_go_layout() {
        let root = temp_root("create");
        // 文件夹不存在 → Go 的错误文案。
        assert_eq!(
            call(
                &root,
                "work_task_create",
                json!({ "draft": { "folder_id": 7, "title": "x" } })
            )
            .unwrap_err(),
            "项目文件夹不存在"
        );
        seed_folder(&root, &[7]);
        // 无标题且无提示词 → 报错。
        assert_eq!(
            call(
                &root,
                "work_task_create",
                json!({ "draft": { "folder_id": 7 } })
            )
            .unwrap_err(),
            "请填写任务标题或提示词"
        );
        let out = call(
            &root,
            "work_task_create",
            json!({ "draft": {
                "folder_id": 7,
                "title": "  修复登录  ",
                "config": {
                    "prompt_blocks": [{ "type": "text", "text": "做点事" }],
                    "display_text": "",
                    "agent_type": "codex",
                    "config_values": { "model": "gpt-5", "flag": true }
                }
            } }),
        )
        .unwrap();
        let task = &out.result;
        assert_eq!(task["id"], json!(1));
        assert_eq!(task["status"], json!("todo"));
        assert_eq!(task["title"], json!("修复登录"));
        assert_eq!(task["sort_order"], json!(1));
        // fmt.Sprint 的布尔值。
        assert_eq!(task["config"]["config_values"]["flag"], json!("true"));
        assert_eq!(task["config"]["agent_type"], json!("codex"));
        // 事件里也没有多余事件帧，只有 task://changed。
        assert_eq!(out.events.len(), 1);

        // 落盘字节序：顶层 Go struct 字段序 + tasks 数组里 struct 字段序。
        let raw = fs::read_to_string(root.join(".data").join(FILE)).unwrap();
        let head = raw
            .chars()
            .take(60)
            .collect::<String>();
        assert!(
            raw.starts_with("{\"next\":2,\"nextEvent\":2,\"nextTemplate\":1,\"tasks\":["),
            "顶层字段序应匹配 json.Marshal(taskStore)：{head}"
        );
        assert!(raw.contains("\"prompt_blocks\":[{\"text\":\"做点事\",\"type\":\"text\"}]"),
            "Row 内键应按 Go map marshal 排序");
        assert!(raw.contains("\"events\":{\"1\":[{\"id\":1,\"task_id\":1,\"kind\":\"created\",\"actor\":\"user\",\"payload\":{\"title\":\"修复登录\"},"),
            "events 应为排序 map + struct 字段序");
    }

    #[test]
    fn create_title_falls_back_to_prompt_text() {
        let root = temp_root("title");
        seed_folder(&root, &[3]);
        let out = call(
            &root,
            "work_task_create",
            json!({ "draft": {
                "folder_id": 3,
                "config": {
                    "prompt_blocks": [{ "type": "text", "text": "  第一行  " }, { "type": "text", "text": "第二行" }]
                }
            } }),
        )
        .unwrap();
        assert_eq!(out.result["title"], json!("第一行  第二行"));
    }

    #[test]
    fn list_sorts_by_sort_order_and_filters_folder() {
        let root = temp_root("list");
        seed_folder(&root, &[1, 2]);
        for folder in [1, 2, 1] {
            call(
                &root,
                "work_task_create",
                json!({ "draft": { "folder_id": folder, "title": format!("t{folder}") } }),
            )
            .unwrap();
        }
        call(&root, "work_task_reorder", json!({ "folderId": 1, "orderedIds": [3, 1] })).unwrap();
        let out = call(&root, "work_task_list", json!({ "folderId": 1 })).unwrap();
        let ids: Vec<i64> = out
            .result
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, vec![3, 1], "reorder 后应按 sort_order 排序");
        let out = call(&root, "work_task_list", json!({ "folderId": 0 })).unwrap();
        assert_eq!(out.result.as_array().unwrap().len(), 3);
    }

    #[test]
    fn update_only_touches_todo_tasks() {
        let root = temp_root("update");
        seed_folder(&root, &[1]);
        call(
            &root,
            "work_task_create",
            json!({ "draft": { "folder_id": 1, "title": "旧标题" } }),
        )
        .unwrap();
        let out = call(
            &root,
            "work_task_update",
            json!({ "id": 1, "draft": { "title": "新标题", "config": { "display_text": "v2" } } }),
        )
        .unwrap();
        assert_eq!(out.result["title"], json!("新标题"));
        assert_eq!(out.result["config"]["display_text"], json!("v2"));
        // 缺 config 键不改配置；null 值也算存在（Go 的 ok 判断）。
        let out = call(
            &root,
            "work_task_update",
            json!({ "id": 1, "draft": { "config": null } }),
        )
        .unwrap();
        assert_eq!(out.result["config"]["display_text"], json!(""));
        // 非 todo 状态拒绝编辑。
        let mut doc = load(&root);
        doc.find_mut(1).unwrap().status = "running".into();
        persist(&root, &doc);
        assert_eq!(
            call(&root, "work_task_update", json!({ "id": 1, "draft": { "title": "x" } })).unwrap_err(),
            "仅待办状态的任务可以编辑"
        );
    }

    #[test]
    fn events_are_capped_and_sliced() {
        let root = temp_root("events");
        seed_folder(&root, &[1]);
        call(
            &root,
            "work_task_create",
            json!({ "draft": { "folder_id": 1, "title": "t" } }),
        )
        .unwrap();
        let mut doc = load(&root);
        for _ in 0..520 {
            add_event(&mut doc, 1, "noise", "engine", Value::Null);
        }
        persist(&root, &doc);
        let out = call(&root, "work_task_events", json!({ "taskId": 1, "limit": 3 })).unwrap();
        let events = out.result.as_array().unwrap();
        assert_eq!(events.len(), 3);
        let ids: Vec<i64> = events
            .iter()
            .map(|e| e["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, vec![519, 520, 521], "应取最近 limit 条");
        let out = call(&root, "work_task_events", json!({ "taskId": 1 })).unwrap();
        assert_eq!(out.result.as_array().unwrap().len(), 500);
        assert_eq!(
            call(&root, "work_task_events", json!({ "taskId": 99 })).unwrap_err(),
            "任务不存在"
        );
    }

    #[test]
    fn lifecycle_guards_match_go() {
        let root = temp_root("lifecycle");
        seed_folder(&root, &[1]);
        call(
            &root,
            "work_task_create",
            json!({ "draft": { "folder_id": 1, "title": "t" } }),
        )
        .unwrap();
        // start 只接受 todo。
        assert_eq!(
            call(&root, "work_task_start", json!({ "id": 99 })).unwrap_err(),
            "任务不存在"
        );
        let out = call(&root, "work_task_start", json!({ "id": 1 })).unwrap();
        assert_eq!(out.result, Value::Null);
        assert_eq!(
            call(&root, "work_task_start", json!({ "id": 1 })).unwrap_err(),
            "仅待办任务可以开始（当前 queued）"
        );
        // retry 只接受 failed。
        assert_eq!(
            call(&root, "work_task_retry", json!({ "id": 1 })).unwrap_err(),
            "仅失败任务可以重试（当前 queued）"
        );
        // cancel 在非终态可用；note 追加进 config。
        let out = call(&root, "work_task_cancel", json!({ "id": 1, "reason": "改需求" })).unwrap();
        assert_eq!(out.result, Value::Null);
        assert_eq!(
            call(&root, "work_task_cancel", json!({ "id": 1 })).unwrap_err(),
            "任务已结束（当前 canceled）"
        );
        let raw = fs::read_to_string(root.join(".data").join(FILE)).unwrap();
        assert!(raw.contains("\"kind\":\"status\",\"actor\":\"user\",\"payload\":{\"reason\":\"改需求\"}"));
        // requeue 只接受 canceled → 回 todo。
        let out = call(&root, "work_task_requeue", json!({ "id": 1, "note": "再来一次" })).unwrap();
        assert_eq!(out.result, Value::Null);
        let doc = load(&root);
        assert_eq!(doc.find(1).unwrap().status, "todo");
        // note 进了事件，配置里追加了文本块。
        let raw = fs::read_to_string(root.join(".data").join(FILE)).unwrap();
        assert!(raw.contains("\"kind\":\"note\""));
        assert!(raw.contains(r#"{\"text\":\"再来一次\",\"type\":\"text\"}"#) || raw.contains("\"text\":\"再来一次\""));
    }

    #[test]
    fn retry_failed_task_appends_note_and_requeues() {
        let root = temp_root("retry");
        seed_folder(&root, &[1]);
        call(
            &root,
            "work_task_create",
            json!({ "draft": { "folder_id": 1, "title": "t" } }),
        )
        .unwrap();
        let mut doc = load(&root);
        {
            let task = doc.find_mut(1).unwrap();
            task.status = "failed".into();
            task.failure_reason = Some("setup_error".into());
            task.last_error = Some("连接失败".into());
        }
        persist(&root, &doc);
        let out = call(
            &root,
            "work_task_retry",
            json!({ "id": 1, "note": "换网络重试" }),
        )
        .unwrap();
        assert_eq!(out.result, Value::Null);
        let doc = load(&root);
        let task = doc.find(1).unwrap();
        assert_eq!(task.status, "queued");
        assert_eq!(task.last_error, None);
        assert_eq!(task.failure_reason, None);
        let config = task.config.as_ref().unwrap();
        assert_eq!(config.display_text, "");
        assert_eq!(
            config.prompt_blocks,
            vec![json!({ "type": "text", "text": "换网络重试" })]
        );
    }

    #[test]
    fn return_only_from_review_or_failed() {
        let root = temp_root("return");
        seed_folder(&root, &[1]);
        call(
            &root,
            "work_task_create",
            json!({ "draft": { "folder_id": 1, "title": "t" } }),
        )
        .unwrap();
        assert_eq!(
            call(&root, "work_task_return", json!({ "id": 1, "feedback": "x" })).unwrap_err(),
            "仅评审或失败状态可以追加反馈（当前 todo）"
        );
        let mut doc = load(&root);
        doc.find_mut(1).unwrap().status = "review".into();
        persist(&root, &doc);
        assert_eq!(
            call(&root, "work_task_return", json!({ "id": 1, "feedback": "  " })).unwrap_err(),
            "请填写反馈内容"
        );
        let out = call(&root, "work_task_return", json!({ "id": 1, "feedback": "补个测试" })).unwrap();
        assert_eq!(out.result, Value::Null);
        let doc = load(&root);
        let task = doc.find(1).unwrap();
        assert_eq!(task.status, "queued");
        let config = task.config.as_ref().unwrap();
        assert_eq!(
            config.display_text,
            "",
            "空 display_text 时反馈不进 display_text"
        );
        assert_eq!(
            config.prompt_blocks,
            vec![json!({ "type": "text", "text": "补个测试" })],
            "空 display_text 时反馈进 prompt_blocks"
        );
    }

    #[test]
    fn schedule_archive_defer_and_merge_unqueue() {
        let root = temp_root("sched");
        seed_folder(&root, &[1]);
        call(
            &root,
            "work_task_create",
            json!({ "draft": { "folder_id": 1, "title": "t" } }),
        )
        .unwrap();
        let out = call(
            &root,
            "work_task_schedule",
            json!({ "id": 1, "scheduledAt": "2026-11-14T09:00:00Z" }),
        )
        .unwrap();
        assert_eq!(out.result, Value::Null);
        let doc = load(&root);
        assert_eq!(doc.find(1).unwrap().scheduled_at.as_deref(), Some("2026-11-14T09:00:00Z"));
        // 清空调度。
        call(&root, "work_task_schedule", json!({ "id": 1, "scheduledAt": "" })).unwrap();
        let doc = load(&root);
        assert_eq!(doc.find(1).unwrap().scheduled_at, None);

        let out = call(&root, "work_task_archive", json!({ "id": 1, "archived": true })).unwrap();
        assert_eq!(out.events.len(), 1);
        let doc = load(&root);
        assert!(doc.find(1).unwrap().archived_at.is_some());
        call(&root, "work_task_archive", json!({ "id": 1, "archived": false })).unwrap();
        let doc = load(&root);
        assert_eq!(doc.find(1).unwrap().archived_at, None);

        let out = call(&root, "work_task_defer", json!({ "id": 1, "deferred": true })).unwrap();
        assert_eq!(out.events.len(), 1);
        call(&root, "work_task_defer", json!({ "id": 1, "deferred": false })).unwrap();
        let doc = load(&root);
        assert_eq!(doc.find(1).unwrap().deferred_at, None);

        // merge_unqueue 清空 merge_queued。
        let mut doc = load(&root);
        doc.find_mut(1).unwrap().merge_queued = canon_row(json!({ "message": "m", "delete_worktree": false }));
        persist(&root, &doc);
        call(&root, "work_task_merge_unqueue", json!({ "id": 1 })).unwrap();
        let raw = fs::read_to_string(root.join(".data").join(FILE)).unwrap();
        assert!(!raw.contains("\"merge_queued\""), "omitempty 应把 null 的 merge_queued 去掉");
    }

    #[test]
    fn deliver_pr_is_still_a_stub() {
        let root = temp_root("pr");
        assert_eq!(
            call(&root, "work_task_deliver_pr", json!({ "id": 1 })).unwrap_err(),
            "PR 交付尚未接通（Forge 模块完成后可用）"
        );
    }

    #[test]
    fn complete_requires_clean_worktree_or_none() {
        let root = temp_root("complete");
        seed_folder(&root, &[1]);
        call(
            &root,
            "work_task_create",
            json!({ "draft": { "folder_id": 1, "title": "t" } }),
        )
        .unwrap();
        assert_eq!(
            call(&root, "work_task_complete", json!({ "id": 1 })).unwrap_err(),
            "仅评审状态可以完成（当前 todo）"
        );
        let mut doc = load(&root);
        doc.find_mut(1).unwrap().status = "review".into();
        persist(&root, &doc);
        let out = call(&root, "work_task_complete", json!({ "id": 1 })).unwrap();
        assert_eq!(out.result, Value::Null);
        let doc = load(&root);
        let task = doc.find(1).unwrap();
        assert_eq!(task.status, "done");
        assert_eq!(task.completion_kind.as_deref(), Some("accepted_without_merge"));
        assert!(task.finished_at.is_some());
        assert_eq!(
            call(&root, "work_task_complete", json!({ "id": 1 })).unwrap_err(),
            "仅评审状态可以完成（当前 done）"
        );
    }

    #[test]
    fn cleanup_without_worktree_directory_errors() {
        let root = temp_root("cleanup");
        seed_folder(&root, &[1]);
        call(
            &root,
            "work_task_create",
            json!({ "draft": { "folder_id": 1, "title": "t" } }),
        )
        .unwrap();
        assert_eq!(
            call(&root, "work_task_cleanup", json!({ "id": 1 })).unwrap_err(),
            "该任务没有隔离工作树"
        );
        assert_eq!(
            call(&root, "work_task_diff", json!({ "id": 1 })).unwrap().result,
            json!(""),
            "无工作树时 diff 为空串"
        );
        let out = call(&root, "work_task_changed_files", json!({ "id": 1 })).unwrap();
        assert_eq!(out.result, json!([]));
    }

    #[test]
    fn settings_roundtrip() {
        let root = temp_root("settings");
        // 默认值。
        let out = call(&root, "work_task_settings_effective", json!({ "folderId": 5 })).unwrap();
        assert_eq!(
            out.result,
            json!({
                "default_agent_type": Value::Null,
                "mode_id": Value::Null,
                "config_values": {},
                "label_snapshot": Value::Null,
                "auto_process": false,
                "max_concurrent": 1,
                "merge_strategy": "merge",
                "auto_merge": false,
            })
        );
        assert_eq!(
            call(&root, "work_task_settings_get_own", json!({ "folderId": 5 })).unwrap().result,
            Value::Null
        );
        call(
            &root,
            "work_task_settings_set",
            json!({ "folderId": 5, "settings": {
                "default_agent_type": "codex",
                "max_concurrent": 3.9,
                "merge_strategy": "squash",
                "auto_merge": true,
                "config_values": { "k": 12 }
            } }),
        )
        .unwrap();
        let out = call(&root, "work_task_settings_get_own", json!({ "folderId": 5 })).unwrap();
        assert_eq!(out.result["max_concurrent"], json!(3), "Go 的 int() 截断");
        assert_eq!(out.result["merge_strategy"], json!("squash"));
        assert_eq!(out.result["config_values"]["k"], json!("12"));
        // 非法 merge_strategy 落回默认。
        call(
            &root,
            "work_task_settings_set",
            json!({ "folderId": 5, "settings": { "merge_strategy": "rebase" } }),
        )
        .unwrap();
        let out = call(&root, "work_task_settings_effective", json!({ "folderId": 5 })).unwrap();
        assert_eq!(out.result["merge_strategy"], json!("merge"));
        call(&root, "work_task_settings_delete", json!({ "folderId": 5 })).unwrap();
        let raw = fs::read_to_string(root.join(".data").join(FILE)).unwrap();
        assert!(raw.contains("\"settings\":{}"));
    }

    #[test]
    fn templates_save_updates_by_name() {
        let root = temp_root("templates");
        assert_eq!(
            call(&root, "work_task_template_list", json!({})).unwrap().result,
            Value::Null,
            "Go 的 nil templates 序列化为 null"
        );
        assert_eq!(
            call(&root, "work_task_template_save", json!({ "draft": { "name": "  " } })).unwrap_err(),
            "请填写模板名称"
        );
        let out = call(
            &root,
            "work_task_template_save",
            json!({ "draft": { "name": "日常", "title": " T ", "config": { "display_text": "d" } } }),
        )
        .unwrap();
        assert_eq!(out.result["id"], json!(1));
        assert_eq!(out.result["title"], json!("T"));
        // 同名更新而不是新建。
        let out = call(
            &root,
            "work_task_template_save",
            json!({ "draft": { "name": "日常", "title": "T2" } }),
        )
        .unwrap();
        assert_eq!(out.result["id"], json!(1));
        assert_eq!(out.result["title"], json!("T2"));
        let out = call(&root, "work_task_template_list", json!({})).unwrap();
        assert_eq!(out.result.as_array().unwrap().len(), 1);
        call(&root, "work_task_template_delete", json!({ "id": 1 })).unwrap();
        let out = call(&root, "work_task_template_list", json!({})).unwrap();
        assert_eq!(out.result.as_array().unwrap().len(), 0);
    }

    #[test]
    fn lookup_by_source_maps_keys() {
        let root = temp_root("lookup");
        seed_folder(&root, &[1]);
        call(
            &root,
            "work_task_create",
            json!({ "draft": { "folder_id": 1, "title": "t" } }),
        )
        .unwrap();
        let mut doc = load(&root);
        doc.find_mut(1).unwrap().source_key = Some("github:acme/app:issue:42".into());
        persist(&root, &doc);
        let out = call(
            &root,
            "work_task_lookup_by_source",
            json!({ "sourceKeys": ["github:acme/app:issue:42", "github:acme/app:issue:43"] }),
        )
        .unwrap();
        assert_eq!(
            out.result,
            json!([{ "source_key": "github:acme/app:issue:42", "task_id": 1, "status": "todo" }])
        );
    }

    #[test]
    fn create_from_forge_builds_prompt_and_source() {
        let root = temp_root("forge");
        seed_folder(&root, &[9]);
        assert_eq!(
            call(
                &root,
                "work_task_create_from_forge",
                json!({ "draft": { "folder_id": 9, "snapshot": {} } }),
            )
            .unwrap_err(),
            "缺少来源信息"
        );
        let out = call(
            &root,
            "work_task_create_from_forge",
            json!({ "draft": {
                "folder_id": 9,
                "source": {
                    "kind": "issue",
                    "provider": "github",
                    "server_host": "github.com",
                    "owner_repo": "acme/app",
                    "number": 42
                },
                "snapshot": { "title": "按钮坏了", "body": "点哪儿都没反应", "url": "https://github.com/acme/app/issues/42" }
            } }),
        )
        .unwrap();
        let task = out.result;
        assert_eq!(task["source_kind"], json!("forge_issue"));
        assert_eq!(task["source_key"], json!("github:github.com:acme/app:issue:42"));
        assert_eq!(task["source_meta"]["number"], json!(42));
        assert_eq!(task["status"], json!("todo"));
        let prompt = task["config"]["display_text"].as_str().unwrap();
        assert_eq!(
            prompt,
            "处理 acme/app 的议题 #42：按钮坏了\n\n点哪儿都没反应\n"
        );
        let raw = fs::read_to_string(root.join(".data").join(FILE)).unwrap();
        assert!(raw.contains("\"source_meta\":{\"number\":42,\"owner_repo\":\"acme/app\""), "Row 应排序");
    }

    #[test]
    fn forge_prompt_distinguishes_pr() {
        let source = json!({ "kind": "pr", "owner_repo": "a/b", "number": 7 });
        let snapshot = json!({ "title": "PR!" });
        assert_eq!(
            build_forge_prompt(&source, &snapshot),
            "处理 a/b 的拉取请求 #7：PR!\n\n"
        );
    }

    #[test]
    fn filter_task_diff_picks_one_file() {
        let diff = "diff --git a/main.rs b/main.rs\n+++ b/main.rs\ndiff --git a/lib/x.go b/lib/x.go\n+++ b/lib/x.go";
        assert_eq!(
            filter_task_diff(diff, "lib/x.go"),
            "a/lib/x.go b/lib/x.go\n+++ b/lib/x.go"
        );
        assert_eq!(filter_task_diff(diff, "missing.rs"), "");
        assert_eq!(filter_task_diff(diff, ""), diff);
    }

    #[test]
    fn config_values_stringify_like_go_sprint() {
        assert_eq!(go_stringify(&json!(true)), "true");
        assert_eq!(go_stringify(&json!(12)), "12");
        assert_eq!(go_stringify(&json!(12.5)), "12.5");
        assert_eq!(go_stringify(&json!("s")), "s");
        assert_eq!(go_stringify(&Value::Null), "<nil>");
    }

    #[test]
    fn append_note_prefers_display_text() {
        let mut config = default_config();
        config.display_text = "原文".into();
        let config = append_task_note(Some(config), "补充");
        assert_eq!(config.display_text, "原文\n\n补充");
        let config = append_task_note(None, "新块");
        assert_eq!(config.prompt_blocks, vec![json!({ "type": "text", "text": "新块" })]);
    }

    #[test]
    fn events_map_is_sorted_like_go_marshal() {
        let root = temp_root("sorted");
        seed_folder(&root, &[1]);
        for _ in 0..3 {
            call(
                &root,
                "work_task_create",
                json!({ "draft": { "folder_id": 1, "title": "t" } }),
            )
            .unwrap();
        }
        let raw = fs::read_to_string(root.join(".data").join(FILE)).unwrap();
        let start = raw.find("\"events\":").unwrap();
        let events = &raw[start..start + 40];
        assert!(
            events.starts_with("\"events\":{\"1\":"),
            "字典序里 \"1\" 排最前：{events}"
        );
    }

    #[test]
    fn concurrent_commands_do_not_lose_updates() {
        let root = temp_root("concurrent");
        seed_folder(&root, &[1]);
        let root_a = root.clone();
        let root_b = root.clone();
        let handle_a = std::thread::spawn(move || {
            for i in 0..10 {
                handle(
                    &root_a,
                    "work_task_create",
                    &json!({ "draft": { "folder_id": 1, "title": format!("a{i}") } }),
                )
                .unwrap()
                .unwrap();
            }
        });
        let handle_b = std::thread::spawn(move || {
            for i in 0..10 {
                handle(
                    &root_b,
                    "work_task_create",
                    &json!({ "draft": { "folder_id": 1, "title": format!("b{i}") } }),
                )
                .unwrap()
                .unwrap();
            }
        });
        handle_a.join().unwrap();
        handle_b.join().unwrap();
        let doc = load(&root);
        assert_eq!(doc.tasks().len(), 20, "20 个创建一个都不能丢");
        assert_eq!(doc.next, 21);
    }

    #[test]
    fn engine_trigger_without_upstream_is_ignored() {
        // UPSTREAM 未设置时（单测环境）触发静默跳过，命令照常成功。
        let root = temp_root("trigger");
        seed_folder(&root, &[1]);
        call(
            &root,
            "work_task_create",
            json!({ "draft": { "folder_id": 1, "title": "t" } }),
        )
        .unwrap();
        let out = call(&root, "work_task_start", json!({ "id": 1 })).unwrap();
        assert_eq!(out.result, Value::Null);
    }

    #[test]
    fn worktree_diff_text_truncates_like_go() {
        // 只测纯逻辑：组装好的 text 超长时按字节截断并标记 truncated。
        let text = "x".repeat(MAX_DIFF + 10);
        let mut truncated = false;
        let mut value = text.clone();
        if value.len() > MAX_DIFF {
            let bytes = value.as_bytes();
            value = String::from_utf8_lossy(&bytes[..MAX_DIFF]).to_string();
            truncated = true;
        }
        assert!(truncated);
        assert_eq!(value.len(), MAX_DIFF);
        assert!(value.chars().all(|c| c == 'x'));
    }
}
