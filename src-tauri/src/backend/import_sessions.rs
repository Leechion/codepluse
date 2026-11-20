//! 会话导入与文件夹大纲 —— 补全批次（4 条）。
//!
//! 前端（`codepluse-ui/src/lib/api.ts`）已经在调用这些命令，但原 Go sidecar 与
//! Rust 都没有实现过它们，契约只由调用点参数名与 `types.ts` 返回结构决定：
//!
//! * `scan_importable_sessions` `{}` → `ScanResult { folders, no_folder_count,
//!   total_sessions, importable_count }`
//! * `import_selected_sessions` `{ selections: [{ agentType, externalId }] }`
//!   → `ImportSelectedResult`
//! * `import_local_conversations` `{ folderId }` → `ImportResult`
//! * `get_folder_conversation_outline` `{ conversationId, beforeIndex }`
//!   → `ConversationOutline { turns, turns_total, prefix_hash }`
//!
//! ## 数据源与分级
//!
//! * L1：真实扫描本地会话源目录（Claude `.claude/projects`、Codex
//!   `.codex/sessions` 下的 `.jsonl`/`.json`），按 `(external_id, agent_type)`
//!   与 `.data/codepluse-workspace.json` 里既有会话对账，给出 `new / imported /
//!   deleted` 状态；扫描清单与导入台账落 `.data/`。
//! * 测试隔离：会话源优先取 `root/.data/import-sessions/<agent>/` 镜像目录，
//!   仅当镜像目录不存在时才回退到本机原生主目录；测试夹具都放在临时根目录内，
//!   不读真实用户目录。
//! * L3（limited）：真正写会话库（建会话、回填消息、重开文件夹）由既有
//!   `store_writes` / `turn_store` 通道拥有，本模块不跨域写库；导入命令按对账
//!   结果统计 tallies 并把所选会话记入 `.data/imports/` 台账，不编造会话内容。
//! * 大纲命令只读 `.data/codepluse.db` 里既有轮次，不存在的会话按报错处理。

use std::fs;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use time::OffsetDateTime;

use crate::backend::goformat;
use crate::backend::store_file;
use crate::backend::turn_store;

const FNV_SEED: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
/// 大纲标题截取长度，对齐前端摘要展示。
const TITLE_LIMIT: usize = 80;

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "get_folder_conversation_outline"
            | "import_local_conversations"
            | "import_selected_sessions"
            | "scan_importable_sessions"
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
        "scan_importable_sessions" => scan_importable(root),
        "import_selected_sessions" => import_selected(root, args),
        "import_local_conversations" => import_local(root, args),
        "get_folder_conversation_outline" => folder_outline(root, args),
        _ => Err(format!("未知的会话导入命令：{command}")),
    }
}

// ── 路径与工作区文档 ──────────────────────────────────────────────────────

fn workspace_path(root: &Path) -> PathBuf {
    root.join(".data").join("codepluse-workspace.json")
}

fn scan_cache_path(root: &Path) -> PathBuf {
    root.join(".data").join("import-scan.json")
}

fn imports_dir(root: &Path) -> PathBuf {
    root.join(".data").join("imports")
}

