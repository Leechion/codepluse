//! Port of Go's `codeg/split.go` registry half: the task-split cards the
//! model proposes and the user confirms. `propose_task_split` itself arrives
//! through the companion MCP (still Go-served), so cards only reach the Rust
//! layer when that domain lands; the registry, wire shape, persistence and
//! resolution paths are all here so `acp_respond_task_split` and
//! `dropConnectionSplits` behave identically from day one.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::backend::acp::connections::LayerState;
use crate::backend::goformat;
use crate::backend::store_file;

/// Go `taskSplit` (split.go:22).
#[derive(Clone, Debug, Default)]
pub struct TaskSplit {
    pub id: String,
    pub parent: String,
    pub root: String,
    pub parent_tool_use_id: String,
    pub tasks: Vec<Value>,
    pub created_at: String,
}

/// Go `splitWire` (split.go:77).
pub fn split_wire(sp: &TaskSplit) -> Value {
    json!({
        "split_id": sp.id,
        "parent_connection_id": sp.parent,
        "root_connection_id": sp.root,
        "parent_tool_use_id": if sp.parent_tool_use_id.is_empty() { Value::Null } else { json!(sp.parent_tool_use_id) },
        "tasks": sp.tasks,
        "created_at": sp.created_at,
    })
}

/// Go `splitFile` (split.go:45).
fn split_file(root: &Path) -> std::path::PathBuf {
    root.join(".data").join("codeg-task-splits.json")
}

/// Go `saveSplitsLocked` (split.go:53).
pub fn save(root: &Path, rows: &[Value]) -> Result<(), String> {
    store_file::write_json(&split_file(root), &json!(rows))
}

/// Go `loadSplits` (split.go:60): pending cards survive restarts, still
/// waiting.
pub fn load(root: &Path, splits: &mut HashMap<String, TaskSplit>) {
    let Ok(Some(data)) = store_file::read_json(&split_file(root)) else {
        return;
    };
    let Some(rows) = data.as_array() else { return };
    for row in rows {
        let get = |key: &str| crate::backend::acp::wire::text(row.get(key));
        let id = get("split_id");
        let parent = get("parent_connection_id");
        if id.is_empty() || parent.is_empty() {
            continue;
        }
        let tasks = row
            .get("tasks")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter(|t| t.is_object()).cloned().collect())
            .unwrap_or_default();
        splits.insert(
            id.clone(),
            TaskSplit {
                id,
                parent,
                root: get("root_connection_id"),
                parent_tool_use_id: get("parent_tool_use_id"),
                tasks,
                created_at: get("created_at"),
            },
        );
    }
}

/// Go `clearSplitLocked` (split.go:520): retire a handled split — gone from
/// the registry, off the parent's snapshot slot, un-claimed on its tool row.
/// Returns the removed split so callers can emit events.
pub fn clear_locked(state: &mut LayerState, split_id: &str) -> Option<TaskSplit> {
    let sp = state.splits.remove(split_id)?;
    if let Some(conn) = state.connections.get(&sp.parent) {
        let mut c = conn.lock().unwrap();
        if crate::backend::acp::wire::text(c.pending_split.get("split_id")) == sp.id {
            c.pending_split = Value::Null;
        }
        if !sp.parent_tool_use_id.is_empty() {
            c.tool_claims.remove(&sp.parent_tool_use_id);
        }
    }
    Some(sp)
}

/// Go `shortenTitle` (split.go:97): prompt-derived title truncated to at most
/// n bytes, backing off to a rune boundary so CJK titles never split mid-rune.
pub fn shorten_title(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut end = n;
    while end > 0 && !std::str::from_utf8(&bytes[..end]).is_ok() {
        end -= 1;
    }
    std::str::from_utf8(&bytes[..end]).unwrap_or("").to_string()
}

/// Go `respondTaskSplit` validation (split.go:466-494): the confirmed task
/// list replaces the model's draft. Returns the normalized rows.
pub fn normalize_confirmed_tasks(raw: &[Value]) -> Result<Vec<Value>, String> {
    if raw.is_empty() || raw.len() > 8 {
        return Err("tasks 需要 1–8 个子任务".into());
    }
    let mut tasks = Vec::new();
    for (i, item) in raw.iter().enumerate() {
        if !item.is_object() {
            return Err(format!("第 {} 个确认子任务无效", i + 1));
        }
        let prompt = crate::backend::acp::wire::text(item.get("prompt"))
            .trim()
            .to_string();
        if prompt.is_empty() || prompt.len() > 120_000 {
            return Err(format!(
                "第 {} 个子任务的 prompt 必须非空且不超过 120000 字节",
                i + 1
            ));
        }
        let mut title = crate::backend::acp::wire::text(item.get("title"))
            .trim()
            .to_string();
        if title.is_empty() {
            title = shorten_title(&prompt, 60);
        }
        tasks.push(json!({
            "index": i,
            "title": title,
            "prompt": prompt,
            "agent_type": item.get("agent_type").cloned().unwrap_or(Value::Null),
            "model": item.get("model").cloned().unwrap_or(Value::Null),
        }));
    }
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("acp-splits-{tag}-{}", goformat::hex_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn split_wire_and_load_roundtrip() {
        let root = temp_root("wire");
        let sp = TaskSplit {
            id: "sp1".into(),
            parent: "c1".into(),
            root: "conversation:9".into(),
            parent_tool_use_id: String::new(),
            tasks: vec![json!({"title": "t", "prompt": "p"})],
            created_at: goformat::timestamp(),
        };
        save(&root, &[split_wire(&sp)]).expect("save");
        let mut loaded = HashMap::new();
        load(&root, &mut loaded);
        assert_eq!(loaded.len(), 1);
        let got = loaded.get("sp1").unwrap();
        assert_eq!(got.parent, "c1");
        assert_eq!(got.tasks.len(), 1);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn load_skips_incomplete_rows() {
        let root = temp_root("skip");
        store_file::write_json(
            &split_file(&root),
            &json!([
                {"split_id": "", "parent_connection_id": "c"},
                {"split_id": "ok", "parent_connection_id": "c1", "tasks": [{"bad": true}]}
            ]),
        )
        .unwrap();
        let mut loaded = HashMap::new();
        load(&root, &mut loaded);
        assert_eq!(loaded.len(), 1);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn shorten_title_backs_off_to_rune_boundary() {
        assert_eq!(shorten_title("short", 10), "short");
        let cjk = "你好世界".repeat(20); // 12 bytes per repetition, 240 total
        let cut = shorten_title(&cjk, 60);
        assert!(std::str::from_utf8(cut.as_bytes()).is_ok());
        assert!(cut.len() <= 60);
        assert!(cut.is_empty() || cjk.starts_with(&cut));
    }

    #[test]
    fn normalize_confirmed_tasks_validates() {
        assert_eq!(
            normalize_confirmed_tasks(&[]).unwrap_err(),
            "tasks 需要 1–8 个子任务"
        );
        let eight: Vec<Value> = (0..9).map(|i| json!({"prompt": format!("p{i}")})).collect();
        assert!(normalize_confirmed_tasks(&eight).is_err());
        let ok = normalize_confirmed_tasks(&[json!({"prompt": "  do it "})]).unwrap();
        assert_eq!(ok[0]["title"], "do it");
        assert_eq!(ok[0]["prompt"], "do it");
        assert_eq!(
            normalize_confirmed_tasks(&[json!({"prompt": ""})]).unwrap_err(),
            "第 1 个子任务的 prompt 必须非空且不超过 120000 字节"
        );
    }
}
