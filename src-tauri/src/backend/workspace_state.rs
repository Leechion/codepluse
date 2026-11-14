//! 工作区状态流：`server-go/internal/codeg/workspace.go` 的移植（3 条命令）。
//!
//! `start/stop_workspace_state_stream` 维护引用计数的观察器，创建时起一个
//! 1.5 秒轮询任务；`get_workspace_snapshot` 顺手也建观察器（refs 不加，
//! 与 Go 的行为一致）。扫描 = 文件树 + 全量文件戳（size:mtime）diff +
//! git 状态聚合；指纹变化才 bump seq 并广播
//! `folder://workspace-state-event`（tree_replace + git_replace 全量载荷）。
//!
//! 扫描要遍历整棵树并调 git 子进程（秒级），全部跑在阻塞池。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use crate::backend::{emit_event, git, goformat};

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "start_workspace_state_stream" | "stop_workspace_state_stream" | "get_workspace_snapshot"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn txt(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// files.go:ignoredDirs。
const IGNORED_DIRS: [&str; 6] = ["node_modules", ".git", ".next", ".data", "dist", "target"];

struct WatchInner {
    refs: i64,
    seq: i64,
    fingerprint: String,
    files: HashMap<String, String>,
    stopped: bool,
}

struct WorkspaceWatch {
    root: String,
    inner: Mutex<WatchInner>,
}

fn watches() -> &'static Mutex<HashMap<PathBuf, Arc<WorkspaceWatch>>> {
    static WATCHES: OnceLock<Mutex<HashMap<PathBuf, Arc<WorkspaceWatch>>>> = OnceLock::new();
    WATCHES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let resolved = git::resolve(root, &txt(args, "rootPath"))?;
    let key = resolved.clone();

    let watch = {
        let mut map = watches().lock().unwrap();
        if command == "stop_workspace_state_stream" {
            if let Some(w) = map.get(&key) {
                let mut inner = w.inner.lock().unwrap();
                inner.refs -= 1;
                if inner.refs <= 0 {
                    inner.stopped = true;
                    drop(inner);
                    map.remove(&key);
                }
            }
            return Ok(Value::Null);
        }
        match map.get(&key) {
            Some(w) => {
                if command == "start_workspace_state_stream" {
                    w.inner.lock().unwrap().refs += 1;
                }
                Arc::clone(w)
            }
            None => {
                let w = Arc::new(WorkspaceWatch {
                    root: resolved.to_string_lossy().to_string(),
                    inner: Mutex::new(WatchInner {
                        refs: 0,
                        seq: 0,
                        fingerprint: String::new(),
                        files: HashMap::new(),
                        stopped: false,
                    }),
                });
                if command == "start_workspace_state_stream" {
                    w.inner.lock().unwrap().refs += 1;
                }
                map.insert(key.clone(), Arc::clone(&w));
                // created == true：起新的轮询任务（Go 的 `go func()`）。
                let ticker = Arc::clone(&w);
                let ticker_root = root.to_path_buf();
                std::thread::spawn(move || {
                    loop {
                        std::thread::sleep(Duration::from_millis(1500));
                        let stopped = ticker.inner.lock().unwrap().stopped;
                        if stopped {
                            return;
                        }
                        // Go 的 ticker 在独立 goroutine 里扫描；失败照旧静默。
                        let _ = scan_workspace(&ticker_root, &ticker, true);
                    }
                });
                w
            }
        }
    };

    scan_workspace(root, &watch, false)
}

