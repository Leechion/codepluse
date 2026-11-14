//! Harness connections: the settings-page "connect this harness to a
//! provider" surface, ported from
//! `server-go/internal/codeg/harness_connections.go` + the four
//! `experts/science_*_install_statuses` companions live in experts.rs.
//!
//! Ported here (3): `codepluse_connection_catalog`, `codepluse_connection_save`
//! and `get_codeg_mcp_service_status`.
//!
//! Go keeps per-harness connection defaults in `Store.Settings` under
//! `harness_connection:<wire>` and mirrors them into the in-memory Hub. The
//! Rust agent registry rebuilds from the workspace document on every request,
//! so saving is a settings write plus the `app://acp-agents-updated` event;
//! warm connections keyed on the old provider are retired through the ACP
//! layer when one is up.

use std::path::Path;

use serde_json::{json, Value};

use crate::backend::acp::commands::{agents_rows, layer_if_ready};
use crate::backend::acp::providers_core;
use crate::backend::goformat;
use crate::backend::harness_instances::{
    validate_profile_auth, validate_profile_model, validate_profile_provider,
};
use crate::backend::provider_catalog;
use crate::backend::store_file;
use crate::backend::store_writes::Outcome;

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "codepluse_connection_catalog"
            | "codepluse_connection_save"
            | "get_codeg_mcp_service_status"
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

fn run(
    root: &Path,
    command: &str,
    args: &Value,
    events: &mut Vec<(String, Value)>,
) -> Result<Value, String> {
    match command {
        "codepluse_connection_catalog" => connection_catalog(root),
        "codepluse_connection_save" => connection_save(root, args, events),
        "get_codeg_mcp_service_status" => mcp_service_status(),
        _ => Err(format!("Go 移植尚未实现此接口：{command}")),
    }
}

