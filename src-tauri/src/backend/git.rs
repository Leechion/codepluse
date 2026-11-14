//! Git command surface, ported from `server-go/internal/codeg/git.go` (48
//! commands) plus the four `open_*_window` helpers from `server.go`.
//!
//! Ported here: every git command except `git_commit`, which stays in Go
//! because it emits `folder://git-commit-succeeded` through the in-memory
//! subscriber bus (Tier B — it moves when the event bus does).
//!
//! The whole module is a git-CLI shell, exactly like the Go original: no
//! repository state is held in memory, so there is nothing to race with the
//! sidecar. Path handling mirrors the Go chain:
//!
//! - `resolve`         = `cmd/serve/state.go:resolveWorkdir`  (symlink-safe,
//!   constrained to the workspace root unless `HARNESS_HUB_ALLOW_ANY_PATH=1`)
//! - `resolve_file`    = `codeg/files.go:resolveFile` (readable roots =
//!   workspace root + every opened folder from `codeg-workspace.json`)
//!
//! Calls run through `spawn_blocking` in the dispatcher: `git fetch/push` can
//! legitimately take tens of seconds, and the proxy must keep relaying WS
//! traffic meanwhile.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::backend::store_file;

const GIT_TIMEOUT: Duration = Duration::from_secs(45);
static CUSTOM_GIT_PROGRAM: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Commands owned by this module. `git_commit` is deliberately absent.
pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "git_init"
            | "get_git_branch"
            | "git_status"
            | "git_diff"
            | "git_diff_with_branch"
            | "git_show_diff"
            | "git_is_tracked"
            | "git_show_file"
            | "git_show_file_base64"
            | "git_add_files"
            | "git_rollback_file"
            | "git_list_branches"
            | "git_list_all_branches"
            | "git_new_branch"
            | "git_checkout"
            | "git_delete_branch"
            | "git_merge"
            | "git_rebase"
            | "git_current_user"
            | "git_commit_files"
            | "git_commit_branches"
            | "git_search_authors"
            | "git_log"
            | "git_list_remotes"
            | "git_add_remote"
            | "git_remove_remote"
            | "git_set_remote_url"
            | "git_fetch"
            | "git_fetch_remote"
            | "git_pull"
            | "git_start_pull_merge"
            | "git_update_branch"
            | "git_delete_remote_branch"
            | "git_push"
            | "git_push_info"
            | "git_stash_push"
            | "git_stash_list"
            | "git_stash_pop"
            | "git_stash_apply"
            | "git_stash_drop"
            | "git_stash_show"
            | "git_stash_clear"
            | "git_list_conflicts"
            | "git_has_merge_head"
            | "git_conflict_file_versions"
            | "git_resolve_conflict"
            | "git_abort_operation"
            | "git_continue_operation"
            | "git_reset"
            | "git_worktree_add"
            | "resolve_worktree_folder"
            | "detect_git"
            | "test_git_path"
            | "get_git_settings"
            | "update_git_settings"
            | "open_commit_window"
            | "open_push_window"
            | "open_stash_window"
            | "open_merge_window"
            | "clone_repository"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

pub fn load_settings(root: &Path) {
    let path = root.join(".data").join("codeg-workspace.json");
    let configured = store_file::read_json(&path)
        .ok()
        .flatten()
        .and_then(|doc| {
            doc.get("settings")
                .and_then(|settings| settings.get("get_git_settings"))
                .cloned()
        })
        .and_then(|value| {
            value
                .get("custom_path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
        .filter(|path| path.is_file());
    *CUSTOM_GIT_PROGRAM.write().unwrap() = configured;
}

fn git_program() -> std::ffi::OsString {
    CUSTOM_GIT_PROGRAM
        .read()
        .unwrap()
        .clone()
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| "git".into())
}

fn probe_git(program: &std::ffi::OsStr) -> Option<String> {
    let mut command = std::process::Command::new(program);
    command.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_info(program: &Path) -> Value {
    match probe_git(program.as_os_str()) {
        Some(version) => {
            json!({"installed":true,"version":version,"path":program.to_string_lossy()})
        }
        None => json!({"installed":false,"version":Value::Null,"path":Value::Null}),
    }
}

fn find_git_in_path() -> Option<PathBuf> {
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        for name in if cfg!(windows) {
            vec!["git.exe"]
        } else {
            vec!["git"]
        } {
            let candidate = dir.join(name);
            if candidate.is_file() && probe_git(candidate.as_os_str()).is_some() {
                return Some(candidate);
            }
        }
    }
    None
}

// ── arg accessors, Go's `text` / `number` ──────────────────────────────────

fn text(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn num(value: &Value) -> i64 {
    match value {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return i;
            }
            if let Some(u) = n.as_u64() {
                return u.min(i64::MAX as u64) as i64;
            }
            if let Some(f) = n.as_f64() {
                if !f.is_finite() {
                    return 0;
                }
                if f >= i64::MAX as f64 {
                    return i64::MAX;
                }
                if f <= i64::MIN as f64 {
                    return i64::MIN;
                }
                return f as i64;
            }
            0
        }
        _ => 0,
    }
}

fn number(args: &Value, key: &str) -> i64 {
    args.get(key).map(num).unwrap_or(0)
}

fn truthy(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

// ── path resolution chain ───────────────────────────────────────────────────

/// `cmd/serve/state.go:resolveExistingPath` — absolutise (relative paths go
/// against the workspace root, which is what the sidecar's working directory
/// amounts to in the desktop build), then walk up to the nearest existing
/// ancestor and resolve symlinks from there, re-joining the remainder.
pub(crate) fn resolve_existing_path(input: &str, root: &Path) -> PathBuf {
    let candidate = if input.is_empty() || Path::new(input).is_absolute() {
        PathBuf::from(if input.is_empty() {
            root.as_os_str().to_string_lossy().to_string()
        } else {
            input.to_string()
        })
    } else {
        root.join(input)
    };

    let mut ancestor = candidate.clone();
    while !ancestor.exists() {
        match ancestor.parent() {
            Some(parent) if parent != ancestor => ancestor = parent.to_path_buf(),
            _ => break,
        }
    }
    let resolved = strip_verbatim(eval_symlinks(&ancestor));
    if resolved != ancestor {
        if let Some(rel) = lexical_rel(&ancestor, &candidate) {
            return resolved.join(rel);
        }
    }
    // Go returns `resolved` here, not `candidate`: when nothing along the path
    // is a symlink the walk-up result wins, so a *non-existent* tail collapses
    // to its nearest existing ancestor. That is why `git show HEAD:nope.txt`
    // reaches git as `HEAD:.` in the current build. Reproduced deliberately —
    // behaviour parity first, then the quirk can be fixed on both sides at
    // once (see docs/RUST-BACKEND-MIGRATION.md §7.4).
    resolved
}

pub(crate) fn eval_symlinks(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(_) => path.to_path_buf(),
    }
}

/// Windows `canonicalize` returns `\\?\`-prefixed paths; Go's `EvalSymlinks`
/// does not. Strip it so lexical comparisons line up.
pub(crate) fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.as_os_str().to_string_lossy().to_string();
    PathBuf::from(text.trim_start_matches(r"\\?\").to_string())
}

/// `filepath.Rel` (lexical, case-sensitive like Go) as a dotted relative path.
pub(crate) fn lexical_rel(base: &Path, target: &Path) -> Option<String> {
    let base_parts: Vec<_> = base.components().collect();
    let target_parts: Vec<_> = target.components().collect();
    let mut common = 0;
    while common < base_parts.len()
        && common < target_parts.len()
        && base_parts[common] == target_parts[common]
    {
        common += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..base_parts.len() {
        parts.push("..".into());
    }
    for part in &target_parts[common..] {
        parts.push(part.as_os_str().to_string_lossy().to_string());
    }
    if parts.is_empty() {
        return Some(".".into());
    }
    Some(parts.join("/"))
}

pub(crate) fn within_root(base: &Path, target: &Path) -> bool {
    match lexical_rel(base, target) {
        Some(rel) => rel != ".." && !rel.starts_with("../"),
        None => false,
    }
}

/// `cmd/serve/state.go:resolveWorkdir`.
pub(crate) fn resolve(root: &Path, input: &str) -> Result<PathBuf, String> {
    let resolved = resolve_existing_path(input, root);
    if std::env::var("HARNESS_HUB_ALLOW_ANY_PATH").as_deref() == Ok("1") {
        return Ok(resolved);
    }
    let real_root = strip_verbatim(eval_symlinks(root));
    if within_root(&real_root, &resolved) {
        Ok(resolved)
    } else {
        Err(format!("工作目录必须位于 {} 之内", root.display()))
    }
}

/// `codeg/files.go:readableRoots` — workspace root + every opened folder.
pub(crate) fn readable_roots(root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![root.to_path_buf()];
    if let Ok(raw) = std::fs::read(root.join(".data").join("codeg-workspace.json")) {
        if let Ok(data) = serde_json::from_slice::<Value>(&raw) {
            if let Some(folders) = data.get("folders").and_then(|v| v.as_array()) {
                for folder in folders {
                    if let Some(path) = folder.get("path").and_then(|v| v.as_str()) {
                        if !path.is_empty() {
                            roots.push(PathBuf::from(path));
                        }
                    }
                }
            }
        }
    }
    roots
}

/// `codeg/files.go:resolveFile`.
pub(crate) fn resolve_file(root: &Path, folder: &Path, input: &str) -> Result<PathBuf, String> {
    let candidate = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        folder.join(input)
    };
    let resolved = resolve(root, &candidate.to_string_lossy())?;
    for base in readable_roots(root) {
        let base = match resolve(root, &base.to_string_lossy()) {
            Ok(base) => base,
            Err(_) => continue,
        };
        if within_root(&base, &resolved) {
            return Ok(resolved);
        }
    }
    Err("文件超出工作区目录".into())
}

// ── git runner ──────────────────────────────────────────────────────────────

pub(crate) struct GitOut {
    pub(crate) stdout: Vec<u8>,
}

fn git_spawn(repo: &str, args: &[&str]) -> Result<GitOut, String> {
    let mut argv: Vec<&str> = vec![
        "--no-optional-locks",
        "--literal-pathspecs",
        "-c",
        "core.quotePath=false",
        "-C",
        repo,
    ];
    argv.extend_from_slice(args);

    let mut cmd = std::process::Command::new(git_program());
    cmd.args(&argv)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Go: syscall.SysProcAttr{HideWindow: true}. CREATE_NO_WINDOW also
        // keeps the console flash away in the Tauri shell.
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = cmd.spawn().map_err(|e| format!("git {}: {}", args[0], e))?;
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break None,
        }
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();

    match status {
        Some(status) if status.success() => Ok(GitOut { stdout }),
        _ => Err(format!(
            "git {}: {}",
            args[0],
            String::from_utf8_lossy(&stderr).trim()
        )),
    }
}

/// Captured stdout as raw bytes (git may emit non-UTF-8 or binary).
pub(crate) fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    git_run(repo, args).map(|out| out.stdout)
}