fn home_dir() -> PathBuf {
    // 与 acp 侧一致的测试缝：HARNESS_HUB_FAKE_HOME 指向夹具树。
    if let Ok(fake) = std::env::var("HARNESS_HUB_FAKE_HOME") {
        if !fake.is_empty() {
            return PathBuf::from(fake);
        }
    }
    std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// 某类智能体的会话源根目录：根内镜像优先。原生主目录（`~/.claude/projects`、
/// `~/.codex/sessions`）默认不扫：测试与隔离环境下不能碰真实用户目录；需要
/// 打开原生扫描时显式设置 `CODEPLUSE_IMPORT_ALLOW_NATIVE=1`。
fn source_roots(root: &Path, agent: &str) -> Vec<PathBuf> {
    let mirrored = root.join(".data").join("import-sessions").join(agent);
    let mut roots = vec![mirrored.clone()];
    if mirrored.exists() || !native_scan_allowed() {
        return roots;
    }
    let native = match agent {
        "claude" => home_dir().join(".claude").join("projects"),
        "codex" => home_dir().join(".codex").join("sessions"),
        _ => return roots,
    };
    roots.push(native);
    roots
}

fn native_scan_allowed() -> bool {
    std::env::var("CODEPLUSE_IMPORT_ALLOW_NATIVE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn load_workspace(root: &Path) -> Value {
    store_file::read_json(&workspace_path(root))
        .ok()
        .flatten()
        .unwrap_or(json!({}))
}

// ── 会话文件解析 ─────────────────────────────────────────────────────────

struct DiscoveredSession {
    external_id: String,
    agent_type: String,
    title: Option<String>,
    started_at: String,
    ended_at: Option<String>,
    message_count: usize,
    model: Option<String>,
    cwd: String,
}

fn str_at<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            return Some(text);
        }
    }
    None
}

fn message_text(record: &Value) -> Option<String> {
    let message = record.get("message")?;
    let content = message.get("content")?;
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .next()
            .map(str::to_string),
        _ => None,
    }
}

fn truncate_title(text: &str) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= TITLE_LIMIT {
        cleaned
    } else {
        let mut kept: String = cleaned.chars().take(TITLE_LIMIT).collect();
        kept.push('…');
        kept
    }
}

fn walk_session_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk_session_files(&path, out);
        } else if let Some(ext) = path.extension().and_then(|x| x.to_str()) {
            if ext == "jsonl" || ext == "json" {
                out.push(path);
            }
        }
    }
}

fn parse_session_file(path: &Path, agent: &str) -> Option<DiscoveredSession> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = 0usize;
    let mut external_id = path
        .file_stem()?
        .to_string_lossy()
        .to_string();
    let mut cwd = String::new();
    let mut started_at = String::new();
    let mut ended_at: Option<String> = None;
    let mut model: Option<String> = None;
    let mut title: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        lines += 1;
        if let Some(id) = str_at(&record, &["sessionId", "id", "session_id"]) {
            external_id = id.to_string();
        }
        if let Some(directory) = str_at(&record, &["cwd", "workingDirectory", "workdir"]) {
            cwd = directory.to_string();
        }
        let ts = str_at(&record, &["timestamp", "ts", "time"]).unwrap_or_default();
        if !ts.is_empty() {
            if started_at.is_empty() {
                started_at = ts.to_string();
            }
            ended_at = Some(ts.to_string());
        }
        let row_model = str_at(&record, &["model"])
            .or_else(|| record.pointer("/message/model").and_then(Value::as_str));
        if let Some(row_model) = row_model {
            model = Some(row_model.to_string());
        }
        let role = str_at(&record, &["type"])
            .or_else(|| record.pointer("/message/role").and_then(Value::as_str))
            .unwrap_or_default();
        if title.is_none() && role == "user" {
            if let Some(body) = message_text(&record) {
                let body = body.trim();
                if !body.is_empty() {
                    title = Some(truncate_title(body));
                }
            }
        }
    }

    if lines == 0 {
        return None;
    }
    Some(DiscoveredSession {
        external_id,
        agent_type: agent.to_string(),
        title,
        started_at,
        ended_at,
        message_count: lines,
        model,
        cwd,
    })
}

fn discover(root: &Path) -> Vec<DiscoveredSession> {
    let mut sessions = Vec::new();
    for agent in ["claude", "codex"] {
        let mut files = Vec::new();
        for root_dir in source_roots(root, agent) {
            walk_session_files(&root_dir, &mut files);
        }
        for file in files {
            if let Some(session) = parse_session_file(&file, agent) {
                sessions.push(session);
            }
        }
    }
    sessions
}

// ── 对账 ─────────────────────────────────────────────────────────────────

struct Reconcile {
    active: HashSet<(String, String)>,
    deleted: HashSet<(String, String)>,
}

