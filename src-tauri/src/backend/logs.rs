//! 日志面（补全批次）：级别设置、内存环形缓冲、磁盘日志文件。
//!
//! 前端 `codeg-ui/src/lib/api.ts` 的 `getLogSettings` / `setLogSettings` /
//! `getRecentLogs` / `listLogFiles` / `openLogsDir` / `readLogFile` 已经在调
//! 这 6 条命令，但原 Go sidecar 与 Rust 都没有实现过它们（`go-sidecar-final`
//! 里搜不到这些字面量），所以契约只由调用点和 `types.ts` 决定：
//!
//! * `LogSettingsView` = `{ level, targets, env_locked }`，`set_log_settings`
//!   回显的是 `LogSettings`（只有 `level` 与 `targets`）。
//! * `LogRecord` = `{ seq, timestamp_ms, level, target, message, fields, spans }`。
//! * `LogFileInfo` = `{ name, size_bytes, modified_ms }`。
//!
//! 落盘位置固定在 `root/.data/`：设置进 `log-settings.json`，日志文件进
//! `logs/`。本项目还没有 tracing subscriber，所以环形缓冲由本模块的
//! [`append`] 填充（其它模块想进日志面板就调它），`get_recent_logs` 在没人
//! 写入时诚实地返回空数组，而不是编造记录。

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// 与 `LogLevel` 联合类型一一对应；`off` 只对设置本身有意义。
const LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];
/// 面板只显示最近的这些条；再多也没人看得完。
const RING_LIMIT: usize = 2000;
const DEFAULT_LEVEL: &str = "info";
/// 单个日志文件一次最多读回这么多字节（取最新的一段）。
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "get_log_settings"
            | "set_log_settings"
            | "get_recent_logs"
            | "list_log_files"
            | "open_logs_dir"
            | "read_log_file"
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
        "get_log_settings" => get_settings(root),
        "set_log_settings" => set_settings(root, args),
        "get_recent_logs" => recent(args),
        "list_log_files" => list_files(root),
        "open_logs_dir" => open_dir(root),
        "read_log_file" => read_file(root, args),
        _ => Err(format!("未知的日志命令：{command}")),
    }
}

// ── 路径 ────────────────────────────────────────────────────────────────

fn data_dir(root: &Path) -> PathBuf {
    root.join(".data")
}

fn logs_dir(root: &Path) -> PathBuf {
    data_dir(root).join("logs")
}

fn settings_path(root: &Path) -> PathBuf {
    data_dir(root).join("log-settings.json")
}

// ── 设置 ────────────────────────────────────────────────────────────────

/// 校验并规范化一个级别字符串（大小写不敏感，回写统一小写）。
fn normalize_level(raw: &str) -> Option<String> {
    let value = raw.trim().to_ascii_lowercase();
    LEVELS.contains(&value.as_str()).then_some(value)
}