pub(crate) fn git_string(repo: &Path, args: &[&str]) -> Result<String, String> {
    git_bytes(repo, args).map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub(crate) fn git_run(repo: &Path, args: &[&str]) -> Result<GitOut, String> {
    git_spawn(&repo.to_string_lossy(), args)
}

pub(crate) fn git_lines(value: &str) -> Vec<String> {
    value
        .trim()
        .split('\n')
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect()
}

pub(crate) fn git_ref(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.starts_with('-') || value.contains(['\r', '\n', '\0']) {
        return Err("Git 引用无效".into());
    }
    Ok(())
}

// ── shared logic ────────────────────────────────────────────────────────────

/// `gitChanges`: numstat + name-status merged per path.
fn git_changes(repo: &Path, commit: &str) -> Result<Vec<Value>, String> {
    let numstat = git_string(
        repo,
        &[
            "show",
            "--format=",
            "--numstat",
            "--no-renames",
            "--root",
            commit,
            "--",
        ],
    )?;
    let name_status = git_string(
        repo,
        &[
            "show",
            "--format=",
            "--name-status",
            "--no-renames",
            "--root",
            commit,
            "--",
        ],
    )?;
    let mut statuses = std::collections::HashMap::new();
    for line in name_status.split('\n') {
        if let Some((status, path)) = line.split_once('\t') {
            statuses.insert(path.to_string(), status.to_string());
        }
    }
    let mut rows = Vec::new();
    for line in numstat.split('\n') {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }
        rows.push(json!({
            "path": parts[2],
            "status": statuses.get(parts[2]).cloned().unwrap_or_default(),
            "additions": parts[0].parse::<i64>().unwrap_or(0),
            "deletions": parts[1].parse::<i64>().unwrap_or(0),
        }));
    }
    Ok(rows)
}

/// `git_list_conflicts`, shared by merge/rebase/pull result envelopes.
fn list_conflicts(repo: &Path) -> Result<Value, String> {
    let files = git_bytes(repo, &["diff", "--name-only", "--diff-filter=U", "-z"])?;
    let list: Vec<String> = String::from_utf8_lossy(&files)
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(|f| f.to_string())
        .collect();
    let mut operation = String::new();
    for (file, name) in [
        ("MERGE_HEAD", "merge"),
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
    ] {
        let resolved = match git_string(repo, &["rev-parse", "--git-path", file]) {
            Ok(value) => value.trim().to_string(),
            Err(_) => String::new(),
        };
        let p = if Path::new(&resolved).is_absolute() {
            PathBuf::from(&resolved)
        } else {
            repo.join(&resolved)
        };
        if p.exists() {
            operation = name.to_string();
            break;
        }
    }
    Ok(json!({
        "has_conflicts": !list.is_empty(),
        "conflicted_files": list,
        "operation": operation,
    }))
}

fn conflict_flag(conflict: &Value) -> bool {
    conflict.get("has_conflicts").and_then(|v| v.as_bool()) == Some(true)
}

fn list_remotes(repo: &Path) -> Result<Vec<Value>, String> {
    let names = git_string(repo, &["remote"])?;
    let mut rows = Vec::new();
    for name in git_lines(&names) {
        let url = git_string(repo, &["remote", "get-url", &name])?;
        rows.push(json!({"name": name, "url": url.trim()}));
    }
    Ok(rows)
}

/// Symbolic branch, preferring the explicit argument.
fn current_branch(repo: &Path, a: &Value) -> Result<String, String> {
    let branch = text(a, "branch");
    if !branch.is_empty() {
        return Ok(branch);
    }
    let value = git_string(repo, &["symbolic-ref", "--short", "HEAD"])?;
    Ok(value.trim().to_string())
}

