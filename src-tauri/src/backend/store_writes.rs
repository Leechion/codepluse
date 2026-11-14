//! Write commands over `codeg-workspace.json`, ported from Go's
//! store-locked handlers (`command_sidebar.go`, `command_workspace.go`,
//! `canvas.go`, `git_settings.go`).
//!
//! This is the batch the read-only port was waiting for: until now a Rust
//! write would be clobbered by the sidecar's next full-file save. Two things
//! make the flip safe:
//!
//! 1. Go re-reads the document before every request
//!    (`Store.reloadIfChangedLocked`), so both processes stay honest through
//!    the file and a Rust write survives.
//! 2. Every handler here re-reads the file, mutates an in-memory copy, and
//!    persists it once at the end via `goformat::marshal_store`, byte-identical
//!    to Go's `json.Marshal`. Errors leave the file untouched, which matches
//!    Go's rollback of its in-memory copy.
//!
//! Events (conversation://changed, canvas://changed, canvas-board://changed,
//! audit://folder_open) are returned to the caller as frames and injected into
//! the connected WebSocket clients by the front door — Go's `emit` writes the
//! same frames to its subscriber channels.
//!
//! Deliberately NOT ported here: `delete_conversation` and the
//! `get_conversation*` joins — they need the SQLite turn store and the ACP
//! connection registry, which live in later batches (④/⑤ of
//! docs/RUST-BACKEND-REMAINING.md).

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::backend::git;
use crate::backend::goformat;
use crate::backend::store_file;

const FILE: &str = "codeg-workspace.json";

/// Result of one command: the HTTP answer plus side effects for the caller.
pub struct Outcome {
    pub result: Result<Value, String>,
    /// `(channel, payload)` frames injected into connected WS clients.
    pub events: Vec<(String, Value)>,
    /// `(command, args)` side calls — canvas deletes must kill the terminals
    /// their nodes owned. mod.rs intercepts `terminal_kill` and routes it to
    /// the Rust terminal module（G 组）instead of the sidecar.
    pub forward: Vec<(String, Value)>,
}

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        // sidebar — command_sidebar.go
        "create_folder_group"
            | "update_folder_group"
            | "delete_folder_group"
            | "set_folder_group"
            | "apply_sidebar_layout"
            | "create_conversation_tag"
            | "update_conversation_tag"
            | "delete_conversation_tag"
            | "reorder_conversation_tags"
            | "update_conversation_branch_tag"
            | "update_conversation_tags"
            | "assistant_create"
            | "assistant_update"
            | "assistant_delete"
            | "assistant_reorder"
            | "quick_messages_create"
            | "quick_messages_update"
            | "quick_messages_delete"
            | "quick_messages_reorder"
            // workspace — command_workspace.go
            | "open_folder"
            | "open_folder_in_workspace"
            | "open_worktree_folder"
            | "remove_folder_from_history"
            | "update_folder_color"
            | "update_folder_alias"
            | "update_folder_default_agent"
            | "remove_folder_from_workspace"
            | "update_system_settings"
            | "save_system_settings"
            | "create_conversation"
            | "create_chat_dir"
            | "create_chat_conversation"
            | "update_conversation_title"
            | "update_conversation_status"
            | "update_conversation_pinned"
            | "save_opened_tabs"
            // canvas — canvas.go
            | "canvas_list_boards"
            | "canvas_create_board"
            | "canvas_update_board"
            | "canvas_list_nodes"
            | "canvas_create_node"
            | "canvas_update_node"
            | "canvas_move_nodes"
            | "canvas_delete_node"
            | "canvas_delete_nodes"
            | "canvas_delete_board"
            | "canvas_group_into_region"
            | "canvas_detach_member"
            // commit settings — git_settings.go
            | "set_commit_message_settings"
            | "delete_commit_message_settings"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Outcome> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

// ── Go value helpers ────────────────────────────────────────────────────────

/// Go's `number()` coercion: ints pass, floats truncate (clamped to int
/// range), everything else is 0.
fn number(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => {
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
                return f.trunc() as i64;
            }
            0
        }
        _ => 0,
    }
}

fn num(args: &Value, key: &str) -> i64 {
    number(args.get(key))
}

/// Go's `text()`: string type assertion, everything else empty.
fn text(value: Option<&Value>) -> String {
    value.and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn txt(args: &Value, key: &str) -> String {
    text(args.get(key))
}

/// Go's `float()`: numbers only, everything else 0.
fn gofloat(value: Option<&Value>) -> f64 {
    value.and_then(|v| v.as_f64()).unwrap_or(0.0)
}

/// Go's `ids()`: numbers out of an array.
fn ids(value: Option<&Value>) -> Vec<i64> {
    match value {
        Some(Value::Array(list)) => list.iter().map(|v| number(Some(v))).collect(),
        _ => Vec::new(),
    }
}

fn has_id(list: &[i64], id: i64) -> bool {
    list.contains(&id)
}

/// Go's `nullable()`: trimmed empty string becomes null, else the trimmed text.
fn nullable(value: Option<&Value>) -> Value {
    let trimmed = text(value).trim().to_string();
    if trimmed.is_empty() {
        Value::Null
    } else {
        json!(trimmed)
    }
}

fn group_color(value: Option<&Value>) -> String {
    let s = text(value);
    if s.is_empty() {
        "default".to_string()
    } else {
        s
    }
}

fn region_kind(kind: &str) -> bool {
    kind == "custom" || kind == "folder" || kind == "group" || kind == "agent"
}

// ── store access ────────────────────────────────────────────────────────────

fn store_path(root: &Path) -> std::path::PathBuf {
    root.join(".data").join(FILE)
}

/// Re-reads the document every call: the file is the shared truth between the
/// two backends. Missing file gets Go's `newStore` seed (`next` starts at 1);
/// a document that is not an object is refused the way Go's unmarshal refuses
/// it at startup — a panic here would drop the request with no answer.
fn load(root: &Path) -> Result<Value, String> {
    match store_file::read_json(&store_path(root))? {
        Some(doc) if doc.is_object() => Ok(doc),
        Some(_) => Err("工作区数据文件损坏：顶层必须是对象".into()),
        None => Ok(json!({ "next": 1 })),
    }
}

/// Persists with Go's exact byte layout. The caller holds the cross-process
/// lock for the whole read-modify-write (see `run`).
fn save(root: &Path, doc: &Value) -> Result<(), String> {
    let bytes = goformat::marshal_store(doc);
    store_file::write_bytes(&store_path(root), &bytes)
}

/// Ensures a top-level object field exists (Go's struct defaults), then hands
/// back the map.
fn ensure_object<'a>(doc: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    if !doc.get(key).map(Value::is_object).unwrap_or(false) {
        doc[key] = json!({});
    }
    doc.get_mut(key).unwrap().as_object_mut().unwrap()
}

/// Ensures a top-level array field exists, then hands back the vec.
fn array_mut<'a>(doc: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    if !doc.get(key).map(Value::is_array).unwrap_or(false) {
        doc[key] = json!([]);
    }
    doc.get_mut(key).unwrap().as_array_mut().unwrap()
}

/// Go's `allocate()`: take `next`, then increment it in the document.
fn allocate(doc: &mut Value) -> i64 {
    let n = number(doc.get("next"));
    doc["next"] = json!(n + 1);
    n
}

fn find_by_id(rows: &[Value], id: i64) -> Option<Value> {
    rows.iter().find(|r| number(r.get("id")) == id).cloned()
}

fn conversation_exists(doc: &Value, id: i64) -> bool {
    find_by_id(rows(doc, "conversations"), id).is_some()
}

fn rows<'a>(doc: &'a Value, key: &str) -> &'a [Value] {
    doc.get(key)
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

fn changed_event(row: &Value) -> (String, Value) {
    (
        "conversation://changed".to_string(),
        json!({"kind": "upsert", "summary": row}),
    )
}

// ── command entry ───────────────────────────────────────────────────────────

fn run(root: &Path, command: &str, args: &Value) -> Outcome {
    let mut events = Vec::new();
    let mut forward = Vec::new();
    // One lock covers the whole read-modify-write: parallel requests from the
    // UI must not lose updates the way two unsynchronised writers would.
    let result = store_file::lock(&store_path(root))
        .and_then(|_guard| run_inner(root, command, args, &mut events, &mut forward));
    // Go emits some events before the save; on a failed save its event is
    // already gone while ours is dropped with the outcome — error paths only.
    if result.is_ok() {
        Outcome {
            result,
            events,
            forward,
        }
    } else {
        Outcome {
            result,
            events: Vec::new(),
            forward: Vec::new(),
        }
    }
}

