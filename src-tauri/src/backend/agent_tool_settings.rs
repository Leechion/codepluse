//! Settings for delegation and the in-conversation tool groups.
//!
//! The frontend edits these records from separate windows, so every write
//! re-reads the shared workspace document under its cross-process lock and
//! broadcasts the committed value to the other windows.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::backend::store_file;

const FILE: &str = "codeg-workspace.json";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "get_delegation_settings"
            | "set_delegation_settings"
            | "get_feedback_settings"
            | "set_feedback_settings"
            | "get_question_settings"
            | "set_question_settings"
            | "get_session_info_settings"
            | "set_session_info_settings"
            | "get_browser_tools_settings"
            | "set_browser_tools_settings"
            | "get_chat_authoring_settings"
            | "set_chat_authoring_settings"
    )
}

#[derive(Debug)]
pub struct Outcome {
    pub result: Value,
    pub events: Vec<(String, Value)>,
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Outcome, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn path(root: &Path) -> std::path::PathBuf {
    root.join(".data").join(FILE)
}

fn read_document(root: &Path) -> Result<Value, String> {
    Ok(store_file::read_json(&path(root))?.unwrap_or_else(|| json!({})))
}

fn read_group(root: &Path, group: &str, defaults: Value) -> Result<Value, String> {
    let doc = read_document(root)?;
    let saved = doc
        .get("settings")
        .and_then(Value::as_object)
        .and_then(|settings| settings.get(group))
        .and_then(Value::as_object);
    let mut result = defaults.as_object().cloned().unwrap_or_default();
    if let Some(saved) = saved {
        for (key, value) in saved {
            if result.contains_key(key) {
                result.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(Value::Object(result))
}

fn write_group(
    root: &Path,
    group: &str,
    value: Value,
    event: Option<&str>,
) -> Result<Outcome, String> {
    let file = path(root);
    let _lock = store_file::lock(&file)?;
    let mut doc = match store_file::read_json(&file)? {
        Some(doc) if doc.is_object() => doc,
        None => json!({}),
        Some(_) => return Err("工作区设置格式无效".into()),
    };
    let root = doc
        .as_object_mut()
        .ok_or_else(|| "工作区设置格式无效".to_string())?;
    let settings = root.entry("settings").or_insert_with(|| json!({}));
    if !settings.is_object() {
        return Err("工作区设置格式无效".into());
    }
    settings
        .as_object_mut()
        .unwrap()
        .insert(group.to_string(), value.clone());
    store_file::write_json(&file, &doc)?;
    Ok(Outcome {
        result: value.clone(),
        events: event
            .map(|name| vec![(name.to_string(), value)])
            .unwrap_or_default(),
    })
}

fn settings_arg(args: &Value) -> Result<&Map<String, Value>, String> {
    args.get("settings")
        .and_then(Value::as_object)
        .ok_or_else(|| "设置格式无效".to_string())
}

fn bool_setting(settings: &Map<String, Value>, key: &str, default: bool) -> Result<bool, String> {
    match settings.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{key} 设置无效")),
        None => Ok(default),
    }
}

fn set_single_toggle(
    root: &Path,
    args: &Value,
    group: &str,
    event: Option<&str>,
    default: bool,
) -> Result<Outcome, String> {
    let settings = settings_arg(args)?;
    let value = json!({
        "enabled": bool_setting(settings, "enabled", default)?,
    });
    write_group(root, group, value, event)
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Outcome, String> {
    match command {
        "get_delegation_settings" => Ok(Outcome {
            result: read_group(
                root,
                "delegation",
                json!({
                    "enabled": false,
                    "depth_limit": 1,
                    "completed_cache_max_mb": 512,
                    "agent_defaults": {},
                }),
            )?,
            events: Vec::new(),
        }),
        "set_delegation_settings" => {
            let settings = settings_arg(args)?;
            let depth = settings
                .get("depth_limit")
                .and_then(Value::as_i64)
                .ok_or_else(|| "depth_limit 设置无效".to_string())?
                .clamp(1, 8);
            let cache_mb = settings
                .get("completed_cache_max_mb")
                .and_then(Value::as_i64)
                .ok_or_else(|| "completed_cache_max_mb 设置无效".to_string())?
                .max(0);
            let agent_defaults = match settings.get("agent_defaults") {
                None | Some(Value::Null) => json!({}),
                Some(Value::Object(value)) => Value::Object(value.clone()),
                Some(_) => return Err("agent_defaults 设置无效".into()),
            };
            let value = json!({
                "enabled": bool_setting(settings, "enabled", false)?,
                "depth_limit": depth,
                "completed_cache_max_mb": cache_mb,
                "agent_defaults": agent_defaults,
            });
            write_group(
                root,
                "delegation",
                value,
                Some("delegation-settings://changed"),
            )
        }
        "get_feedback_settings" => Ok(Outcome {
            result: read_group(root, "feedback", json!({ "enabled": false }))?,
            events: Vec::new(),
        }),
        "set_feedback_settings" => set_single_toggle(
            root,
            args,
            "feedback",
            Some("feedback-settings://changed"),
            false,
        ),
        "get_question_settings" => Ok(Outcome {
            result: read_group(root, "question", json!({ "enabled": true }))?,
            events: Vec::new(),
        }),
        "set_question_settings" => set_single_toggle(root, args, "question", None, true),
        "get_session_info_settings" => Ok(Outcome {
            result: read_group(root, "session_info", json!({ "enabled": true }))?,
            events: Vec::new(),
        }),
        "set_session_info_settings" => set_single_toggle(root, args, "session_info", None, true),
        "get_browser_tools_settings" => Ok(Outcome {
            result: read_group(
                root,
                "browser_tools",
                json!({ "enabled": false, "eval": false }),
            )?,
            events: Vec::new(),
        }),
        "set_browser_tools_settings" => {
            let settings = settings_arg(args)?;
            let enabled = bool_setting(settings, "enabled", false)?;
            let value = json!({
                "enabled": enabled,
                "eval": enabled && bool_setting(settings, "eval", false)?,
            });
            write_group(
                root,
                "browser_tools",
                value,
                Some("browser-tools-settings://changed"),
            )
        }
        "get_chat_authoring_settings" => Ok(Outcome {
            result: read_group(
                root,
                "chat_authoring",
                json!({ "automations_enabled": false, "work_tasks_enabled": false }),
            )?,
            events: Vec::new(),
        }),
        "set_chat_authoring_settings" => {
            let settings = settings_arg(args)?;
            let value = json!({
                "automations_enabled": bool_setting(settings, "automations_enabled", false)?,
                "work_tasks_enabled": bool_setting(settings, "work_tasks_enabled", false)?,
            });
            write_group(
                root,
                "chat_authoring",
                value,
                Some("chat-authoring-settings://changed"),
            )
        }
        _ => Err(format!("未知设置命令：{command}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("agent-tool-settings-{nonce}"));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn defaults_and_round_trip_are_stable() {
        let root = temp_root();
        let defaults = handle(&root, "get_delegation_settings", &json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(defaults.result["enabled"], false);
        assert_eq!(defaults.result["depth_limit"], 1);
        assert_eq!(defaults.result["completed_cache_max_mb"], 512);

        let saved = handle(
            &root,
            "set_browser_tools_settings",
            &json!({ "settings": { "enabled": false, "eval": true } }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(saved.result, json!({ "enabled": false, "eval": false }));
        assert_eq!(saved.events[0].0, "browser-tools-settings://changed");
        let loaded = handle(&root, "get_browser_tools_settings", &json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.result, json!({ "enabled": false, "eval": false }));
        std::fs::remove_dir_all(root).unwrap();
    }
}
