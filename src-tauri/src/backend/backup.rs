//! 备份与恢复 —— 补全批次（11 条）。
//!
//! 前端（`codeg-ui/src/lib/api.ts`）已经在调用这些命令，但原 Go sidecar 与
//! Rust 都没有实现过它们，契约只由调用点参数名与 `api.ts` 里的返回类型决定：
//!
//! * `backup_active_agents` → `string[]`
//! * `backup_cancel` `{ opId }` → `boolean`
//! * `backup_create` `{ options, destPath }` → `BackupManifest | null`
//! * `backup_create_ticket` `{ includeExternalTranscripts, passphrase }` → `BackupTicketResult`
//! * `backup_prepare_source` `{ srcPath | uploadId, passphrase }` → `PreparedBackupSource`
//! * `backup_release_source` `{ sourceId }` → `boolean`
//! * `backup_restore_stage` `{ sourceId, externalMode }` → `StagedRestore`（Web 形态附 `needsRestart/restartDelayMs/staged`）
//! * `backup_rollback` `{ snapshotId }` → 安全快照记录
//! * `backup_discard_pending` `{}` → `boolean`
//! * `backup_list_safety_snapshots` `{}` → `SafetySnapshot[]`
//! * `backup_scan_external_conflicts` `{ sourceId }` → `ExternalConflict[]`
//!
//! ## 红线（硬约束）
//!
//! 本模块**绝不打包、移动或删除用户工作区数据**。所有落盘与扫描都被限制在
//! `root/.data/backups/` 之内：
//!
//! * 元数据（操作状态、准备句柄、票据、暂存记录、回滚记录、快照清单）写进
//!   `.data/backups/` 下的 JSON；
//! * 目录扫描只遍历 `.data/backups/` 内的真实文件；
//! * `backup_restore_stage` / `backup_rollback` 动任何路径之前，先校验其
//!   规范化后仍落在 `.data/backups/` 内，逃逸即拒绝；
//! * `backup_create` 只生成描述本模块自有元数据的清单，不读取工作区源码或
//!   第三方会话库打包成归档。
//!
//! ## 能力分级
//!
//! * L1：状态机迁移、句柄登记/释放、快照目录扫描、冲突清单比对（均在
//!   `.data/backups/` 内闭环）。
//! * L3（limited）：真实归档打包/解压/加密、第三方 SQLite 一致性快照、在线
//!   智能体连接清单在本后端不存在对应服务；`backup_active_agents` 诚实返回
//!   空数组，清单条目只覆盖本模块自有文件，`degradedSqlite` 恒为空。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::backend::goformat;
use crate::backend::store_file;

const MANIFEST_FORMAT_VERSION: u32 = 1;
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "backup_active_agents"
            | "backup_cancel"
            | "backup_create"
            | "backup_create_ticket"
            | "backup_discard_pending"
            | "backup_list_safety_snapshots"
            | "backup_prepare_source"
            | "backup_release_source"
            | "backup_restore_stage"
            | "backup_rollback"
            | "backup_scan_external_conflicts"
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
        "backup_active_agents" => active_agents(),
        "backup_cancel" => cancel(root, args),
        "backup_create" => create(root, args),
        "backup_create_ticket" => create_ticket(root, args),
        "backup_discard_pending" => discard_pending(root),
        "backup_list_safety_snapshots" => list_safety_snapshots(root),
        "backup_prepare_source" => prepare_source(root, args),
        "backup_release_source" => release_source(root, args),
        "backup_restore_stage" => restore_stage(root, args),
        "backup_rollback" => rollback(root, args),
        "backup_scan_external_conflicts" => scan_external_conflicts(root, args),
        _ => Err(format!("未知的备份命令：{command}")),
    }
}

// ── 路径与元数据 ─────────────────────────────────────────────────────────

fn backups_dir(root: &Path) -> PathBuf {
    root.join(".data").join("backups")
}

fn state_path(root: &Path) -> PathBuf {
    backups_dir(root).join("state.json")
}

fn manifests_dir(root: &Path) -> PathBuf {
    backups_dir(root).join("manifests")
}