fn run_inner(
    root: &Path,
    command: &str,
    args: &Value,
    events: &mut Vec<(String, Value)>,
    forward: &mut Vec<(String, Value)>,
) -> Result<Value, String> {
    if command == "create_chat_dir" {
        return create_chat_dir(root);
    }
    if command.starts_with("canvas_") {
        return canvas_command(root, command, args, events, forward);
    }
    if command == "set_commit_message_settings" || command == "delete_commit_message_settings" {
        return commit_settings(root, command, args);
    }
    if command.starts_with("assistant_") || command.starts_with("quick_messages_") {
        return sidebar_entity_command(root, command, args);
    }
    if command == "update_conversation_tags" {
        return update_conversation_tags(root, args, events);
    }

    let mut doc = load(root)?;
    let outcome = match command {
        // ---- sidebar: folder groups & tags ----
        "create_folder_group" => {
            let group = json!({
                "id": allocate(&mut doc),
                "name": txt(args, "name"),
                "color": group_color(args.get("color")),
                "sort_order": rows(&doc, "folderGroups").len(),
            });
            array_mut(&mut doc, "folderGroups").push(group.clone());
            save(root, &doc)?;
            Ok(group)
        }
        "update_folder_group" => {
            let gid = num(args, "groupId");
            let Some(mut group) = find_by_id(rows(&doc, "folderGroups"), gid) else {
                return Err("分组不存在".into());
            };
            if let Some(name) = args.get("name") {
                group["name"] = json!(text(Some(name)));
            }
            if let Some(color) = args.get("color") {
                group["color"] = json!(text(Some(color)));
            }
            replace_by_id(array_mut(&mut doc, "folderGroups"), gid, &group);
            save(root, &doc)?;
            Ok(group)
        }
        "delete_folder_group" => {
            let gid = num(args, "groupId");
            array_mut(&mut doc, "folderGroups").retain(|g| number(g.get("id")) != gid);
            for folder in array_mut(&mut doc, "folders").iter_mut() {
                if number(folder.get("group_id")) == gid {
                    folder["group_id"] = Value::Null;
                }
            }
            save(root, &doc)?;
            Ok(Value::Null)
        }
        "set_folder_group" => {
            let fid = num(args, "folderId");
            if find_by_id(rows(&doc, "folders"), fid).is_none() {
                return Err("项目不存在".into());
            }
            let group_id = match args.get("groupId") {
                None | Some(Value::Null) => Value::Null,
                Some(value) => json!(number(Some(value))),
            };
            let folder = array_mut(&mut doc, "folders")
                .iter_mut()
                .find(|f| number(f.get("id")) == fid)
                .expect("刚检查过存在");
            folder["group_id"] = group_id;
            save(root, &doc)?;
            Ok(Value::Null)
        }
        "apply_sidebar_layout" => {
            let entries = args
                .get("entries")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for (index, entry) in entries.iter().enumerate() {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                let entry = Value::Object(entry.clone());
                let eid = num(&entry, "id");
                if text(entry.get("kind")) == "group" {
                    if let Some(group) = find_by_id(rows(&doc, "folderGroups"), eid) {
                        let mut group = group;
                        group["sort_order"] = json!(index);
                        replace_by_id(array_mut(&mut doc, "folderGroups"), eid, &group);
                    }
                    continue;
                }
                if find_by_id(rows(&doc, "folders"), eid).is_some() {
                    let group_id = match entry.get("groupId") {
                        None | Some(Value::Null) => Value::Null,
                        Some(value) => json!(number(Some(value))),
                    };
                    let folder = array_mut(&mut doc, "folders")
                        .iter_mut()
                        .find(|f| number(f.get("id")) == eid)
                        .expect("刚检查过存在");
                    folder["sort_order"] = json!(index);
                    folder["group_id"] = group_id;
                }
            }
            save(root, &doc)?;
            Ok(Value::Null)
        }
        "create_conversation_tag" => {
            let tag = json!({
                "id": allocate(&mut doc),
                "folder_id": args.get("folderId").cloned().unwrap_or(Value::Null),
                "name": txt(args, "name"),
                "color": txt(args, "color"),
                "sort_order": rows(&doc, "conversationTags").len(),
            });
            array_mut(&mut doc, "conversationTags").push(tag.clone());
            save(root, &doc)?;
            Ok(tag)
        }
        "update_conversation_tag" => {
            let tid = num(args, "tagId");
            let Some(mut tag) = find_by_id(rows(&doc, "conversationTags"), tid) else {
                return Err("标签不存在".into());
            };
            if let Some(name) = args.get("name") {
                tag["name"] = json!(text(Some(name)));
            }
            if let Some(color) = args.get("color") {
                tag["color"] = json!(text(Some(color)));
            }
            replace_by_id(array_mut(&mut doc, "conversationTags"), tid, &tag);
            save(root, &doc)?;
            Ok(tag)
        }
        "delete_conversation_tag" => {
            let tid = num(args, "tagId");
            array_mut(&mut doc, "conversationTags").retain(|t| number(t.get("id")) != tid);
            for conversation in array_mut(&mut doc, "conversations").iter_mut() {
                if let Some(list) = conversation
                    .get_mut("tag_ids")
                    .and_then(|v| v.as_array_mut())
                {
                    list.retain(|v| number(Some(v)) != tid);
                }
            }
            save(root, &doc)?;
            Ok(Value::Null)
        }
        "reorder_conversation_tags" => {
            for (index, tid) in ids(args.get("tagIds")).into_iter().enumerate() {
                if let Some(mut tag) = find_by_id(rows(&doc, "conversationTags"), tid) {
                    tag["sort_order"] = json!(index);
                    replace_by_id(array_mut(&mut doc, "conversationTags"), tid, &tag);
                }
            }
            save(root, &doc)?;
            Ok(Value::Null)
        }
        "update_conversation_branch_tag" => {
            let mut color = txt(args, "color");
            if color.is_empty() {
                color = "default".into();
            }
            let setting = json!({
                "color": color,
                "enabled": args.get("enabled") == Some(&Value::Bool(true)),
            });
            ensure_object(&mut doc, "settings")
                .insert("conversation_branch_tag".to_string(), setting.clone());
            save(root, &doc)?;
            Ok(setting)
        }

        // ---- workspace: folders ----
        "open_folder" | "open_folder_in_workspace" => {
            let resolved = git::resolve(root, &txt(args, "path"))?;
            match std::fs::metadata(&resolved) {
                Ok(meta) if meta.is_dir() => {}
                _ => return Err("项目目录不存在".into()),
            }
            let folder = open_folder_locked(&mut doc, &resolved.to_string_lossy(), "regular");
            events.push((
                "audit://folder_open".to_string(),
                json!({
                    "folder_id": folder.get("id").cloned().unwrap_or(Value::Null),
                    "path": resolved.to_string_lossy(),
                }),
            ));
            save(root, &doc)?;
            Ok(folder)
        }
        "open_worktree_folder" => {
            let source_id = num(args, "sourceFolderId");
            let source = find_by_id(rows(&doc, "folders"), source_id).ok_or("来源项目不存在")?;
            let parent_id = number(source.get("parent_id"));
            let root_id = if parent_id > 0 { parent_id } else { source_id };
            let resolved = git::resolve(root, &txt(args, "path"))?;
            if !resolved.is_dir() {
                return Err("工作树目录不存在".into());
            }
            let mut folder = open_folder_locked(&mut doc, &resolved.to_string_lossy(), "regular");
            folder["parent_id"] = json!(root_id);
            replace_by_id(
                array_mut(&mut doc, "folders"),
                number(folder.get("id")),
                &folder,
            );
            events.push((
                "audit://folder_open".into(),
                json!({"folder_id":folder.get("id"),"path":resolved.to_string_lossy()}),
            ));
            save(root, &doc)?;
            Ok(folder)
        }
        "remove_folder_from_history" => {
            let path = txt(args, "path");
            let folder_rows = array_mut(&mut doc, "folders");
            folder_rows.retain(|row| text(row.get("path")) != path);
            save(root, &doc)?;
            Ok(Value::Null)
        }
        "update_folder_color" | "update_folder_alias" | "update_folder_default_agent" => {
            let fid = num(args, "folderId");
            let Some(mut folder) = find_by_id(rows(&doc, "folders"), fid) else {
                return Err("项目不存在".into());
            };
            match command {
                "update_folder_color" => folder["color"] = json!(txt(args, "color")),
                "update_folder_alias" => {
                    folder["alias"] = args.get("alias").cloned().unwrap_or(Value::Null)
                }
                _ => {
                    folder["default_agent_type"] =
                        args.get("defaultAgentType").cloned().unwrap_or(Value::Null)
                }
            }
            replace_by_id(array_mut(&mut doc, "folders"), fid, &folder);
            save(root, &doc)?;
            Ok(folder)
        }
        "remove_folder_from_workspace" => {
            let fid = num(args, "folderId");
            let Some(mut folder) = find_by_id(rows(&doc, "folders"), fid) else {
                return Err("项目不存在".into());
            };
            folder["open"] = json!(false);
            replace_by_id(array_mut(&mut doc, "folders"), fid, &folder);
            save(root, &doc)?;
            Ok(Value::Null)
        }

        // ---- workspace: settings ----
        "update_system_settings" | "save_system_settings" => {
            let settings = ensure_object(&mut doc, "settings");
            if let Some(patch) = args.as_object() {
                for (key, value) in patch {
                    settings.insert(key.clone(), value.clone());
                }
            }
            save(root, &doc)?;
            Ok(Value::Null)
        }

        // ---- workspace: conversations ----
        "create_conversation" => {
            let folder_id = num(args, "folderId");
            if find_by_id(rows(&doc, "folders"), folder_id).is_none() {
                return Err("项目不存在".into());
            }
            let row = create_conversation_locked(
                &mut doc,
                folder_id,
                &txt(args, "agentType"),
                &txt(args, "title"),
                0,
            );
            events.push(changed_event(&row));
            let id = row["id"].clone();
            save(root, &doc)?;
            Ok(id)
        }
        "create_chat_conversation" => {
            let existing = txt(args, "existingDir");
            let dir = if existing.is_empty() {
                mkdir_chat_dir(root)?
            } else {
                git::resolve(root, &existing)?
            };
            let folder = open_folder_locked(&mut doc, &dir.to_string_lossy(), "chat");
            let folder_id = number(folder.get("id"));
            let row = create_conversation_locked(
                &mut doc,
                folder_id,
                &txt(args, "agentType"),
                &txt(args, "title"),
                0,
            );
            events.push(changed_event(&row));
            save(root, &doc)?;
            Ok(json!({
                "conversationId": row["id"],
                "folderId": folder_id,
                "folder": folder,
            }))
        }
        "update_conversation_title"
        | "update_conversation_status"
        | "update_conversation_pinned" => {
            let cid = num(args, "conversationId");
            let Some(mut row) = find_by_id(rows(&doc, "conversations"), cid) else {
                return Err("会话不存在".into());
            };
            match command {
                "update_conversation_title" => {
                    row["title"] = args.get("title").cloned().unwrap_or(Value::Null);
                    row["title_locked"] = json!(true);
                }
                "update_conversation_status" => {
                    row["status"] = args.get("status").cloned().unwrap_or(Value::Null);
                }
                _ => {
                    row["pinned_at"] = Value::Null;
                    if args.get("pinned") == Some(&Value::Bool(true)) {
                        row["pinned_at"] = json!(goformat::timestamp());
                    }
                }
            }
            events.push(changed_event(&row));
            replace_by_id(array_mut(&mut doc, "conversations"), cid, &row);
            save(root, &doc)?;
            Ok(Value::Null)
        }
        "save_opened_tabs" => {
            let version = number(doc.get("tabsVersion"));
            if num(args, "expectedVersion") == version {
                // Go marshals the items and unmarshals into []Row: arrays of
                // objects replace, null yields an empty list; a scalar array
                // fails that unmarshal silently and keeps the current rows
                // (the version still moves).
                let current: Vec<Value> = rows(&doc, "tabs").to_vec();
                let tabs = match args.get("items") {
                    Some(Value::Array(list)) => {
                        let all_rows = list.iter().all(|item| item.is_object() || item.is_null());
                        if all_rows {
                            list.clone()
                        } else {
                            current
                        }
                    }
                    Some(Value::Null) | None => Vec::new(),
                    Some(_) => current,
                };
                doc["tabs"] = Value::Array(tabs.clone());
                doc["tabsVersion"] = json!(version + 1);
                save(root, &doc)?;
                Ok(json!({"accepted": true, "version": version + 1, "tabs": tabs}))
            } else {
                Ok(json!({
                    "accepted": false,
                    "version": version,
                    "tabs": doc.get("tabs").cloned().unwrap_or(Value::Null),
                }))
            }
        }
        _ => Err(format!("Go 移植尚未实现此接口：{command}")),
    };
    outcome
}