fn read_settings(root: &Path) -> (String, Vec<Value>) {
    let raw = fs::read(settings_path(root)).ok();
    let parsed = raw
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
    let level = parsed
        .as_ref()
        .and_then(|value| value.get("level"))
        .and_then(Value::as_str)
        .and_then(normalize_level)
        .unwrap_or_else(|| DEFAULT_LEVEL.to_string());
    let targets = parsed
        .as_ref()
        .and_then(|value| value.get("targets"))
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let target = row.get("target").and_then(Value::as_str)?.trim();
                    let level = row.get("level").and_then(Value::as_str)?;
                    if target.is_empty() {
                        return None;
                    }
                    Some(json!({ "target": target, "level": normalize_level(level)? }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (level, targets)
}

/// `env_locked`：`CODEG_LOG` / `RUST_LOG` 一旦存在就由环境变量接管实时级别，
/// 界面上的控件必须灰掉——这正是前端读取该字段的原因。
fn env_locked() -> bool {
    ["CODEG_LOG", "RUST_LOG"].iter().any(|name| {
        std::env::var(name)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    })
}

fn get_settings(root: &Path) -> Result<Value, String> {
    let (level, targets) = read_settings(root);
    Ok(json!({
        "level": level,
        "targets": targets,
        "env_locked": env_locked(),
    }))
}

fn set_settings(root: &Path, args: &Value) -> Result<Value, String> {
    let settings = args
        .get("settings")
        .ok_or_else(|| "缺少 settings 参数".to_string())?;
    let level = settings
        .get("level")
        .and_then(Value::as_str)
        .and_then(normalize_level)
        .ok_or_else(|| "日志级别无效；可选 off、error、warn、info、debug 或 trace".to_string())?;
    let mut targets = Vec::new();
    if let Some(rows) = settings.get("targets") {
        let rows = rows
            .as_array()
            .ok_or_else(|| "targets 必须是数组".to_string())?;
        for row in rows {
            let target = row
                .get("target")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "targets 里的 target 不能为空".to_string())?;
            let target_level = row
                .get("level")
                .and_then(Value::as_str)
                .and_then(normalize_level)
                .ok_or_else(|| "targets 里的 level 无效".to_string())?;
            targets.push(json!({ "target": target, "level": target_level }));
        }
    }
    let payload = json!({ "level": level, "targets": targets });
    let path = settings_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建设置目录失败：{e}"))?;
    }
    // 先写临时文件再改名：写一半被杀也不会留下半个 JSON。
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&payload).unwrap_or_default())
        .map_err(|e| format!("写入日志设置失败：{e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("写入日志设置失败：{e}"))?;
    // 设置变更要跨窗口同步，前端监听这个频道。
    crate::backend::emit_event("log-settings://changed", payload.clone());
    Ok(payload)
}

// ── 环形缓冲 ────────────────────────────────────────────────────────────

fn ring() -> &'static Mutex<VecDeque<Value>> {
    static RING: OnceLock<Mutex<VecDeque<Value>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(RING_LIMIT)))
}

static SEQ: AtomicU64 = AtomicU64::new(1);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// 级别严重度：数字越小越严重。`off` 不参与记录。
fn severity(level: &str) -> Option<u8> {
    match level.trim().to_ascii_lowercase().as_str() {
        "error" => Some(0),
        "warn" => Some(1),
        "info" => Some(2),
        "debug" => Some(3),
        "trace" => Some(4),
        _ => None,
    }
}

/// 把一条记录写进环形缓冲，并追加到当天文件（写盘失败不影响内存里的记录）。
///
/// 其它模块想让自己进日志面板就调这里；本项目还没有 tracing subscriber，
/// 所以这是唯一的写入口。返回值是记录自增序号。
#[allow(dead_code)]
pub fn append(root: &Path, level: &str, target: &str, message: &str, fields: Value) -> u64 {
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let timestamp_ms = now_ms();
    let level = level.trim().to_ascii_uppercase();
    let record = json!({
        "seq": seq,
        "timestamp_ms": timestamp_ms,
        "level": level,
        "target": target,
        "message": message,
        "fields": fields,
        "spans": [],
    });
    {
        let mut buffer = match ring().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if buffer.len() == RING_LIMIT {
            buffer.pop_front();
        }
        buffer.push_back(record.clone());
    }
    // 磁盘侧按天切。`off` 是"不记录"，只在设置里有效，写进来的记录照样落盘。
    let dir = logs_dir(root);
    if fs::create_dir_all(&dir).is_ok() {
        let day = {
            let ms = timestamp_ms;
            // 只用 UNIX 纪元天数命名，避免引入时间库。
            ms / 86_400_000
        };
        let file = dir.join(format!("codepluse-{day}.log"));
        if let Ok(mut handle) = fs::OpenOptions::new().create(true).append(true).open(&file) {
            let _ = writeln!(
                handle,
                "{timestamp_ms}\t{level}\t{target}\t{message}"
            );
        }
    }
    seq
}