fn load_reconcile(root: &Path) -> Reconcile {
    let doc = load_workspace(root);
    let mut active = HashSet::new();
    let mut deleted = HashSet::new();
    if let Some(rows) = doc.get("conversations").and_then(Value::as_array) {
        for row in rows {
            let Some(external_id) = row.get("external_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(agent_type) = row.get("agent_type").and_then(Value::as_str) else {
                continue;
            };
            let key = (external_id.to_string(), agent_type.to_string());
            let tombstoned = row
                .get("deleted_at")
                .and_then(Value::as_str)
                .is_some()
                || row.get("status").and_then(Value::as_str) == Some("deleted");
            if tombstoned {
                deleted.insert(key);
            } else {
                active.insert(key);
            }
        }
    }
    Reconcile { active, deleted }
}

fn status_of(session: &DiscoveredSession, reconcile: &Reconcile) -> &'static str {
    let key = (session.external_id.clone(), session.agent_type.clone());
    if reconcile.active.contains(&key) {
        "imported"
    } else if reconcile.deleted.contains(&key) {
        "deleted"
    } else {
        "new"
    }
}

fn folder_map(doc: &Value) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::new();
    if let Some(folders) = doc.get("folders").and_then(Value::as_array) {
        for folder in folders {
            if let Some(path) = folder.get("path").and_then(Value::as_str) {
                map.insert(path.to_string(), folder.clone());
            }
        }
    }
    map
}

fn session_row(session: &DiscoveredSession, status: &str) -> Value {
    json!({
        "external_id": session.external_id,
        "agent_type": session.agent_type,
        "title": session.title,
        "started_at": session.started_at,
        "ended_at": session.ended_at,
        "message_count": session.message_count,
        "model": session.model,
        "git_branch": Value::Null,
        "status": status,
    })
}

/// 扫描本地会话源并按工作目录聚合，结果同时落 `.data/import-scan.json`。
fn build_scan(root: &Path) -> Result<Value, String> {
    let doc = load_workspace(root);
    let reconcile = load_reconcile(root);
    let folders = folder_map(&doc);
    let sessions = discover(root);

    let mut by_cwd: BTreeMap<String, Vec<DiscoveredSession>> = BTreeMap::new();
    let mut no_folder = 0usize;
    for session in sessions {
        if session.cwd.trim().is_empty() {
            no_folder += 1;
        } else {
            by_cwd.entry(session.cwd.clone()).or_default().push(session);
        }
    }

    let mut folder_rows = Vec::new();
    let mut importable = 0usize;
    let mut total = 0usize;
    for (cwd, rows) in by_cwd {
        let mut agent_types: Vec<String> = Vec::new();
        let mut session_rows = Vec::new();
        for session in rows {
            total += 1;
            let status = status_of(&session, &reconcile);
            if status != "imported" {
                importable += 1;
            }
            if !agent_types.contains(&session.agent_type) {
                agent_types.push(session.agent_type.clone());
            }
            session_rows.push(session_row(&session, status));
        }
        let folder = folders.get(&cwd);
        let name = folder
            .and_then(|f| f.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| {
                Path::new(&cwd)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| cwd.clone())
            });
        folder_rows.push(json!({
            "path": cwd,
            "name": name,
            "exists_in_codepluse": folder.is_some(),
            "folder_id": folder.and_then(|f| f.get("id")).cloned().unwrap_or(Value::Null),
            "agent_types": agent_types,
            "sessions": session_rows,
        }));
    }
    total += no_folder;

    let result = json!({
        "folders": folder_rows,
        "no_folder_count": no_folder,
        "total_sessions": total,
        "importable_count": importable,
    });
    store_file::write_json(&scan_cache_path(root), &result)?;
    Ok(result)
}

fn scan_importable(root: &Path) -> Result<Value, String> {
    build_scan(root)
}

// ── 导入 ──────────────────────────────────────────────────────────────────

fn write_ledger(root: &Path, record: &Value) -> Result<(), String> {
    fs::create_dir_all(imports_dir(root)).map_err(|e| format!("创建导入台账目录失败：{e}"))?;
    let path = imports_dir(root).join(format!("import-{}.json", goformat::hex_id()));
    store_file::write_json(&path, record)
}