/// Go's `openFolderLocked`: reopen by exact path or append a new row.
fn open_folder_locked(doc: &mut Value, path: &str, kind: &str) -> Value {
    let now = goformat::timestamp();
    let existing = rows(doc, "folders")
        .iter()
        .position(|f| text(f.get("path")) == path);
    if let Some(index) = existing {
        let folder = &mut array_mut(doc, "folders")[index];
        folder["last_opened_at"] = json!(now);
        folder["open"] = json!(true);
        return folder.clone();
    }
    let sort_order = rows(doc, "folders").len();
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let row = json!({
        "id": allocate(doc),
        "name": name,
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
    array_mut(doc, "folders").push(row.clone());
    row
}

/// Go's `createConversationLocked`.
fn create_conversation_locked(
    doc: &mut Value,
    folder: i64,
    agent: &str,
    title: &str,
    parent: i64,
) -> Value {
    let now = goformat::timestamp();
    let mut kind = "regular";
    if rows(doc, "folders")
        .iter()
        .any(|f| number(f.get("id")) == folder && text(f.get("kind")) == "chat")
    {
        kind = "chat";
    }
    let mut parent_id = Value::Null;
    if parent > 0 {
        kind = "delegate";
        parent_id = json!(parent);
    }
    let row = json!({
        "id": allocate(doc),
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
        "parent_id": parent_id,
        "tag_ids": [],
    });
    if parent > 0 {
        if let Some(parent_row) = find_by_id(rows(doc, "conversations"), parent) {
            let mut parent_row = parent_row;
            parent_row["child_count"] = json!(number(parent_row.get("child_count")) + 1);
            replace_by_id(array_mut(doc, "conversations"), parent, &parent_row);
        }
    }
    array_mut(doc, "conversations").push(row.clone());
    row
}

/// Go's `os.MkdirTemp(<root>/.data, "chat-")`.
fn mkdir_chat_dir(root: &Path) -> Result<std::path::PathBuf, String> {
    let base = root.join(".data");
    std::fs::create_dir_all(&base).map_err(|e| format!("创建目录失败：{e}"))?;
    for _ in 0..64 {
        let dir = base.join(format!("chat-{:08x}", rand::random::<u32>()));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.to_string()),
        }
    }
    Err("创建会话目录失败：重试多次均撞名".into())
}

fn create_chat_dir(root: &Path) -> Result<Value, String> {
    let dir = mkdir_chat_dir(root)?;
    Ok(json!({"path": dir.to_string_lossy()}))
}

/// Go's delta-merge `update_conversation_tags`.
fn update_conversation_tags(
    root: &Path,
    args: &Value,
    events: &mut Vec<(String, Value)>,
) -> Result<Value, String> {
    let mut doc = load(root)?;
    let cid = num(args, "conversationId");
    let Some(mut row) = find_by_id(rows(&doc, "conversations"), cid) else {
        return Err("会话不存在".into());
    };
    let remove: Vec<i64> = ids(args.get("remove"));
    let mut merged: Vec<i64> = Vec::new();
    // Go keeps every existing entry (duplicates included) and only consults
    // its `present` set for the added ids.
    for existing in row
        .get("tag_ids")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let n = number(Some(existing));
        if !remove.contains(&n) {
            merged.push(n);
        }
    }
    for added in ids(args.get("add")) {
        if !merged.contains(&added) && !remove.contains(&added) {
            merged.push(added);
        }
    }
    row["tag_ids"] = json!(merged);
    events.push(changed_event(&row));
    replace_by_id(array_mut(&mut doc, "conversations"), cid, &row);
    save(root, &doc)?;
    Ok(row)
}

// ── assistants & quick messages (p1.go) ─────────────────────────────────────

fn sidebar_entity_command(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let mut doc = load(root)?;
    match command {
        "assistant_create" => {
            let draft = args.get("draft").cloned().unwrap_or(Value::Null);
            let now = goformat::timestamp();
            let mut row = json!({
                "id": allocate(&mut doc),
                "uid": goformat::hex_id(),
                "name": text(draft.get("name")),
                "description": text(draft.get("description")),
                "avatar": draft.get("avatar").cloned().unwrap_or(Value::Null),
                "instructions": draft.get("instructions").cloned().unwrap_or(Value::Null),
                "enabled": draft.get("enabled").cloned().unwrap_or(Value::Null),
                "sort_order": rows(&doc, "assistants").len(),
                "revision": 1,
                "created_at": now,
                "updated_at": now,
                "deleted_at": Value::Null,
            });
            flatten_agent_config(&mut row, &draft);
            array_mut(&mut doc, "assistants").push(row.clone());
            save(root, &doc)?;
            Ok(row)
        }
        "assistant_update" => {
            let aid = num(args, "id");
            let Some(mut row) = find_by_id(rows(&doc, "assistants"), aid) else {
                return Err("助手不存在".into());
            };
            let patch = args.get("patch").cloned().unwrap_or(Value::Null);
            if let Some(name) = patch.get("name") {
                row["name"] = json!(text(Some(name)));
            }
            if let Some(description) = patch.get("description") {
                row["description"] = json!(text(Some(description)));
            }
            if let Some(avatar) = patch.get("avatar") {
                row["avatar"] = avatar.clone();
            }
            if let Some(instructions) = patch.get("instructions") {
                row["instructions"] = instructions.clone();
            }
            if let Some(enabled) = patch.get("enabled") {
                row["enabled"] = enabled.clone();
            }
            if patch.get("agent_config").is_some() {
                flatten_agent_config(&mut row, &patch);
            }
            row["revision"] = json!(number(row.get("revision")) + 1);
            row["updated_at"] = json!(goformat::timestamp());
            replace_by_id(array_mut(&mut doc, "assistants"), aid, &row);
            save(root, &doc)?;
            Ok(row)
        }
        "assistant_delete" => {
            let aid = num(args, "id");
            let Some(mut row) = find_by_id(rows(&doc, "assistants"), aid) else {
                return Err("助手不存在".into());
            };
            let now = goformat::timestamp();
            row["deleted_at"] = json!(now);
            row["revision"] = json!(number(row.get("revision")) + 1);
            row["updated_at"] = json!(now);
            replace_by_id(array_mut(&mut doc, "assistants"), aid, &row);
            save(root, &doc)?;
            Ok(row)
        }
        "assistant_reorder" => {
            let mut changed_rows = Vec::new();
            for (index, aid) in ids(args.get("ids")).into_iter().enumerate() {
                let Some(mut row) = find_by_id(rows(&doc, "assistants"), aid) else {
                    continue;
                };
                if number(row.get("sort_order")) == index as i64 {
                    continue;
                }
                row["sort_order"] = json!(index);
                row["revision"] = json!(number(row.get("revision")) + 1);
                row["updated_at"] = json!(goformat::timestamp());
                replace_by_id(array_mut(&mut doc, "assistants"), aid, &row);
                changed_rows.push(row);
            }
            save(root, &doc)?;
            Ok(Value::Array(changed_rows))
        }
        "quick_messages_create" => {
            let now = goformat::timestamp();
            let row = json!({
                "id": allocate(&mut doc),
                "title": txt(args, "title"),
                "content": txt(args, "content"),
                "sort_order": rows(&doc, "quickMessages").len(),
                "created_at": now,
                "updated_at": now,
            });
            array_mut(&mut doc, "quickMessages").push(row.clone());
            save(root, &doc)?;
            Ok(row)
        }
        "quick_messages_update" => {
            let qid = num(args, "id");
            let Some(mut row) = find_by_id(rows(&doc, "quickMessages"), qid) else {
                return Err("快捷消息不存在".into());
            };
            // Go checks `title != nil`, so an explicit null keeps the old value.
            if !args.get("title").map(Value::is_null).unwrap_or(true) {
                row["title"] = json!(text(args.get("title")));
            }
            if !args.get("content").map(Value::is_null).unwrap_or(true) {
                row["content"] = json!(text(args.get("content")));
            }
            row["updated_at"] = json!(goformat::timestamp());
            replace_by_id(array_mut(&mut doc, "quickMessages"), qid, &row);
            save(root, &doc)?;
            Ok(row)
        }
        "quick_messages_delete" => {
            let qid = num(args, "id");
            array_mut(&mut doc, "quickMessages").retain(|m| number(m.get("id")) != qid);
            save(root, &doc)?;
            Ok(Value::Null)
        }
        "quick_messages_reorder" => {
            for (index, qid) in ids(args.get("ids")).into_iter().enumerate() {
                if let Some(mut row) = find_by_id(rows(&doc, "quickMessages"), qid) {
                    row["sort_order"] = json!(index);
                    replace_by_id(array_mut(&mut doc, "quickMessages"), qid, &row);
                }
            }
            save(root, &doc)?;
            Ok(Value::Null)
        }
        _ => Err(format!("Go 移植尚未实现此接口：{command}")),
    }
}

