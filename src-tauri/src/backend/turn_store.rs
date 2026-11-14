//! Turn storage (SQLite), ported from `server-go/internal/codeg/turnstore.go`
//! + `history.go`, plus the conversation commands that read it:
//! `get_conversation`, `get_folder_conversation`, `get_folder_conversation_turns`,
//! `delete_conversation`, and the four `token_usage_*` aggregations.
//!
//! The Go runtime migrated turns out of codeg-workspace.json into SQLite
//! (.data/codeg.db, WAL); the JSON document keeps only conversation metadata.
//! Each row carries cumulative prefix state (FNV-1a fingerprint, role counts,
//! max timestamp) so paging never re-reads the prefix. Fingerprints must stay
//! byte-compatible with the client's extendPrefixFingerprint (turn-window.ts)
//! and with Go's fnvTurn: per turn, the role tag byte (user=0, assistant=1,
//! system=2) then the timestamp epoch-millis as 8 little-endian bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::backend::goformat;
use crate::backend::store_file;
use crate::backend::store_writes::Outcome;

const FNV_SEED: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
/// Exclusive upper bound for "all turns" (fits int32, like Go).
const MAX_TURN_SEQ: i64 = (1 << 31) - 1;

const TURN_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS turns (
	conversation_id INTEGER NOT NULL,
	seq             INTEGER NOT NULL,
	data            TEXT    NOT NULL,
	role            TEXT    NOT NULL,
	timestamp       TEXT    NOT NULL,
	hash_after      TEXT    NOT NULL,
	assistant_upto  INTEGER NOT NULL,
	user_upto       INTEGER NOT NULL,
	max_ts          TEXT    NOT NULL,
	PRIMARY KEY (conversation_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_turns_conv_seq ON turns(conversation_id, seq);
";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "get_conversation"
            | "get_folder_conversation"
            | "get_folder_conversation_turns"
            | "delete_conversation"
            | "token_usage_report"
            | "token_usage_facets"
            | "token_usage_status"
            | "token_usage_sync"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Outcome> {
    if !owns(command) {
        return None;
    }
    let mut events = Vec::new();
    let result = run(root, command, args, &mut events);
    Some(Outcome {
        result,
        events,
        forward: Vec::new(),
    })
}

// ── database (one connection, like Go's SetMaxOpenConns(1)) ─────────────────

fn db_path(root: &Path) -> std::path::PathBuf {
    root.join(".data").join("codeg.db")
}

fn open_db(root: &Path) -> Result<Connection, String> {
    let path = db_path(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let connection = Connection::open(&path).map_err(|e| e.to_string())?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| format!("初始化会话数据库失败：{e}"))?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| format!("初始化会话数据库失败：{e}"))?;
    connection
        .busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(|e| format!("初始化会话数据库失败：{e}"))?;
    connection
        .execute_batch(TURN_SCHEMA)
        .map_err(|e| format!("创建 turns 表失败：{e}"))?;
    Ok(connection)
}

/// 连接按库文件路径缓存：单进程单根时就是一条连接（Go 的
/// SetMaxOpenConns(1)），测试/多根之间互不串库。
static DBS: Mutex<Option<std::collections::HashMap<std::path::PathBuf, Connection>>> =
    Mutex::new(None);

fn with_db<T>(root: &Path, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String> {
    let key = db_path(root);
    let mut guard = DBS.lock().unwrap();
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    if !map.contains_key(&key) {
        let connection = open_db(root)?;
        // Legacy documents still carried a "turns" map (pre-SQLite); move it
        // into the database once, like Go's importLegacyTurnsLocked.
        migrate_legacy_turns(root, &connection)?;
        map.insert(key.clone(), connection);
    }
    f(map.get(&key).unwrap())
}

/// turnstore.go 的 importLegacyTurnsLocked：把旧文档里的 turns 迁进 SQLite，
/// 备份原文件后重写文档（幂等：库里已有行的会话跳过）。
fn migrate_legacy_turns(root: &Path, connection: &Connection) -> Result<(), String> {
    let path = root.join(".data").join("codeg-workspace.json");
    let Some(doc) = store_file::read_json(&path)? else {
        return Ok(());
    };
    let Some(turns) = doc.get("turns").and_then(Value::as_object).cloned() else {
        return Ok(());
    };
    if turns.is_empty() {
        return Ok(());
    }
    if let Ok(bytes) = std::fs::read(&path) {
        let stamp = goformat::timestamp().replace([':', '.'], "-");
        let backup = format!("{}.bak-{stamp}", path.display());
        let _ = std::fs::write(backup, bytes);
    }
    for (key, list) in &turns {
        let Ok(conversation) = key.parse::<i64>() else {
            continue;
        };
        if count(connection, conversation)? > 0 {
            continue;
        }
        let rows = list.as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            continue;
        }
        replace_turns(connection, conversation, &rows)?;
    }
    let mut doc = doc;
    if let Some(object) = doc.as_object_mut() {
        object.remove("turns");
    }
    let bytes = goformat::marshal_store(&doc);
    let _guard = store_file::lock(&path)?;
    store_file::write_bytes(&path, &bytes)?;
    Ok(())
}

// ── value helpers (Go text/number on any) ───────────────────────────────────

fn value_text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or("").to_string()
}

fn number_of(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

fn parse_rfc3339_millis(ts: &str) -> Option<i64> {
    time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
}

fn format_rfc3339_millis(millis: i64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp_nanos(millis as i128 * 1_000_000)
        .ok()
        .map(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        })
}

// ── cumulative prefix state ─────────────────────────────────────────────────

#[derive(Clone, Default)]
struct TurnState {
    hash: String,
    assistant: i64,
    user: i64,
    max_ts: String,
}