// ── command dispatch ────────────────────────────────────────────────────────

fn run(root: &Path, command: &str, a: &Value) -> Result<Value, String> {
    if command == "clone_repository" {
        return clone_repository(root, a);
    }
    match command {
        "detect_git" => {
            if let Some(path) = CUSTOM_GIT_PROGRAM.read().unwrap().clone() {
                return Ok(git_info(&path));
            }
            return Ok(find_git_in_path()
                .map(|path| git_info(&path))
                .unwrap_or_else(
                    || json!({"installed":false,"version":Value::Null,"path":Value::Null}),
                ));
        }
        "test_git_path" => {
            let raw_value = text(a, "path");
            let raw = raw_value.trim();
            if raw.is_empty() {
                return Ok(json!({"installed":false,"version":Value::Null,"path":Value::Null}));
            }
            let path = PathBuf::from(raw);
            return Ok(git_info(&path));
        }
        "get_git_settings" => {
            let path = root.join(".data").join("codeg-workspace.json");
            let doc = store_file::read_json(&path)?.unwrap_or_else(|| json!({}));
            return Ok(doc
                .get("settings")
                .and_then(|s| s.get("get_git_settings"))
                .cloned()
                .unwrap_or_else(|| json!({"custom_path":Value::Null})));
        }
        "update_git_settings" => {
            let settings = a
                .get("settings")
                .and_then(Value::as_object)
                .ok_or("Git 设置无效")?;
            let custom_path = match settings.get("custom_path") {
                None | Some(Value::Null) => Value::Null,
                Some(Value::String(path)) if !path.trim().is_empty() => {
                    let info = git_info(Path::new(path));
                    if info["installed"] != true {
                        return Err("Git 路径无效或无法执行".into());
                    }
                    json!(path)
                }
                _ => return Err("Git 路径无效".into()),
            };
            let updated = json!({"custom_path":custom_path});
            let path = root.join(".data").join("codeg-workspace.json");
            let _guard = store_file::lock(&path)?;
            let mut doc = store_file::read_json(&path)?.unwrap_or_else(|| json!({}));
            let object = doc.as_object_mut().ok_or("工作区设置格式无效")?;
            let saved = object.entry("settings").or_insert_with(|| json!({}));
            saved
                .as_object_mut()
                .ok_or("工作区设置格式无效")?
                .insert("get_git_settings".into(), updated.clone());
            store_file::write_json(&path, &doc)?;
            *CUSTOM_GIT_PROGRAM.write().unwrap() =
                updated["custom_path"].as_str().map(PathBuf::from);
            return Ok(updated);
        }
        _ => {}
    }
    if command.starts_with("open_") {
        return Ok(git_window(command, a));
    }
    if command == "resolve_worktree_folder" {
        let repo = resolve(root, &text(a, "repoPath"))?;
        let branch = text(a, "branch");
        if branch.is_empty() {
            return Err("分支名不能为空".into());
        }
        let output = git_string(&repo, &["worktree", "list", "--porcelain"])?;
        let mut found: Option<PathBuf> = None;
        for block in output.split("\n\n") {
            let mut path = None;
            let mut branch_name = None;
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(value));
                }
                if let Some(value) = line.strip_prefix("branch refs/heads/") {
                    branch_name = Some(value);
                }
            }
            if branch_name == Some(branch.as_str()) {
                found = path;
                break;
            }
        }
        let Some(path) = found else {
            return Ok(json!({"path":Value::Null,"folder_id":Value::Null}));
        };
        let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        let path_key = normalize_path(&path.to_string_lossy());
        let doc = crate::backend::store_file::read_json(
            &root.join(".data").join("codeg-workspace.json"),
        )?;
        let folder_id = doc
            .as_ref()
            .and_then(|doc| doc.get("folders"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|folder| {
                folder
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|p| {
                        normalize_path(
                            &crate::backend::git::strip_verbatim(
                                crate::backend::git::eval_symlinks(Path::new(p)),
                            )
                            .to_string_lossy(),
                        ) == path_key
                    })
                    .unwrap_or(false)
            })
            .and_then(|folder| folder.get("id"))
            .cloned()
            .unwrap_or(Value::Null);
        return Ok(json!({"path":path.to_string_lossy(),"folder_id":folder_id}));
    }

    let path = resolve(root, &text(a, "path"))?;
    let repo = path.to_string_lossy().to_string();

    // `file` is resolved and converted to a repo-relative slash path once.
    let raw_file = text(a, "file");
    let file = if raw_file.is_empty() {
        String::new()
    } else {
        let resolved = resolve_file(root, &path, &raw_file)?;
        let rel = lexical_rel(&path, &resolved).ok_or_else(|| "文件超出工作区目录".to_string())?;
        rel.replace('\\', "/")
    };

    let ref_arg = |key: &str| -> Result<String, String> {
        let value = text(a, key);
        git_ref(&value)?;
        Ok(value)
    };
    let branch_name = text(a, "branchName");

    match command {
        "git_init" => {
            git_run(&path, &["init"])?;
            Ok(Value::Null)
        }
        "get_git_branch" => match git_string(&path, &["symbolic-ref", "--short", "HEAD"]) {
            Ok(branch) => Ok(Value::String(branch.trim().to_string())),
            Err(_) => Ok(Value::Null),
        },
        "git_status" => {
            let mode = if truthy(a, "showAllUntracked") {
                "--untracked-files=all"
            } else {
                "--untracked-files=normal"
            };
            let out = git_bytes(&path, &["status", "--porcelain=v1", "-z", mode])?;
            let parts: Vec<&[u8]> = out.split(|b| *b == 0).collect();
            let mut rows = Vec::new();
            let mut i = 0;
            while i < parts.len() {
                let entry = parts[i];
                if entry.len() < 4 {
                    i += 1;
                    continue;
                }
                let status = String::from_utf8_lossy(&entry[..2]).trim().to_string();
                let name = String::from_utf8_lossy(&entry[3..]).into_owned();
                if entry[..2].iter().any(|b| *b == b'R' || *b == b'C') && i + 1 < parts.len() {
                    i += 1;
                }
                rows.push(json!({"status": status, "file": name}));
                i += 1;
            }
            Ok(Value::Array(rows))
        }
        "git_diff" | "git_diff_with_branch" | "git_show_diff" => {
            let mut args: Vec<String> = match command {
                "git_diff_with_branch" => {
                    vec!["diff".into(), "--no-ext-diff".into(), ref_arg("branch")?]
                }
                "git_show_diff" => {
                    vec![
                        "show".into(),
                        "--format=".into(),
                        "--no-ext-diff".into(),
                        ref_arg("commit")?,
                    ]
                }
                _ => vec!["diff".into(), "--no-ext-diff".into(), "HEAD".into()],
            };
            args.push("--".into());
            if !file.is_empty() {
                args.push(file.clone());
            }
            let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            match git_string(&path, &argv) {
                Ok(out) => Ok(Value::String(out)),
                Err(_) if command == "git_diff" => {
                    let mut args: Vec<String> = vec![
                        "diff".into(),
                        "--cached".into(),
                        "--no-ext-diff".into(),
                        "--".into(),
                    ];
                    if !file.is_empty() {
                        args.push(file.clone());
                    }
                    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    Ok(Value::String(git_string(&path, &argv)?))
                }
                Err(err) => Err(err),
            }
        }
        "git_is_tracked" => {
            let out = git_run(&path, &["ls-files", "--error-unmatch", "--", &file]);
            Ok(Value::Bool(out.is_ok()))
        }
        "git_show_file" | "git_show_file_base64" => {
            let mut revision = text(a, "refName");
            if revision.is_empty() {
                revision = "HEAD".into();
            }
            git_ref(&revision)?;
            let ref_err = git_run(
                &path,
                &["rev-parse", "--verify", "--end-of-options", &revision],
            );
            let data = git_bytes(&path, &["show", &format!("{revision}:{file}")]);
            if command == "git_show_file" {
                let data = data?;
                if data.contains(&0) {
                    return Err("该文件为二进制，请使用图片预览".into());
                }
                return Ok(Value::String(String::from_utf8_lossy(&data).into_owned()));
            }
            let exists = data.is_ok();
            let data = data.unwrap_or_default();
            let mut cap = number(a, "maxBytes");
            if cap <= 0 || cap > 8 << 20 {
                cap = 8 << 20;
            }
            let encoded = if exists && data.len() as i64 <= cap {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(&data)
            } else {
                String::new()
            };
            Ok(json!({
                "exists": exists,
                "ref_missing": ref_err.is_err(),
                "data": encoded,
                "byte_size": data.len(),
                "too_large": data.len() as i64 > cap,
            }))
        }
        "git_add_files" => {
            let mut files: Vec<String> = Vec::new();
            if let Some(list) = a.get("files").and_then(|v| v.as_array()) {
                for raw in list {
                    let input = raw.as_str().unwrap_or("");
                    let p = resolve_file(root, &path, input)?;
                    let rel = lexical_rel(&path, &p).ok_or("文件超出工作区目录")?;
                    files.push(rel.replace('\\', "/"));
                }
            }
            if files.is_empty() {
                return Err("请选择要提交或暂存的文件".into());
            }
            let staged = git_string(
                &path,
                &["diff", "--cached", "--name-only", "--diff-filter=D", "-z"],
            )
            .unwrap_or_default();
            let deleted: std::collections::HashSet<&str> = staged.split('\0').collect();
            let to_add: Vec<&str> = files
                .iter()
                .map(|s| s.as_str())
                .filter(|p| !deleted.contains(p))
                .collect();
            if !to_add.is_empty() {
                let mut args: Vec<&str> = vec!["add", "--"];
                args.extend(to_add.iter().copied());
                git_run(&path, &args)?;
            }
            Ok(Value::Null)
        }
        "git_rollback_file" => {
            if file.is_empty() {
                return Err("文件不能为空".into());
            }
            git_run(
                &path,
                &[
                    "restore",
                    "--source=HEAD",
                    "--staged",
                    "--worktree",
                    "--",
                    &file,
                ],
            )?;
            Ok(Value::Null)
        }
        "git_list_branches" | "git_list_all_branches" => {
            let local = git_string(
                &path,
                &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
            )?;
            if command == "git_list_branches" {
                return Ok(Value::Array(
                    git_lines(&local).into_iter().map(Value::String).collect(),
                ));
            }
            let remote = git_string(
                &path,
                &["for-each-ref", "--format=%(refname:short)", "refs/remotes/"],
            )?;
            let worktrees =
                git_string(&path, &["worktree", "list", "--porcelain"]).unwrap_or_default();
            let mut busy: Vec<Value> = Vec::new();
            let mut main = Value::Null;
            let repo_norm = normalize_path(&repo);
            for (index, block) in worktrees.trim().split("\n\n").enumerate() {
                let mut p = String::new();
                let mut b = String::new();
                for line in block.split('\n') {
                    if let Some(v) = line.strip_prefix("worktree ") {
                        p = v.to_string();
                    }
                    if let Some(v) = line.strip_prefix("branch refs/heads/") {
                        b = v.to_string();
                    }
                }
                if normalize_path(&p) != repo_norm && !b.is_empty() {
                    busy.push(Value::String(b.clone()));
                    if index == 0 {
                        main = Value::String(b);
                    }
                }
            }
            Ok(json!({
                "local": git_lines(&local),
                "remote": git_lines(&remote),
                "worktree_branches": busy,
                "main_worktree_branch": main,
            }))
        }
        "git_new_branch" | "git_checkout" | "git_delete_branch" | "git_merge" | "git_rebase" => {
            git_ref(&branch_name)?;
            match command {
                "git_new_branch" => {
                    let mut args: Vec<String> =
                        vec!["checkout".into(), "-b".into(), branch_name.clone()];
                    let start = text(a, "startPoint");
                    if !start.is_empty() {
                        git_ref(&start)?;
                        args.push(start);
                    }
                    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    git_run(&path, &argv)?;
                    Ok(Value::Null)
                }
                "git_checkout" => {
                    git_run(&path, &["checkout", &branch_name, "--"])?;
                    Ok(Value::Null)
                }
                "git_delete_branch" => {
                    let flag = if truthy(a, "force") { "-D" } else { "-d" };
                    Ok(Value::String(git_string(
                        &path,
                        &["branch", flag, "--", &branch_name],
                    )?))
                }
                _ => {
                    // merge / rebase: the git subcommand is not the command id.
                    let op = if command == "git_rebase" {
                        "rebase"
                    } else {
                        "merge"
                    };
                    let before = git_string(&path, &["rev-parse", "HEAD"]).unwrap_or_default();
                    let outcome = git_run(&path, &[op, &branch_name]);
                    let conflict =
                        list_conflicts(&path).unwrap_or_else(|_| json!({"has_conflicts": false}));
                    if let Err(err) = outcome {
                        if !conflict_flag(&conflict) {
                            return Err(err);
                        }
                    }
                    if op == "merge" {
                        let count = git_string(
                            &path,
                            &["rev-list", "--count", &format!("{}..HEAD", before.trim())],
                        )
                        .unwrap_or_default();
                        let n = count.trim().parse::<i64>().unwrap_or(0);
                        Ok(json!({"merged_commits": n, "conflict": conflict}))
                    } else {
                        Ok(json!({"message": "rebase completed", "conflict": conflict}))
                    }
                }
            }
        }
        "git_current_user" => match git_string(&path, &["config", "user.name"]) {
            Ok(value) => Ok(Value::String(value.trim().to_string())),
            Err(_) => Ok(Value::Null),
        },
        "git_commit_files" => {
            let commit = ref_arg("commit")?;
            Ok(Value::Array(git_changes(&path, &commit)?))
        }
        "git_commit_branches" => {
            let commit = ref_arg("commit")?;
            let value = git_string(
                &path,
                &[
                    "branch",
                    "-a",
                    "--contains",
                    &commit,
                    "--format=%(refname:short)",
                ],
            )?;
            Ok(Value::Array(
                git_lines(&value).into_iter().map(Value::String).collect(),
            ))
        }
        "git_search_authors" => {
            let value = git_string(&path, &["shortlog", "-sne", "--all"])?;
            let mut rows: Vec<Value> = Vec::new();
            let mut limit = number(a, "limit");
            if limit <= 0 {
                limit = 30;
            }
            let query = text(a, "query").to_lowercase();
            for line in value.split('\n') {
                let parts: Vec<&str> = line.trim().splitn(2, '\t').collect();
                if parts.len() == 2 && parts[1].to_lowercase().contains(&query) {
                    rows.push(Value::String(parts[1].to_string()));
                    if rows.len() as i64 >= limit {
                        break;
                    }
                }
            }
            Ok(Value::Array(rows))
        }
        "git_log" => {
            let mut limit = number(a, "limit");
            if limit <= 0 {
                limit = 50;
            }
            limit = limit.min(500);
            let skip = number(a, "skip").max(0);
            let mut args: Vec<String> = vec![
                "log".into(),
                "--date=iso-strict".into(),
                "--format=%h%x00%H%x00%an%x00%aI%x00%s%x00".into(),
                "-n".into(),
                limit.to_string(),
                format!("--skip={skip}"),
            ];
            if truthy(a, "allBranches") {
                args.push("--all".into());
            } else {
                let branch = text(a, "branch");
                if !branch.is_empty() {
                    git_ref(&branch)?;
                    args.push(branch);
                }
            }
            let author = text(a, "author");
            if !author.is_empty() {
                args.push(format!("--author={author}"));
                args.push("--fixed-strings".into());
            }
            args.push("--".into());
            let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let value = match git_string(&path, &argv) {
                Ok(value) => value,
                Err(err) => {
                    if git_run(&path, &["rev-parse", "--verify", "HEAD"]).is_err() {
                        return Ok(json!({"entries": [], "has_upstream": false}));
                    }
                    return Err(err);
                }
            };
            let has_upstream = git_run(&path, &["rev-parse", "--verify", "@{upstream}"]).is_ok();
            let mut unpushed = std::collections::HashSet::new();
            if has_upstream {
                let diff =
                    git_string(&path, &["rev-list", "@{upstream}..HEAD"]).unwrap_or_default();
                for id in git_lines(&diff) {
                    unpushed.insert(id);
                }
            }
            let mut entries: Vec<Value> = Vec::new();
            let parts: Vec<&str> = value.split('\0').collect();
            let mut i = 0;
            while i + 4 < parts.len() {
                let hash = parts[i].trim();
                if !hash.is_empty() {
                    let full_hash = parts[i + 1];
                    let files = if truthy(a, "withFiles") {
                        match git_changes(&path, full_hash) {
                            Ok(rows) => Value::Array(rows),
                            Err(_) => Value::Null,
                        }
                    } else {
                        Value::Array(Vec::new())
                    };
                    let pushed = if has_upstream {
                        Value::Bool(!unpushed.contains(full_hash))
                    } else {
                        Value::Null
                    };
                    entries.push(json!({
                        "hash": hash,
                        "full_hash": full_hash,
                        "author": parts[i + 2],
                        "date": parts[i + 3],
                        "message": parts[i + 4],
                        "files": files,
                        "pushed": pushed,
                    }));
                }
                i += 5;
            }
            Ok(json!({"entries": entries, "has_upstream": has_upstream}))
        }
        "git_list_remotes" => Ok(Value::Array(list_remotes(&path)?)),
        "git_add_remote" | "git_remove_remote" | "git_set_remote_url" => {
            let name = ref_arg("name")?;
            let url = text(a, "url");
            match command {
                "git_remove_remote" => {
                    git_run(&path, &["remote", "remove", &name])?;
                }
                "git_add_remote" => {
                    git_run(&path, &["remote", "add", &name, &url])?;
                }
                _ => {
                    git_run(&path, &["remote", "set-url", &name, &url])?;
                }
            }
            Ok(Value::Null)
        }
        "git_delete_remote_branch" => {
            let remote = text(a, "remote");
            let branch = text(a, "branch");
            git_ref(&remote)?;
            git_ref(&branch)?;
            if a.get("credentials")
                .and_then(Value::as_object)
                .is_some_and(|credentials| !credentials.is_empty())
            {
                return Err("当前移植使用系统 Git 凭据配置，尚未接入自定义凭据".into());
            }
            git_run(
                &path,
                &["push", remote.as_str(), "--delete", branch.as_str()],
            )?;
            Ok(Value::Null)
        }
        "git_fetch" | "git_fetch_remote" | "git_pull" | "git_push" => {
            if let Some(credentials) = a.get("credentials").and_then(|v| v.as_object()) {
                if !credentials.is_empty() {
                    return Err("当前移植使用系统 Git 凭据配置，尚未接入自定义凭据".into());
                }
            }
            match command {
                "git_fetch" => Ok(Value::String(git_string(&path, &["fetch", "--all"])?)),
                "git_fetch_remote" => {
                    let remote = text(a, "remote");
                    if !remote.is_empty() {
                        git_ref(&remote)?;
                    }
                    Ok(Value::String(git_string(&path, &["fetch", &remote])?))
                }
                "git_pull" => {
                    let before = git_string(&path, &["rev-parse", "HEAD"]).unwrap_or_default();
                    let outcome = git_run(&path, &["pull", "--no-rebase"]);
                    let conflict =
                        list_conflicts(&path).unwrap_or_else(|_| json!({"has_conflicts": false}));
                    if let Err(err) = outcome {
                        if !conflict_flag(&conflict) {
                            return Err(err);
                        }
                    }
                    let files =
                        git_string(&path, &["diff", "--name-only", before.trim(), "HEAD", "--"])
                            .unwrap_or_default();
                    Ok(json!({
                        "updated_files": git_lines(&files).len(),
                        "conflict": conflict,
                    }))
                }
                _ => {
                    // git_push
                    let branch = current_branch(&path, a)?;
                    git_ref(&branch)?;
                    let mut remote = text(a, "remote");
                    if !remote.is_empty() {
                        git_ref(&remote)?;
                    }
                    if remote.is_empty() {
                        let cfg_key = format!("branch.{branch}.remote");
                        remote = git_string(&path, &["config", cfg_key.as_str()])
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        if remote.is_empty() {
                            remote = "origin".into();
                        }
                    }
                    let count = git_string(
                        &path,
                        &[
                            "rev-list",
                            "--count",
                            &format!("{remote}/{branch}..{branch}"),
                        ],
                    )
                    .unwrap_or_default();
                    let n = count.trim().parse::<i64>().unwrap_or(0);
                    git_run(
                        &path,
                        &["push", "--set-upstream", remote.as_str(), branch.as_str()],
                    )?;
                    Ok(json!({"pushed_commits": n, "upstream_set": true}))
                }
            }
        }
        "git_push_info" => {
            let branch = current_branch(&path, a)?;
            git_ref(&branch)?;
            let remotes = list_remotes(&path)?;
            let cfg_key = format!("branch.{branch}.remote");
            let tracking = git_string(&path, &["config", cfg_key.as_str()])
                .unwrap_or_default()
                .trim()
                .to_string();
            let tracking_remote = if tracking.is_empty() {
                Value::Null
            } else {
                Value::String(tracking)
            };
            Ok(json!({
                "branch": branch,
                "remotes": remotes,
                "tracking_remote": tracking_remote,
            }))
        }
        "git_stash_push" => {
            let mut args: Vec<String> =
                vec!["stash".into(), "push".into(), "--include-untracked".into()];
            if truthy(a, "keepIndex") {
                args.push("--keep-index".into());
            }
            let message = text(a, "message");
            if !message.is_empty() {
                args.push("-m".into());
                args.push(message);
            }
            let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            Ok(Value::String(git_string(&path, &argv)?))
        }
        "git_stash_list" => {
            let value = git_string(&path, &["stash", "list", "--format=%gd%x00%gs%x00%ci"])?;
            let mut rows = Vec::new();
            for (i, line) in git_lines(&value).iter().enumerate() {
                let parts: Vec<&str> = line.split('\0').collect();
                if parts.len() < 3 {
                    continue;
                }
                let mut branch = String::new();
                if let Some((head, _)) = parts[1].split_once(':') {
                    branch = head.strip_prefix("On ").unwrap_or(head).to_string();
                }
                rows.push(json!({
                    "index": i,
                    "ref_name": parts[0],
                    "message": parts[1],
                    "branch": branch,
                    "date": parts[2],
                }));
            }
            Ok(Value::Array(rows))
        }
        "git_stash_pop" | "git_stash_apply" | "git_stash_drop" | "git_stash_show"
        | "git_stash_clear" => {
            let op = command.strip_prefix("git_stash_").unwrap();
            let mut args: Vec<String> = vec!["stash".into(), op.into()];
            if op == "show" {
                args.push("-p".into());
            }
            let stash = text(a, "stashRef");
            if !stash.is_empty() {
                git_ref(&stash)?;
                args.push(stash);
            }
            let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            Ok(Value::String(git_string(&path, &argv)?))
        }
        "git_list_conflicts" => list_conflicts(&path),
        "git_start_pull_merge" => {
            let upstream = text(a, "upstreamCommit");
            let upstream = if upstream.trim().is_empty() {
                "@{upstream}".to_string()
            } else {
                git_ref(&upstream)?;
                upstream
            };
            git_run(&path, &["merge", "--no-commit", "--no-ff", &upstream])?;
            Ok(Value::Null)
        }
        "git_update_branch" => {
            let branch = text(a, "branch");
            git_ref(&branch)?;
            if a.get("credentials")
                .and_then(Value::as_object)
                .is_some_and(|credentials| !credentials.is_empty())
            {
                return Err("当前移植使用系统 Git 凭据配置，尚未接入自定义凭据".into());
            }
            let is_remote = truthy(a, "isRemote");
            let current = git_string(&path, &["symbolic-ref", "--short", "HEAD"])
                .unwrap_or_default()
                .trim()
                .to_string();
            if !is_remote && branch == current {
                let before = git_string(&path, &["rev-parse", "HEAD"])?;
                let outcome = git_run(&path, &["pull", "--no-rebase"]);
                let conflict =
                    list_conflicts(&path).unwrap_or_else(|_| json!({"has_conflicts":false}));
                if let Err(error) = outcome {
                    if !conflict_flag(&conflict) {
                        return Err(error);
                    }
                }
                let files =
                    git_string(&path, &["diff", "--name-only", before.trim(), "HEAD", "--"])
                        .unwrap_or_default();
                return Ok(json!({
                    "updated_files": git_lines(&files).len(),
                    "conflict": conflict,
                }));
            }
            git_run(&path, &["fetch", "--all"])?;
            if is_remote {
                let (remote, remote_branch) = branch.split_once('/').ok_or("远端分支格式无效")?;
                git_ref(remote)?;
                git_ref(remote_branch)?;
                git_run(&path, &["fetch", remote, remote_branch])?;
                return Ok(json!({"updated_files":0,"conflict":null}));
            }
            let upstream = git_string(
                &path,
                &[
                    "rev-parse",
                    "--symbolic-full-name",
                    &format!("{branch}@{{upstream}}"),
                ],
            )?;
            let upstream = upstream.trim();
            let old = git_string(&path, &["rev-parse", &format!("refs/heads/{branch}")])?;
            let new = git_string(&path, &["rev-parse", upstream])?;
            if git_run(
                &path,
                &["merge-base", "--is-ancestor", old.trim(), new.trim()],
            )
            .is_err()
            {
                return Err(format!("分支 {branch} 无法快进到其上游"));
            }
            git_run(
                &path,
                &[
                    "update-ref",
                    &format!("refs/heads/{branch}"),
                    new.trim(),
                    old.trim(),
                ],
            )?;
            let files = git_string(
                &path,
                &["diff", "--name-only", old.trim(), new.trim(), "--"],
            )?;
            Ok(json!({"updated_files":git_lines(&files).len(),"conflict":null}))
        }
        "git_has_merge_head" => {
            let out = git_string(&path, &["rev-parse", "--verify", "MERGE_HEAD"]);
            Ok(Value::Bool(matches!(out, Ok(v) if !v.trim().is_empty())))
        }
        "git_conflict_file_versions" => {
            let mut versions = Map::new();
            for (name, stage) in [("base", ":1:"), ("ours", ":2:"), ("theirs", ":3:")] {
                let value =
                    git_string(&path, &["show", &format!("{stage}{file}")]).unwrap_or_default();
                versions.insert(name.into(), Value::String(value));
            }
            let p = resolve_file(root, &path, &raw_file)?;
            let data = std::fs::read(&p).map_err(|e| e.to_string())?;
            versions.insert(
                "merged".into(),
                Value::String(String::from_utf8_lossy(&data).into_owned()),
            );
            Ok(Value::Object(versions))
        }
        "git_resolve_conflict" => {
            let p = resolve_file(root, &path, &raw_file)?;
            std::fs::write(&p, text(a, "content").as_bytes()).map_err(|e| e.to_string())?;
            git_run(&path, &["add", "--", &file])?;
            Ok(Value::Null)
        }
        "git_abort_operation" | "git_continue_operation" => {
            let op = text(a, "operation");
            if op != "merge" && op != "rebase" && op != "cherry-pick" {
                return Err("Git 操作类型无效".into());
            }
            let flag = if command == "git_continue_operation" {
                "--continue"
            } else {
                "--abort"
            };
            git_run(&path, &[&op, flag])?;
            Ok(Value::Null)
        }
        "git_reset" => {
            let commit = ref_arg("commit")?;
            let mode = text(a, "mode");
            if !matches!(mode.as_str(), "soft" | "mixed" | "hard" | "keep") {
                return Err("Reset 模式无效".into());
            }
            git_run(&path, &["reset", &format!("--{mode}"), &commit, "--"])?;
            Ok(Value::Null)
        }
        "git_worktree_add" => {
            git_ref(&branch_name)?;
            let target = resolve(root, &text(a, "worktreePath"))?;
            git_run(
                &path,
                &[
                    "worktree",
                    "add",
                    "--",
                    &target.to_string_lossy(),
                    &branch_name,
                ],
            )?;
            Ok(Value::Null)
        }
        _ => Err(format!("Go 移植尚未实现此 Git 接口：{command}")),
    }
}