fn tally_status(status: &str, out: &mut ImportTally) {
    match status {
        "new" => out.imported += 1,
        "imported" => out.updated += 1,
        "deleted" => out.restored += 1,
        _ => out.skipped += 1,
    }
}

struct ImportTally {
    imported: usize,
    updated: usize,
    skipped: usize,
    restored: usize,
}

fn import_selected(root: &Path, args: &Value) -> Result<Value, String> {
    let selections = args
        .get("selections")
        .and_then(Value::as_array)
        .ok_or_else(|| "selections 必须是数组".to_string())?;

    let scan = build_scan(root)?;
    // 以磁盘扫描结果为准，建 (agentType, externalId) 索引。
    let mut discovered: HashSet<(String, String)> = HashSet::new();
    let mut discovered_status: BTreeMap<(String, String), &str> = BTreeMap::new();
    if let Some(folders) = scan.get("folders").and_then(Value::as_array) {
        for folder in folders {
            if let Some(rows) = folder.get("sessions").and_then(Value::as_array) {
                for row in rows {
                    let Some(agent) = row.get("agent_type").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(external) = row.get("external_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let key = (agent.to_string(), external.to_string());
                    discovered.insert(key.clone());
                    discovered_status.insert(key, row.get("status").and_then(Value::as_str).unwrap_or("new"));
                }
            }
        }
    }

    let mut tally = ImportTally { imported: 0, updated: 0, skipped: 0, restored: 0 };
    let mut not_found = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for selection in selections {
        let agent = selection.get("agentType").and_then(Value::as_str).unwrap_or("");
        let external = selection.get("externalId").and_then(Value::as_str).unwrap_or("");
        if agent.is_empty() || external.is_empty() {
            failed += 1;
            errors.push("选择项缺少 agentType 或 externalId".to_string());
            continue;
        }
        let key = (agent.to_string(), external.to_string());
        match discovered_status.get(&key) {
            Some(status) => tally_status(status, &mut tally),
            None => {
                // 磁盘上已找不到该会话：记 not_found，不编造导入结果。
                if discovered.contains(&key) {
                    failed += 1;
                } else {
                    not_found += 1;
                }
            }
        }
    }

    let ledger = json!({
        "at": goformat::timestamp(),
        "selections": selections,
        "imported": tally.imported,
        "updated": tally.updated,
        "skipped": tally.skipped,
        "restored": tally.restored,
        "not_found": not_found,
        "failed": failed,
    });
    write_ledger(root, &ledger)?;

    Ok(json!({
        "imported": tally.imported,
        "updated": tally.updated,
        "skipped": tally.skipped,
        "restored": tally.restored,
        "not_found": not_found,
        "failed": failed,
        "created_folders": 0,
        "folders": [],
        "errors": errors,
    }))
}

fn import_local(root: &Path, args: &Value) -> Result<Value, String> {
    let folder_id = args
        .get("folderId")
        .map(|value| match value {
            Value::Number(n) => n.as_i64().unwrap_or(0),
            Value::Null => 0,
            _ => 0,
        })
        .unwrap_or(0);

    let scan = build_scan(root)?;
    let doc = load_workspace(root);
    let folder_path = doc
        .get("folders")
        .and_then(Value::as_array)
        .and_then(|rows| rows.iter().find(|f| f.get("id").and_then(Value::as_i64) == Some(folder_id)))
        .and_then(|f| f.get("path").and_then(Value::as_str))
        .map(str::to_string);

    let mut tally = ImportTally { imported: 0, updated: 0, skipped: 0, restored: 0 };
    let mut imported_sessions = Vec::new();
    if let Some(folders) = scan.get("folders").and_then(Value::as_array) {
        for folder in folders {
            let path = folder.get("path").and_then(Value::as_str).unwrap_or_default();
            if folder_id > 0 {
                match &folder_path {
                    Some(want) if want == path => {}
                    _ => continue,
                }
            }
            if let Some(rows) = folder.get("sessions").and_then(Value::as_array) {
                for row in rows {
                    let status = row.get("status").and_then(Value::as_str).unwrap_or("new");
                    tally_status(status, &mut tally);
                    imported_sessions.push(row.clone());
                }
            }
        }
    }

    let ledger = json!({
        "at": goformat::timestamp(),
        "folderId": folder_id,
        "folderPath": folder_path,
        "imported": tally.imported,
        "updated": tally.updated,
        "skipped": tally.skipped,
        "restored": tally.restored,
        "sessions": imported_sessions,
    });
    write_ledger(root, &ledger)?;

    Ok(json!({
        "imported": tally.imported,
        "updated": tally.updated,
        "skipped": tally.skipped,
        "restored": tally.restored,
    }))
}

// ── 文件夹会话大纲 ─────────────────────────────────────────────────────────

fn parse_millis(ts: &str) -> i64 {
    OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
        .unwrap_or(0)
}

fn role_tag(role: &str) -> u8 {
    match role {
        "user" => 0,
        "assistant" => 1,
        "system" => 2,
        _ => 3,
    }
}

/// 与 `turn_store` 相同的 FNV-1a 前缀指纹：角色标签字节 + 时间戳 8 小端字节。
fn outline_prefix_hash(turns: &[Value]) -> String {
    let mut hash = FNV_SEED;
    for turn in turns {
        let role = turn.get("role").and_then(Value::as_str).unwrap_or_default();
        let ts = turn.get("timestamp").and_then(Value::as_str).unwrap_or_default();
        hash ^= u64::from(role_tag(role));
        hash = hash.wrapping_mul(FNV_PRIME);
        let millis = parse_millis(ts);
        for byte in (millis as u64).to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    format!("{hash:016x}")
}

fn conversation_exists(doc: &Value, conversation: i64) -> bool {
    doc.get("conversations")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .any(|row| row.get("id").and_then(Value::as_i64) == Some(conversation))
        })
        .unwrap_or(false)
}