/// turnstore.go `fnvTurn`: role tag byte, then epoch-millis as 8 LE bytes.
fn fnv_turn(mut hash: u64, role: &str, ts: &str) -> u64 {
    let tag: u8 = match role {
        "user" => 0,
        "assistant" => 1,
        "system" => 2,
        _ => 3,
    };
    hash ^= u64::from(tag);
    hash = hash.wrapping_mul(FNV_PRIME);
    let millis = parse_rfc3339_millis(ts).unwrap_or(0);
    for byte in (millis as u64).to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn later_timestamp(a: &str, b: &str) -> String {
    if a.is_empty() {
        return b.to_string();
    }
    if b.is_empty() {
        return a.to_string();
    }
    let (Some(ta), Some(tb)) = (parse_rfc3339_millis(a), parse_rfc3339_millis(b)) else {
        return b.to_string();
    };
    if tb > ta {
        b.to_string()
    } else {
        a.to_string()
    }
}

/// turnstore.go `nextTurnState`: extend the cumulative state with one turn and
/// render its payload exactly as Go's `json.Marshal(turn)` does (map key sort,
/// HTML escapes) — the data column is part of the on-disk contract.
fn next_turn_state(prev: Option<&TurnState>, turn: &Value) -> Result<(TurnState, String), String> {
    let role = value_text(turn.get("role"));
    // Legacy turns may carry no timestamp; keep it empty (the fingerprint
    // treats that as epoch 0) — inventing "now" would rewrite prefix hashes.
    let ts = value_text(turn.get("timestamp"));
    let mut hash = FNV_SEED;
    let mut assistant = 0i64;
    let mut user = 0i64;
    let mut max_ts = String::new();
    if let Some(prev) = prev {
        hash = u64::from_str_radix(&prev.hash, 16).map_err(|e| format!("损坏的历史指纹：{e}"))?;
        assistant = prev.assistant;
        user = prev.user;
        max_ts = prev.max_ts.clone();
    }
    hash = fnv_turn(hash, &role, &ts);
    if role == "assistant" {
        assistant += 1;
    }
    if role == "user" {
        user += 1;
    }
    max_ts = later_timestamp(&max_ts, &ts);
    let data = goformat::marshal_preserve(&goformat::canon(turn));
    Ok((
        TurnState {
            hash: format!("{hash:016x}"),
            assistant,
            user,
            max_ts,
        },
        String::from_utf8(data).map_err(|e| e.to_string())?,
    ))
}

// ── primitives ──────────────────────────────────────────────────────────────

fn count(connection: &Connection, conversation: i64) -> Result<i64, String> {
    connection
        .query_row(
            "SELECT COALESCE(MAX(seq) + 1, 0) FROM turns WHERE conversation_id = ?",
            [conversation],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())
}

fn latest(connection: &Connection, conversation: i64) -> Result<Option<TurnState>, String> {
    connection
        .query_row(
            "SELECT hash_after, assistant_upto, user_upto, max_ts
             FROM turns WHERE conversation_id = ? ORDER BY seq DESC LIMIT 1",
            [conversation],
            |row| {
                Ok(TurnState {
                    hash: row.get(0)?,
                    assistant: row.get(1)?,
                    user: row.get(2)?,
                    max_ts: row.get(3)?,
                })
            },
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other.to_string()),
        })
}

fn state_at(connection: &Connection, conversation: i64, seq: i64) -> Result<Option<TurnState>, String> {
    connection
        .query_row(
            "SELECT hash_after, assistant_upto, user_upto, max_ts
             FROM turns WHERE conversation_id = ? AND seq = ?",
            rusqlite::params![conversation, seq],
            |row| {
                Ok(TurnState {
                    hash: row.get(0)?,
                    assistant: row.get(1)?,
                    user: row.get(2)?,
                    max_ts: row.get(3)?,
                })
            },
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other.to_string()),
        })
}

fn insert_turn(
    connection: &Connection,
    conversation: i64,
    seq: i64,
    data: &str,
    role: &str,
    state: &TurnState,
    ts: &str,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO turns (conversation_id, seq, data, role, timestamp, hash_after, assistant_upto, user_upto, max_ts)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![conversation, seq, data, role, ts, state.hash, state.assistant, state.user, state.max_ts],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn append(connection: &Connection, conversation: i64, turn: &Value) -> Result<(), String> {
    let seq = count(connection, conversation)?;
    let prev = latest(connection, conversation)?;
    let (state, data) = next_turn_state(prev.as_ref(), turn)?;
    let ts = value_text(turn.get("timestamp"));
    let role = value_text(turn.get("role"));
    insert_turn(connection, conversation, seq, &data, &role, &state, &ts)
}

fn replace_turns(connection: &Connection, conversation: i64, turns: &[Value]) -> Result<(), String> {
    connection.execute("BEGIN", []).map_err(|e| e.to_string());
    if let Err(err) = connection.execute(
        "DELETE FROM turns WHERE conversation_id = ?",
        [conversation],
    ) {
        let _ = connection.execute("ROLLBACK", []);
        return Err(err.to_string());
    }
    let mut state: Option<TurnState> = None;
    for (index, turn) in turns.iter().enumerate() {
        let (next, data) = match next_turn_state(state.as_ref(), turn) {
            Ok(result) => result,
            Err(err) => {
                let _ = connection.execute("ROLLBACK", []);
                return Err(err);
            }
        };
        let ts = value_text(turn.get("timestamp"));
        let role = value_text(turn.get("role"));
        if let Err(err) = insert_turn(connection, conversation, index as i64, &data, &role, &next, &ts) {
            let _ = connection.execute("ROLLBACK", []);
            return Err(err);
        }
        state = Some(next);
    }
    connection.execute("COMMIT", []).map(|_| ()).map_err(|e| e.to_string())
}

fn range_rows(connection: &Connection, conversation: i64, start: i64, end: i64) -> Result<Vec<Value>, String> {
    let mut statement = connection
        .prepare("SELECT data FROM turns WHERE conversation_id = ? AND seq >= ? AND seq < ? ORDER BY seq")
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(rusqlite::params![conversation, start, end], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for data in rows {
        let data = data.map_err(|e| e.to_string())?;
        out.push(serde_json::from_str(&data).map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn all_turns(connection: &Connection) -> Result<BTreeMap<i64, Vec<Value>>, String> {
    let mut statement = connection
        .prepare("SELECT conversation_id, data FROM turns ORDER BY conversation_id, seq")
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut out: BTreeMap<i64, Vec<Value>> = BTreeMap::new();
    for row in rows {
        let (conversation, data) = row.map_err(|e| e.to_string())?;
        let turn: Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
        out.entry(conversation).or_default().push(turn);
    }
    Ok(out)
}

// ── workspace document access ───────────────────────────────────────────────

fn load_workspace(root: &Path) -> Value {
    store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
        .ok()
        .flatten()
        .unwrap_or(json!({}))
}

fn conversation_row<'a>(doc: &'a Value, conversation: i64) -> Option<&'a Value> {
    doc.get("conversations")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| number_of(row.get("id")) == conversation)
        })
}

