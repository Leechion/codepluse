//! Pi project resource disclosure and trust decisions.
//!
//! Pi stores trust as a map of canonical project directories to booleans in
//! `~/.pi/agent/trust.json`. Codepluse keeps a separate acknowledgement list
//! so grants written by older versions are disclosed once before ACP resumes.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::backend::{goformat, store_file};

const ACK_FILE: &str = "pi-project-trust-ack.json";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "acp_pi_project_trust_state"
            | "acp_pi_set_project_trust"
            | "acp_pi_acknowledge_project_trust"
            | "acp_pi_list_trust_entries"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

pub(super) fn agent_dir() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .or_else(|| std::env::var_os("PI_AGENT_DIR"))
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|path| PathBuf::from(path).join(".pi/agent"))
        })
        .or_else(|| std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".pi/agent")))
        .unwrap_or_else(|| PathBuf::from(".pi/agent"))
}

fn canonical_dir(raw: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw.trim());
    if !path.is_absolute() {
        return Err("Pi 工作目录必须是绝对路径".into());
    }
    let path =
        std::fs::canonicalize(path).map_err(|error| format!("Pi 工作目录无法访问：{error}"))?;
    if !path.is_dir() {
        return Err("Pi 工作目录不是目录".into());
    }
    Ok(path)
}

fn display_path(path: &Path) -> String {
    crate::backend::git::strip_verbatim(path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn trust_path() -> PathBuf {
    agent_dir().join("trust.json")
}

fn read_object(path: &Path) -> Result<Map<String, Value>, String> {
    let Some(value) = store_file::read_json(path)? else {
        return Ok(Map::new());
    };
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("Pi 信任文件格式无效：{}", path.display()))
}

fn read_ack(root: &Path) -> Result<std::collections::HashSet<String>, String> {
    let path = root.join(".data").join(ACK_FILE);
    let Some(value) = store_file::read_json(&path)? else {
        return Ok(std::collections::HashSet::new());
    };
    let rows = value.as_array().ok_or("Pi 授权确认记录格式无效")?;
    Ok(rows
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect())
}

fn write_ack(root: &Path, path_key: &str, acknowledged: bool) -> Result<(), String> {
    let file = root.join(".data").join(ACK_FILE);
    let _lock = store_file::lock(&file)?;
    let mut entries = read_ack(root)?;
    if acknowledged {
        entries.insert(path_key.to_string());
    } else {
        entries.remove(path_key);
    }
    let mut entries: Vec<_> = entries.into_iter().collect();
    entries.sort();
    store_file::write_bytes(&file, &goformat::marshal_preserve(&json!(entries)))
}

fn push_resource(resources: &mut Vec<Value>, path: &Path, kind: &str, executes: bool) {
    if resources.len() >= 500 {
        return;
    }
    resources.push(json!({
        "path": display_path(path),
        "kind": kind,
        "executesCode": executes,
    }));
}

fn walk_resource_dir(root: &Path, kind: &str, executes: bool, resources: &mut Vec<Value>) {
    if !root.exists() {
        return;
    }
    let mut stack = vec![root.to_path_buf()];
    let mut found = false;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if resources.len() >= 500 {
                return;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() || file_type.is_symlink() {
                push_resource(resources, &path, kind, executes);
                found = true;
            }
        }
    }
    if !found {
        push_resource(resources, root, kind, executes);
    }
}

fn project_resources(workspace: &Path) -> Vec<Value> {
    let mut resources = Vec::new();
    let pi = workspace.join(".pi");
    for (name, kind, executes) in [
        ("settings.json", ".pi/settings.json", true),
        ("mcp.json", ".pi/mcp.json", true),
        ("SYSTEM.md", ".pi/SYSTEM.md", false),
        ("APPEND_SYSTEM.md", ".pi/APPEND_SYSTEM.md", false),
    ] {
        let path = pi.join(name);
        if path.is_file() {
            push_resource(&mut resources, &path, kind, executes);
        }
    }
    for (name, kind, executes) in [
        ("extensions", ".pi/extensions", true),
        ("skills", ".pi/skills", false),
        ("prompts", ".pi/prompts", false),
        ("themes", ".pi/themes", false),
    ] {
        walk_resource_dir(&pi.join(name), kind, executes, &mut resources);
    }
    let mut cursor = Some(workspace);
    while let Some(dir) = cursor {
        walk_resource_dir(
            &dir.join(".agents").join("skills"),
            ".agents/skills",
            false,
            &mut resources,
        );
        cursor = dir.parent();
    }
    resources.sort_by_key(|resource| {
        resource
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase()
    });
    resources.dedup_by(|a, b| a.get("path") == b.get("path"));
    resources
}