/// p1.go `flattenAgentConfig`: copy agent_type / agent_options / label_snapshot
/// from a draft/patch's agent_config onto the row.
fn flatten_agent_config(row: &mut Value, src: &Value) {
    if let Some(cfg) = src.get("agent_config").filter(|v| v.is_object()) {
        row["agent_type"] = cfg.get("agent_type").cloned().unwrap_or(Value::Null);
        row["agent_options"] = cfg.get("agent_options").cloned().unwrap_or(Value::Null);
        row["label_snapshot"] = cfg.get("label_snapshot").cloned().unwrap_or(Value::Null);
    }
}

// ── commit-message settings (git_settings.go) ───────────────────────────────

fn commit_settings(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let mut doc = load(root)?;
    let key = format!("commit-message:{}", num(args, "folderId"));
    match command {
        "set_commit_message_settings" => {
            let Some(settings) = args.get("settings").filter(|v| v.is_object()) else {
                return Err("提交设置必须是对象".into());
            };
            ensure_object(&mut doc, "settings").insert(key.clone(), settings.clone());
            save(root, &doc)?;
            Ok(settings.clone())
        }
        "delete_commit_message_settings" => {
            ensure_object(&mut doc, "settings").remove(key.as_str());
            save(root, &doc)?;
            Ok(Value::Null)
        }
        _ => Err("未知提交设置".into()),
    }
}

// ── canvas (canvas.go) ──────────────────────────────────────────────────────

/// Go's `canvasGeometry`: clamp geometry per key; non-finite input rejected
/// (unreachable over JSON, which cannot carry NaN/Inf).
fn canvas_geometry(key: &str, value: Option<&Value>) -> Result<f64, String> {
    let n = gofloat(value);
    if !n.is_finite() {
        return Err("画布坐标必须是有限数值".into());
    }
    if key == "width" || key == "height" {
        return Ok(n.clamp(48.0, 20000.0));
    }
    Ok(n.clamp(-1_000_000.0, 1_000_000.0))
}

/// canvas.go `makeCanvasNodeLocked`.
fn make_canvas_node(doc: &mut Value, input: &Value) -> Result<Value, String> {
    let board = num(input, "boardId");
    if find_by_id(rows(doc, "canvasBoards"), board).is_none() {
        return Err("画布不存在".into());
    }
    let kind = txt(input, "kind");
    if !region_kind(&kind)
        && kind != "conversation"
        && kind != "note"
        && kind != "file"
        && kind != "terminal"
    {
        return Err("不支持的画布节点类型".into());
    }
    let now = goformat::timestamp();
    let mut node = json!({
        "board_id": board,
        "kind": kind,
        "folder_id": Value::Null,
        "folder_group_id": Value::Null,
        "agent_type": Value::Null,
        "conversation_id": Value::Null,
        "path": Value::Null,
        "member_ids": [],
        "title": nullable(input.get("title")),
        "content": nullable(input.get("content")),
        "color": nullable(input.get("color")),
        "collapsed": false,
        "grid_columns": 0,
        "grid_rows": 0,
        "created_at": now,
        "updated_at": now,
    });
    const BINDINGS: [(&str, &str); 5] = [
        ("folderId", "folder_id"),
        ("folderGroupId", "folder_group_id"),
        ("agentType", "agent_type"),
        ("conversationId", "conversation_id"),
        ("path", "path"),
    ];
    let binding = match kind.as_str() {
        "folder" => Some("folderId"),
        "group" => Some("folderGroupId"),
        "agent" => Some("agentType"),
        "conversation" => Some("conversationId"),
        "file" | "terminal" => Some("path"),
        _ => None,
    };
    if let Some(binding_key) = binding {
        let value = input.get(binding_key).cloned().unwrap_or(Value::Null);
        let is_text_binding = binding_key == "agentType" || binding_key == "path";
        let missing = if is_text_binding {
            text(Some(&value)).is_empty()
        } else {
            number(Some(&value)) <= 0
        };
        if missing {
            return Err(format!("{kind} 节点需要 {binding_key}"));
        }
        let wire = BINDINGS
            .iter()
            .find(|(key, _)| *key == binding_key)
            .map(|(_, wire)| *wire)
            .expect("binding 表固定");
        node[wire] = value;
    }
    for (key, wire) in BINDINGS {
        if Some(key) != binding && input.get(key).map(|v| !v.is_null()).unwrap_or(false) {
            return Err(format!("{kind} 节点不能绑定 {wire}"));
        }
    }
    for key in ["x", "y", "width", "height"] {
        // Go checks `a[key] == nil`, so both a missing key and an explicit
        // null are "缺少".
        let raw = match input.get(key) {
            None | Some(Value::Null) => return Err(format!("缺少节点几何字段 {key}")),
            Some(raw) => raw,
        };
        node[key] = json!(canvas_geometry(key, Some(raw))?);
    }
    if region_kind(&kind) {
        node["grid_columns"] = json!(num(input, "gridColumns").clamp(0, 12));
        node["grid_rows"] = json!(num(input, "gridRows").clamp(0, 12));
    }
    node["id"] = json!(allocate(doc));
    Ok(node)
}