fn clone_repository(root: &Path, args: &Value) -> Result<Value, String> {
    let url = text(args, "url");
    let raw_target = text(args, "targetDir");
    if url.trim().is_empty() || raw_target.trim().is_empty() {
        return Err("仓库地址和目标目录不能为空".into());
    }
    let parsed = url::Url::parse(&url);
    let scp_style_ssh = url.starts_with("git@") && url.contains(':');
    if let Ok(parsed) = parsed {
        if !matches!(parsed.scheme(), "https" | "http" | "ssh" | "git") {
            return Err("仅支持 HTTP、HTTPS、SSH 或 Git 仓库地址".into());
        }
    } else if !scp_style_ssh {
        return Err("仓库地址无效".into());
    }
    let target = resolve_existing_path(&raw_target, root);
    if target.exists() {
        return Err("目标目录已存在".into());
    }
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or("目标目录无效")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("创建目标父目录失败：{e}"))?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
    let target = canonical_parent.join(target.file_name().ok_or("目标目录无效")?);

    let mut command = std::process::Command::new(git_program());
    command
        .arg("clone")
        .arg("--")
        .arg(&url)
        .arg(&target)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let (Some(username), Some(password)) = (
        args.pointer("/credentials/username")
            .and_then(Value::as_str),
        args.pointer("/credentials/password")
            .and_then(Value::as_str),
    ) {
        if !username.is_empty() || !password.is_empty() {
            use base64::Engine;
            let auth =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            command
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "http.extraheader")
                .env("GIT_CONFIG_VALUE_0", format!("Authorization: Basic {auth}"));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("启动 Git clone 失败：{e}"))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    });
    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let _stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    let Some(status) = status else {
        let _ = std::fs::remove_dir_all(&target);
        return Err("Git clone 超时（45 秒）".into());
    };
    if !status.success() {
        let _ = std::fs::remove_dir_all(&target);
        let details = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(if details.is_empty() {
            format!("Git clone 失败，退出码 {:?}", status.code())
        } else {
            format!("Git clone 失败：{details}")
        });
    }
    Ok(Value::Null)
}