/// Go `scanWorkspace`：文件树 + 文件戳 diff + git 聚合；`broadcast` 决定
/// 变化时是否广播事件（首扫不广播）。
fn scan_workspace(root: &Path, w: &WorkspaceWatch, broadcast: bool) -> Result<Value, String> {
    let mut inner = w.inner.lock().unwrap();
    let watch_path = PathBuf::from(&w.root);
    let tree = crate::backend::files::handle(
        root,
        "get_file_tree",
        &json!({"path": w.root, "maxDepth": 8}),
    )
    .ok_or("文件树命令不可用")??;

    let mut files: HashMap<String, String> = HashMap::new();
    walk_stamps(&watch_path, &watch_path, &mut files)?;

    let mut changed: Vec<String> = Vec::new();
    for (path, stamp) in &files {
        if inner.files.get(path).map(String::as_str) != Some(stamp.as_str()) {
            changed.push(path.clone());
        }
    }
    for path in inner.files.keys() {
        if !files.contains_key(path) {
            changed.push(path.clone());
        }
    }

    let in_git_repo = git::git_string(&watch_path, &["rev-parse", "--git-dir"]).is_ok();
    let mut git_rows: Vec<Value> = Vec::new();
    if in_git_repo {
        let raw = git::handle(
            root,
            "git_status",
            &json!({"path": w.root, "showAllUntracked": true}),
        )
        .ok_or("git_status 命令不可用")??;
        let mut counts: HashMap<String, (i64, i64)> = HashMap::new();
        let stats = match git::git_string(&watch_path, &["diff", "--numstat", "HEAD", "--"]) {
            Ok(out) => out,
            Err(_) => git::git_string(&watch_path, &["diff", "--numstat", "--cached", "--"])
                .unwrap_or_default(),
        };
        for line in stats.split('\n') {
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            if parts.len() == 3 {
                let add = parts[0].parse::<i64>().unwrap_or(0);
                let del = parts[1].parse::<i64>().unwrap_or(0);
                counts.insert(parts[2].to_string(), (add, del));
            }
        }
        for row in raw.as_array().cloned().unwrap_or_default() {
            let path = row
                .get("file")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            let (add, del) = counts.get(&path).copied().unwrap_or((0, 0));
            git_rows.push(json!({
                "path": path,
                "status": row.get("status").cloned().unwrap_or(Value::Null),
                "additions": add,
                "deletions": del,
            }));
        }
    }

    let signature =
        goformat::marshal_preserve(&goformat::canon(&json!({"tree": tree, "git": git_rows})));
    let fingerprint = String::from_utf8_lossy(&signature).to_string();
    let different = !changed.is_empty() || inner.fingerprint != fingerprint;
    if different {
        inner.seq += 1;
    }
    inner.files = files;
    inner.fingerprint = fingerprint;
    let snapshot = json!({
        "root_path": w.root,
        "seq": inner.seq,
        "version": 1,
        "full": true,
        "tree_snapshot": tree,
        "git_snapshot": git_rows,
        "deltas": [],
        "degraded": false,
        "is_git_repo": in_git_repo,
    });
    if different && broadcast {
        let changed_sorted = {
            // Go 的 changed 来自 map 迭代，顺序不定；canon 后保持原序仅作
            // 载荷（对比 parity 时按无序集合处理）。
            changed.clone()
        };
        emit_event(
            "folder://workspace-state-event",
            json!({
                "root_path": w.root,
                "seq": inner.seq,
                "version": 1,
                "kind": "fs_delta",
                "payload": [
                    {"kind": "tree_replace", "nodes": tree},
                    {"kind": "git_replace", "entries": git_rows},
                ],
                "requires_resync": false,
                "changed_paths": changed_sorted,
            }),
        );
    }
    Ok(snapshot)
}

/// `filepath.WalkDir` 的近似：收集 size:mtime 文件戳，跳过符号链接与
/// ignoredDirs 目录（SkipDir 语义）。
fn walk_stamps(base: &Path, dir: &Path, files: &mut HashMap<String, String>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if metadata.is_dir() {
            if IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk_stamps(base, &path, files)?;
        } else if metadata.file_type().is_symlink() {
            continue;
        } else {
            let mtime = metadata
                .modified()
                .map_err(|e| e.to_string())?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i128)
                .unwrap_or_else(|e| -(e.duration().as_nanos() as i128));
            files.insert(
                path.to_string_lossy().to_string(),
                format!("{}:{}", metadata.len(), mtime),
            );
        }
    }
    Ok(())
}
