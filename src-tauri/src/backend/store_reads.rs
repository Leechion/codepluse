//! Read-only commands served straight from `codeg-workspace.json`.
//!
//! Why reads can move before writes: the Go side flushes the whole store on
//! every mutation (`saveLocked`), so the file on disk is always the same state
//! Go holds in memory. A Rust reader that re-opens the file per call therefore
//! answers identically, while a Rust *writer* would be silently clobbered the
//! next time Go saves its own copy. Writes stay in Go until the whole
//! store-owning command set flips together.
//!
//! Ported here (20 aliases, all pure reads):
//!   get_sidebar_data, get_stats, list_open_folder_details, load_folder_history,
//!   list_all_folder_details, list_folders, get_folder, open_folder_by_id,
//!   list_remote_connections, get_system_settings, load_system_settings,
//!   list_all_conversations, list_conversations, list_child_conversations,
//!   list_opened_tabs, list_folder_groups, list_conversation_tags,
//!   get_conversation_branch_tag, assistant_list, quick_messages_list,
//!   get_commit_message_settings, get_main_bot_settings
//!
//! `get_folder_conversation` / `get_conversation` are NOT here: they join in the
//! SQLite turn store, which needs its own port.

use std::path::Path;

use serde_json::{json, Map, Value};

const FILE: &str = "codeg-workspace.json";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "get_sidebar_data"
            | "get_stats"
            | "list_open_folder_details"
            | "load_folder_history"
            | "list_all_folder_details"
            | "list_folders"
            | "get_folder"
            | "open_folder_by_id"
            | "list_remote_connections"
            | "get_system_settings"
            | "load_system_settings"
            | "list_all_conversations"
            | "list_conversations"
            | "list_child_conversations"
            | "list_opened_tabs"
            | "list_folder_groups"
            | "list_conversation_tags"
            | "get_conversation_branch_tag"
            | "assistant_list"
            | "quick_messages_list"
            | "get_commit_message_settings"
            | "get_main_bot_settings"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

// ── store access ────────────────────────────────────────────────────────────

fn load(root: &Path) -> Value {
    let path = root.join(".data").join(FILE);
    match std::fs::read(&path) {
        Ok(raw) => serde_json::from_slice(&raw).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    }
}

fn rows<'a>(data: &'a Value, key: &str) -> &'a [Value] {
    data.get(key)
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