fn text(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Go `connectionCatalog`：每个非实例执行端一行，带上账户元数据和默认连接。
fn connection_catalog(root: &Path) -> Result<Value, String> {
    let data = provider_catalog::public(root)?;
    let providers: Vec<Value> = data
        .get("providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let settings = load_settings(root);
    let mut rows = Vec::new();
    for row in agents_rows(root) {
        if row.get("instance") == Some(&Value::Bool(true)) {
            continue;
        }
        let wire = row
            .get("agent_type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut tool = row
            .get("real_tool")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if tool.is_empty() {
            tool = wire.clone();
        }
        let key = format!("harness_connection:{wire}");
        let defaults = match settings.get(&key).filter(|v| v.is_object()) {
            Some(saved) => saved.clone(),
            None => {
                let mut picked = json!({"auth_mode": "", "provider_id": "", "model": ""});
                for provider in &providers {
                    let agent = provider.get("agent").and_then(Value::as_str).unwrap_or("");
                    let cc_current = provider.get("ccCurrent") == Some(&Value::Bool(true));
                    let api = provider.get("api") == Some(&Value::Bool(true));
                    if agent == tool && cc_current && api {
                        picked = json!({
                            "auth_mode": "api",
                            "provider_id": provider.get("id").cloned().unwrap_or(Value::Null),
                            "model": provider.get("model").cloned().unwrap_or(Value::Null),
                        });
                        break;
                    }
                }
                picked
            }
        };
        rows.push(json!({
            "agent_type": wire,
            "tool": tool,
            "account": providers_core::account(&tool),
            "defaults": defaults,
        }));
    }
    Ok(Value::Array(rows))
}

/// Go `saveHarnessConnection`：校验连接方式/供应商/模型，写 Settings，
/// 退役按旧 provider 预热好的连接，发事件。
fn connection_save(
    root: &Path,
    args: &Value,
    events: &mut Vec<(String, Value)>,
) -> Result<Value, String> {
    let wire = text(args, "agentType");
    let mode = text(args, "authMode");
    let pin = text(args, "providerId");
    let model = text(args, "model");
    let row = agents_rows(root)
        .into_iter()
        .find(|row| row.get("agent_type").and_then(Value::as_str) == Some(wire.as_str()));
    let Some(row) = row else {
        return Err("请选择智能体与连接方式".into());
    };
    if mode.is_empty() {
        return Err("请选择智能体与连接方式".into());
    }
    let mut tool = row
        .get("real_tool")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if tool.is_empty() {
        tool = wire.clone();
    }
    validate_profile_auth(&tool, &mode, &pin)?;
    if mode == "api" {
        let provider = validate_profile_provider(root, &tool, &pin)?;
        validate_profile_model(&provider, &model)?;
    }
    let cfg = json!({"auth_mode": mode, "provider_id": pin, "model": model});
    set_store_setting(root, &format!("harness_connection:{wire}"), &cfg)?;
    // Hub.UpdateAgent 在无状态注册表里没有等价物；预热连接按 agent id 退役。
    let agent_id = row
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if let Some(layer) = layer_if_ready() {
        layer.retire_agent(&agent_id);
    }
    events.push(("app://acp-agents-updated".into(), json!({})));
    Ok(cfg)
}

/// Go `get_codeg_mcp_service_status`：companion/会话/活跃委托计数。
fn mcp_service_status() -> Result<Value, String> {
    let (companions, sessions, active, base) = match layer_if_ready() {
        Some(layer) => {
            let (a, b, c) = layer.service_counts();
            (a, b, c, layer.upstream_base().to_string())
        }
        None => (0, 0, 0, String::new()),
    };
    Ok(json!({
        "state": "running",
        "listening": true,
        "socket_path": format!("{base}/api/codeg_mcp"),
        "binary_path": std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        "tool_groups": [{"key": "delegation", "enabled": true}, {"key": "feedback", "enabled": true}],
        "companion_count": companions,
        "session_count": sessions,
        "active_delegations": active,
        "depth_limit": 4,
        "started_at": Value::Null,
        "last_error": Value::Null,
        "can_start": false,
    }))
}

fn load_settings(root: &Path) -> Value {
    store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
        .ok()
        .flatten()
        .and_then(|doc| doc.get("settings").cloned())
        .unwrap_or(json!({}))
}

/// 写 `settings[key]` 并整文档落盘（Go 的 saveLocked 语义）。
fn set_store_setting(root: &Path, key: &str, value: &Value) -> Result<(), String> {
    let path = root.join(".data").join("codeg-workspace.json");
    let mut doc = match store_file::read_json(&path)? {
        Some(doc) if doc.is_object() => doc,
        _ => json!({"next": 1}),
    };
    let settings = doc
        .as_object_mut()
        .unwrap()
        .entry("settings")
        .or_insert_with(|| json!({}));
    if !settings.is_object() {
        *settings = json!({});
    }
    settings
        .as_object_mut()
        .unwrap()
        .insert(key.to_string(), value.clone());
    let bytes = goformat::marshal_store(&doc);
    let _guard = store_file::lock(&path)?;
    store_file::write_bytes(&path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-harness-conn-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn call(root: &Path, command: &str, args: Value) -> (Result<Value, String>, Vec<(String, Value)>) {
        let outcome = handle(root, command, &args).unwrap();
        (outcome.result, outcome.events)
    }

    #[test]
    fn catalog_lists_non_instance_agents_with_defaults() {
        let root = temp_root("catalog");
        // 供应商：codex 的当前 API 供应商 → 默认值来自它。
        let providers = json!({
            "providers": [{
                "id": "p1", "name": "P", "agent": "codex", "url": "https://api.openai.com",
                "model": "gpt-5", "hasKey": true, "api": true, "ccCurrent": true
            }],
            "active": {}
        });
        std::fs::write(
            root.join(".data").join("api-providers.json"),
            serde_json::to_vec_pretty(&providers).unwrap(),
        )
        .unwrap();
        let (result, _) = call(&root, "codepluse_connection_catalog", json!({}));
        let rows = result.unwrap();
        assert!(rows.is_array());
        for row in rows.as_array().unwrap() {
            assert!(row.get("tool").is_some());
            assert!(row.get("account").is_some(), "账户元数据恒在");
            assert!(row.get("defaults").unwrap().is_object());
        }
    }

    #[test]
    fn save_requires_known_agent_and_auth_mode() {
        let root = temp_root("save");
        assert_eq!(
            call(
                &root,
                "codepluse_connection_save",
                json!({"agentType": "nope", "authMode": "api"})
            )
            .0
            .unwrap_err(),
            "请选择智能体与连接方式"
        );
    }

    #[test]
    fn mcp_status_shape() {
        let root = temp_root("mcp");
        let (result, _) = call(&root, "get_codeg_mcp_service_status", json!({}));
        let value = result.unwrap();
        assert_eq!(value["state"], "running");
        assert_eq!(value["depth_limit"], 4);
        assert_eq!(value["tool_groups"][0]["key"], "delegation");
        assert_eq!(value["tool_groups"][1]["key"], "feedback");
        assert_eq!(value["can_start"], false);
    }
}