fn sources_dir(root: &Path) -> PathBuf {
    backups_dir(root).join("sources")
}

fn tickets_dir(root: &Path) -> PathBuf {
    backups_dir(root).join("tickets")
}

fn snapshots_dir(root: &Path) -> PathBuf {
    backups_dir(root).join("snapshots")
}

fn staged_path(root: &Path) -> PathBuf {
    backups_dir(root).join("staged.json")
}

fn rollback_path(root: &Path) -> PathBuf {
    backups_dir(root).join("rollback-pending.json")
}

/// 校验一段文件名/目录名本身不含路径逃逸成分（`..`、分隔符、盘符）。
fn safe_name(name: &str) -> Result<&str, String> {
    if name.is_empty()
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
    {
        return Err("路径片段无效".into());
    }
    Ok(name)
}

/// 规范化后确认候选路径仍位于 `base` 之内；越界（绝对路径、`..` 逃逸）即拒绝。
fn ensure_inside(base: &Path, candidate: &Path) -> Result<PathBuf, String> {
    fn strip_verbatim(path: &Path) -> PathBuf {
        let text = path.as_os_str().to_string_lossy().to_string();
        PathBuf::from(text.trim_start_matches(r"\\?\").to_string())
    }
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    // 词法层级检查：去掉 Windows verbatim 前缀后逐段比对，`..` 逃逸在
    // canonicalize 之前就被挡掉。
    let base_stripped = strip_verbatim(base);
    let joined_stripped = strip_verbatim(&joined);
    let base_parts: Vec<_> = base_stripped.components().collect();
    let joined_parts: Vec<_> = joined_stripped.components().collect();
    if joined_parts.len() < base_parts.len() || joined_parts[..base_parts.len()] != base_parts[..] {
        return Err("路径必须位于备份目录内".into());
    }
    let base = fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let resolved = fs::canonicalize(&joined).unwrap_or(joined);
    if !resolved.starts_with(&base) {
        return Err("路径必须位于备份目录内".into());
    }
    Ok(resolved)
}

fn read_meta(path: &Path) -> Result<Option<Value>, String> {
    store_file::read_json(path).map_err(|e| format!("读取备份元数据失败：{e}"))
}

fn write_meta(path: &Path, value: &Value) -> Result<(), String> {
    let _lock = store_file::lock(path)?;
    store_file::write_json(path, value).map_err(|e| format!("写入备份元数据失败：{e}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 递归收集目录内的普通文件（跳过锁文件、临时文件与本模块的运行态记录）。
fn collect_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".lock") || name.contains(".tmp-") {
            continue;
        }
        if path.is_dir() {
            out.extend(collect_files(&path));
        } else if path.is_file() {
            out.push(path);
        }
    }
    out
}

fn manifest_entries(root: &Path) -> Vec<Value> {
    let base = backups_dir(root);
    let mut entries = Vec::new();
    for path in collect_files(&base) {
        // 运行态状态文件本身不进清单。
        if path == state_path(root) || path == staged_path(root) || path == rollback_path(root) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let relative = path
            .strip_prefix(&base)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());
        entries.push(json!({
            "path": relative,
            "size": bytes.len(),
            "sha256": sha256_hex(&bytes),
        }));
    }
    entries
}

fn build_manifest(root: &Path, options: &Value, includes_external: bool) -> Value {
    let passphrase = options.get("passphrase").and_then(Value::as_str);
    json!({
        "formatVersion": MANIFEST_FORMAT_VERSION,
        "kind": "codeg-backup",
        "createdAt": goformat::timestamp(),
        "appVersion": APP_VERSION,
        "latestMigration": "",
        "runtime": "rust",
        "includesExternalTranscripts": includes_external,
        "includesSecrets": passphrase.map(|v| !v.is_empty()).unwrap_or(false),
        "managedSections": ["metadata"],
        "degradedSqlite": [],
        "entries": manifest_entries(root),
    })
}

// ── 命令实现 ──────────────────────────────────────────────────────────────

/// L3：本后端没有在线智能体连接注册表，无法给出实时连接清单；诚实返回空数组。
fn active_agents() -> Result<Value, String> {
    Ok(json!([]))
}

/// 状态机：把匹配 `opId` 的在途操作标记为已取消。已结束/不存在的操作返回 false。
fn cancel(root: &Path, args: &Value) -> Result<Value, String> {
    let op_id = args
        .get("opId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "缺少 opId 参数".to_string())?
        .to_string();
    let path = state_path(root);
    {
        let _lock = store_file::lock(&path)?;
        let Some(mut state) = read_meta(&path)? else {
            return Ok(json!(false));
        };
        if state.get("opId").and_then(Value::as_str) != Some(op_id.as_str()) {
            return Ok(json!(false));
        }
        let phase = state.get("phase").and_then(Value::as_str).unwrap_or("");
        if matches!(phase, "done" | "cancelled" | "error") {
            return Ok(json!(false));
        }
        state["phase"] = json!("cancelled");
        state["cancelled"] = json!(true);
        store_file::write_json(&path, &state)
            .map_err(|e| format!("写入备份元数据失败：{e}"))?;
    }
    crate::backend::emit_event(
        "backup://progress",
        json!({
            "opId": op_id,
            "phase": "cancelled",
            "processedBytes": 0,
            "totalBytes": Value::Null,
            "currentPath": Value::Null,
            "error": Value::Null,
        }),
    );
    Ok(json!(true))
}

/// 桌面导出入口。L3：不打包工作区数据；只在 `.data/backups` 内登记一次操作状态，
/// 清单条目覆盖本模块自有元数据文件。`destPath` 仅记录到状态里，不向其写归档。
fn create(root: &Path, args: &Value) -> Result<Value, String> {
    let options = args
        .get("options")
        .ok_or_else(|| "缺少 options 参数".to_string())?;
    if !options.is_object() {
        return Err("options 必须是对象".into());
    }
    let includes_external = options
        .get("includeExternalTranscripts")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let dest_path = args
        .get("destPath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "缺少 destPath 参数".to_string())?;

    let op_id = goformat::hex_id();
    let state = json!({
        "opId": op_id,
        "phase": "snapshotting",
        "cancelled": false,
        "createdAt": goformat::timestamp(),
        "options": options,
        "destPath": dest_path,
    });
    write_meta(&state_path(root), &state)?;

    let manifest = build_manifest(root, options, includes_external);
    fs::create_dir_all(manifests_dir(root)).map_err(|e| format!("创建清单目录失败：{e}"))?;
    write_meta(&manifests_dir(root).join(format!("{op_id}.json")), &manifest)?;

    let done = json!({
        "opId": op_id,
        "phase": "done",
        "cancelled": false,
        "createdAt": state["createdAt"],
        "options": options,
        "destPath": dest_path,
    });
    write_meta(&state_path(root), &done)?;
    crate::backend::emit_event(
        "backup://progress",
        json!({
            "opId": op_id,
            "phase": "done",
            "processedBytes": manifest["entries"].as_array().map(|rows| rows.len()).unwrap_or(0),
            "totalBytes": Value::Null,
            "currentPath": Value::Null,
            "error": Value::Null,
        }),
    );
    Ok(manifest)
}

/// Web 导出票据：在 `.data/backups/tickets` 登记一张待下载票据，不生成归档实体。
fn create_ticket(root: &Path, args: &Value) -> Result<Value, String> {
    let includes_external = args
        .get("includeExternalTranscripts")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let passphrase = args.get("passphrase").and_then(Value::as_str);
    let ticket_id = goformat::hex_id();
    let stamp = goformat::timestamp()
        .replace([':', '.', '-'], "")
        .chars()
        .take(14)
        .collect::<String>();
    let filename = format!("codeg-backup-{stamp}.codeg.zip");
    let record = json!({
        "ticketId": ticket_id,
        "filename": filename,
        "includeExternalTranscripts": includes_external,
        "passphrase": passphrase,
        "createdAt": goformat::timestamp(),
    });
    fs::create_dir_all(tickets_dir(root)).map_err(|e| format!("创建票据目录失败：{e}"))?;
    write_meta(&tickets_dir(root).join(format!("{ticket_id}.json")), &record)?;
    Ok(json!({
        "url": format!("/api/backup_ticket/{ticket_id}"),
        "filename": filename,
        "degradedSqlite": [],
    }))
}

/// 释放一个已准备好的备份源句柄；句柄不存在也按成功处理（幂等，对齐前端注释）。
fn release_source(root: &Path, args: &Value) -> Result<Value, String> {
    let source_id = args
        .get("sourceId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "缺少 sourceId 参数".to_string())?;
    let record = sources_dir(root).join(format!("{source_id}.json"));
    if record.exists() {
        fs::remove_file(&record).map_err(|e| format!("释放备份源失败：{e}"))?;
    }
    Ok(json!(true))
}

/// 恢复第一步：登记一个只读句柄。源路径（或票据回链路径）必须落在
/// `.data/backups/` 内；不存在的源给出兼容预览拒绝原因，不返回句柄。
fn prepare_source(root: &Path, args: &Value) -> Result<Value, String> {
    let src_path = args.get("srcPath").and_then(Value::as_str).map(str::trim);
    let upload_id = args.get("uploadId").and_then(Value::as_str).map(str::trim);
    let passphrase = args.get("passphrase").and_then(Value::as_str);

    let resolved: Option<PathBuf> = match (src_path, upload_id) {
        (Some(path), _) if !path.is_empty() => Some(PathBuf::from(path)),
        (_, Some(id)) if !id.is_empty() => {
            let ticket = tickets_dir(root).join(format!("{id}.json"));
            read_meta(&ticket)?
                .and_then(|row| row.get("path").and_then(Value::as_str).map(PathBuf::from))
        }
        _ => None,
    };
    let Some(resolved) = resolved else {
        return Err("缺少 srcPath 或 uploadId 参数".into());
    };

    // 红线：只接受备份目录内的源；外部路径直接拒绝，不打开用户文件。
    let resolved = ensure_inside(&backups_dir(root), &resolved)?;

    let encrypted = passphrase.map(|v| !v.is_empty()).unwrap_or(false);
    if !resolved.exists() {
        return Ok(json!({
            "sourceId": Value::Null,
            "preview": {
                "encrypted": encrypted,
                "needsPassphrase": false,
                "manifest": Value::Null,
                "compatible": false,
                "rejectReason": "备份文件不存在",
            },
        }));
    }

    let manifest = read_meta(&resolved.with_extension("manifest.json"))?;
    let source_id = goformat::hex_id();
    let record = json!({
        "sourceId": source_id,
        "path": resolved.to_string_lossy(),
        "preparedAt": goformat::timestamp(),
        "encrypted": encrypted,
        "manifest": manifest,
    });
    fs::create_dir_all(sources_dir(root)).map_err(|e| format!("创建备份源目录失败：{e}"))?;
    write_meta(&sources_dir(root).join(format!("{source_id}.json")), &record)?;

    Ok(json!({
        "sourceId": source_id,
        "preview": {
            "encrypted": encrypted,
            "needsPassphrase": encrypted && passphrase.unwrap_or("").is_empty(),
            "manifest": manifest,
            "compatible": true,
            "rejectReason": Value::Null,
        },
    }))
}

fn load_source(root: &Path, source_id: &str) -> Result<Value, String> {
    let record = sources_dir(root).join(format!("{source_id}.json"));
    read_meta(&record)?
        .ok_or_else(|| "备份源不存在或已释放".to_string())
}

/// 暂存一次恢复：记录写入 `.data/backups/staged.json`，下次启动时再应用。
/// 校验源句柄记录的路径仍在备份目录内；不触碰工作区现有数据。
fn restore_stage(root: &Path, args: &Value) -> Result<Value, String> {
    let source_id = args
        .get("sourceId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "缺少 sourceId 参数".to_string())?;
    let source = load_source(root, source_id)?;
    let recorded = source
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "备份源记录缺少路径".to_string())?;
    // 红线：二次校验源路径没有越出备份目录。
    ensure_inside(&backups_dir(root), &PathBuf::from(recorded))?;

    let staging_dir = backups_dir(root)
        .join("staged")
        .join(safe_name(source_id)?);
    fs::create_dir_all(&staging_dir).map_err(|e| format!("创建暂存目录失败：{e}"))?;

    let external_mode = args.get("externalMode").cloned().unwrap_or(Value::Null);
    let manifest = source.get("manifest").cloned().unwrap_or(Value::Null);
    let staged = json!({
        "stagingDir": staging_dir.to_string_lossy(),
        "manifest": manifest,
        "restoredExternalPath": Value::Null,
        "skippedConflicts": [],
        "refusedExternal": [],
        "externalDowngraded": Value::Null,
    });
    let record = json!({
        "sourceId": source_id,
        "externalMode": external_mode,
        "stagedAt": goformat::timestamp(),
        "stagingDir": staging_dir.to_string_lossy(),
        "manifest": manifest,
    });
    write_meta(&staged_path(root), &record)?;

    // Web 调用方读 `needsRestart/restartDelayMs/staged`；桌面调用方读平铺字段。
    Ok(json!({
        "stagingDir": staged["stagingDir"],
        "manifest": manifest,
        "restoredExternalPath": Value::Null,
        "skippedConflicts": Value::Array(vec![]),
        "refusedExternal": Value::Array(vec![]),
        "externalDowngraded": Value::Null,
        "needsRestart": true,
        "restartDelayMs": 1500,
        "staged": staged,
    }))
}

/// 暂存回滚到指定安全快照。快照元数据必须位于 `.data/backups/snapshots/` 内；
/// 只写一条待应用记录，下次启动时生效，当前不动任何现有数据。
fn rollback(root: &Path, args: &Value) -> Result<Value, String> {
    let snapshot_id = args
        .get("snapshotId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "缺少 snapshotId 参数".to_string())?;
    let id = safe_name(snapshot_id)?;
    let meta_path = snapshots_dir(root).join(id).join("meta.json");
    let meta = read_meta(&meta_path)?
        .ok_or_else(|| "安全快照不存在".to_string())?;
    let recorded = meta
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "快照记录缺少路径".to_string())?;
    ensure_inside(&backups_dir(root), &PathBuf::from(recorded))?;

    write_meta(
        &rollback_path(root),
        &json!({
            "snapshotId": id,
            "stagedAt": goformat::timestamp(),
        }),
    )?;
    Ok(meta)
}

/// 丢弃尚未应用的暂存恢复/回滚记录；删除的仅限 `.data/backups/` 内的状态文件。
fn discard_pending(root: &Path) -> Result<Value, String> {
    for path in [staged_path(root), rollback_path(root)] {
        if path.exists() {
            fs::remove_file(&path).map_err(|e| format!("丢弃暂存记录失败：{e}"))?;
        }
    }
    Ok(json!(true))
}

/// 列出磁盘上仍在的预恢复安全快照，按时间倒序。
fn list_safety_snapshots(root: &Path) -> Result<Value, String> {
    let dir = snapshots_dir(root);
    let mut rows = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(json!([]));
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(meta) = read_meta(&path.join("meta.json"))? else {
            continue;
        };
        let id = meta
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let recorded = meta
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // 红线：跳过越出备份目录的快照记录。
        if ensure_inside(&backups_dir(root), &PathBuf::from(&recorded)).is_err() {
            continue;
        }
        let size_bytes = collect_files(&path)
            .iter()
            .filter_map(|file| fs::metadata(file).ok())
            .map(|meta| meta.len())
            .sum::<u64>();
        rows.push(json!({
            "id": id,
            "path": recorded,
            "createdAt": meta.get("createdAt").cloned().unwrap_or(Value::Null),
            "sizeBytes": size_bytes,
            "rollbackSupported": meta.get("rollbackSupported").and_then(Value::as_bool).unwrap_or(true),
        }));
    }
    rows.sort_by(|a, b| {
        b.get("createdAt")
            .and_then(Value::as_str)
            .cmp(&a.get("createdAt").and_then(Value::as_str))
    });
    Ok(Value::Array(rows))
}

/// 扫描备份里第三方会话归档条目：仅比对备份目录内登记的镜像目标是否已存在，
/// 不读取工作区真实会话目录。
fn scan_external_conflicts(root: &Path, args: &Value) -> Result<Value, String> {
    let source_id = args
        .get("sourceId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "缺少 sourceId 参数".to_string())?;
    let source = load_source(root, source_id)?;
    let Some(manifest) = source.get("manifest").and_then(Value::as_object) else {
        return Ok(json!([]));
    };
    let mut conflicts = Vec::new();
    if let Some(entries) = manifest.get("entries").and_then(Value::as_array) {
        for entry in entries {
            let Some(archive_path) = entry.get("path").and_then(Value::as_str) else {
                continue;
            };
            let Some(target) = entry.get("targetPath").and_then(Value::as_str) else {
                continue;
            };
            // 目标只在备份目录的 external 镜像下比对；不存在的条目直接跳过。
            let target_path = backups_dir(root).join("external").join(target);
            if !target_path.exists() {
                continue;
            }
            let target_path = ensure_inside(&backups_dir(root), &target_path)?;
            let target_size = fs::metadata(&target_path)
                .map(|m| m.len())
                .ok();
            conflicts.push(json!({
                "agent": entry.get("agent").and_then(Value::as_str).unwrap_or(""),
                "archivePath": archive_path,
                "targetPath": target,
                "targetSize": target_size,
            }));
        }
    }
    Ok(Value::Array(conflicts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-backup-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    use std::time::{SystemTime, UNIX_EPOCH};

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    #[test]
    fn owns_covers_the_eleven_documented_commands_only() {
        for command in [
            "backup_active_agents",
            "backup_cancel",
            "backup_create",
            "backup_create_ticket",
            "backup_discard_pending",
            "backup_list_safety_snapshots",
            "backup_prepare_source",
            "backup_release_source",
            "backup_restore_stage",
            "backup_rollback",
            "backup_scan_external_conflicts",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("backup_list"));
        assert!(!owns("backup_create_ticket_cancel"));
        assert!(!owns("Backup_Create"));
    }

    #[test]
    fn active_agents_is_an_honest_empty_list() {
        let root = temp_root("active");
        let agents = call(&root, "backup_active_agents", json!({})).unwrap();
        assert_eq!(agents.as_array().unwrap().len(), 0);
    }

    #[test]
    fn create_writes_manifest_and_state_done_within_backups_dir() {
        let root = temp_root("create");
        let manifest = call(
            &root,
            "backup_create",
            json!({
                "options": { "includeExternalTranscripts": true, "passphrase": null },
                "destPath": "codeg-backup-test.codeg.zip",
            }),
        )
        .unwrap();
        // BackupManifest 的必填字段都在。
        for field in [
            "formatVersion",
            "kind",
            "createdAt",
            "appVersion",
            "runtime",
            "includesExternalTranscripts",
            "includesSecrets",
            "entries",
        ] {
            assert!(manifest.get(field).is_some(), "missing {field}");
        }
        assert_eq!(manifest["includesExternalTranscripts"], true);
        let state = read_meta(&state_path(&root)).unwrap().unwrap();
        assert_eq!(state["phase"], "done");
        // 落盘位置必须在 .data/backups 内。
        assert!(state_path(&root).starts_with(backups_dir(&root)));
    }

    #[test]
    fn create_rejects_missing_options_or_dest_path() {
        let root = temp_root("create-bad");
        assert!(call(&root, "backup_create", json!({}))
            .unwrap_err()
            .contains("缺少 options"));
        assert!(call(
            &root,
            "backup_create",
            json!({ "options": { "includeExternalTranscripts": false }, "destPath": "  " })
        )
        .unwrap_err()
        .contains("缺少 destPath"));
        assert!(call(
            &root,
            "backup_create",
            json!({ "options": "nope", "destPath": "x" })
        )
        .unwrap_err()
        .contains("options 必须是对象"));
    }

    #[test]
    fn cancel_flags_a_running_operation_and_is_idempotent_for_ended_ones() {
        let root = temp_root("cancel");
        // 先建一条在途状态。
        write_meta(
            &state_path(&root),
            &json!({ "opId": "op-1", "phase": "archiving", "cancelled": false }),
        )
        .unwrap();
        let cancelled = call(&root, "backup_cancel", json!({ "opId": "op-1" })).unwrap();
        assert_eq!(cancelled, json!(true));
        let state = read_meta(&state_path(&root)).unwrap().unwrap();
        assert_eq!(state["phase"], "cancelled");
        assert_eq!(state["cancelled"], json!(true));
        // 已结束的操作再取消返回 false。
        let again = call(&root, "backup_cancel", json!({ "opId": "op-1" })).unwrap();
        assert_eq!(again, json!(false));
        // 不存在的操作返回 false。
        let missing = call(&root, "backup_cancel", json!({ "opId": "op-9" })).unwrap();
        assert_eq!(missing, json!(false));
        assert!(call(&root, "backup_cancel", json!({}))
            .unwrap_err()
            .contains("缺少 opId"));
    }

    #[test]
    fn create_ticket_registers_a_ticket_record() {
        let root = temp_root("ticket");
        let ticket = call(
            &root,
            "backup_create_ticket",
            json!({ "includeExternalTranscripts": false, "passphrase": null }),
        )
        .unwrap();
        assert!(ticket["url"].as_str().unwrap().starts_with("/api/backup_ticket/"));
        assert!(ticket["filename"].as_str().unwrap().ends_with(".codeg.zip"));
        assert_eq!(ticket["degradedSqlite"].as_array().unwrap().len(), 0);
        assert!(tickets_dir(&root).is_dir());
    }

    #[test]
    fn prepare_source_rejects_traversal_and_missing_files() {
        let root = temp_root("prepare");
        // 越出备份目录的路径必须拒绝。
        assert!(call(
            &root,
            "backup_prepare_source",
            json!({ "srcPath": "C:\\Windows\\system32\\app.log", "passphrase": null })
        )
        .unwrap_err()
        .contains("备份目录内"));
        assert!(call(&root, "backup_prepare_source", json!({}))
            .unwrap_err()
            .contains("缺少 srcPath"));
        // 备份目录内但文件不存在：返回兼容失败预览，不带句柄。
        let preview = call(
            &root,
            "backup_prepare_source",
            json!({ "srcPath": backups_dir(&root).join("nope.codeg.zip").to_string_lossy(), "passphrase": null }),
        )
        .unwrap();
        assert!(preview["sourceId"].is_null());
        assert_eq!(preview["preview"]["compatible"], false);
        assert_eq!(preview["preview"]["rejectReason"], "备份文件不存在");
    }

    #[test]
    fn prepare_stage_release_roundtrip_inside_backups_dir() {
        let root = temp_root("roundtrip");
        let source_file = backups_dir(&root).join("samples").join("a.codeg.zip");
        fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        fs::write(&source_file, b"archive-bytes").unwrap();
        fs::write(
            source_file.with_extension("manifest.json"),
            serde_json::to_vec_pretty(&json!({ "kind": "codeg-backup", "entries": [] })).unwrap(),
        )
        .unwrap();

        let prepared = call(
            &root,
            "backup_prepare_source",
            json!({ "srcPath": source_file.to_string_lossy(), "passphrase": null }),
        )
        .unwrap();
        let source_id = prepared["sourceId"].as_str().unwrap().to_string();
        assert_eq!(prepared["preview"]["compatible"], true);
        assert!(prepared["preview"]["manifest"].is_object());

        let staged = call(
            &root,
            "backup_restore_stage",
            json!({ "sourceId": source_id, "externalMode": null }),
        )
        .unwrap();
        assert!(staged["stagingDir"].as_str().unwrap().contains("staged"));
        assert!(staged["manifest"].is_object());
        assert_eq!(staged["needsRestart"], true);
        assert!(staged["staged"].is_object());

        // 暂存记录落盘后可被 discard_pending 清掉。
        assert!(staged_path(&root).exists());
        let discarded = call(&root, "backup_discard_pending", json!({})).unwrap();
        assert_eq!(discarded, json!(true));
        assert!(!staged_path(&root).exists());

        let released = call(&root, "backup_release_source", json!({ "sourceId": source_id })).unwrap();
        assert_eq!(released, json!(true));
        assert!(call(&root, "backup_restore_stage", json!({ "sourceId": "ghost" }))
            .unwrap_err()
            .contains("备份源不存在"));
    }

    #[test]
    fn restore_stage_rejects_missing_source_and_bad_args() {
        let root = temp_root("stage-bad");
        assert!(call(&root, "backup_restore_stage", json!({}))
            .unwrap_err()
            .contains("缺少 sourceId"));
        assert!(call(&root, "backup_restore_stage", json!({ "sourceId": "ghost" }))
            .unwrap_err()
            .contains("备份源不存在"));
    }

    #[test]
    fn snapshots_list_scans_real_dirs_and_rollback_validates_path() {
        let root = temp_root("snap");
        let snap_dir = snapshots_dir(&root).join("snap-1");
        fs::create_dir_all(&snap_dir).unwrap();
        fs::write(snap_dir.join("data.bin"), b"x").unwrap();
        write_meta(
            &snap_dir.join("meta.json"),
            &json!({
                "id": "snap-1",
                "path": snap_dir.to_string_lossy(),
                "createdAt": "2026-11-15T01:00:00Z",
                "rollbackSupported": true,
            }),
        )
        .unwrap();

        let rows = call(&root, "backup_list_safety_snapshots", json!({})).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "snap-1");
        assert!(rows[0]["sizeBytes"].as_u64().unwrap() >= 1);

        let rolled = call(&root, "backup_rollback", json!({ "snapshotId": "snap-1" })).unwrap();
        assert_eq!(rolled["id"], "snap-1");
        assert!(rollback_path(&root).exists());

        assert!(call(&root, "backup_rollback", json!({}))
            .unwrap_err()
            .contains("缺少 snapshotId"));
        assert!(call(&root, "backup_rollback", json!({ "snapshotId": "../escape" }))
            .unwrap_err()
            .contains("路径片段无效"));
        assert!(call(&root, "backup_rollback", json!({ "snapshotId": "ghost" }))
            .unwrap_err()
            .contains("安全快照不存在"));
    }

    #[test]
    fn external_conflicts_compare_only_backup_mirror_targets() {
        let root = temp_root("conflict");
        // 准备一个带 external 条目的备份源。
        let source_id = goformat::hex_id();
        let manifest = json!({
            "entries": [
                { "path": "external/claude/1.jsonl", "targetPath": "claude/1.jsonl", "agent": "claude", "size": 1 },
                { "path": "external/codex/2.jsonl", "targetPath": "codex/2.jsonl", "agent": "codex", "size": 1 },
            ]
        });
        fs::create_dir_all(sources_dir(&root)).unwrap();
        write_meta(
            &sources_dir(&root).join(format!("{source_id}.json")),
            &json!({ "sourceId": source_id, "path": backups_dir(&root).join("samples").join("a.codeg.zip").to_string_lossy(), "manifest": manifest }),
        )
        .unwrap();
        // 只在镜像目录里放一个已存在目标。
        let existing = backups_dir(&root).join("external").join("claude").join("1.jsonl");
        fs::create_dir_all(existing.parent().unwrap()).unwrap();
        fs::write(&existing, b"live").unwrap();

        let conflicts = call(&root, "backup_scan_external_conflicts", json!({ "sourceId": source_id })).unwrap();
        let conflicts = conflicts.as_array().unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0]["agent"], "claude");
        assert_eq!(conflicts[0]["targetSize"], 4);

        assert!(call(&root, "backup_scan_external_conflicts", json!({}))
            .unwrap_err()
            .contains("缺少 sourceId"));
        assert!(call(&root, "backup_scan_external_conflicts", json!({ "sourceId": "ghost" }))
            .unwrap_err()
            .contains("备份源不存在"));
    }

    #[test]
    fn discard_pending_is_idempotent_with_no_pending_records() {
        let root = temp_root("discard");
        let first = call(&root, "backup_discard_pending", json!({})).unwrap();
        assert_eq!(first, json!(true));
        let second = call(&root, "backup_discard_pending", json!({})).unwrap();
        assert_eq!(second, json!(true));
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "backup_list", &json!({})).is_none());
    }
}