/// history.go `turnsWithDelegationsLocked`: turns with tool_output_text
/// normalisation and delegation metadata merged from persisted child rows.
fn turns_with_delegations(doc: &Value, conversation: i64, turns: &mut [Value]) {
    for turn in turns.iter_mut() {
        if let Some(blocks) = turn.get_mut("blocks").and_then(Value::as_array_mut) {
            for block in blocks.iter_mut() {
                if value_text(block.get("type")) == "tool_result" {
                    block["output_preview"] =
                        json!(tool_output_text(&value_text(block.get("output_preview"))));
                }
            }
        }
    }
    let children: Vec<&Value> = doc
        .get("conversations")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter(|child| number_of(child.get("parent_id")) == conversation)
                .collect()
        })
        .unwrap_or_default();
    for child in children {
        let tool_id = value_text(child.get("parent_tool_use_id"));
        if tool_id.is_empty() {
            continue;
        }
        let status = match value_text(child.get("status")).as_str() {
            "pending_review" | "completed" => "completed",
            "cancelled" => "failed",
            _ => "running",
        };
        for turn in turns.iter_mut() {
            let Some(blocks) = turn.get_mut("blocks").and_then(Value::as_array_mut) else {
                continue;
            };
            for block in blocks.iter_mut() {
                if value_text(block.get("type")) != "tool_use"
                    || value_text(block.get("tool_use_id")) != tool_id
                {
                    continue;
                }
                let meta = block
                    .as_object_mut()
                    .unwrap()
                    .entry("meta")
                    .or_insert_with(|| json!({}));
                if !meta.is_object() {
                    *meta = json!({});
                }
                meta.as_object_mut().unwrap().insert(
                    "codeg.delegation".into(),
                    json!({
                        "status": status,
                        "child_conversation_id": child.get("id").cloned().unwrap_or(Value::Null),
                        "agent_type": child.get("agent_type").cloned().unwrap_or(Value::Null),
                        "task_id": child.get("delegation_call_id").cloned().unwrap_or(Value::Null),
                        "task_preview": child.get("title").cloned().unwrap_or(Value::Null),
                    }),
                );
            }
        }
    }
}

/// history.go `toolOutputText`: ACP adapters can wrap MCP text in content
/// arrays; history stores the text shown by the tool.
fn tool_output_text(raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };
    fn extract(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Array(list) => {
                for item in list {
                    extract(item, out);
                }
            }
            Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(Value::as_str) {
                    out.push(text.to_string());
                } else if let Some(nested) = map.get("content") {
                    extract(nested, out);
                }
            }
            _ => {}
        }
    }
    let mut texts = Vec::new();
    extract(&value, &mut texts);
    if texts.is_empty() {
        raw.to_string()
    } else {
        texts.join("\n")
    }
}

// ── commands ────────────────────────────────────────────────────────────────

