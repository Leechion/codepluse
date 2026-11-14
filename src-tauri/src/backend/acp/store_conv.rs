//! Conversation/turn store primitives the ACP flow writes through, ported
//! from Go's `codeg/store.go` (folder/conversation/turn helpers) and the
//! store sections of `connections.go` (prompt + completion) and `fork.go`
//! (the fork transaction).
//!
//! Contract (batch-5 convention): every entry point takes the cross-process
//! file lock, re-reads `codeg-workspace.json`, mutates an in-memory copy and
//! persists once via `goformat::marshal_store` — Go's
//! `Store.reloadIfChangedLocked` picks the change up on its side. Errors leave
//! the file untouched, which matches Go's in-memory rollback.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::backend::goformat;
use crate::backend::store_file;

const FILE: &str = "codeg-workspace.json";

/// 工作台文档的落盘位置。这里收到的 `root` 是工作区根目录（与
/// `store_reads` / `store_writes` / `turn_store` 一致），文档在 `.data/` 下。
/// 早期 ACP 侧把文件拼在根目录，结果 `create_conversation` 写进 `.data/` 而
/// `prompt_side_write` 读的是根目录下那份空文档，新建会话第一条消息必然
/// 报「会话不存在」。两条路径必须由同一个 helper 给出，避免再次分叉。
fn store_path(root: &Path) -> std::path::PathBuf {
    root.join(".data").join(FILE)
}

/// `(conversation_id, folder_id)` plus the WS frames the write produced.
pub struct StoreOutcome {
    pub conversation_id: i64,
    pub folder_id: i64,
    pub events: Vec<(String, Value)>,
}

fn locked_doc(root: &Path) -> Result<(store_file::WriteLock, Map<String, Value>), String> {
    let path = store_path(root);
    let guard = store_file::lock(&path)?;
    let doc = store_file::read_json(&path)?
        .unwrap_or(json!({"next": 1}))
        .as_object()
        .cloned()
        .ok_or_else(|| "工作区存储格式无效".to_string())?;
    Ok((guard, doc))
}

fn persist(root: &Path, doc: &Map<String, Value>) -> Result<(), String> {
    // turns 已迁至 SQLite（turnstore.go）：文档绝不回写这份旧 map，否则会把
    // 迁移去掉的历史重新塞回去。
    let mut doc = doc.clone();
    doc.remove("turns");
    let body = goformat::marshal_store(&Value::Object(doc));
    store_file::write_bytes(&store_path(root), &body)
}

fn allocate(doc: &mut Map<String, Value>) -> i64 {
    let n = doc.get("next").and_then(Value::as_i64).unwrap_or(1);
    doc.insert("next".into(), json!(n + 1));
    n
}

fn rows_mut<'a>(doc: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    doc.entry(key.to_string())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("row array")
}

fn turns_of_mut<'a>(doc: &'a mut Map<String, Value>, conversation_id: i64) -> &'a mut Vec<Value> {
    let turns = doc.entry("turns".to_string()).or_insert_with(|| json!({}));
    let map = turns.as_object_mut().expect("turns object");
    map.entry(conversation_id.to_string())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("turn list")
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

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or("").to_string()
}

fn folder_kind(doc: &Map<String, Value>, folder_id: i64) -> String {
    for row in doc
        .get("folders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if number(row.get("id")) == folder_id {
            return text(row.get("kind"));
        }
    }
    String::new()
}

/// Go `openFolderLocked` (store.go:167).
pub fn open_folder_locked(doc: &mut Map<String, Value>, path: &str, kind: &str) -> i64 {
    for row in rows_mut(doc, "folders").iter_mut() {
        if text(row.get("path")) == path {
            let now = goformat::timestamp();
            if let Some(obj) = row.as_object_mut() {
                obj.insert("last_opened_at".into(), json!(now));
                obj.insert("open".into(), json!(true));
            }
            return number(row.get("id"));
        }
    }
    let id = allocate(doc);
    let now = goformat::timestamp();
    let sort_order = doc
        .get("folders")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let row = json!({
        "id": id,
        "name": std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string()),
        "path": path,
        "git_branch": Value::Null,
        "default_agent_type": Value::Null,
        "last_opened_at": now,
        "sort_order": sort_order,
        "color": "default",
        "parent_id": Value::Null,
        "kind": kind,
        "alias": Value::Null,
        "group_id": Value::Null,
        "open": true,
    });
    rows_mut(doc, "folders").push(row);
    id
}