fn recent(args: &Value) -> Result<Value, String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .ok_or_else(|| "缺少 limit 参数".to_string())?;
    if limit == 0 {
        return Err("limit 必须大于 0".into());
    }
    let limit = limit.min(RING_LIMIT as u64) as usize;
    let min_level = match args.get("minLevel").and_then(Value::as_str) {
        Some(raw) if !raw.trim().is_empty() => {
            Some(severity(raw).ok_or_else(|| "minLevel 无效".to_string())?)
        }
        _ => None,
    };
    let search = args
        .get("search")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    let buffer = match ring().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut matched = Vec::new();
    // 倒序取最新的 limit 条，再正序返回（面板按时间从上往下读）。
    for record in buffer.iter().rev() {
        let level = record.get("level").and_then(Value::as_str).unwrap_or("");
        if let Some(min) = min_level {
            match severity(&level.to_ascii_lowercase()) {
                Some(current) if current <= min => {}
                _ => continue,
            }
        }
        if let Some(needle) = search.as_ref() {
            let haystack = format!(
                "{}\u{1}{}",
                record.get("message").and_then(Value::as_str).unwrap_or(""),
                record.get("target").and_then(Value::as_str).unwrap_or(""),
            )
            .to_ascii_lowercase();
            if !haystack.contains(needle) {
                continue;
            }
        }
        matched.push(record.clone());
        if matched.len() == limit {
            break;
        }
    }
    matched.reverse();
    Ok(Value::Array(matched))
}

// ── 文件 ────────────────────────────────────────────────────────────────

fn list_files(root: &Path) -> Result<Value, String> {
    let dir = logs_dir(root);
    let mut rows = Vec::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        // 目录还不存在时不是错误：面板显示空列表即可。
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Array(rows)),
        Err(err) => return Err(format!("读取日志目录失败：{err}")),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|value| value.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        if !name.ends_with(".log") {
            continue;
        }
        let meta = entry.metadata().ok();
        let size_bytes = meta.as_ref().map(|value| value.len()).unwrap_or(0);
        let modified_ms = meta
            .as_ref()
            .and_then(|value| value.modified().ok())
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis() as u64)
            .unwrap_or(0);
        rows.push(json!({
            "name": name,
            "size_bytes": size_bytes,
            "modified_ms": modified_ms,
        }));
    }
    // 新的在前：面板默认想先看到今天那份。
    rows.sort_by(|left, right| {
        let left = left.get("modified_ms").and_then(Value::as_u64).unwrap_or(0);
        let right = right.get("modified_ms").and_then(Value::as_u64).unwrap_or(0);
        right.cmp(&left)
    });
    Ok(Value::Array(rows))
}

fn open_dir(root: &Path) -> Result<Value, String> {
    let dir = logs_dir(root);
    fs::create_dir_all(&dir).map_err(|e| format!("创建日志目录失败：{e}"))?;
    let absolute = fs::canonicalize(&dir).unwrap_or(dir);
    Ok(Value::String(absolute.to_string_lossy().to_string()))
}