fn decision_for(path: &Path, entries: &Map<String, Value>) -> (Option<bool>, Option<String>) {
    let mut candidate = Some(path);
    while let Some(dir) = candidate {
        let key = display_path(dir);
        if let Some(value) = entries.get(&key).and_then(Value::as_bool) {
            return (Some(value), Some(key));
        }
        candidate = dir.parent();
    }
    (None, None)
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "acp_pi_project_trust_state" => {
            let workspace =
                canonical_dir(args.get("workspace").and_then(Value::as_str).unwrap_or(""))?;
            let entries = read_object(&trust_path())?;
            let (decision, decided_at) = decision_for(&workspace, &entries);
            let workspace = display_path(&workspace);
            let acknowledged = read_ack(root)?.contains(&workspace);
            Ok(json!({
                "workspace": workspace,
                "resources": project_resources(Path::new(&workspace)),
                "decision": decision,
                "decidedAt": decided_at,
                "trustFile": display_path(&trust_path()),
                "acknowledged": acknowledged,
            }))
        }
        "acp_pi_set_project_trust" => {
            let workspace =
                canonical_dir(args.get("workspace").and_then(Value::as_str).unwrap_or(""))?;
            let key = display_path(&workspace);
            let decision = args.get("trusted").ok_or("trusted 字段缺失")?;
            let path = trust_path();
            let _lock = store_file::lock(&path)?;
            let mut entries = read_object(&path)?;
            match decision {
                Value::Bool(value) => {
                    entries.insert(key.clone(), json!(value));
                }
                Value::Null => {
                    entries.remove(&key);
                }
                _ => return Err("trusted 必须是布尔值或 null".into()),
            }
            store_file::write_json(&path, &Value::Object(entries))?;
            write_ack(root, &key, decision == &Value::Bool(true))?;
            Ok(Value::Null)
        }
        "acp_pi_acknowledge_project_trust" => {
            let workspace =
                canonical_dir(args.get("workspace").and_then(Value::as_str).unwrap_or(""))?;
            let key = display_path(&workspace);
            let entries = read_object(&trust_path())?;
            if decision_for(&workspace, &entries).0 != Some(true) {
                return Err("此工作目录没有待确认的 Pi 信任授权".into());
            }
            write_ack(root, &key, true)?;
            Ok(Value::Null)
        }
        "acp_pi_list_trust_entries" => {
            let entries = read_object(&trust_path())?;
            let mut rows: Vec<_> = entries
                .into_iter()
                .filter_map(|(path, value)| {
                    value
                        .as_bool()
                        .map(|trusted| json!({"path":path,"trusted":trusted}))
                })
                .collect();
            rows.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
            Ok(Value::Array(rows))
        }
        _ => unreachable!("owned command must have a handler"),
    }
}

/// Refuse to start Pi on project resources before the trust banner has been
/// reviewed. This gate is intentionally separate from Pi's trust store: it
/// prevents an old saved grant from being silently re-enabled by Codepluse.
pub fn check_launch(root: &Path, workspace: &Path) -> Result<(), String> {
    let resources = project_resources(workspace);
    if resources.is_empty() {
        return Ok(());
    }
    let entries = read_object(&trust_path())?;
    let (decision, decided_at) = decision_for(workspace, &entries);
    let key = display_path(workspace);
    if decision == Some(true) && read_ack(root)?.contains(&key) {
        return Ok(());
    }
    Err(if decision == Some(false) {
        "Pi 项目资源已被拒绝；请在项目授权提示中重新决定".into()
    } else if decision == Some(true) {
        format!(
            "Pi 项目授权尚未确认（授权来自 {}）",
            decided_at.unwrap_or_default()
        )
    } else {
        "Pi 项目包含尚未审阅的项目资源，请先在聊天窗口确认授权".into()
    })
}