/// Go `createConversationLocked` (store.go:179).
pub fn create_conversation_locked(
    doc: &mut Map<String, Value>,
    folder: i64,
    agent: &str,
    title: &str,
    parent: i64,
) -> i64 {
    let id = allocate(doc);
    let now = goformat::timestamp();
    let mut kind = "regular".to_string();
    if folder_kind(doc, folder) == "chat" {
        kind = "chat".into();
    }
    let mut parent_value = json!(Value::Null);
    if parent > 0 {
        kind = "delegate".into();
        parent_value = json!(parent);
        for row in rows_mut(doc, "conversations").iter_mut() {
            if number(row.get("id")) == parent {
                if let Some(obj) = row.as_object_mut() {
                    let count = number(obj.get("child_count"));
                    obj.insert("child_count".into(), json!(count + 1));
                }
            }
        }
    }
    let row = json!({
        "id": id,
        "folder_id": folder,
        "title": title,
        "title_locked": false,
        "agent_type": agent,
        "status": "pending",
        "kind": kind,
        "model": Value::Null,
        "git_branch": Value::Null,
        "external_id": Value::Null,
        "message_count": 0,
        "child_count": 0,
        "created_at": now,
        "updated_at": now,
        "pinned_at": Value::Null,
        "parent_id": parent_value,
        "tag_ids": [],
    });
    rows_mut(doc, "conversations").push(row);
    id
}

/// 任务引擎建会话：在任务所属文件夹里建 row 并落盘（Go
/// `createConversationLocked(t.FolderID, agentType, title, 0)`）。
pub fn create_task_conversation(
    root: &Path,
    folder_id: i64,
    agent_type: &str,
    title: &str,
) -> Result<i64, String> {
    let (_guard, mut doc) = locked_doc(root)?;
    let id = create_conversation_locked(&mut doc, folder_id, agent_type, title, 0);
    persist(root, &doc)?;
    Ok(id)
}

/// Go `appendTurnLocked` (store.go:196) with the SQLite turn store: append the
/// row through `turn_store` and mirror message_count/updated_at into the JSON
/// document (the document no longer carries turns — turnstore.go).
pub fn append_turn_locked(
    root: &Path,
    doc: &mut Map<String, Value>,
    conversation_id: i64,
    turn: &Value,
) -> Result<(), String> {
    let count = crate::backend::turn_store::append_raw(root, conversation_id, turn)?;
    for row in rows_mut(doc, "conversations").iter_mut() {
        if number(row.get("id")) == conversation_id {
            if let Some(obj) = row.as_object_mut() {
                obj.insert("message_count".into(), json!(count));
                obj.insert("updated_at".into(), json!(goformat::timestamp()));
            }
        }
    }
    Ok(())
}

/// The changed event the connections layer emits after a store write
/// (Go `s.changed`): `conversation://changed` with an upsert summary.
fn changed_event(row: &Value) -> (String, Value) {
    (
        "conversation://changed".into(),
        json!({"kind": "upsert", "summary": row.clone()}),
    )
}

/// The store half of Go `prompt` (connections.go:594-631): ensure the
/// conversation (create under the prompt's working folder when absent), append
/// the user turn with its blocks, flip status to in_progress and stamp the
/// native session id. One lock, one save.
pub fn prompt_side_write(
    root: &Path,
    conversation_id_arg: i64,
    folder_id_arg: i64,
    workdir: &str,
    agent_type: &str,
    prompt_text: &str,
    user_blocks: &[Value],
    native_session_id: &str,
) -> Result<StoreOutcome, String> {
    crate::backend::turn_store::ensure_initialized(root)?;
    let (_guard, mut doc) = locked_doc(root)?;
    let mut conversation_id = conversation_id_arg;
    let mut folder_id = folder_id_arg;
    if conversation_id == 0 {
        let folder = open_folder_locked(&mut doc, workdir, "regular");
        folder_id = folder;
        conversation_id = create_conversation_locked(&mut doc, folder, agent_type, prompt_text, 0);
    }
    let mut row = None;
    for candidate in rows_mut(&mut doc, "conversations").iter() {
        if number(candidate.get("id")) == conversation_id {
            row = Some(candidate.clone());
            break;
        }
    }
    let Some(mut row) = row else {
        return Err("会话不存在".into());
    };
    let user_turn = json!({
        "id": goformat::hex_id(),
        "role": "user",
        "blocks": json!(user_blocks),
        "timestamp": goformat::timestamp(),
    });
    append_turn_locked(root, &mut doc, conversation_id, &user_turn)?;
    // Mutate the LIVE row in place — a stale-snapshot overwrite would clobber
    // appendTurnLocked's message_count (Go mutates the pointer).
    let mut event_row = row.clone();
    for candidate in rows_mut(&mut doc, "conversations").iter_mut() {
        if number(candidate.get("id")) == conversation_id {
            if let Some(obj) = candidate.as_object_mut() {
                obj.insert("status".into(), json!("in_progress"));
                obj.insert(
                    "external_id".into(),
                    if native_session_id.is_empty() {
                        Value::Null
                    } else {
                        json!(native_session_id)
                    },
                );
            }
            event_row = candidate.clone();
        }
    }
    persist(root, &doc)?;
    Ok(StoreOutcome {
        conversation_id,
        folder_id,
        events: vec![changed_event(&event_row)],
    })
}