fn read_file(root: &Path, args: &Value) -> Result<Value, String> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 name 参数".to_string())?;
    // 只接受目录里的文件名本身：挡掉 `..`、绝对路径和分隔符。
    if name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || !name.ends_with(".log")
    {
        return Err("日志文件名无效".into());
    }
    let path = logs_dir(root).join(name);
    // 参数先校验再读盘：缺文件不该掩盖非法 maxBytes。
    let max_bytes = match args.get("maxBytes").and_then(Value::as_u64) {
        Some(0) => return Err("maxBytes 必须大于 0".into()),
        Some(value) => value.min(MAX_READ_BYTES),
        None => MAX_READ_BYTES,
    } as usize;
    let bytes = fs::read(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => format!("日志文件不存在：{name}"),
        _ => format!("读取日志文件失败：{err}"),
    })?;
    let start = bytes.len().saturating_sub(max_bytes);
    Ok(Value::String(
        String::from_utf8_lossy(&bytes[start..]).to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-logs-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    #[test]
    fn owns_covers_the_six_documented_commands_only() {
        for command in [
            "get_log_settings",
            "set_log_settings",
            "get_recent_logs",
            "list_log_files",
            "open_logs_dir",
            "read_log_file",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("get_recent_turns"));
        assert!(!owns("log_settings_write"));
        // 大小写不同就不是同一条命令。
        assert!(!owns("Get_Log_Settings"));
    }

    #[test]
    fn settings_default_to_info_and_no_targets() {
        let root = temp_root("settings-default");
        let view = call(&root, "get_log_settings", json!({})).unwrap();
        assert_eq!(view["level"], "info");
        assert_eq!(view["targets"].as_array().unwrap().len(), 0);
        assert!(view["env_locked"].is_boolean());
    }

    #[test]
    fn set_settings_persists_and_echoes_contract_shape() {
        let root = temp_root("settings-write");
        let echoed = call(
            &root,
            "set_log_settings",
            json!({"settings": {"level": "DEBUG", "targets": [{"target": "codepluse::backend::acp", "level": "trace"}]}}),
        )
        .unwrap();
        // `LogSettings` 只有 level + targets；级别按小写规范化。
        assert_eq!(echoed["level"], "debug");
        assert_eq!(echoed["targets"][0]["target"], "codepluse::backend::acp");
        assert_eq!(echoed["targets"][0]["level"], "trace");
        assert!(echoed.get("env_locked").is_none());

        // 落盘后重新读回，跨进程也是这份设置。
        let view = call(&root, "get_log_settings", json!({})).unwrap();
        assert_eq!(view["level"], "debug");
        assert_eq!(view["targets"][0]["level"], "trace");
        assert!(settings_path(&root).is_file());
    }

    #[test]
    fn set_settings_rejects_bad_input() {
        let root = temp_root("settings-bad");
        assert_eq!(
            call(&root, "set_log_settings", json!({}))
                .unwrap_err()
                .contains("缺少 settings"),
            true
        );
        assert!(call(
            &root,
            "set_log_settings",
            json!({"settings": {"level": "verbose"}})
        )
        .unwrap_err()
        .contains("日志级别无效"));
        assert!(call(
            &root,
            "set_log_settings",
            json!({"settings": {"level": "info", "targets": {"target": "x"}}})
        )
        .unwrap_err()
        .contains("targets 必须是数组"));
        assert!(call(
            &root,
            "set_log_settings",
            json!({"settings": {"level": "info", "targets": [{"target": "  "}]}})
        )
        .unwrap_err()
        .contains("target 不能为空"));
    }

    #[test]
    fn recent_logs_filter_by_level_and_search() {
        let root = temp_root("recent");
        let tag = format!("codepluse::logs::test::{}", std::process::id());
        append(&root, "error", &tag, "boom", json!({"k": "v"}));
        append(&root, "debug", &tag, "noise", json!({}));
        append(&root, "info", &format!("{tag}::other"), "hello", json!({}));

        let all = call(
            &root,
            "get_recent_logs",
            json!({"limit": 10, "search": tag}),
        )
        .unwrap();
        assert_eq!(all.as_array().unwrap().len(), 3);
        // 顺序按时间正序，seq 递增。
        assert_eq!(all[0]["message"], "boom");
        assert_eq!(all[2]["message"], "hello");
        // 每条记录都带 LogRecord 的完整字段。
        for field in ["seq", "timestamp_ms", "level", "target", "message", "fields", "spans"] {
            assert!(all[0].get(field).is_some(), "missing {field}");
        }

        // minLevel: warn → 只剩 error/warn 级别。
        let filtered = call(
            &root,
            "get_recent_logs",
            json!({"limit": 10, "search": tag, "minLevel": "warn"}),
        )
        .unwrap();
        assert_eq!(filtered.as_array().unwrap().len(), 1);
        assert_eq!(filtered[0]["level"], "ERROR");

        // limit 截断取最新的一段。
        let newest = call(
            &root,
            "get_recent_logs",
            json!({"limit": 1, "search": tag}),
        )
        .unwrap();
        assert_eq!(newest.as_array().unwrap().len(), 1);
        assert_eq!(newest[0]["message"], "hello");
    }

    #[test]
    fn recent_logs_require_a_positive_limit() {
        let root = temp_root("recent-bad");
        assert!(call(&root, "get_recent_logs", json!({}))
            .unwrap_err()
            .contains("缺少 limit"));
        assert!(call(&root, "get_recent_logs", json!({"limit": 0}))
            .unwrap_err()
            .contains("limit 必须大于 0"));
        assert!(call(&root, "get_recent_logs", json!({"limit": 5, "minLevel": "loud"}))
            .unwrap_err()
            .contains("minLevel 无效"));
    }

    #[test]
    fn recent_logs_are_empty_when_nothing_was_recorded() {
        // 没人写过就返回空数组，不能编造记录。
        let root = temp_root("recent-empty");
        let empty = call(
            &root,
            "get_recent_logs",
            json!({"limit": 5, "search": "definitely-not-logged-anywhere"}),
        )
        .unwrap();
        assert_eq!(empty.as_array().unwrap().len(), 0);
    }

    #[test]
    fn open_logs_dir_creates_the_directory_and_returns_an_absolute_path() {
        let root = temp_root("open");
        let path = call(&root, "open_logs_dir", json!({})).unwrap();
        let path = path.as_str().unwrap().to_string();
        assert!(Path::new(&path).is_absolute(), "{path} should be absolute");
        assert!(Path::new(&path).is_dir());
        assert!(path.ends_with("logs"));
    }

    #[test]
    fn list_log_files_reports_name_size_and_mtime_but_ignores_non_logs() {
        let root = temp_root("list");
        let dir = logs_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("codepluse-1.log"), b"one\ntwo\n").unwrap();
        fs::write(dir.join("notes.txt"), b"ignored").unwrap();
        let rows = call(&root, "list_log_files", json!({})).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "codepluse-1.log");
        assert_eq!(rows[0]["size_bytes"], 8);
        assert!(rows[0]["modified_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn list_log_files_is_an_empty_list_before_anything_was_written() {
        let root = temp_root("list-empty");
        let rows = call(&root, "list_log_files", json!({})).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 0);
    }

    #[test]
    fn read_log_file_returns_the_newest_bytes_when_capped() {
        let root = temp_root("read");
        let dir = logs_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("codepluse-2.log"), "AAAABBBBCCCC").unwrap();
        let full = call(&root, "read_log_file", json!({"name": "codepluse-2.log"})).unwrap();
        assert_eq!(full, json!("AAAABBBBCCCC"));
        // 上限保留的是最新的一段。
        let capped = call(
            &root,
            "read_log_file",
            json!({"name": "codepluse-2.log", "maxBytes": 4}),
        )
        .unwrap();
        assert_eq!(capped, json!("CCCC"));
    }

    #[test]
    fn read_log_file_refuses_path_traversal_and_missing_files() {
        let root = temp_root("read-bad");
        for name in [
            "../secret.log",
            "..\\secret.log",
            "C:\\Windows\\system.log",
            "nested/dir/app.log",
            "app.txt",
        ] {
            assert!(
                call(&root, "read_log_file", json!({"name": name}))
                    .unwrap_err()
                    .contains("日志文件名无效"),
                "{name} should be rejected"
            );
        }
        assert!(call(&root, "read_log_file", json!({}))
            .unwrap_err()
            .contains("缺少 name"));
        assert!(call(&root, "read_log_file", json!({"name": "codepluse-9.log"}))
            .unwrap_err()
            .contains("日志文件不存在"));
        assert!(call(
            &root,
            "read_log_file",
            json!({"name": "codepluse-9.log", "maxBytes": 0})
        )
        .unwrap_err()
        .contains("maxBytes"));
    }

    #[test]
    fn append_rotates_into_a_daily_file_and_caps_the_ring() {
        let root = temp_root("append");
        append(&root, "info", "codepluse::backend::logs", "kept", json!({}));
        let dir = logs_dir(&root);
        let files: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".log"))
            .collect();
        assert_eq!(files.len(), 1);
        // 落盘那行能被 `read_log_file` 原样读回。
        let name = files[0].file_name().to_string_lossy().to_string();
        let text = call(&root, "read_log_file", json!({"name": name})).unwrap();
        assert!(text.as_str().unwrap().contains("kept"));

        // 环形缓冲只保留最近 RING_LIMIT 条。
        let extra = RING_LIMIT + 10;
        for index in 0..extra {
            append(
                &root,
                "trace",
                "codepluse::logs::cap",
                &format!("line-{index}"),
                json!({}),
            );
        }
        let buffer = ring().lock().unwrap();
        assert_eq!(buffer.len(), RING_LIMIT);
        assert_eq!(buffer.back().unwrap()["message"], format!("line-{}", extra - 1));
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