/// `gitWindow` from git.go — builds the route for the commit/push/stash/merge
/// windows. Keys are emitted sorted, like Go's `url.Values.Encode`.
fn git_window(command: &str, a: &Value) -> Value {
    let page = command
        .strip_prefix("open_")
        .map(|p| p.strip_suffix("_window").unwrap_or(p))
        .unwrap_or("");
    let mut params: Vec<(String, String)> =
        vec![("folderId".into(), number(a, "folderId").to_string())];
    for key in ["operation", "upstreamCommit", "branch"] {
        let value = text(a, key);
        if !value.is_empty() {
            params.push((key.into(), value));
        }
    }
    params.sort_by(|x, y| x.0.cmp(&y.0));
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", query_escape(k), query_escape(v)))
        .collect();
    json!({"path": format!("/{}?{}", page, query.join("&"))})
}

/// Go's `url.QueryEscape`.
fn query_escape(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Case-insensitive, separator-normalised comparison for `filepath.Clean` +
/// `EqualFold` on Windows paths.
fn normalize_path(path: &str) -> String {
    path.to_lowercase().replace('/', "\\")
}

/// Go git.go 的 `git_commit`（与 git_add_files 同一个 case 的续段）：
/// 暂存选中文件（排除已暂存的删除）→ `commit --only` → 发
/// `folder://git-commit-succeeded` 事件。事件无法走 [`handle`] 的返回
/// 类型，由 mod.rs 的专用分支调用本函数并转发。
pub(crate) fn commit_command(
    root: &Path,
    args: &Value,
) -> Result<(Value, Vec<(String, Value)>), String> {
    let path = resolve(root, &text(args, "path"))?;
    let mut files: Vec<String> = Vec::new();
    if let Some(list) = args.get("files").and_then(|v| v.as_array()) {
        for raw in list {
            let input = raw.as_str().unwrap_or("");
            let p = resolve_file(root, &path, input)?;
            let rel = lexical_rel(&path, &p).ok_or("文件超出工作区目录")?;
            files.push(rel.replace('\\', "/"));
        }
    }
    if files.is_empty() {
        return Err("请选择要提交或暂存的文件".into());
    }
    if text(args, "message").trim().is_empty() {
        return Err("提交说明不能为空".into());
    }
    let staged = git_string(
        &path,
        &["diff", "--cached", "--name-only", "--diff-filter=D", "-z"],
    )
    .unwrap_or_default();
    let deleted: std::collections::HashSet<&str> = staged.split('\0').collect();
    let to_add: Vec<&str> = files
        .iter()
        .map(|s| s.as_str())
        .filter(|p| !deleted.contains(p))
        .collect();
    if !to_add.is_empty() {
        let mut argv: Vec<&str> = vec!["add", "--"];
        argv.extend(to_add.iter().copied());
        git_run(&path, &argv)?;
    }
    let message = text(args, "message");
    let mut argv: Vec<&str> = vec!["commit", "--only", "-m", &message, "--"];
    argv.extend(files.iter().map(|s| s.as_str()));
    git_run(&path, &argv)?;
    let events = vec![(
        "folder://git-commit-succeeded".to_string(),
        json!({
            "folder_id": args.get("folderId").cloned().unwrap_or(Value::Null),
            "committed_files": files.len(),
        }),
    )];
    Ok((json!({"committed_files": files.len()}), events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-git-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Repository with one commit and a configured identity.
    fn repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        let path = dir.to_string_lossy().to_string();
        git_run(&dir, &["init"]).unwrap();
        git_run(&dir, &["config", "user.email", "test@example.com"]).unwrap();
        git_run(&dir, &["config", "user.name", "Tester"]).unwrap();
        fs::write(dir.join("a.txt"), "hello\n").unwrap();
        git_run(&dir, &["add", "--", "a.txt"]).unwrap();
        git_run(&dir, &["commit", "-m", "first"]).unwrap();
        let _ = path;
        dir
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        run(root, command, &args)
    }

    #[test]
    fn git_ref_matches_go_validation() {
        for bad in ["", "   ", "-x", "--force", "a\nb", "a\rb", "a\0b"] {
            assert_eq!(git_ref(bad).unwrap_err(), "Git 引用无效", "input {bad:?}");
        }
        assert!(git_ref("main").is_ok());
        assert!(git_ref("origin/main").is_ok());
    }

    #[test]
    fn pull_merge_command_is_owned_and_stages_a_merge_for_the_resolver() {
        let root = repo("pull-merge-command");
        let branch = git_string(&root, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .to_string();
        git_run(&root, &["checkout", "-b", "incoming"]).unwrap();
        fs::write(root.join("incoming.txt"), "from upstream\n").unwrap();
        git_run(&root, &["add", "--", "incoming.txt"]).unwrap();
        git_run(&root, &["commit", "-m", "incoming"]).unwrap();
        git_run(&root, &["checkout", &branch]).unwrap();

        assert!(owns("git_start_pull_merge"));
        call(
            &root,
            "git_start_pull_merge",
            json!({"path":root,"upstreamCommit":"incoming"}),
        )
        .unwrap();
        assert!(matches!(
            call(&root, "git_has_merge_head", json!({"path":root})),
            Ok(Value::Bool(true))
        ));
    }

    #[test]
    fn query_escape_matches_go() {
        assert_eq!(query_escape("a b"), "a+b");
        assert_eq!(query_escape("a/b"), "a%2Fb");
        assert_eq!(query_escape("feat-1_.~"), "feat-1_.~");
        // 中文 -> UTF-8 percent escapes
        assert_eq!(query_escape("中"), "%E4%B8%AD");
    }

    #[test]
    fn window_paths_are_sorted_like_url_values_encode() {
        let out = run(
            Path::new("."),
            "open_commit_window",
            &json!({"folderId": 3, "branch": "feat/x", "operation": "merge"}),
        )
        .unwrap();
        assert_eq!(
            out["path"],
            json!("/commit?branch=feat%2Fx&folderId=3&operation=merge")
        );
        let out = run(Path::new("."), "open_push_window", &json!({"folderId": 0})).unwrap();
        assert_eq!(out["path"], json!("/push?folderId=0"));
    }

    #[test]
    fn lexical_rel_and_within_root() {
        let base = Path::new("E:/ws");
        assert!(within_root(base, Path::new("E:/ws/a/b")));
        assert!(within_root(base, base));
        assert!(!within_root(base, Path::new("E:/ws2")));
        assert!(!within_root(base, Path::new("E:/")));
        assert_eq!(
            lexical_rel(base, Path::new("E:/ws/a/b")).as_deref(),
            Some("a/b")
        );
    }

    #[test]
    fn status_log_and_branches_roundtrip() {
        let root = repo("round");
        let path = root.to_string_lossy().to_string();
        fs::write(root.join("b.txt"), "x\n").unwrap();

        let status = call(&root, "git_status", json!({"path": path})).unwrap();
        let rows = status.as_array().unwrap();
        assert!(
            rows.iter().any(|r| r["file"] == "b.txt"),
            "status: {status}"
        );

        let log = call(&root, "git_log", json!({"path": path})).unwrap();
        assert_eq!(log["entries"][0]["message"], "first");
        assert_eq!(log["has_upstream"], false);
        assert_eq!(log["entries"][0]["pushed"], Value::Null);

        let branches = call(&root, "git_list_branches", json!({"path": path})).unwrap();
        let names: Vec<String> = branches
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "main" || n == "master"),
            "branches: {names:?}"
        );

        let branch = call(&root, "get_git_branch", json!({"path": path})).unwrap();
        assert!(branch.as_str().unwrap().len() > 0);

        let user = call(&root, "git_current_user", json!({"path": path})).unwrap();
        assert_eq!(user, json!("Tester"));

        let tracked = call(
            &root,
            "git_is_tracked",
            json!({"path": path, "file": "a.txt"}),
        )
        .unwrap();
        assert_eq!(tracked, json!(true));

        let file = call(
            &root,
            "git_show_file",
            json!({"path": path, "file": "a.txt"}),
        )
        .unwrap();
        assert_eq!(file, json!("hello\n"));

        let b64 = call(
            &root,
            "git_show_file_base64",
            json!({"path": path, "file": "a.txt"}),
        )
        .unwrap();
        assert_eq!(b64["exists"], true);
        assert_eq!(b64["data"], json!("aGVsbG8K"));
        assert_eq!(b64["byte_size"], 6);
        assert_eq!(b64["too_large"], false);

        let missing = call(
            &root,
            "git_show_file_base64",
            json!({"path": path, "file": "nope.txt"}),
        )
        .unwrap();
        assert_eq!(missing["exists"], false);
    }

    #[test]
    fn empty_repo_log_returns_empty_page() {
        let root = temp_dir("empty");
        let path = root.to_string_lossy().to_string();
        git_run(&root, &["init"]).unwrap();
        let log = call(&root, "git_log", json!({"path": path})).unwrap();
        assert_eq!(log["entries"], json!([]));
        assert_eq!(log["has_upstream"], false);
        // `git init` leaves HEAD on an unborn branch, so the name exists even
        // though there are no commits yet.
        let branch = call(&root, "get_git_branch", json!({"path": path})).unwrap();
        assert!(matches!(branch.as_str(), Some("main") | Some("master")));
        let conflicts = call(&root, "git_list_conflicts", json!({"path": path})).unwrap();
        assert_eq!(conflicts["has_conflicts"], false);
    }

    #[test]
    fn stash_and_diff_roundtrip() {
        let root = repo("stash");
        let path = root.to_string_lossy().to_string();
        fs::write(root.join("a.txt"), "changed\n").unwrap();
        let diff = call(&root, "git_diff", json!({"path": path})).unwrap();
        assert!(diff.as_str().unwrap().contains("changed"));

        call(
            &root,
            "git_stash_push",
            json!({"path": path, "message": "wip"}),
        )
        .unwrap();
        let list = call(&root, "git_stash_list", json!({"path": path})).unwrap();
        let message = list[0]["message"].as_str().unwrap();
        assert!(
            message.starts_with("On ") && message.ends_with(": wip"),
            "{message}"
        );
        assert_eq!(list[0]["ref_name"], json!("stash@{0}"));

        call(&root, "git_stash_pop", json!({"path": path})).unwrap();
        // Windows checkout applies autocrlf; compare the logical content.
        let content = fs::read_to_string(root.join("a.txt")).unwrap();
        assert_eq!(content.trim_end(), "changed");
    }

    #[test]
    fn paths_outside_the_workspace_are_rejected() {
        let root = temp_dir("ws-root");
        let outside = temp_dir("ws-outside");
        let err = call(
            &root,
            "git_status",
            json!({"path": outside.to_string_lossy()}),
        )
        .unwrap_err();
        assert!(err.contains("工作目录必须位于"), "{err}");
    }

    #[test]
    fn remotes_and_push_info() {
        let root = repo("remote");
        let path = root.to_string_lossy().to_string();
        call(
            &root,
            "git_add_remote",
            json!({"path": path, "name": "origin", "url": "https://example.com/r.git"}),
        )
        .unwrap();
        let remotes = call(&root, "git_list_remotes", json!({"path": path})).unwrap();
        assert_eq!(remotes[0]["name"], json!("origin"));

        let info = call(&root, "git_push_info", json!({"path": path})).unwrap();
        assert_eq!(
            info["remotes"][0]["url"],
            json!("https://example.com/r.git")
        );
        assert_eq!(info["tracking_remote"], Value::Null);

        // Regression: the config lookup must pass the `config` subcommand
        // (`git config branch.X.remote`), otherwise tracking stays null.
        let branch = info["branch"].as_str().unwrap().to_string();
        git_run(
            &root,
            &["config", &format!("branch.{branch}.remote"), "origin"],
        )
        .unwrap();
        let info = call(&root, "git_push_info", json!({"path": path})).unwrap();
        assert_eq!(info["tracking_remote"], json!("origin"));

        call(
            &root,
            "git_remove_remote",
            json!({"path": path, "name": "origin"}),
        )
        .unwrap();
        let remotes = call(&root, "git_list_remotes", json!({"path": path})).unwrap();
        assert_eq!(remotes, json!([]));
    }

    #[test]
    fn untracked_mode_toggle_changes_status_shape() {
        let root = repo("untracked");
        let path = root.to_string_lossy().to_string();
        fs::create_dir_all(root.join("dir")).unwrap();
        fs::write(root.join("dir/one.txt"), "1\n").unwrap();
        fs::write(root.join("dir/two.txt"), "2\n").unwrap();

        let normal = call(&root, "git_status", json!({"path": path})).unwrap();
        let all = call(
            &root,
            "git_status",
            json!({"path": path, "showAllUntracked": true}),
        )
        .unwrap();
        assert_eq!(
            normal.as_array().unwrap().len(),
            1,
            "normal folds the directory: {normal}"
        );
        assert_eq!(all.as_array().unwrap().len(), 2, "all lists files: {all}");
    }
}