#[allow(clippy::too_many_arguments)]
/// The store half of Go `runWithContent` (connections.go:682-715): the
/// assistant turn with promoted blocks, usage/model/duration metadata and the
/// terminal status. Missing conversations are silently skipped, like Go.
pub fn completion_side_write(
    root: &Path,
    conversation_id: i64,
    ok: bool,
    output: &str,
    blocks: &[Value],
    created_at: &str,
    source_turn_id: &str,
    usage: Option<&Value>,
    model: &str,
    duration_ms: Option<i64>,
    native_session_id: &str,
) -> Result<Vec<(String, Value)>, String> {
    if conversation_id == 0 {
        return Ok(Vec::new());
    }
    crate::backend::turn_store::ensure_initialized(root)?;
    let (_guard, mut doc) = locked_doc(root)?;
    let mut row = None;
    for candidate in rows_mut(&mut doc, "conversations").iter() {
        if number(candidate.get("id")) == conversation_id {
            row = Some(candidate.clone());
            break;
        }
    }
    let Some(mut row) = row else {
        return Ok(Vec::new());
    };
    if ok {
        let mut assistant_turn = json!({
            "id": goformat::hex_id(),
            "role": "assistant",
            "blocks": json!(blocks),
            "timestamp": if created_at.is_empty() { goformat::timestamp() } else { created_at.to_string() },
        });
        if !source_turn_id.is_empty() {
            assistant_turn["source_turn_id"] = json!(source_turn_id);
        }
        if let Some(usage) = usage.filter(|u| u.as_object().map(|m| !m.is_empty()).unwrap_or(false))
        {
            assistant_turn["usage"] = usage.clone();
        }
        if !model.is_empty() {
            assistant_turn["model"] = json!(model);
        }
        if let Some(duration) = duration_ms {
            assistant_turn["duration_ms"] = json!(duration);
        }
        append_turn_locked(root, &mut doc, conversation_id, &assistant_turn)?;
    }
    // Mutate the LIVE row in place (Go mutates the pointer); overwriting with
    // an earlier snapshot would clobber appendTurnLocked's message_count.
    let mut event_row = Value::Null;
    for candidate in rows_mut(&mut doc, "conversations").iter_mut() {
        if number(candidate.get("id")) == conversation_id {
            if let Some(obj) = candidate.as_object_mut() {
                obj.insert(
                    "status".into(),
                    json!(if ok { "pending_review" } else { "cancelled" }),
                );
                if !native_session_id.is_empty() {
                    obj.insert("external_id".into(), json!(native_session_id));
                }
            }
            event_row = candidate.clone();
        }
    }
    let event = changed_event(&event_row);
    persist(root, &doc)?;
    Ok(vec![event])
}

/// Read-only lookups the fork transaction needs (fork.go:62-91): the
/// conversation row and its turn list.
pub fn fork_prepare(root: &Path, conversation_id: i64) -> Result<(Value, Vec<Value>), String> {
    crate::backend::turn_store::ensure_initialized(root)?;
    let (_guard, doc) = locked_doc(root)?;
    let mut row = None;
    for candidate in doc
        .get("conversations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if number(candidate.get("id")) == conversation_id {
            row = Some(candidate.clone());
            break;
        }
    }
    let Some(row) = row else {
        return Err("会话不存在".into());
    };
    let turns = crate::backend::turn_store::all_turns_for(root, conversation_id)?;
    Ok((row, turns))
}