fn run(root: &Path, command: &str, args: &Value, events: &mut Vec<(String, Value)>) -> Result<Value, String> {
    match command {
        "get_conversation" | "get_folder_conversation" => {
            let conversation = number_of(args.get("conversationId"));
            let doc = load_workspace(root);
            let Some(row) = conversation_row(&doc, conversation) else {
                return Err("会话不存在".into());
            };
            let mut turns = with_db(root, |connection| {
                range_rows(connection, conversation, 0, MAX_TURN_SEQ)
            })?;
            turns_with_delegations(&doc, conversation, &mut turns);
            Ok(json!({
                "summary": row,
                "turns": turns,
                "turns_offset": 0,
                "turns_total": turns.len(),
            }))
        }
        "get_folder_conversation_turns" => {
            let conversation = number_of(args.get("conversationId"));
            let doc = load_workspace(root);
            if conversation_row(&doc, conversation).is_none() {
                return Err("会话不存在".into());
            }
            let before_index = number_of(args.get("beforeIndex"));
            let mut limit = number_of(args.get("limit"));
            // Go clamps: negative limit panicked the slice once.
            if !(1..=500).contains(&limit) {
                limit = limit.clamp(1, 500);
                if limit < 1 {
                    limit = 50;
                }
                if limit > 500 {
                    limit = 500;
                }
            }
            let total = with_db(root, |connection| count(connection, conversation))?;
            let end = before_index.clamp(0, total);
            let mut start = end - limit;
            if start < 0 {
                start = 0;
            }
            if start > end {
                start = end;
            }
            let page = with_db(root, |connection| range_rows(connection, conversation, start, end))?;
            let boundary = before_index.clamp(0, total);
            let mut assistant_before = 0i64;
            let mut user_before = 0i64;
            let mut prefix_hash = format!("{FNV_SEED:016x}");
            let mut uncovered_max_ts: Value = Value::Null;
            if start > 0 {
                if let Some(state) = with_db(root, |connection| state_at(connection, conversation, start - 1))? {
                    assistant_before = state.assistant;
                    user_before = state.user;
                    prefix_hash = state.hash.clone();
                    if !state.max_ts.is_empty() {
                        uncovered_max_ts = match parse_rfc3339_millis(&state.max_ts)
                            .and_then(format_rfc3339_millis)
                        {
                            Some(normalised) => json!(normalised),
                            None => json!(state.max_ts),
                        };
                    }
                }
            }
            let mut boundary_hash = format!("{FNV_SEED:016x}");
            if boundary > 0 {
                if let Some(state) = with_db(root, |connection| state_at(connection, conversation, boundary - 1))? {
                    boundary_hash = state.hash;
                }
            }
            Ok(json!({
                "turns": page,
                "turns_offset": start,
                "turns_total": total,
                "assistant_turns_before_offset": assistant_before,
                "user_turns_before_offset": user_before,
                "prefix_hash": prefix_hash,
                "prefix_hash_before_index": boundary_hash,
                "uncovered_prefix_max_ts": uncovered_max_ts,
            }))
        }
        "delete_conversation" => {
            let conversation = number_of(args.get("conversationId"));
            if conversation <= 0 {
                return Err("会话 ID 无效".into());
            }
            let mut doc = match store_file::read_json(&root.join(".data").join("codeg-workspace.json"))? {
                Some(doc) if doc.is_object() => doc,
                Some(_) => return Err("会话 ID 无效".into()),
                None => return Err("会话 ID 无效".into()),
            };
            if conversation_row(&doc, conversation).is_none() {
                // Go: unknown conversation is a silent no-op.
                return Ok(Value::Null);
            }
            // Cascade: every descendant of the deleted conversation.
            let mut removed: BTreeSet<i64> = BTreeSet::from([conversation]);
            loop {
                let mut changed = false;
                for row in doc
                    .get("conversations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let id = number_of(row.get("id"));
                    let parent = number_of(row.get("parent_id"));
                    if !removed.contains(&id) && removed.contains(&parent) {
                        removed.insert(id);
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            // Turn rows go first (Go deletes them before the document save and
            // restores on failure; with a single atomic doc write the turn
            // delete happens after the doc commit succeeds).
            let parents: BTreeSet<i64> = doc
                .get("conversations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|row| removed.contains(&number_of(row.get("id"))))
                .map(|row| number_of(row.get("parent_id")))
                .collect();
            // Remove conversations + their tabs; bump tabsVersion when tabs go.
            let conversations: Vec<Value> = doc
                .get("conversations")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter(|row| !removed.contains(&number_of(row.get("id"))))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            doc["conversations"] = json!(conversations);
            let tabs_before = doc
                .get("tabs")
                .and_then(Value::as_array)
                .map(|tabs| tabs.len())
                .unwrap_or(0);
            let tabs: Vec<Value> = doc
                .get("tabs")
                .and_then(Value::as_array)
                .map(|tabs| {
                    tabs.iter()
                        .filter(|tab| {
                            !removed.contains(&number_of(tab.get("conversation_id")))
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if tabs.len() != tabs_before {
                doc["tabs"] = json!(tabs);
                doc["tabsVersion"] = json!(number_of(doc.get("tabsVersion")) + 1);
            }
            // child_count refresh on the surviving parents.
            let updates: Vec<Value> = doc
                .get("conversations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|row| parents.contains(&number_of(row.get("id"))))
                .map(|row| {
                    let parent = number_of(row.get("id"));
                    let count = doc
                        .get("conversations")
                        .and_then(Value::as_array)
                        .map(|rows| {
                            rows.iter()
                                .filter(|child| number_of(child.get("parent_id")) == parent)
                                .count()
                        })
                        .unwrap_or(0);
                    let mut updated = row.clone();
                    updated["child_count"] = json!(count);
                    updated
                })
                .collect();
            for updated in &updates {
                let id = number_of(updated.get("id"));
                for row in doc
                    .get_mut("conversations")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                {
                    if number_of(row.get("id")) == id {
                        *row = updated.clone();
                    }
                }
            }
            let bytes = goformat::marshal_store(&doc);
            {
                let path = root.join(".data").join("codeg-workspace.json");
                let _guard = store_file::lock(&path)?;
                store_file::write_bytes(&path, &bytes)?;
            }
            for id in &removed {
                with_db(root, |connection| {
                    connection
                        .execute("DELETE FROM turns WHERE conversation_id = ?", [*id])
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })?;
            }
            for id in &removed {
                events.push((
                    "conversation://changed".into(),
                    json!({"kind": "deleted", "id": id}),
                ));
            }
            for updated in &updates {
                events.push((
                    "conversation://changed".into(),
                    json!({"kind": "upsert", "summary": updated}),
                ));
            }
            Ok(Value::Null)
        }
        "token_usage_report" => token_usage_report(root, args),
        "token_usage_facets" => token_usage_facets(root),
        "token_usage_status" => token_usage_status(root),
        "token_usage_sync" => token_usage_sync(root),
        _ => Err(format!("Go 移植尚未实现此接口：{command}")),
    }
}

// ── token usage (token_usage.go) ────────────────────────────────────────────

const USAGE_SCAN_CAP: usize = 50000;

#[derive(Clone)]
struct UsageFact {
    conv_id: i64,
    folder_id: i64,
    agent_type: String,
    model: String,
    at_millis: i64,
    input: f64,
    output: f64,
    cache_creation: f64,
    cache_read: f64,
    duration_ms: f64,
}

impl UsageFact {
    fn total(&self) -> f64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }
}

fn usage_f64(value: Option<&Value>) -> f64 {
    value.and_then(Value::as_f64).unwrap_or(0.0)
}

/// token_usage.go `collectUsageFacts`: usage-bearing assistant turns, oldest
/// first, capped at the most recent scan-cap rows.
fn collect_usage_facts(root: &Path) -> Result<(Vec<UsageFact>, bool), String> {
    let doc = load_workspace(root);
    let all = with_db(root, |connection| all_turns(connection))?;
    let mut facts = Vec::new();
    for (conv_id, turns) in &all {
        let conversation = if *conv_id > 0 {
            conversation_row(&doc, *conv_id)
        } else {
            None
        };
        let folder_id = conversation
            .map(|row| number_of(row.get("folder_id")))
            .unwrap_or(0);
        let agent_type = conversation
            .map(|row| value_text(row.get("agent_type")))
            .unwrap_or_default();
        for turn in turns {
            if value_text(turn.get("role")) != "assistant" {
                continue;
            }
            let Some(usage) = turn.get("usage").filter(|u| {
                u.as_object().map(|m| !m.is_empty()).unwrap_or(false)
            }) else {
                continue;
            };
            let Some(at) = parse_rfc3339_millis(&value_text(turn.get("timestamp"))) else {
                continue;
            };
            facts.push(UsageFact {
                conv_id: *conv_id,
                folder_id,
                agent_type: agent_type.clone(),
                model: value_text(turn.get("model")),
                at_millis: at,
                input: usage_f64(usage.get("input_tokens")),
                output: usage_f64(usage.get("output_tokens")),
                cache_creation: usage_f64(usage.get("cache_creation_input_tokens")),
                cache_read: usage_f64(usage.get("cache_read_input_tokens")),
                duration_ms: usage_f64(turn.get("duration_ms")),
            });
        }
    }
    facts.sort_by_key(|fact| fact.at_millis);
    let mut truncated = false;
    if facts.len() > USAGE_SCAN_CAP {
        facts = facts.split_off(facts.len() - USAGE_SCAN_CAP);
        truncated = true;
    }
    Ok((facts, truncated))
}

/// token_usage.go `usageZone`/`usageBucketKey`：查看者时区（JS 偏移约定，
/// UTC+8 → 480）下的桶键。返回 (键, 桶起, 桶止) 的毫秒时间。
fn usage_bucket_key(at_millis: i64, tz_minutes: i64, bucket: &str) -> (String, i64, i64) {
    // 固定偏移时区：at + tz 分钟，再按本地日历分桶。
    let local_millis = at_millis + tz_minutes * 60_000;
    let local = time::OffsetDateTime::from_unix_timestamp_nanos(local_millis as i128 * 1_000_000)
        .unwrap();
    let day_start = local
        .replace_time(time::Time::MIDNIGHT)
        .unix_timestamp_nanos() as i64
        / 1_000_000
        - tz_minutes * 60_000;
    match bucket {
        "month" => {
            let start_local = local
                .replace_day(1)
                .ok()
                .map(|d| d.replace_time(time::Time::MIDNIGHT))
                .unwrap_or(local);
            let key = format!("{:04}-{:02}", start_local.year(), u8::from(start_local.month()));
            let start = start_local.unix_timestamp_nanos() as i64 / 1_000_000 - tz_minutes * 60_000;
            // 月长：下个月 1 日的 day_start。
            let (next_year, next_month) = if start_local.month() == time::Month::December {
                (start_local.year() + 1, time::Month::January)
            } else {
                (start_local.year(), start_local.month().next())
            };
            let next_start = start_local
                .replace_year(next_year)
                .ok()
                .and_then(|d| d.replace_month(next_month).ok())
                .map(|d| d.replace_time(time::Time::MIDNIGHT))
                .map(|d| d.unix_timestamp_nanos() as i64 / 1_000_000 - tz_minutes * 60_000)
                .unwrap_or(start);
            (key, start, next_start)
        }
        "week" => {
            // Go: weekdayMonday = (weekday + 6) % 7。
            let weekday = (local.weekday().number_days_from_sunday() + 6) % 7;
            let start = day_start - weekday as i64 * 86_400_000;
            let key = format_day(start + tz_minutes * 60_000);
            (key, start, start + 7 * 86_400_000)
        }
        _ => {
            let key = format_day(day_start + tz_minutes * 60_000);
            (key, day_start, day_start + 86_400_000)
        }
    }
}

/// Go `time.RFC3339` 在固定偏移时区下的输出：`+08:00` / `Z`。
fn format_rfc3339_in_zone(utc_millis: i64, tz_minutes: i64) -> String {
    let local = utc_millis + tz_minutes * 60_000;
    let Some(stamp) = time::OffsetDateTime::from_unix_timestamp_nanos(local as i128 * 1_000_000)
        .ok()
    else {
        return String::new();
    };
    let base = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        stamp.year(),
        u8::from(stamp.month()),
        stamp.day(),
        stamp.hour(),
        stamp.minute(),
        stamp.second()
    );
    if tz_minutes == 0 {
        return format!("{base}Z");
    }
    let sign = if tz_minutes < 0 { '-' } else { '+' };
    let offset = tz_minutes.abs();
    format!("{base}{sign}{:02}:{:02}", offset / 60, offset % 60)
}

fn format_day(utc_millis: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(utc_millis as i128 * 1_000_000)
        .map(|t| format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day()))
        .unwrap_or_default()
}

fn weekday_monday(utc_millis: i64) -> i64 {
    time::OffsetDateTime::from_unix_timestamp_nanos(utc_millis as i128 * 1_000_000)
        .map(|t| (i64::from(t.weekday().number_days_from_sunday()) + 6) % 7)
        .unwrap_or(0)
}

#[derive(Default, Clone)]
struct UsageSum {
    input: f64,
    output: f64,
    cache_creation: f64,
    cache_read: f64,
    duration_ms: f64,
    turns: i64,
    convs: BTreeSet<i64>,
    days: BTreeSet<String>,
}

impl UsageSum {
    fn add(&mut self, fact: &UsageFact, tz_minutes: i64) {
        self.input += fact.input;
        self.output += fact.output;
        self.cache_creation += fact.cache_creation;
        self.cache_read += fact.cache_read;
        self.duration_ms += fact.duration_ms;
        self.turns += 1;
        self.convs.insert(fact.conv_id);
        let local = fact.at_millis + tz_minutes * 60_000;
        self.days.insert(format_day(local));
    }

    fn totals_row(&self) -> Value {
        json!({
            "input_tokens": self.input,
            "output_tokens": self.output,
            "cache_creation_tokens": self.cache_creation,
            "cache_read_tokens": self.cache_read,
            "total_tokens": self.input + self.output + self.cache_creation + self.cache_read,
            "turn_count": self.turns,
            "conversation_count": self.convs.len(),
            "duration_ms": self.duration_ms,
            "active_days": self.days.len(),
        })
    }

    fn total(&self) -> f64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }
}

/// Go `expandFoldersLocked`：选区 + 全部后代文件夹。
fn expand_folders(doc: &Value, ids: &[i64]) -> BTreeSet<i64> {
    let mut children: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for folder in doc.get("folders").and_then(Value::as_array).into_iter().flatten() {
        let parent = number_of(folder.get("parent_id"));
        if parent > 0 {
            children.entry(parent).or_default().push(number_of(folder.get("id")));
        }
    }
    let mut out: BTreeSet<i64> = BTreeSet::new();
    fn walk(id: i64, children: &BTreeMap<i64, Vec<i64>>, out: &mut BTreeSet<i64>) {
        if !out.insert(id) {
            return;
        }
        if let Some(list) = children.get(&id) {
            for child in list.clone() {
                walk(child, children, out);
            }
        }
    }
    for id in ids {
        walk(*id, &children, &mut out);
    }
    out
}

struct UsageWindow {
    facts: Vec<UsageFact>,
    start: Option<i64>,
    end: Option<i64>,
}

fn apply_usage_filter(
    root: &Path,
    facts: &[UsageFact],
    filter: &Value,
    start: Option<i64>,
    end: Option<i64>,
) -> UsageWindow {
    let agents: BTreeSet<String> = filter
        .pointer("/agentTypes")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let models: BTreeSet<String> = filter
        .pointer("/models")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let folder_ids: Vec<i64> = filter
        .pointer("/folderIds")
        .and_then(Value::as_array)
        .map(|list| list.iter().map(|v| number_of(Some(v))).collect())
        .unwrap_or_default();
    let folder_sel: Option<BTreeSet<i64>> = if folder_ids.is_empty() {
        None
    } else {
        Some(expand_folders(&load_workspace(root), &folder_ids))
    };
    let facts: Vec<UsageFact> = facts
        .iter()
        .filter(|fact| {
            if let Some(start) = start {
                if fact.at_millis < start {
                    return false;
                }
            }
            if let Some(end) = end {
                if fact.at_millis >= end {
                    return false;
                }
            }
            if !agents.is_empty() && !agents.contains(&fact.agent_type) {
                return false;
            }
            if !models.is_empty() && !models.contains(&fact.model) {
                return false;
            }
            if let Some(selection) = &folder_sel {
                if !selection.contains(&fact.folder_id) {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();
    UsageWindow { facts, start, end }
}

fn token_usage_report(root: &Path, args: &Value) -> Result<Value, String> {
    let filter = args.get("filter").cloned().unwrap_or(json!({}));
    let tz_minutes = usage_f64(filter.get("tzOffsetMinutes")) as i64;
    let mut bucket = value_text(filter.get("bucket"));
    if bucket != "day" && bucket != "week" && bucket != "month" {
        bucket = "day".into();
    }
    let start = value_text(filter.get("start"))
        .parse::<i64>()
        .ok()
        .or_else(|| parse_rfc3339_millis(&value_text(filter.get("start"))));
    let end = value_text(filter.get("end"))
        .parse::<i64>()
        .ok()
        .or_else(|| parse_rfc3339_millis(&value_text(filter.get("end"))));

    let (facts, truncated) = collect_usage_facts(root)?;
    let window = apply_usage_filter(root, &facts, &filter, start, end);
    let compare = filter.get("comparePrevious") == Some(&Value::Bool(true))
        && window.start.is_some()
        && window.end.is_some();
    let previous = if compare {
        let span = window.end.unwrap() - window.start.unwrap();
        Some(apply_usage_filter(
            root,
            &facts,
            &filter,
            Some(window.start.unwrap() - span),
            window.start,
        ))
    } else {
        None
    };

    let mut sum = UsageSum::default();
    let mut series: Vec<Value> = Vec::new();
    let mut series_index: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_folder: BTreeMap<String, UsageSum> = BTreeMap::new();
    let mut by_agent: BTreeMap<String, UsageSum> = BTreeMap::new();
    let mut by_model: BTreeMap<String, UsageSum> = BTreeMap::new();
    let mut heat: BTreeMap<(i64, i64), UsageSum> = BTreeMap::new();
    let mut per_conv: BTreeMap<i64, UsageSum> = BTreeMap::new();
    let mut conv_meta: BTreeMap<i64, UsageFact> = BTreeMap::new();
    let mut bucket_convs: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();

    let doc = load_workspace(root);
    for fact in &window.facts {
        sum.add(fact, tz_minutes);
        let (key, bucket_start, bucket_end) = usage_bucket_key(fact.at_millis, tz_minutes, &bucket);
        let index = match series_index.get(&key) {
            Some(index) => *index,
            None => {
                let index = series.len();
                series_index.insert(key.clone(), index);
                series.push(json!({
                    "bucket_key": key,
                    "start": format_rfc3339_in_zone(bucket_start, tz_minutes),
                    "end": format_rfc3339_in_zone(bucket_end, tz_minutes),
                    "input_tokens": 0.0,
                    "output_tokens": 0.0,
                    "cache_creation_tokens": 0.0,
                    "cache_read_tokens": 0.0,
                    "total_tokens": 0.0,
                    "turn_count": 0,
                    "conversation_count": 0,
                }));
                index
            }
        };
        let point = &mut series[index];
        point["input_tokens"] = json!(point["input_tokens"].as_f64().unwrap_or(0.0) + fact.input);
        point["output_tokens"] = json!(point["output_tokens"].as_f64().unwrap_or(0.0) + fact.output);
        point["cache_creation_tokens"] =
            json!(point["cache_creation_tokens"].as_f64().unwrap_or(0.0) + fact.cache_creation);
        point["cache_read_tokens"] =
            json!(point["cache_read_tokens"].as_f64().unwrap_or(0.0) + fact.cache_read);
        point["total_tokens"] = json!(point["total_tokens"].as_f64().unwrap_or(0.0) + fact.total());
        point["turn_count"] = json!(point["turn_count"].as_i64().unwrap_or(0) + 1);

        conv_meta.insert(fact.conv_id, fact.clone());
        per_conv.entry(fact.conv_id).or_default().add(fact, tz_minutes);
        by_folder.entry(fact.folder_id.to_string()).or_default().add(fact, tz_minutes);
        by_agent.entry(fact.agent_type.clone()).or_default().add(fact, tz_minutes);
        if !fact.model.is_empty() {
            by_model.entry(fact.model.clone()).or_default().add(fact, tz_minutes);
        }
        let local = fact.at_millis + tz_minutes * 60_000;
        let cell = (weekday_monday(local), local / 3_600_000 % 24);
        heat.entry(cell).or_default().add(fact, tz_minutes);
        bucket_convs.entry(key).or_default().insert(fact.conv_id);
    }
    for (key, convs) in &bucket_convs {
        if let Some(index) = series_index.get(key) {
            series[*index]["conversation_count"] = json!(convs.len());
        }
    }

    let folder_label = |doc: &Value, id: i64| -> Value {
        let folder = doc
            .get("folders")
            .and_then(Value::as_array)
            .and_then(|rows| rows.iter().find(|row| number_of(row.get("id")) == id));
        match folder {
            None => Value::Null,
            Some(folder) => {
                let alias = value_text(folder.get("alias"));
                if alias.is_empty() {
                    json!(value_text(folder.get("name")))
                } else {
                    json!(alias)
                }
            }
        }
    };
    let atoi_safe = |key: &str| key.parse::<i64>().unwrap_or(0);

    let breakdown = |entries: BTreeMap<String, UsageSum>, label: Box<dyn Fn(&str) -> Value>| -> Vec<Value> {
        let mut out: Vec<Value> = entries
            .iter()
            .map(|(key, sum)| {
                json!({
                    "key": key,
                    "label": label(key),
                    "input_tokens": sum.input,
                    "output_tokens": sum.output,
                    "cache_creation_tokens": sum.cache_creation,
                    "cache_read_tokens": sum.cache_read,
                    "total_tokens": sum.total(),
                    "turn_count": sum.turns,
                    "conversation_count": sum.convs.len(),
                })
            })
            .collect();
        out.sort_by(|a, b| {
            b["total_tokens"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&a["total_tokens"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    };

    let mut heatmap = Vec::new();
    for weekday in 0..7 {
        for hour in 0..24 {
            let cell = match heat.get(&(weekday, hour)) {
                Some(sum) => json!({
                    "weekday": weekday, "hour": hour,
                    "total_tokens": sum.total(), "turn_count": sum.turns,
                }),
                None => json!({"weekday": weekday, "hour": hour, "total_tokens": 0.0, "turn_count": 0}),
            };
            heatmap.push(cell);
        }
    }

    let mut top: Vec<Value> = Vec::new();
    let mut conv_ids: Vec<i64> = per_conv.keys().cloned().collect();
    conv_ids.sort_by(|a, b| {
        let total = |id: i64| per_conv.get(&id).map(|sum| sum.total()).unwrap_or(0.0);
        total(*b).partial_cmp(&total(*a)).unwrap_or(std::cmp::Ordering::Equal)
    });
    for id in conv_ids.into_iter().take(5) {
        let sum = per_conv.get(&id).unwrap();
        let last = conv_meta
            .get(&id)
            .and_then(|fact| format_rfc3339_millis(fact.at_millis))
            .unwrap_or_default();
        let conversation = conversation_row(&doc, id);
        let title = conversation
            .and_then(|row| row.get("title"))
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null);
        let agent = conversation
            .map(|row| value_text(row.get("agent_type")))
            .unwrap_or_default();
        let folder_label_value = conv_meta
            .get(&id)
            .map(|fact| folder_label(&doc, fact.folder_id))
            .unwrap_or(Value::Null);
        top.push(json!({
            "conversation_id": id,
            "title": title,
            "agent_type": agent,
            "folder_label": folder_label_value,
            "total_tokens": sum.total(),
            "turn_count": sum.turns,
            "last_activity_at": last,
        }));
    }

    let streak = usage_streak_row(&window.facts, tz_minutes);
    let first = window
        .facts
        .first()
        .and_then(|fact| format_rfc3339_millis(fact.at_millis))
        .unwrap_or_default();
    let last = window
        .facts
        .last()
        .and_then(|fact| format_rfc3339_millis(fact.at_millis))
        .unwrap_or_default();
    let range_start = window
        .start
        .and_then(format_rfc3339_millis)
        .map(Value::String)
        .unwrap_or(Value::Null);
    let range_end = window
        .end
        .and_then(format_rfc3339_millis)
        .map(Value::String)
        .unwrap_or(Value::Null);
    let previous_totals = previous.map(|window| {
        let mut prev_sum = UsageSum::default();
        for fact in &window.facts {
            prev_sum.add(fact, tz_minutes);
        }
        prev_sum.totals_row()
    });

    Ok(json!({
        "range_start": range_start,
        "range_end": range_end,
        "bucket": bucket,
        "totals": sum.totals_row(),
        "previous_totals": previous_totals,
        "series": series,
        "by_folder": breakdown(by_folder, Box::new(|key: &str| {
            let id = atoi_safe(key);
            match folder_label(&doc, id) {
                Value::Null => json!(key),
                label => label,
            }
        })),
        "by_agent": breakdown(by_agent, Box::new(|key: &str| json!(key))),
        "by_model": breakdown(by_model, Box::new(|key: &str| json!(key))),
        "heatmap": heatmap,
        "top_conversations": top,
        "streak": streak,
        "first_activity_at": if first.is_empty() { Value::Null } else { json!(first) },
        "last_activity_at": if last.is_empty() { Value::Null } else { json!(last) },
        "truncated": truncated,
    }))
}

/// token_usage.go `usageStreakRow`：最长连续活跃天数 + 从最近活跃日回溯的连击。
fn usage_streak_row(facts: &[UsageFact], tz_minutes: i64) -> Value {
    let mut days: BTreeSet<String> = BTreeSet::new();
    for fact in facts {
        days.insert(format_day(fact.at_millis + tz_minutes * 60_000));
    }
    let keys: Vec<String> = days.iter().cloned().collect();
    let next_day = |day: &str| {
        time::OffsetDateTime::parse(&format!("{day}T00:00:00Z"), &time::format_description::well_known::Rfc3339)
            .ok()
            .map(|t| {
                format!(
                    "{:04}-{:02}-{:02}",
                    (t + time::Duration::days(1)).year(),
                    u8::from((t + time::Duration::days(1)).month()),
                    (t + time::Duration::days(1)).day()
                )
            })
            .unwrap_or_default()
    };
    let mut longest = 0;
    let mut run_start = 0;
    for (index, day) in keys.iter().enumerate() {
        if index == 0 || next_day(&keys[index - 1]) != *day {
            run_start = index;
        }
        longest = longest.max(index - run_start + 1);
    }
    let mut current = 0;
    if !keys.is_empty() {
        current = 1;
        for index in (1..keys.len()).rev() {
            if next_day(&keys[index - 1]) == keys[index] {
                current += 1;
            } else {
                break;
            }
        }
    }
    let ends_on = keys.last().cloned().map(Value::String).unwrap_or(Value::Null);
    json!({"longest_days": longest, "current_days": current, "current_ends_on": ends_on})
}

fn token_usage_facets(root: &Path) -> Result<Value, String> {
    let (facts, _) = collect_usage_facts(root)?;
    let mut agents: BTreeSet<String> = BTreeSet::new();
    let mut models: BTreeSet<String> = BTreeSet::new();
    for fact in &facts {
        if !fact.agent_type.is_empty() {
            agents.insert(fact.agent_type.clone());
        }
        if !fact.model.is_empty() {
            models.insert(fact.model.clone());
        }
    }
    let doc = load_workspace(root);
    let folders: Vec<Value> = doc
        .get("folders")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let alias = value_text(row.get("alias"));
                    let name = value_text(row.get("name"));
                    let label = if alias.is_empty() { name.clone() } else { alias.clone() };
                    let parent = number_of(row.get("parent_id"));
                    json!({
                        "folder_id": number_of(row.get("id")),
                        "label": label,
                        "name": name,
                        "alias": if alias.is_empty() { Value::Null } else { json!(alias) },
                        "path": value_text(row.get("path")),
                        "parent_id": if parent > 0 { json!(parent) } else { Value::Null },
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let data_start = facts
        .first()
        .and_then(|fact| format_rfc3339_millis(fact.at_millis))
        .map(Value::String)
        .unwrap_or(Value::Null);
    let data_end = facts
        .last()
        .and_then(|fact| format_rfc3339_millis(fact.at_millis))
        .map(Value::String)
        .unwrap_or(Value::Null);
    Ok(json!({
        "folders": folders,
        "agents": agents.into_iter().collect::<Vec<_>>(),
        "models": models.into_iter().collect::<Vec<_>>(),
        "data_start": data_start,
        "data_end": data_end,
    }))
}

fn token_usage_status(root: &Path) -> Result<Value, String> {
    let (facts, _) = collect_usage_facts(root)?;
    let convs: BTreeSet<i64> = facts.iter().map(|fact| fact.conv_id).collect();
    let total = load_workspace(root)
        .get("conversations")
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    let last_sync = facts
        .last()
        .and_then(|fact| format_rfc3339_millis(fact.at_millis))
        .map(Value::String)
        .unwrap_or(Value::Null);
    Ok(json!({
        "total_conversations": total,
        "synced_conversations": convs.len(),
        "stale_conversations": 0,
        "fact_rows": facts.len(),
        "last_synced_at": last_sync,
        "running": false,
    }))
}

fn token_usage_sync(root: &Path) -> Result<Value, String> {
    let (facts, _) = collect_usage_facts(root)?;
    let total: i64 = with_db(root, |connection| {
        connection
            .query_row(
                "SELECT COUNT(*) FROM turns WHERE role = 'assistant'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| e.to_string())
    })?;
    let tokens: f64 = facts.iter().map(|fact| fact.total()).sum();
    Ok(json!({
        "scanned": total,
        "synced": facts.len(),
        "skipped": total - facts.len() as i64,
        "failed": 0,
        "lost": 0,
        "turns_written": 0,
        "tokens_written": tokens,
        "pruned_conversations": 0,
    }))
}

/// store_conv 的会话写（prompt/completion）所用的 SQLite 追加入口：维护
/// message_count/updated_at（Go appendTurnLocked）。由 acp::connections 调用。
pub fn append_turn(
    root: &Path,
    conversation: i64,
    role: &str,
    body: &str,
    blocks: Option<&Value>,
) -> Result<Value, String> {
    let blocks = match blocks {
        Some(blocks) if blocks.as_array().map(|a| !a.is_empty()).unwrap_or(false) => {
            blocks.clone()
        }
        _ => json!([{ "type": "text", "text": body }]),
    };
    let turn = json!({
        "id": goformat::hex_id(),
        "role": role,
        "blocks": blocks,
        "timestamp": goformat::timestamp(),
    });
    with_db(root, |connection| append(connection, conversation, &turn))?;
    Ok(turn)
}

/// 会话状态（引擎轮询用：pending_review / cancelled / failed）。
pub fn conversation_status(root: &Path, conversation: i64) -> String {
    load_workspace(root)
        .get("conversations")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| number_of(row.get("id")) == conversation)
                .and_then(|row| row.get("status"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

pub fn turn_count(root: &Path, conversation: i64) -> i64 {
    with_db(root, |connection| count(connection, conversation)).unwrap_or(0)
}

/// Append one fully-formed turn row (the ACP writers build the whole payload:
/// blocks, usage, model, duration). Returns the conversation's turn count.
pub fn append_raw(root: &Path, conversation: i64, turn: &Value) -> Result<i64, String> {
    with_db(root, |connection| append(connection, conversation, turn))?;
    Ok(turn_count(root, conversation))
}

/// All turns of one conversation in seq order.
pub fn all_turns_for(root: &Path, conversation: i64) -> Result<Vec<Value>, String> {
    with_db(root, |connection| {
        range_rows(connection, conversation, 0, MAX_TURN_SEQ)
    })
}

/// Overwrite a conversation's whole turn list (fork: sibling copy + truncation).
pub fn replace_all(root: &Path, conversation: i64, turns: &[Value]) -> Result<(), String> {
    with_db(root, |connection| {
        replace_turns(connection, conversation, turns)
    })
}

pub fn delete_turns(root: &Path, conversation: i64) -> Result<(), String> {
    with_db(root, |connection| {
        connection
            .execute("DELETE FROM turns WHERE conversation_id = ?", [conversation])
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}