fn number(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|u| u.min(i64::MAX as u64) as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

fn text(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn id_of(row: &Value, key: &str) -> i64 {
    number(row.get(key))
}

/// Go's `regularFoldersLocked`: `kind != "chat" && open != false`.
fn regular_folders(data: &Value) -> Vec<Value> {
    rows(data, "folders")
        .iter()
        .filter(|f| text(f, "kind") != "chat" && f.get("open") != Some(&Value::Bool(false)))
        .cloned()
        .collect()
}

/// Go's `statsLocked`. `by_agent` order is random there (map iteration); this
/// returns it sorted by `agent_type` so the response is reproducible. Callers
/// comparing against Go must order-insensitively match that array.
fn stats(data: &Value) -> Value {
    let conversations = rows(data, "conversations");
    let mut messages = 0i64;
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for row in conversations {
        messages += id_of(row, "message_count");
        *counts.entry(text(row, "agent_type")).or_insert(0) += 1;
    }
    let by_agent: Vec<Value> = counts
        .into_iter()
        .map(|(agent, count)| json!({"agent_type": agent, "conversation_count": count}))
        .collect();
    json!({
        "total_conversations": conversations.len(),
        "total_messages": messages,
        "by_agent": by_agent,
    })
}

/// `sort.SliceStable` over `sort_order`, then `id` when requested.
fn sorted_rows(mut list: Vec<Value>, tiebreak_id: bool) -> Vec<Value> {
    list.sort_by(|a, b| {
        let order = id_of(a, "sort_order").cmp(&id_of(b, "sort_order"));
        if order != std::cmp::Ordering::Equal || !tiebreak_id {
            return order;
        }
        id_of(a, "id").cmp(&id_of(b, "id"))
    });
    list
}

fn folder_by_id(data: &Value, id: i64) -> Option<Value> {
    rows(data, "folders")
        .iter()
        .find(|f| id_of(f, "id") == id)
        .cloned()
}

fn ids(args: &Value, key: &str) -> Vec<i64> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|list| list.iter().map(|v| number(Some(v))).collect())
        .unwrap_or_default()
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    // Constant answer, no store read at all.
    if command == "list_remote_connections" {
        return Ok(json!([]));
    }

    let data = load(root);
    match command {
        "get_sidebar_data" => Ok(json!({
            "folders": regular_folders(&data),
            "stats": stats(&data),
        })),
        "get_stats" => Ok(stats(&data)),
        "get_main_bot_settings" => Ok(data
            .get("settings")
            .and_then(|settings| settings.get("get_main_bot_settings"))
            .cloned()
            .unwrap_or(Value::Null)),
        "list_open_folder_details" | "load_folder_history" => {
            Ok(Value::Array(regular_folders(&data)))
        }
        "list_all_folder_details" => Ok(Value::Array(rows(&data, "folders").to_vec())),
        "list_folders" => Ok(Value::Array(
            rows(&data, "folders")
                .iter()
                .map(|f| {
                    json!({
                        "path": f.get("path").cloned().unwrap_or(Value::Null),
                        "name": f.get("name").cloned().unwrap_or(Value::Null),
                        "agent_types": [],
                        "conversation_count": 0,
                    })
                })
                .collect(),
        )),
        "get_folder" | "open_folder_by_id" => {
            folder_by_id(&data, id_of(args, "folderId")).ok_or_else(|| "项目不存在".to_string())
        }
        "get_system_settings" | "load_system_settings" => Ok(data
            .get("settings")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()))),
        "list_all_conversations" | "list_conversations" | "list_child_conversations" => {
            let folder_ids = ids(args, "folderIds");
            let status = args.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let agent = args.get("agentType").and_then(|v| v.as_str()).unwrap_or("");
            let folder_path = args
                .get("folderPath")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let search = args.get("search").and_then(|v| v.as_str()).unwrap_or("");
            let parent = id_of(args, "parentConversationId");
            let include_children = args.get("includeChildren") == Some(&Value::Bool(true));

            let mut out = Vec::new();
            for row in rows(&data, "conversations").iter().rev() {
                if !folder_ids.is_empty() && !folder_ids.contains(&id_of(row, "folder_id")) {
                    continue;
                }
                if !status.is_empty() && row.get("status").and_then(|v| v.as_str()) != Some(status)
                {
                    continue;
                }
                if !agent.is_empty()
                    && row.get("agent_type").and_then(|v| v.as_str()) != Some(agent)
                {
                    continue;
                }
                if !folder_path.is_empty() {
                    let folder = folder_by_id(&data, id_of(row, "folder_id"));
                    match folder {
                        Some(folder) if text(&folder, "path") == folder_path => {}
                        _ => continue,
                    }
                }
                if command == "list_child_conversations" && id_of(row, "parent_id") != parent {
                    continue;
                }
                if command == "list_all_conversations"
                    && id_of(row, "parent_id") > 0
                    && !include_children
                {
                    continue;
                }
                if !search.is_empty()
                    && !text(row, "title")
                        .to_lowercase()
                        .contains(&search.to_lowercase())
                {
                    continue;
                }
                out.push(row.clone());
            }
            Ok(Value::Array(out))
        }
        "list_opened_tabs" => Ok(json!({
            "items": data.get("tabs").cloned().unwrap_or_else(|| json!([])),
            "version": number(data.get("tabsVersion")),
        })),
        "list_folder_groups" => Ok(Value::Array(sorted_rows(
            rows(&data, "folderGroups").to_vec(),
            false,
        ))),
        "list_conversation_tags" => Ok(Value::Array(sorted_rows(
            rows(&data, "conversationTags").to_vec(),
            true,
        ))),
        "assistant_list" => Ok(Value::Array(sorted_rows(
            rows(&data, "assistants").to_vec(),
            true,
        ))),
        "quick_messages_list" => Ok(Value::Array(sorted_rows(
            rows(&data, "quickMessages").to_vec(),
            true,
        ))),
        "get_conversation_branch_tag" => Ok(data
            .get("settings")
            .and_then(|s| s.get("conversation_branch_tag"))
            .cloned()
            .unwrap_or_else(|| json!({"enabled": false, "color": "default"}))),
        "get_commit_message_settings" => {
            let folder = id_of(args, "folderId");
            let key = format!("commit-message:{folder}");
            if let Some(saved) = data.get("settings").and_then(|s| s.get(&key)) {
                return Ok(saved.clone());
            }
            if folder > 0 {
                return Ok(Value::Null);
            }
            Ok(json!({"agent": null, "instructions": "", "auto_commit": false}))
        }
        _ => Err(format!("Go 移植尚未实现此接口：{command}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-store-reads-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn seed(root: &Path) {
        let store = json!({
            "next": 10,
            "folders": [
                {"id": 1, "path": "E:/ws/a", "name": "a", "kind": "regular", "open": true, "group_id": 1, "sort_order": 1},
                {"id": 2, "path": "E:/ws/b", "name": "b", "kind": "chat", "open": true, "sort_order": 0},
                {"id": 3, "path": "E:/ws/c", "name": "c", "kind": "regular", "open": false, "sort_order": 2}
            ],
            "conversations": [
                {"id": 1, "title": "Alpha", "folder_id": 1, "status": "completed", "agent_type": "claude", "message_count": 3, "parent_id": 0},
                {"id": 2, "title": "Beta", "folder_id": 1, "status": "running", "agent_type": "codex", "message_count": 5, "parent_id": 1},
                {"id": 3, "title": "gamma", "folder_id": 2, "status": "running", "agent_type": "claude", "message_count": 2, "parent_id": 0}
            ],
            "settings": {"theme": "dark", "conversation_branch_tag": {"enabled": true, "color": "blue"}},
            "tabs": [{"id": "t1"}],
            "tabsVersion": 4,
            "folderGroups": [
                {"id": 2, "name": "second", "sort_order": 1},
                {"id": 1, "name": "first", "sort_order": 0}
            ],
            "conversationTags": [
                {"id": 3, "name": "t3", "sort_order": 1},
                {"id": 1, "name": "t1", "sort_order": 0},
                {"id": 2, "name": "t2", "sort_order": 0}
            ],
            "assistants": [{"id": 2, "name": "b", "sort_order": 0}, {"id": 1, "name": "a", "sort_order": 0}],
            "quickMessages": [{"id": 1, "title": "q", "sort_order": 0, "content": "c"}]
        });
        fs::write(
            root.join(".data").join(FILE),
            serde_json::to_vec_pretty(&store).unwrap(),
        )
        .unwrap();
    }

    fn call(root: &Path, command: &str, args: Value) -> Value {
        handle(root, command, &args).unwrap().unwrap()
    }

    #[test]
    fn sidebar_data_uses_regular_folders_and_stats() {
        let root = temp_root("sidebar");
        seed(&root);
        let out = call(&root, "get_sidebar_data", json!({}));
        let ids: Vec<i64> = out["folders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_i64().unwrap())
            .collect();
        // kind=chat 与 open=false 都被过滤
        assert_eq!(ids, vec![1]);
        assert_eq!(out["stats"]["total_conversations"], 3);
        assert_eq!(out["stats"]["total_messages"], 10);
        assert_eq!(out["stats"]["by_agent"][0]["agent_type"], "claude");
        assert_eq!(out["stats"]["by_agent"][0]["conversation_count"], 2);
    }

    #[test]
    fn folder_lookup_reports_missing() {
        let root = temp_root("folder");
        seed(&root);
        assert_eq!(
            call(&root, "get_folder", json!({"folderId": 2}))["name"],
            "b"
        );
        assert_eq!(
            handle(&root, "open_folder_by_id", &json!({"folderId": 99}))
                .unwrap()
                .unwrap_err(),
            "项目不存在"
        );
    }

    #[test]
    fn conversations_filter_and_order() {
        let root = temp_root("conv");
        seed(&root);
        let all = call(&root, "list_all_conversations", json!({}));
        let titles: Vec<&str> = all
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["title"].as_str().unwrap())
            .collect();
        // 逆序：只有 parent_id == 0 的顶级会话
        assert_eq!(titles, vec!["gamma", "Alpha"]);

        let with_children = call(
            &root,
            "list_all_conversations",
            json!({"includeChildren": true}),
        );
        assert_eq!(with_children.as_array().unwrap().len(), 3);

        let child = call(
            &root,
            "list_child_conversations",
            json!({"parentConversationId": 1}),
        );
        assert_eq!(child.as_array().unwrap().len(), 1);
        assert_eq!(child[0]["title"], "Beta");

        let by_folder = call(&root, "list_conversations", json!({"folderIds": [2]}));
        assert_eq!(by_folder.as_array().unwrap().len(), 1);

        let by_path = call(
            &root,
            "list_conversations",
            json!({"folderPath": "E:/ws/a"}),
        );
        assert_eq!(by_path.as_array().unwrap().len(), 2);

        let by_search = call(&root, "list_conversations", json!({"search": "ALP"}));
        assert_eq!(by_search.as_array().unwrap().len(), 1);

        let by_status = call(&root, "list_conversations", json!({"status": "running"}));
        assert_eq!(by_status.as_array().unwrap().len(), 2);

        let by_agent = call(&root, "list_conversations", json!({"agentType": "codex"}));
        assert_eq!(by_agent.as_array().unwrap().len(), 1);
    }

    #[test]
    fn sorted_lists_follow_sort_order_then_id() {
        let root = temp_root("sorted");
        seed(&root);
        let groups: Vec<i64> = call(&root, "list_folder_groups", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["id"].as_i64().unwrap())
            .collect();
        assert_eq!(groups, vec![1, 2]);
        let tags: Vec<i64> = call(&root, "list_conversation_tags", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_i64().unwrap())
            .collect();
        assert_eq!(tags, vec![1, 2, 3]);
        let assistants: Vec<i64> = call(&root, "assistant_list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_i64().unwrap())
            .collect();
        assert_eq!(assistants, vec![1, 2]);
        assert_eq!(
            call(&root, "quick_messages_list", json!({}))[0]["title"],
            "q"
        );
    }

    #[test]
    fn settings_and_tabs_read_through() {
        let root = temp_root("settings");
        seed(&root);
        assert_eq!(
            call(&root, "get_system_settings", json!({}))["theme"],
            "dark"
        );
        assert_eq!(
            call(&root, "get_conversation_branch_tag", json!({}))["color"],
            "blue"
        );
        let tabs = call(&root, "list_opened_tabs", json!({}));
        assert_eq!(tabs["version"], 4);
        assert_eq!(tabs["items"][0]["id"], "t1");

        // 未保存过的键：folder<=0 返回默认行，folder>0 返回 null。
        assert_eq!(
            call(&root, "get_commit_message_settings", json!({})),
            json!({"agent": null, "instructions": "", "auto_commit": false})
        );
        assert_eq!(
            call(&root, "get_commit_message_settings", json!({"folderId": 3})),
            Value::Null
        );
    }

    #[test]
    fn missing_store_degrades_to_empty_answers() {
        let root = temp_root("missing");
        let out = call(&root, "get_sidebar_data", json!({}));
        assert_eq!(out["folders"], json!([]));
        assert_eq!(out["stats"]["total_conversations"], 0);
        assert_eq!(call(&root, "get_system_settings", json!({})), json!({}));
        assert_eq!(
            call(&root, "list_opened_tabs", json!({})),
            json!({"items": [], "version": 0})
        );
        assert_eq!(
            call(&root, "get_conversation_branch_tag", json!({})),
            json!({"enabled": false, "color": "default"})
        );
    }

    #[test]
    fn only_read_commands_are_owned() {
        let root = temp_root("owns");
        assert!(handle(&root, "create_folder_group", &json!({})).is_none());
        assert!(handle(&root, "save_system_settings", &json!({})).is_none());
        assert!(handle(&root, "git_status", &json!({})).is_none());
    }
}