/// Go `fork.go` commit (fork.go:151-165): keep the original as a sibling,
/// truncate the current row's turns at the fork point and point it at the new
/// native session. `Next`/rows rollback on persist failure matches Go.
pub fn fork_commit(
    root: &Path,
    conversation_id: i64,
    point: usize,
    original_native: &str,
    fork_native: &str,
) -> Result<i64, String> {
    crate::backend::turn_store::ensure_initialized(root)?;
    let (_guard, mut doc) = locked_doc(root)?;
    let mut row = None;
    for candidate in doc
        .get("conversations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if number(candidate.get("id")) == conversation_id {
            row = Some(candidate.clone());
            break;
        }
    }
    let Some(mut row) = row else {
        return Err("会话不存在".into());
    };
    let turns: Vec<Value> = crate::backend::turn_store::all_turns_for(root, conversation_id)?;
    let sibling_id = allocate(&mut doc);
    let now = goformat::timestamp();
    let mut sibling = row.clone();
    if let Some(obj) = sibling.as_object_mut() {
        obj.insert("id".into(), json!(sibling_id));
        obj.insert("created_at".into(), json!(now));
        obj.insert("updated_at".into(), json!(now));
        obj.insert("external_id".into(), json!(original_native));
        obj.insert("pinned_at".into(), Value::Null);
    }
    // Go keeps `turns[:point+1]` — `point` is the inclusive last kept turn.
    let kept: Vec<Value> = turns[..(point + 1).min(turns.len())].to_vec();
    let kept_len = kept.len() as i64;
    {
        let conversations = rows_mut(&mut doc, "conversations");
        conversations.push(sibling.clone());
        for candidate in conversations.iter_mut() {
            if number(candidate.get("id")) == conversation_id {
                if let Some(obj) = candidate.as_object_mut() {
                    obj.insert("external_id".into(), json!(fork_native));
                    obj.insert("message_count".into(), json!(kept_len));
                    obj.insert("updated_at".into(), json!(now));
                }
            }
        }
    }
    let _ = row;
    persist(root, &doc)?;
    // Turns live in SQLite now: give the sibling the full list and truncate
    // the original at the fork point (fork.go keeps turns[:point+1]).
    crate::backend::turn_store::replace_all(root, sibling_id, &turns)?;
    crate::backend::turn_store::replace_all(root, conversation_id, &kept)?;
    Ok(sibling_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("acp-storeconv-{tag}-{}", goformat::hex_id()));
        // The workspace document (and the SQLite turn store) live under `.data`.
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn seed(root: &Path) {
        fs::write(
            store_path(root),
            json!({
                "next": 3,
                "folders": [{"id": 1, "name": "ws", "path": "E:/ws/a", "kind": "regular", "open": false}],
                "conversations": [{"id": 2, "folder_id": 1, "title": "t", "status": "pending",
                                   "kind": "regular", "message_count": 0, "child_count": 0}],
                "turns": {"2": []}
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn prompt_side_write_creates_and_updates() {
        let root = temp_root("prompt");
        seed(&root);
        let out = prompt_side_write(
            &root,
            0,
            0,
            "E:/ws/new",
            "claude_code",
            "你好",
            &[
                json!({"type": "text", "text": "你好"}),
                json!({"type": "image", "data": "x"}),
            ],
            "native-1",
        )
        .expect("write");
        // new folder allocated from next=3, conversation 4
        assert_eq!(out.conversation_id, 4);
        assert_eq!(out.folder_id, 3);
        assert_eq!(out.events.len(), 1);
        assert_eq!(out.events[0].0, "conversation://changed");
        assert_eq!(out.events[0].1["kind"], "upsert");
        assert_eq!(out.events[0].1["summary"]["status"], "in_progress");
        assert_eq!(out.events[0].1["summary"]["external_id"], "native-1");

        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        let folders = doc.get("folders").unwrap().as_array().unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[1]["path"], "E:/ws/new");
        // Turns live in SQLite (turnstore.go), not the JSON document.
        assert!(doc.get("turns").is_none(), "文档不再承载 turns");
        let turns = crate::backend::turn_store::all_turns_for(&root, 4).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["role"], "user");
        assert_eq!(turns[0]["blocks"].as_array().unwrap().len(), 2);
        fs::remove_dir_all(root).ok();
    }

    /// Regression for the new-conversation send failure: the front door's
    /// `create_conversation` persists the row into
    /// `<root>/.data/codeg-workspace.json`. If the ACP prompt half ever drifts
    /// back to a different path, the first message of a brand-new conversation
    /// dies with 「会话不存在」. Keep both halves on the same document.
    #[test]
    fn prompt_adopts_conversation_created_by_front_door() {
        let root = temp_root("shared-store");
        seed(&root);
        let created = crate::backend::store_writes::handle(
            &root,
            "create_conversation",
            &json!({"folderId": 1, "agentType": "claude_code", "title": "新会话"}),
        )
        .expect("store_writes owns create_conversation")
        .result
        .expect("create conversation");
        let id = created.as_i64().expect("numeric conversation id");
        assert_eq!(id, 3, "id comes from the seeded next counter");
        // No stray root-level document: both halves must share one file.
        assert!(!root.join(FILE).exists());

        let out = prompt_side_write(
            &root,
            id,
            1,
            "",
            "claude_code",
            "你好",
            &[json!({"type": "text", "text": "你好"})],
            "native-1",
        )
        .expect("prompt must see the row create_conversation just wrote");
        assert_eq!(out.conversation_id, id);

        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        let row = doc["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"].as_i64() == Some(id))
            .expect("created row survived the prompt write");
        assert_eq!(row["status"], "in_progress");
        assert_eq!(row["message_count"], 1);
        assert_eq!(
            crate::backend::turn_store::all_turns_for(&root, id)
                .unwrap()
                .len(),
            1
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn completion_side_write_promotes_and_cancels() {
        let root = temp_root("completion");
        seed(&root);
        prompt_side_write(
            &root,
            2,
            1,
            "",
            "",
            "问",
            &[json!({"type": "text", "text": "问"})],
            "native-9",
        )
        .expect("prompt write");
        let events = completion_side_write(
            &root,
            2,
            true,
            "答",
            &[json!({"type": "text", "text": "答"})],
            "2026-01-01T00:00:00Z",
            "fixture-message-1",
            Some(&json!({"input_tokens": 10, "output_tokens": 5})),
            "fixture-v1",
            Some(1200),
            "native-9",
        )
        .expect("completion write");
        assert_eq!(events.len(), 1);
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        let row = &doc.get("conversations").unwrap().as_array().unwrap()[0];
        assert_eq!(row["status"], "pending_review");
        assert_eq!(row["message_count"], 2);
        let turns = crate::backend::turn_store::all_turns_for(&root, 2).unwrap();
        let last = turns.last().unwrap();
        assert_eq!(last["role"], "assistant");
        assert_eq!(last["source_turn_id"], "fixture-message-1");
        assert_eq!(last["model"], "fixture-v1");
        assert_eq!(last["duration_ms"], 1200);
        assert_eq!(last["usage"]["input_tokens"], 10);

        // failure path flips status to cancelled without appending
        let events =
            completion_side_write(&root, 2, false, "", &[], "", "", None, "", None, "native-9")
                .expect("cancel write");
        assert_eq!(events.len(), 1);
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        let row = &doc.get("conversations").unwrap().as_array().unwrap()[0];
        assert_eq!(row["status"], "cancelled");
        let turns = crate::backend::turn_store::all_turns_for(&root, 2).unwrap();
        assert_eq!(turns.len(), 2, "no assistant turn appended on failure");
    }

    #[test]
    fn fork_commit_keeps_original_as_sibling() {
        let root = temp_root("fork");
        seed(&root);
        prompt_side_write(
            &root,
            2,
            1,
            "",
            "",
            "q",
            &[json!({"type": "text", "text": "q"})],
            "n1",
        )
        .unwrap();
        completion_side_write(
            &root,
            2,
            true,
            "a1",
            &[],
            "",
            "fixture-message-1",
            None,
            "",
            None,
            "n1",
        )
        .unwrap();
        completion_side_write(
            &root,
            2,
            true,
            "a2",
            &[],
            "",
            "fixture-message-2",
            None,
            "",
            None,
            "n1",
        )
        .unwrap();
        let sibling = fork_commit(&root, 2, 0, "n1", "n2").expect("fork commit");
        assert_eq!(sibling, 3);
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        let conversations = doc.get("conversations").unwrap().as_array().unwrap();
        assert_eq!(conversations.len(), 2);
        assert_eq!(
            conversations[1]["external_id"], "n1",
            "sibling keeps the original native id"
        );
        assert_eq!(
            crate::backend::turn_store::all_turns_for(&root, 2)
                .unwrap()
                .len(),
            1,
            "current row truncated at the fork point"
        );
        assert_eq!(
            crate::backend::turn_store::all_turns_for(&root, 3)
                .unwrap()
                .len(),
            3,
            "sibling keeps the full history"
        );
        assert_eq!(conversations[0]["external_id"], "n2");
        assert_eq!(conversations[0]["message_count"], 1);
    }
}