fn canvas_command(
    root: &Path,
    command: &str,
    args: &Value,
    events: &mut Vec<(String, Value)>,
    forward: &mut Vec<(String, Value)>,
) -> Result<Value, String> {
    let mut doc = load(root)?;
    let input = args.get("input").cloned().unwrap_or(json!({}));

    let board_event = |events: &mut Vec<(String, Value)>, kind: &str, board: &Value| {
        let payload = if kind == "deleted" {
            json!({"kind": kind, "id": board.get("id").cloned().unwrap_or(Value::Null)})
        } else {
            json!({"kind": kind, "board": board})
        };
        events.push(("canvas-board://changed".to_string(), payload));
    };

    match command {
        "canvas_list_boards" => {
            let mut result = Vec::new();
            for board in rows(&doc, "canvasBoards") {
                let mut preview: Vec<Value> = Vec::new();
                let mut terminal_count = 0i64;
                for node in rows(&doc, "canvasNodes") {
                    if number(node.get("board_id")) != number(board.get("id")) {
                        continue;
                    }
                    preview.push(json!({
                        "kind": node.get("kind").cloned().unwrap_or(Value::Null),
                        "x": node.get("x").cloned().unwrap_or(Value::Null),
                        "y": node.get("y").cloned().unwrap_or(Value::Null),
                        "width": node.get("width").cloned().unwrap_or(Value::Null),
                        "height": node.get("height").cloned().unwrap_or(Value::Null),
                        "color": node.get("color").cloned().unwrap_or(Value::Null),
                    }));
                    if text(node.get("kind")) == "terminal" {
                        terminal_count += 1;
                    }
                }
                let count = preview.len();
                // Largest area first, stable (Go sort.SliceStable).
                preview.sort_by(|a, b| {
                    let area = |v: &Value| gofloat(v.get("width")) * gofloat(v.get("height"));
                    area(b)
                        .partial_cmp(&area(a))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                preview.truncate(32);
                result.push(json!({
                    "board": board,
                    "node_count": count,
                    "terminal_count": terminal_count,
                    "preview": preview,
                }));
            }
            result.sort_by(|a, b| {
                let updated = |v: &Value| text(v.get("board").and_then(|b| b.get("updated_at")));
                updated(b).cmp(&updated(a))
            });
            Ok(Value::Array(result))
        }
        "canvas_create_board" => {
            let now = goformat::timestamp();
            let board = json!({
                "id": allocate(&mut doc),
                "name": nullable(input.get("name")),
                "description": nullable(input.get("description")),
                "color": nullable(input.get("color")),
                "created_at": now,
                "updated_at": now,
            });
            array_mut(&mut doc, "canvasBoards").push(board.clone());
            save(root, &doc)?;
            board_event(events, "upsert", &board);
            Ok(board)
        }
        "canvas_update_board" => {
            let bid = num(args, "boardId");
            let Some(mut board) = find_by_id(rows(&doc, "canvasBoards"), bid) else {
                return Err("画布不存在".into());
            };
            let patch = args.get("patch").cloned().unwrap_or(json!({}));
            for key in ["name", "description", "color"] {
                if let Some(value) = patch.get(key) {
                    board[key] = nullable(Some(value));
                }
            }
            board["updated_at"] = json!(goformat::timestamp());
            replace_by_id(array_mut(&mut doc, "canvasBoards"), bid, &board);
            save(root, &doc)?;
            board_event(events, "upsert", &board);
            Ok(board)
        }
        "canvas_list_nodes" => {
            let board = num(args, "boardId");
            if find_by_id(rows(&doc, "canvasBoards"), board).is_none() {
                return Err("画布不存在".into());
            }
            let nodes: Vec<Value> = rows(&doc, "canvasNodes")
                .iter()
                .filter(|n| number(n.get("board_id")) == board)
                .cloned()
                .collect();
            Ok(json!({
                "board_id": board,
                "nodes": nodes,
                "revision": number(doc.get("canvasRevision")),
            }))
        }
        "canvas_create_node" => {
            let mut input = input.clone();
            let kind = text(input.get("kind"));
            if kind == "file" || kind == "terminal" {
                let resolved = git::resolve(root, &text(input.get("path")))?;
                input["path"] = json!(resolved.to_string_lossy());
            }
            let node = make_canvas_node(&mut doc, &input)?;
            let board_id = number(node.get("board_id"));
            array_mut(&mut doc, "canvasNodes").push(node.clone());
            touch_board(&mut doc, board_id);
            let response = commit_canvas(
                root,
                &mut doc,
                events,
                json!({"kind": "upsert", "node": node.clone()}),
                &node,
            )?;
            Ok(response)
        }
        "canvas_update_node" => {
            let nid = num(args, "nodeId");
            let Some(mut node) = find_by_id(rows(&doc, "canvasNodes"), nid) else {
                return Err("画布节点不存在".into());
            };
            let patch = args.get("patch").cloned().unwrap_or(json!({}));
            for key in ["title", "content", "color"] {
                if let Some(value) = patch.get(key) {
                    node[key] = nullable(Some(value));
                }
            }
            for key in ["x", "y", "width", "height"] {
                if let Some(value) = patch.get(key) {
                    node[key] = json!(canvas_geometry(key, Some(value))?);
                }
            }
            if let Some(value) = patch.get("collapsed") {
                if !value.is_boolean() {
                    return Err("collapsed 必须为布尔值".into());
                }
                node["collapsed"] = value.clone();
            }
            for (key, wire) in [("gridColumns", "grid_columns"), ("gridRows", "grid_rows")] {
                if let Some(value) = patch.get(key) {
                    if !region_kind(&text(node.get("kind"))) {
                        return Err("只有区域支持网格设置".into());
                    }
                    node[wire] = json!(number(Some(value)).clamp(0, 12));
                }
            }
            let mut members = ids(node.get("member_ids"));
            for (key, add) in [("memberAdd", true), ("memberRemove", false)] {
                if let Some(value) = patch.get(key) {
                    if text(node.get("kind")) != "custom" {
                        return Err("只有自定义区域可修改成员".into());
                    }
                    let id = number(Some(value));
                    if add {
                        if !conversation_exists(&doc, id) {
                            return Err("成员会话不存在".into());
                        }
                        if !has_id(&members, id) {
                            members.push(id);
                        }
                    } else {
                        members.retain(|m| *m != id);
                    }
                }
            }
            if members.len() > 200 {
                return Err("区域最多包含 200 个会话".into());
            }
            node["member_ids"] = json!(members);
            node["updated_at"] = json!(goformat::timestamp());
            let board_id = number(node.get("board_id"));
            touch_board(&mut doc, board_id);
            replace_by_id(array_mut(&mut doc, "canvasNodes"), nid, &node);
            let response = commit_canvas(
                root,
                &mut doc,
                events,
                json!({"kind": "upsert", "node": node.clone()}),
                &node,
            )?;
            Ok(response)
        }
        "canvas_move_nodes" => {
            let moves = args
                .get("moves")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut applied = Vec::new();
            for raw in &moves {
                let Some(move_row) = raw.as_object() else {
                    continue;
                };
                let move_row = Value::Object(move_row.clone());
                let nid = num(&move_row, "id");
                let Some(mut node) = find_by_id(rows(&doc, "canvasNodes"), nid) else {
                    continue;
                };
                let x = canvas_geometry("x", move_row.get("x"))?;
                let y = canvas_geometry("y", move_row.get("y"))?;
                node["x"] = json!(x);
                node["y"] = json!(y);
                node["updated_at"] = json!(goformat::timestamp());
                touch_board(&mut doc, number(node.get("board_id")));
                replace_by_id(array_mut(&mut doc, "canvasNodes"), nid, &node);
                applied.push(json!({"id": node["id"], "x": x, "y": y}));
            }
            if applied.is_empty() {
                return Ok(json!({
                    "value": applied,
                    "revision": number(doc.get("canvasRevision")),
                }));
            }
            let response = commit_canvas(
                root,
                &mut doc,
                events,
                json!({"kind": "moved", "moves": applied.clone()}),
                &Value::Array(applied.clone()),
            )?;
            Ok(response)
        }
        "canvas_delete_node" | "canvas_delete_nodes" | "canvas_delete_board" => {
            let mut remove = ids(args.get("nodeIds"));
            if command == "canvas_delete_node" {
                remove = vec![num(args, "nodeId")];
            }
            if command == "canvas_delete_board" {
                let board = num(args, "boardId");
                for node in rows(&doc, "canvasNodes") {
                    if number(node.get("board_id")) == board {
                        remove.push(number(node.get("id")));
                    }
                }
                array_mut(&mut doc, "canvasBoards").retain(|b| number(b.get("id")) != board);
            }
            let deleted = prune_nodes(&mut doc, &remove);
            let (event, value): (Value, Value) = if command == "canvas_delete_node" {
                (
                    json!({"kind": "deleted", "id": num(args, "nodeId")}),
                    Value::Null,
                )
            } else {
                (
                    json!({"kind": "pruned", "deleted_ids": deleted, "updated": []}),
                    json!(deleted.clone()),
                )
            };
            let response = commit_canvas(root, &mut doc, events, event, &value)?;
            for id in &deleted {
                forward.push((
                    "terminal_kill".to_string(),
                    json!({"terminalId": format!("canvas-term-{id}")}),
                ));
            }
            if command == "canvas_delete_board" {
                board_event(events, "deleted", &json!({"id": num(args, "boardId")}));
            }
            Ok(response)
        }
        "canvas_group_into_region" => {
            let board = num(&input, "boardId");
            let target = num(&input, "targetRegionId");
            let mut region = find_by_id(rows(&doc, "canvasNodes"), target);
            if target > 0 {
                let valid = region
                    .as_ref()
                    .map(|n| text(n.get("kind")) == "custom" && number(n.get("board_id")) == board)
                    .unwrap_or(false);
                if !valid {
                    return Err("目标必须是当前画布的自定义区域".into());
                }
            }
            let mut members: Vec<i64> = region
                .as_ref()
                .map(|n| ids(n.get("member_ids")))
                .unwrap_or_default();
            for id in ids(input.get("memberIds")) {
                if !conversation_exists(&doc, id) {
                    return Err("成员会话不存在".into());
                }
                if !has_id(&members, id) {
                    members.push(id);
                }
            }
            if members.len() > 200 {
                return Err("区域最多包含 200 个会话".into());
            }
            if region.is_none() {
                let mut copy = input.clone();
                copy["kind"] = json!("custom");
                if let Some(obj) = copy.as_object_mut() {
                    obj.remove("targetRegionId");
                }
                let created = make_canvas_node(&mut doc, &copy)?;
                array_mut(&mut doc, "canvasNodes").push(created.clone());
                region = Some(created);
            }
            let region_id = number(region.as_ref().and_then(|n| n.get("id")));
            let mut region = region.expect("上面保证存在或已创建");
            region["member_ids"] = json!(members.clone());
            region["updated_at"] = json!(goformat::timestamp());
            let mut remove = Vec::new();
            for id in ids(input.get("consumeNodeIds")) {
                let pin = find_by_id(rows(&doc, "canvasNodes"), id);
                let consumed = pin
                    .as_ref()
                    .map(|pin| {
                        text(pin.get("kind")) == "conversation"
                            && number(pin.get("board_id")) == board
                            && has_id(&members, number(pin.get("conversation_id")))
                    })
                    .unwrap_or(false);
                if consumed {
                    remove.push(id);
                }
            }
            let deleted = prune_nodes(&mut doc, &remove);
            touch_board(&mut doc, board);
            replace_by_id(array_mut(&mut doc, "canvasNodes"), region_id, &region);
            let response = commit_canvas(
                root,
                &mut doc,
                events,
                json!({"kind": "grouped", "node": region.clone(), "deleted_ids": deleted}),
                &json!({"node": region, "deletedIds": deleted}),
            )?;
            Ok(response)
        }
        "canvas_detach_member" => {
            let region_id = num(args, "regionId");
            let conversation = num(args, "conversationId");
            let Some(mut region) = find_by_id(rows(&doc, "canvasNodes"), region_id) else {
                return Err("区域或成员会话不存在".into());
            };
            if !region_kind(&text(region.get("kind"))) || !conversation_exists(&doc, conversation) {
                return Err("区域或成员会话不存在".into());
            }
            let mut removed = Value::Null;
            if text(region.get("kind")) == "custom" {
                let members = ids(region.get("member_ids"));
                if !has_id(&members, conversation) {
                    return Err("成员已移出区域".into());
                }
                let kept: Vec<i64> = members.into_iter().filter(|m| *m != conversation).collect();
                region["member_ids"] = json!(kept);
                removed = region["id"].clone();
                replace_by_id(array_mut(&mut doc, "canvasNodes"), region_id, &region);
            }
            let node = make_canvas_node(
                &mut doc,
                &json!({
                    "boardId": region.get("board_id").cloned().unwrap_or(Value::Null),
                    "kind": "conversation",
                    "conversationId": conversation,
                    "x": args.get("x").cloned().unwrap_or(Value::Null),
                    "y": args.get("y").cloned().unwrap_or(Value::Null),
                    "width": 320,
                    "height": 220,
                }),
            )?;
            let board_id = number(node.get("board_id"));
            array_mut(&mut doc, "canvasNodes").push(node.clone());
            touch_board(&mut doc, board_id);
            let response = commit_canvas(
                root,
                &mut doc,
                events,
                json!({"kind": "detached", "removed_from": removed, "node": node.clone()}),
                &node,
            )?;
            Ok(response)
        }
        _ => Err(format!("未知画布接口：{command}")),
    }
}

fn touch_board(doc: &mut Value, board_id: i64) {
    if let Some(board) = array_mut(doc, "canvasBoards")
        .iter_mut()
        .find(|b| number(b.get("id")) == board_id)
    {
        board["updated_at"] = json!(goformat::timestamp());
    }
}

/// canvas.go `prune`: drop the listed nodes, touch each one's board, return
/// the ids actually removed (in node order).
fn prune_nodes(doc: &mut Value, remove: &[i64]) -> Vec<i64> {
    let mut deleted = Vec::new();
    let mut boards_touched = Vec::new();
    let mut kept = Vec::new();
    for node in array_mut(doc, "canvasNodes").drain(..) {
        let id = number(node.get("id"));
        if remove.contains(&id) {
            deleted.push(id);
            boards_touched.push(number(node.get("board_id")));
        } else {
            kept.push(node);
        }
    }
    *array_mut(doc, "canvasNodes") = kept;
    for board_id in boards_touched {
        touch_board(doc, board_id);
    }
    deleted
}

/// canvas.go `commit`: bump the revision, stamp the event, persist, emit.
fn commit_canvas(
    root: &Path,
    doc: &mut Value,
    events: &mut Vec<(String, Value)>,
    mut event: Value,
    value: &Value,
) -> Result<Value, String> {
    let revision = number(doc.get("canvasRevision")) + 1;
    doc["canvasRevision"] = json!(revision);
    event["revision"] = json!(revision);
    save(root, doc)?;
    events.push(("canvas://changed".to_string(), event));
    Ok(json!({"value": value, "revision": revision}))
}

/// Replaces the row with the given id inside `rows`, keeping position.
fn replace_by_id(rows: &mut Vec<Value>, id: i64, row: &Value) {
    for existing in rows.iter_mut() {
        if number(existing.get("id")) == id {
            *existing = row.clone();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-store-writes-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn seed(root: &Path, extra: Value) {
        let mut store = json!({
            "next": 10,
            "folders": [
                {"id": 1, "path": "E:/ws/a", "name": "a", "kind": "regular", "open": true, "group_id": 1, "sort_order": 1, "color": "default", "alias": Value::Null, "last_opened_at": "2026-11-13T00:00:00Z"},
                {"id": 2, "path": "E:/ws/b", "name": "b", "kind": "chat", "open": true, "sort_order": 0, "color": "default"},
                {"id": 3, "path": "E:/ws/c", "name": "c", "kind": "regular", "open": false, "sort_order": 2, "color": "default"}
            ],
            "conversations": [
                {"id": 4, "title": "Alpha", "folder_id": 1, "status": "completed", "agent_type": "claude", "message_count": 3, "parent_id": Value::Null, "child_count": 1, "kind": "regular", "tag_ids": [7, 8]},
                {"id": 5, "title": "Beta", "folder_id": 1, "status": "running", "agent_type": "codex", "message_count": 5, "parent_id": 4, "child_count": 0, "kind": "delegate", "tag_ids": [8]}
            ],
            "settings": {"theme": "dark"},
            "tabs": [{"id": "t1", "conversation_id": 4}],
            "tabsVersion": 4,
            "canvasBoards": [{"id": 2, "name": "board", "description": Value::Null, "color": Value::Null, "created_at": "2026-11-13T00:00:00Z", "updated_at": "2026-11-13T00:00:00Z"}],
            "canvasNodes": [],
            "canvasRevision": 5,
            "folderGroups": [{"id": 1, "name": "g", "color": "default", "sort_order": 0}],
            "conversationTags": [
                {"id": 7, "name": "t7", "color": "", "sort_order": 0},
                {"id": 8, "name": "t8", "color": "", "sort_order": 1}
            ],
            "assistants": [],
            "quickMessages": []
        });
        if let Some(patch) = extra.as_object() {
            for (key, value) in patch {
                store[key.as_str()] = value.clone();
            }
        }
        fs::write(
            root.join(".data").join(FILE),
            serde_json::to_vec_pretty(&store).unwrap(),
        )
        .unwrap();
    }

    fn call(
        root: &Path,
        command: &str,
        args: Value,
    ) -> (Result<Value, String>, Vec<(String, Value)>) {
        let outcome = handle(root, command, &args).expect("命令应被接管");
        (outcome.result, outcome.events)
    }

    fn ok(root: &Path, command: &str, args: Value) -> Value {
        call(root, command, args).0.unwrap()
    }

    fn err(root: &Path, command: &str, args: Value) -> String {
        call(root, command, args).0.unwrap_err()
    }

    #[test]
    fn folder_groups_crud_roundtrip() {
        let root = temp_root("groups");
        seed(&root, json!({}));
        let group = ok(
            &root,
            "create_folder_group",
            json!({"name": "新组", "color": "red"}),
        );
        assert_eq!(group["id"], 10);
        assert_eq!(group["sort_order"], 1);

        let updated = ok(
            &root,
            "update_folder_group",
            json!({"groupId": 10, "color": "blue"}),
        );
        assert_eq!(updated["name"], "新组");
        assert_eq!(updated["color"], "blue");
        assert_eq!(
            err(&root, "update_folder_group", json!({"groupId": 99})),
            "分组不存在"
        );

        assert_eq!(
            ok(&root, "delete_folder_group", json!({"groupId": 10})),
            Value::Null
        );
        // 删除分组时，属于它的 folder 的 group_id 置空。
        assert_eq!(
            ok(
                &root,
                "set_folder_group",
                json!({"folderId": 1, "groupId": Value::Null})
            ),
            Value::Null
        );
        assert_eq!(
            err(
                &root,
                "set_folder_group",
                json!({"folderId": 999, "groupId": 1})
            ),
            "项目不存在"
        );
    }

    #[test]
    fn branch_tag_setting_is_written() {
        let root = temp_root("branch");
        seed(&root, json!({}));
        let out = ok(
            &root,
            "update_conversation_branch_tag",
            json!({"enabled": true, "color": "blue"}),
        );
        assert_eq!(out, json!({"color": "blue", "enabled": true}));
        let out = ok(
            &root,
            "update_conversation_branch_tag",
            json!({"enabled": false}),
        );
        assert_eq!(out, json!({"color": "default", "enabled": false}));
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(
            doc["settings"]["conversation_branch_tag"]["color"],
            "default"
        );
    }

    #[test]
    fn tags_delta_merge_and_delete_cascade() {
        let root = temp_root("tags");
        seed(&root, json!({}));
        let (out, events) = call(
            &root,
            "update_conversation_tags",
            json!({"conversationId": 4, "add": [9], "remove": [7]}),
        );
        let row = out.unwrap();
        assert_eq!(row["tag_ids"], json!([8, 9]));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "conversation://changed");
        assert_eq!(events[0].1["kind"], "upsert");
        assert_eq!(events[0].1["summary"]["id"], 4);

        // 会话 5 也带着 8：删除标签时所有会话同步摘除。
        assert_eq!(
            ok(&root, "delete_conversation_tag", json!({"tagId": 8})),
            Value::Null
        );
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(doc["conversations"][0]["tag_ids"], json!([9]));
        assert_eq!(doc["conversations"][1]["tag_ids"], json!([]));
        assert_eq!(doc["conversationTags"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn assistant_lifecycle_matches_p1() {
        let root = temp_root("assistant");
        seed(&root, json!({}));
        let created = ok(
            &root,
            "assistant_create",
            json!({"draft": {"name": " reviewer ", "agent_config": {"agent_type": "claude", "agent_options": {"m": 1}, "label_snapshot": "L"}}}),
        );
        assert_eq!(created["id"], 10);
        assert_eq!(created["revision"], 1);
        assert_eq!(created["agent_type"], "claude");
        assert_eq!(created["label_snapshot"], "L");
        assert_eq!(created["uid"].as_str().unwrap().len(), 32);

        let updated = ok(
            &root,
            "assistant_update",
            json!({"id": 10, "patch": {"enabled": true}}),
        );
        assert_eq!(updated["revision"], 2);
        assert_eq!(updated["enabled"], true);

        let deleted = ok(&root, "assistant_delete", json!({"id": 10}));
        assert!(deleted["deleted_at"].as_str().is_some(), "墓碑保留");
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(doc["assistants"].as_array().unwrap().len(), 1);

        // reorder 只返回位置变化过的行。
        seed(
            &root,
            json!({"assistants": [
                {"id": 1, "name": "a", "sort_order": 0, "revision": 1},
                {"id": 2, "name": "b", "sort_order": 1, "revision": 1}
            ]}),
        );
        let changed = ok(&root, "assistant_reorder", json!({"ids": [2, 1]}));
        assert_eq!(changed.as_array().unwrap().len(), 2);
        let changed = ok(&root, "assistant_reorder", json!({"ids": [2, 1]}));
        assert_eq!(changed.as_array().unwrap().len(), 0, "幂等时无行返回");
        assert_eq!(
            err(&root, "assistant_update", json!({"id": 42, "patch": {}})),
            "助手不存在"
        );
    }

    #[test]
    fn quick_messages_lifecycle() {
        let root = temp_root("quick");
        seed(&root, json!({}));
        let created = ok(
            &root,
            "quick_messages_create",
            json!({"title": "t", "content": "c"}),
        );
        assert_eq!(created["id"], 10);
        // 显式 null 不更新（Go 判断的是 != nil，不是键存在）。
        let updated = ok(
            &root,
            "quick_messages_update",
            json!({"id": 10, "title": Value::Null, "content": "new"}),
        );
        assert_eq!(updated["title"], "t");
        assert_eq!(updated["content"], "new");
        assert_eq!(
            err(&root, "quick_messages_update", json!({"id": 99})),
            "快捷消息不存在"
        );
        assert_eq!(
            ok(&root, "quick_messages_delete", json!({"id": 10})),
            Value::Null
        );
    }

    #[test]
    fn system_settings_merge_is_wholesale() {
        let root = temp_root("settings");
        seed(&root, json!({}));
        ok(
            &root,
            "update_system_settings",
            json!({"theme": "light", "extra": {"a": 1}}),
        );
        ok(&root, "save_system_settings", json!({"theme": "dark"}));
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(doc["settings"]["theme"], "dark");
        assert_eq!(doc["settings"]["extra"]["a"], 1, "合并语义：不整体替换");
    }

    #[test]
    fn save_opened_tabs_honors_expected_version() {
        let root = temp_root("tabs");
        seed(&root, json!({}));
        let rejected = ok(
            &root,
            "save_opened_tabs",
            json!({"expectedVersion": 3, "items": [{"id": "x"}]}),
        );
        assert_eq!(rejected["accepted"], false);
        assert_eq!(rejected["version"], 4);

        let accepted = ok(
            &root,
            "save_opened_tabs",
            json!({"expectedVersion": 4, "items": [{"id": "t2", "conversation_id": 5}]}),
        );
        assert_eq!(accepted["accepted"], true);
        assert_eq!(accepted["version"], 5);
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(doc["tabsVersion"], 5);
        assert_eq!(doc["tabs"][0]["id"], "t2");

        // 标量数组：Go 的 unmarshal 静默失败，旧行保留但版本照走。
        let scalar = ok(
            &root,
            "save_opened_tabs",
            json!({"expectedVersion": 5, "items": [1, 2]}),
        );
        assert_eq!(scalar["accepted"], true);
        assert_eq!(scalar["version"], 6);
        assert_eq!(scalar["tabs"][0]["id"], "t2", "标量数组不替换行");
    }

    #[test]
    fn create_conversation_bumps_next_and_emits() {
        let root = temp_root("create-conv");
        seed(&root, json!({}));
        let (result, events) = call(
            &root,
            "create_conversation",
            json!({"folderId": 1, "agentType": "claude", "title": "新会话"}),
        );
        assert_eq!(result.unwrap(), json!(10));
        assert_eq!(events.len(), 1);
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(doc["next"], 11);
        let row = &doc["conversations"][2];
        assert_eq!(row["kind"], "regular");
        assert_eq!(row["status"], "pending");
        assert_eq!(row["tag_ids"], json!([]));

        assert_eq!(
            err(&root, "create_conversation", json!({"folderId": 99})),
            "项目不存在"
        );
    }

    #[test]
    fn conversation_updates_emit_changed() {
        let root = temp_root("conv-upd");
        seed(&root, json!({}));
        let (result, events) = call(
            &root,
            "update_conversation_pinned",
            json!({"conversationId": 4, "pinned": true}),
        );
        assert_eq!(result.unwrap(), Value::Null);
        assert_eq!(events.len(), 1);
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert!(doc["conversations"][0]["pinned_at"].as_str().is_some());

        ok(
            &root,
            "update_conversation_title",
            json!({"conversationId": 4, "title": "renamed"}),
        );
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(doc["conversations"][0]["title"], "renamed");
        assert_eq!(doc["conversations"][0]["title_locked"], true);

        assert_eq!(
            err(
                &root,
                "update_conversation_status",
                json!({"conversationId": 999})
            ),
            "会话不存在"
        );
    }

    #[test]
    fn canvas_node_creation_validates_bindings_and_geometry() {
        let root = temp_root("canvas-create");
        seed(&root, json!({}));
        let node = ok(
            &root,
            "canvas_create_node",
            json!({"input": {"boardId": 2, "kind": "note", "title": "  ", "x": 10, "y": -5.5, "width": 30, "height": 25000}}),
        );
        assert_eq!(node["value"]["id"], 10);
        assert_eq!(node["revision"], 6);
        assert_eq!(node["value"]["width"], 48.0, "宽度下限 48");
        assert_eq!(node["value"]["height"], 20000.0, "高度上限 20000");
        assert_eq!(node["value"]["title"], Value::Null, "空串归一为 null");
        assert_eq!(node["value"]["y"], -5.5);

        assert_eq!(
            err(
                &root,
                "canvas_create_node",
                json!({"input": {"boardId": 2, "kind": "note", "x": 1, "y": 1, "width": 100, "height": 100, "folderId": 1}})
            ),
            "note 节点不能绑定 folder_id"
        );
        assert_eq!(
            err(
                &root,
                "canvas_create_node",
                json!({"input": {"boardId": 2, "kind": "conversation", "x": 1, "y": 1, "width": 100, "height": 100}})
            ),
            "conversation 节点需要 conversationId"
        );
        assert_eq!(
            err(
                &root,
                "canvas_create_node",
                json!({"input": {"boardId": 99, "kind": "note", "x": 1, "y": 1, "width": 100, "height": 100}})
            ),
            "画布不存在"
        );
        assert_eq!(
            err(
                &root,
                "canvas_create_node",
                json!({"input": {"boardId": 2, "kind": "widget", "x": 1, "y": 1, "width": 100, "height": 100}})
            ),
            "不支持的画布节点类型"
        );
        assert_eq!(
            err(
                &root,
                "canvas_create_node",
                json!({"input": {"boardId": 2, "kind": "note", "x": 1, "y": 1, "width": 100}})
            ),
            "缺少节点几何字段 height"
        );
    }

    #[test]
    fn canvas_region_grid_and_members() {
        let root = temp_root("canvas-region");
        seed(&root, json!({}));
        // 自定义区域：网格值被钳制到 0..=12。
        let node = ok(
            &root,
            "canvas_create_node",
            json!({"input": {"boardId": 2, "kind": "custom", "title": "区域", "gridColumns": 99, "gridRows": -1, "x": 0, "y": 0, "width": 800, "height": 600}}),
        );
        let region_id = node["value"]["id"].as_i64().unwrap();
        assert_eq!(node["value"]["grid_columns"], 12);
        assert_eq!(node["value"]["grid_rows"], 0);

        // note 节点不是区域：网格与成员都拒绝。
        let note = ok(
            &root,
            "canvas_create_node",
            json!({"input": {"boardId": 2, "kind": "note", "x": 1, "y": 1, "width": 100, "height": 100}}),
        );
        let note_id = note["value"]["id"].as_i64().unwrap();
        assert_eq!(
            err(
                &root,
                "canvas_update_node",
                json!({"nodeId": note_id, "patch": {"gridColumns": 3}})
            ),
            "只有区域支持网格设置"
        );
        assert_eq!(
            err(
                &root,
                "canvas_update_node",
                json!({"nodeId": note_id, "patch": {"memberAdd": 4}})
            ),
            "只有自定义区域可修改成员"
        );
        // collapsed 必须是布尔。
        assert_eq!(
            err(
                &root,
                "canvas_update_node",
                json!({"nodeId": note_id, "patch": {"collapsed": "yes"}})
            ),
            "collapsed 必须为布尔值"
        );
        // 成员会话必须存在。
        assert_eq!(
            err(
                &root,
                "canvas_update_node",
                json!({"nodeId": region_id, "patch": {"memberAdd": 999}})
            ),
            "成员会话不存在"
        );
        // 区域内网格更新成功。
        let updated = ok(
            &root,
            "canvas_update_node",
            json!({"nodeId": region_id, "patch": {"gridColumns": 3}}),
        );
        assert_eq!(updated["value"]["grid_columns"], 3);
    }

    #[test]
    fn canvas_group_into_region_and_detach() {
        let root = temp_root("canvas-group");
        seed(&root, json!({}));
        seed(
            &root,
            json!({"canvasNodes": [
                {"id": 6, "board_id": 2, "kind": "conversation", "conversation_id": 4, "x": 1.0, "y": 1.0, "width": 320.0, "height": 220.0, "title": Value::Null, "content": Value::Null, "color": Value::Null, "collapsed": false, "grid_columns": 0, "grid_rows": 0, "member_ids": [], "folder_id": Value::Null, "folder_group_id": Value::Null, "agent_type": Value::Null, "path": Value::Null, "created_at": "t", "updated_at": "t"}
            ]}),
        );
        let (result, events) = call(
            &root,
            "canvas_group_into_region",
            json!({"input": {"boardId": 2, "memberIds": [4], "consumeNodeIds": [6], "title": "组", "x": 0, "y": 0, "width": 800, "height": 600}}),
        );
        let out = result.unwrap();
        let node = &out["value"]["node"];
        assert_eq!(node["kind"], "custom");
        assert_eq!(node["member_ids"], json!([4]));
        assert_eq!(out["value"]["deletedIds"], json!([6]));
        assert_eq!(out["revision"], 6);
        assert_eq!(events[0].0, "canvas://changed");
        assert_eq!(events[0].1["kind"], "grouped");

        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(doc["canvasNodes"].as_array().unwrap().len(), 1);

        // detach：把成员从区域拆出成独立会话节点。
        let region_id = doc["canvasNodes"][0]["id"].as_i64().unwrap();
        let (result, _) = call(
            &root,
            "canvas_detach_member",
            json!({"regionId": region_id, "conversationId": 4, "x": 50, "y": 60}),
        );
        let out = result.unwrap();
        assert_eq!(out["value"]["kind"], "conversation");
        assert_eq!(out["value"]["width"].as_f64(), Some(320.0));
        assert_eq!(out["value"]["conversation_id"], 4);

        assert_eq!(
            err(
                &root,
                "canvas_detach_member",
                json!({"regionId": region_id, "conversationId": 4, "x": 1, "y": 1})
            ),
            "成员已移出区域"
        );
    }

    #[test]
    fn canvas_delete_board_prunes_and_kills_terminals() {
        let root = temp_root("canvas-delete");
        seed(
            &root,
            json!({"canvasNodes": [
                {"id": 6, "board_id": 2, "kind": "terminal", "path": "p", "x": 0.0, "y": 0.0, "width": 320.0, "height": 220.0, "title": Value::Null, "content": Value::Null, "color": Value::Null, "collapsed": false, "grid_columns": 0, "grid_rows": 0, "member_ids": [], "folder_id": Value::Null, "folder_group_id": Value::Null, "agent_type": Value::Null, "conversation_id": Value::Null, "created_at": "t", "updated_at": "t"}
            ]}),
        );
        let outcome = handle(&root, "canvas_delete_board", &json!({"boardId": 2})).unwrap();
        let out = outcome.result.unwrap();
        assert_eq!(out["value"], json!([6]), "板删除连带节点");
        assert_eq!(
            outcome.events.len(),
            2,
            "canvas://changed + canvas-board://changed"
        );
        assert_eq!(outcome.events[1].0, "canvas-board://changed");
        assert_eq!(outcome.events[1].1["kind"], "deleted");
        // terminal_kill 转发由 mod.rs 拦截改道本地终端模块（G 组）——
        // store 侧只负责生成「删除节点 → 杀终端」这个语义对。
        assert_eq!(
            outcome.forward,
            vec![(
                "terminal_kill".to_string(),
                json!({"terminalId": "canvas-term-6"})
            )]
        );

        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(doc["canvasBoards"].as_array().unwrap().len(), 0);
        assert_eq!(doc["canvasNodes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn canvas_move_nodes_without_hits_skips_commit() {
        let root = temp_root("canvas-move");
        seed(&root, json!({}));
        let out = ok(
            &root,
            "canvas_move_nodes",
            json!({"moves": [{"id": 999, "x": 1, "y": 2}]}),
        );
        assert_eq!(out["value"], json!([]));
        assert_eq!(out["revision"], 5, "未提交，版本不变");
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(doc["canvasRevision"], 5);
    }

    #[test]
    fn canvas_list_boards_sorts_by_area_and_recency() {
        let root = temp_root("canvas-list");
        seed(
            &root,
            json!({
                "canvasNodes": [
                    {"id": 1, "board_id": 2, "kind": "terminal", "path": "p", "x": 0.0, "y": 0.0, "width": 100.0, "height": 100.0, "title": Value::Null, "content": Value::Null, "color": Value::Null, "collapsed": false, "grid_columns": 0, "grid_rows": 0, "member_ids": [], "folder_id": Value::Null, "folder_group_id": Value::Null, "agent_type": Value::Null, "conversation_id": Value::Null, "created_at": "t", "updated_at": "t"},
                    {"id": 2, "board_id": 2, "kind": "note", "path": Value::Null, "x": 0.0, "y": 0.0, "width": 500.0, "height": 400.0, "title": Value::Null, "content": Value::Null, "color": Value::Null, "collapsed": false, "grid_columns": 0, "grid_rows": 0, "member_ids": [], "folder_id": Value::Null, "folder_group_id": Value::Null, "agent_type": Value::Null, "conversation_id": Value::Null, "created_at": "t", "updated_at": "t"}
                ]
            }),
        );
        let boards = ok(&root, "canvas_list_boards", json!({}));
        let row = &boards.as_array().unwrap()[0];
        assert_eq!(row["node_count"], 2);
        assert_eq!(row["terminal_count"], 1);
        let preview = row["preview"].as_array().unwrap();
        assert_eq!(preview[0]["kind"], "note", "按面积降序");
        assert_eq!(preview[1]["kind"], "terminal");

        let nodes = ok(&root, "canvas_list_nodes", json!({"boardId": 2}));
        assert_eq!(nodes["revision"], 5);
        assert_eq!(nodes["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(
            err(&root, "canvas_list_nodes", json!({"boardId": 99})),
            "画布不存在"
        );
    }

    #[test]
    fn commit_message_settings_write_and_delete() {
        let root = temp_root("commit-settings");
        seed(&root, json!({}));
        let saved = ok(
            &root,
            "set_commit_message_settings",
            json!({"folderId": 3, "settings": {"agent": "claude", "instructions": "x", "auto_commit": true}}),
        );
        assert_eq!(saved["agent"], "claude");
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert_eq!(doc["settings"]["commit-message:3"]["auto_commit"], true);

        assert_eq!(
            ok(
                &root,
                "delete_commit_message_settings",
                json!({"folderId": 3})
            ),
            Value::Null
        );
        let doc = store_file::read_json(&store_path(&root)).unwrap().unwrap();
        assert!(doc["settings"].get("commit-message:3").is_none());
        assert_eq!(
            err(
                &root,
                "set_commit_message_settings",
                json!({"folderId": 3, "settings": "nope"})
            ),
            "提交设置必须是对象"
        );
    }

    #[test]
    fn store_file_stays_go_shaped_after_writes() {
        let root = temp_root("shape");
        seed(&root, json!({}));
        ok(
            &root,
            "create_folder_group",
            json!({"name": "g<b>", "color": ""}),
        );
        let raw = fs::read(root.join(".data").join(FILE)).unwrap();
        let text = String::from_utf8(raw).unwrap();
        // 紧凑 + HTML 转义 + 顶层字段序。
        assert!(!text.contains('\n'), "Go 落盘是紧凑 JSON：{text}");
        assert!(text.contains(r#""name":"g\u003cb\u003e""#), "{text}");
        let next_pos = text.find("\"next\":11").expect("next 递增");
        let folders_pos = text.find("\"folders\":").unwrap();
        let settings_pos = text.find("\"settings\":").unwrap();
        assert!(next_pos < folders_pos && folders_pos < settings_pos);
        // 整段仍是合法 JSON 且能被 Go 形状读取。
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["folderGroups"][0]["color"], "default");
    }

    #[test]
    fn only_write_commands_are_owned() {
        let root = temp_root("owns");
        assert!(handle(&root, "get_sidebar_data", &json!({})).is_none());
        assert!(handle(&root, "list_folder_groups", &json!({})).is_none());
        assert!(handle(&root, "get_conversation", &json!({})).is_none());
        assert!(handle(&root, "delete_conversation", &json!({})).is_none());
        assert!(handle(&root, "git_status", &json!({})).is_none());
    }
}