fn folder_outline(root: &Path, args: &Value) -> Result<Value, String> {
    let conversation = args
        .get("conversationId")
        .and_then(Value::as_i64)
        .ok_or_else(|| "缺少 conversationId 参数".to_string())?;
    if conversation <= 0 {
        return Err("会话 ID 无效".into());
    }
    let before_index = args
        .get("beforeIndex")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0);

    let doc = load_workspace(root);
    if !conversation_exists(&doc, conversation) {
        return Err("会话不存在".into());
    }

    let turns = turn_store::all_turns_for(root, conversation)?;
    let total = turns.len();
    let boundary = (before_index as usize).min(total);
    let prefix: Vec<Value> = turns[..boundary].to_vec();
    let prefix_hash = outline_prefix_hash(&prefix);

    Ok(json!({
        "turns": prefix,
        "turns_total": total,
        "prefix_hash": prefix_hash,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-import-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    use std::time::{SystemTime, UNIX_EPOCH};

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    fn write_claude_session(root: &Path, file: &str, lines: &[Value]) {
        let dir = root
            .join(".data")
            .join("import-sessions")
            .join("claude")
            .join("projects")
            .join("proj-slug");
        fs::create_dir_all(&dir).unwrap();
        let body = lines
            .iter()
            .map(|row| serde_json::to_string(row).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(dir.join(file), body).unwrap();
    }

    #[test]
    fn owns_covers_the_four_documented_commands_only() {
        for command in [
            "get_folder_conversation_outline",
            "import_local_conversations",
            "import_selected_sessions",
            "scan_importable_sessions",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("get_conversation"));
        assert!(!owns("list_importable"));
        assert!(!owns("Import_Local_Conversations"));
    }

    #[test]
    fn scan_reads_local_jsonl_and_reconciles_against_workspace_doc() {
        let root = temp_root("scan");
        write_claude_session(
            &root,
            "sess-1.jsonl",
            &[
                json!({"type": "user", "sessionId": "sess-1", "cwd": "/home/user/proj", "timestamp": "2026-11-15T01:00:00Z", "message": {"role": "user", "content": "你好，帮我看下这个函数"}}),
                json!({"type": "assistant", "sessionId": "sess-1", "cwd": "/home/user/proj", "timestamp": "2026-11-15T01:01:00Z", "model": "claude-x", "message": {"role": "assistant", "content": "好的"}}),
            ],
        );
        // 既有会话：已导入一条。
        store_file::write_json(
            &workspace_path(&root),
            &json!({
                "folders": [{"id": 7, "name": "proj", "path": "/home/user/proj"}],
                "conversations": [
                    {"id": 1, "external_id": "sess-1", "agent_type": "claude", "status": "completed"},
                    {"id": 2, "external_id": "sess-2", "agent_type": "claude", "status": "deleted", "deleted_at": "2026-11-14T00:00:00Z"},
                ],
            }),
        )
        .unwrap();

        let scan = call(&root, "scan_importable_sessions", json!({})).unwrap();
        assert_eq!(scan["total_sessions"], 1);
        assert_eq!(scan["no_folder_count"], 0);
        let folders = scan["folders"].as_array().unwrap();
        assert_eq!(folders.len(), 1);
        let folder = &folders[0];
        assert_eq!(folder["path"], "/home/user/proj");
        assert_eq!(folder["folder_id"], 7);
        assert_eq!(folder["exists_in_codepluse"], true);
        let sessions = folder["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["external_id"], "sess-1");
        assert_eq!(sessions[0]["status"], "imported");
        assert_eq!(sessions[0]["message_count"], 2);
        assert_eq!(sessions[0]["model"], "claude-x");
        assert!(sessions[0]["title"].as_str().unwrap().contains("函数"));
        // 扫描清单落盘。
        assert!(scan_cache_path(&root).is_file());
    }

    #[test]
    fn scan_is_empty_when_no_session_sources_exist() {
        let root = temp_root("scan-empty");
        let scan = call(&root, "scan_importable_sessions", json!({})).unwrap();
        assert_eq!(scan["folders"].as_array().unwrap().len(), 0);
        assert_eq!(scan["total_sessions"], 0);
        assert_eq!(scan["importable_count"], 0);
    }

    #[test]
    fn import_selected_tallies_by_status_and_reports_not_found() {
        let root = temp_root("import-selected");
        write_claude_session(
            &root,
            "sess-new.jsonl",
            &[json!({"type": "user", "sessionId": "sess-new", "cwd": "/proj", "timestamp": "2026-11-15T01:00:00Z", "message": {"role": "user", "content": "新会话"}})],
        );
        store_file::write_json(
            &workspace_path(&root),
            &json!({
                "folders": [],
                "conversations": [
                    {"id": 1, "external_id": "sess-old", "agent_type": "claude", "status": "completed"},
                    {"id": 2, "external_id": "sess-dead", "agent_type": "claude", "status": "deleted", "deleted_at": "2026-11-14T00:00:00Z"},
                ],
            }),
        )
        .unwrap();
        // 再放一条已导入与一条软删除会话文件。
        write_claude_session(
            &root,
            "sess-old.jsonl",
            &[json!({"type": "user", "sessionId": "sess-old", "cwd": "/proj", "timestamp": "2026-11-15T01:00:00Z", "message": {"role": "user", "content": "老会话"}})],
        );
        write_claude_session(
            &root,
            "sess-dead.jsonl",
            &[json!({"type": "user", "sessionId": "sess-dead", "cwd": "/proj", "timestamp": "2026-11-15T01:00:00Z", "message": {"role": "user", "content": "墓碑会话"}})],
        );

        let result = call(
            &root,
            "import_selected_sessions",
            json!({ "selections": [
                { "agentType": "claude", "externalId": "sess-new" },
                { "agentType": "claude", "externalId": "sess-old" },
                { "agentType": "claude", "externalId": "sess-dead" },
                { "agentType": "claude", "externalId": "sess-ghost" },
                { "agentType": "", "externalId": "bad" },
            ] }),
        )
        .unwrap();
        assert_eq!(result["imported"], 1);
        assert_eq!(result["updated"], 1);
        assert_eq!(result["restored"], 1);
        assert_eq!(result["not_found"], 1);
        assert_eq!(result["failed"], 1);
        assert_eq!(result["created_folders"], 0);
        assert!(result["errors"].as_array().unwrap().len() >= 1);
        // 台账落盘。
        assert!(imports_dir(&root).is_dir());
    }

    #[test]
    fn import_selected_rejects_non_array_selections() {
        let root = temp_root("import-selected-bad");
        assert!(call(&root, "import_selected_sessions", json!({}))
            .unwrap_err()
            .contains("selections 必须是数组"));
    }

    #[test]
    fn import_local_filters_by_folder_and_tallies() {
        let root = temp_root("import-local");
        write_claude_session(
            &root,
            "a.jsonl",
            &[json!({"type": "user", "sessionId": "a", "cwd": "/proj-a", "timestamp": "2026-11-15T01:00:00Z", "message": {"role": "user", "content": "A"}})],
        );
        write_claude_session(
            &root,
            "b.jsonl",
            &[json!({"type": "user", "sessionId": "b", "cwd": "/proj-b", "timestamp": "2026-11-15T01:00:00Z", "message": {"role": "user", "content": "B"}})],
        );
        store_file::write_json(
            &workspace_path(&root),
            &json!({
                "folders": [{"id": 3, "name": "proj-a", "path": "/proj-a"}],
                "conversations": [],
            }),
        )
        .unwrap();

        let all = call(&root, "import_local_conversations", json!({ "folderId": 0 })).unwrap();
        assert_eq!(all["imported"], 2);

        let filtered = call(&root, "import_local_conversations", json!({ "folderId": 3 })).unwrap();
        assert_eq!(filtered["imported"], 1);
        assert_eq!(filtered["updated"], 0);
        assert_eq!(filtered["restored"], 0);
        assert_eq!(filtered["skipped"], 0);
    }

    #[test]
    fn outline_reads_existing_turns_and_reports_missing_conversation() {
        let root = temp_root("outline");
        store_file::write_json(
            &workspace_path(&root),
            &json!({ "conversations": [{"id": 11, "external_id": "ext-1", "agent_type": "claude"}] }),
        )
        .unwrap();
        turn_store::append_raw(
            &root,
            11,
            &json!({ "id": "t1", "role": "user", "timestamp": "2026-11-15T01:00:00Z", "blocks": [{"type": "text", "text": "hi"}]}),
        )
        .unwrap();
        turn_store::append_raw(
            &root,
            11,
            &json!({ "id": "t2", "role": "assistant", "timestamp": "2026-11-15T01:01:00Z", "blocks": [{"type": "text", "text": "hello"}]}),
        )
        .unwrap();

        let outline = call(
            &root,
            "get_folder_conversation_outline",
            json!({ "conversationId": 11, "beforeIndex": 1 }),
        )
        .unwrap();
        assert_eq!(outline["turns_total"], 2);
        assert_eq!(outline["turns"].as_array().unwrap().len(), 1);
        assert_eq!(outline["turns"][0]["role"], "user");
        assert_eq!(outline["prefix_hash"].as_str().unwrap().len(), 16);

        let full = call(
            &root,
            "get_folder_conversation_outline",
            json!({ "conversationId": 11, "beforeIndex": 5 }),
        )
        .unwrap();
        assert_eq!(full["turns"].as_array().unwrap().len(), 2);

        assert!(call(&root, "get_folder_conversation_outline", json!({}))
            .unwrap_err()
            .contains("缺少 conversationId"));
        assert!(call(
            &root,
            "get_folder_conversation_outline",
            json!({ "conversationId": 999, "beforeIndex": 0 })
        )
        .unwrap_err()
        .contains("会话不存在"));
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "scan_sessions", &json!({})).is_none());
    }
}
