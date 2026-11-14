//! Harness instances: named copies of a built-in tool (Claude 1号/2号…) that
//! pin their own provider/model, ported from
//! `server-go/internal/codeg/instances.go` + `main_bot.go`.
//!
//! Ported (5): `codepluse_instance_list/create/update/delete` and
//! `update_main_bot_settings`.
//!
//! The Go side keeps the registry in the in-memory Hub and mirrors it to
//! `.data/harness-instances.json`; the Rust agent registry (acp::agents)
//! rebuilds statelessly from that same file on every request, so the CRUD
//! here is a direct read-modify-write of the spec list — no hub bookkeeping.
//! Responses are rows from `acp::commands::agents_rows` (Go `s.agents()`).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::backend::acp::agents;
use crate::backend::acp::commands::agents_rows;
use crate::backend::goformat;
use crate::backend::provider_catalog;

use crate::backend::store_file;

const EVENT_AGENT_CHANGED: &str = "agent://changed";
const EVENT_ACP_AGENTS: &str = "app://acp-agents-updated";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "codepluse_instance_list"
            | "codepluse_instance_create"
            | "codepluse_instance_update"
            | "codepluse_instance_delete"
            | "update_main_bot_settings"
    )
}

/// 返回 (HTTP 结果, 事件帧)——事件由前端门注入 WS 客户端。
pub fn handle(
    root: &Path,
    command: &str,
    args: &Value,
) -> Option<(Result<Value, String>, Vec<(String, Value)>)> {
    if !owns(command) {
        return None;
    }
    let mut events = Vec::new();
    let result = run(root, command, args, &mut events);
    Some((result, events))
}

// ── spec store（instances.go 的 instanceSpec / saveInstancesLocked）─────────

/// Go `instanceSpec` 的落盘字段序（MarshalIndent 两空格）。
#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
struct InstanceSpec {
    #[serde(default)]
    id: String,
    #[serde(default)]
    wire: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    real_tool: String,
    #[serde(rename = "providerId", default, skip_serializing_if = "String::is_empty")]
    provider_id: String,
    #[serde(
        rename = "defaultModel",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    default_model: String,
    #[serde(rename = "authMode", default, skip_serializing_if = "String::is_empty")]
    auth_mode: String,
    #[serde(
        rename = "baseAgentType",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    base_agent_type: String,
}

fn instances_path(root: &Path) -> PathBuf {
    root.join(".data").join("harness-instances.json")
}

fn load_specs(root: &Path) -> Vec<InstanceSpec> {
    match store_file::read_json(&instances_path(root)) {
        Ok(Some(Value::Array(list))) => list
            .into_iter()
            .filter_map(|item| serde_json::from_value(item).ok())
            .collect(),
        _ => Vec::new(),
    }
}

fn save_specs(root: &Path, specs: &[InstanceSpec]) -> Result<(), String> {
    // Go: json.MarshalIndent(saved, "", "  ") + tmp+rename。
    let value = serde_json::to_value(specs).map_err(|e| e.to_string())?;
    let body = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    store_file::write_bytes(&instances_path(root), &body)
}

// ── profile 校验（main_bot.go / harness_connections.go）────────────────────

pub(crate) fn validate_profile_auth(tool: &str, mode: &str, pin: &str) -> Result<(), String> {
    if !mode.is_empty() && mode != "api" && mode != "subscription" {
        return Err("连接方式无效".into());
    }
    if mode == "subscription" {
        if !pin.is_empty() {
            return Err("官网订阅不能同时指定 API 供应商".into());
        }
        if !crate::backend::acp::providers_core::supports_subscription(tool) {
            return Err("此 Harness 没有已接入的官网订阅登录方式，请使用其原生 API 配置".into());
        }
    }
    if mode == "api" && pin.is_empty() {
        return Err("请选择 API 供应商".into());
    }
    Ok(())
}

pub(crate) fn validate_profile_provider(root: &Path, tool: &str, id: &str) -> Result<provider_catalog::Provider, String> {
    let provider = provider_catalog::find(root, id)?;
    if provider.agent != tool {
        return Err("供应商不属于所选 Harness".into());
    }
    if !provider_catalog::is_api(&provider) {
        return Err("请选择有 API 密钥的供应商，官方订阅不能用作此配置".into());
    }
    if provider.enabled == Some(false) {
        return Err("此 API 供应商已停用".into());
    }
    Ok(provider)
}

pub(crate) fn validate_profile_model(provider: &provider_catalog::Provider, model: &str) -> Result<(), String> {
    for model_entry in &provider.models {
        if model_entry.id == model && model_entry.enabled == Some(false) {
            return Err("此模型已停用，请选择其他模型".into());
        }
    }
    Ok(())
}

fn normalize_tool(tool: &str) -> String {
    match tool {
        "claude_code" => "claude".into(),
        "deepseek" => "dsh".into(),
        other => other.to_string(),
    }
}

/// Go `freeWire`：最小的未占用 `inst_<tool><N>`，从 2 起（内置保持「1号」）。
fn free_wire(root: &Path, tool: &str) -> String {
    let taken: Vec<String> = load_specs(root)
        .iter()
        .map(|spec| spec.wire.clone())
        .collect();
    for n in 2..10000 {
        let wire = format!("inst_{tool}{n}");
        if !taken.iter().any(|w| *w == wire) {
            return wire;
        }
    }
    String::new()
}

fn base_wire_of(agent: &agents::Agent) -> String {
    agents::wire_agent(agent)
}

// ── commands ────────────────────────────────────────────────────────────────

fn run(root: &Path, command: &str, args: &Value, events: &mut Vec<(String, Value)>) -> Result<Value, String> {
    match command {
        "codepluse_instance_list" => {
            let rows: Vec<Value> = agents_rows(root)
                .into_iter()
                .filter(|row| {
                    row.get("instance").and_then(Value::as_bool).unwrap_or(false)
                        && !row
                            .get("agent_type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .starts_with("inst_main_")
                })
                .collect();
            Ok(Value::Array(rows))
        }
        "codepluse_instance_create" => {
            let name = txt(args, "name").trim().to_string();
            let mut tool = txt(args, "realTool");
            if tool.is_empty() {
                tool = txt(args, "agentType");
            }
            tool = normalize_tool(&tool);
            // Go `profileBase`：内置 base 必须存在且可用。
            let base_wire = crate::backend::acp::agents::base_wire(&tool);
            let base = agents::pick(root, &base_wire)
                .ok_or_else(|| format!("本机未安装 {tool}，无法创建实例"))?;
            let provider_id = txt(args, "providerId").trim().to_string();
            let default_model = txt(args, "defaultModel").trim().to_string();
            let auth_mode = txt(args, "authMode");
            validate_profile_auth(&tool, &auth_mode, &provider_id)?;
            if !provider_id.is_empty() {
                let provider = validate_profile_provider(root, &tool, &provider_id)
                    .map_err(|e| format!("钉住供应商失败: {e}"))?;
                validate_profile_model(&provider, &default_model)?;
            }
            let id = goformat::hex_id()[..12].to_string();
            let real_tool = base.real_tool.clone();
            let spec = InstanceSpec {
                id: id.clone(),
                wire: free_wire(root, &tool.replace(':', "_")),
                name,
                real_tool: real_tool.clone(),
                provider_id,
                default_model,
                auth_mode,
                base_agent_type: base_wire_of(&base),
            };
            let mut specs = load_specs(root);
            specs.push(spec.clone());
            save_specs(root, &specs)?;
            let wire = spec.wire.clone();
            events.push((
                EVENT_AGENT_CHANGED.into(),
                json!({"kind": "upsert", "agent": wire}),
            ));
            events.push((EVENT_ACP_AGENTS.into(), json!({})));
            instance_view(root, &wire)
        }
        "codepluse_instance_update" => {
            let agent_id = resolve_instance_id(root, args);
            let has_provider = args.get("providerId").and_then(Value::as_str).is_some();
            let has_auth = args.get("authMode").and_then(Value::as_str).is_some();
            let has_model = args.get("defaultModel").and_then(Value::as_str).is_some();
            let provider_id = txt(args, "providerId").trim().to_string();
            let default_model = txt(args, "defaultModel").trim().to_string();
            let name = txt(args, "name").trim().to_string();

            let mut specs = load_specs(root);
            let index = specs
                .iter()
                .position(|spec| format!("agt_inst_{}", spec.id) == agent_id)
                .ok_or_else(|| "实例不存在".to_string())?;
            if specs[index].wire.is_empty() || !specs[index].wire.starts_with("inst_") {
                return Err("实例不存在".into());
            }
            if has_auth {
                let tool = if specs[index].real_tool.is_empty() {
                    specs[index].base_agent_type.clone()
                } else {
                    specs[index].real_tool.clone()
                };
                let pin = if has_provider {
                    provider_id.clone()
                } else {
                    specs[index].provider_id.clone()
                };
                validate_profile_auth(&tool, &auth_mode_of(args, has_auth), &pin)?;
            }
            if has_provider && !provider_id.is_empty() {
                let provider = validate_profile_provider(root, &specs[index].real_tool, &provider_id)
                    .map_err(|e| format!("钉住供应商失败: {e}"))?;
                let model = if has_model {
                    default_model.clone()
                } else {
                    specs[index].default_model.clone()
                };
                validate_profile_model(&provider, &model)?;
            }
            if !name.is_empty() {
                specs[index].name = name;
            }
            if has_provider {
                specs[index].provider_id = provider_id;
            }
            if has_auth {
                specs[index].auth_mode = txt(args, "authMode");
            }
            if has_model {
                specs[index].default_model = default_model.trim().to_string();
            }
            let wire = specs[index].wire.clone();
            save_specs(root, &specs)?;
            events.push((
                EVENT_AGENT_CHANGED.into(),
                json!({"kind": "upsert", "agent": wire}),
            ));
            events.push((EVENT_ACP_AGENTS.into(), json!({})));
            instance_view(root, &wire)
        }
        "codepluse_instance_delete" => {
            let agent_id = resolve_instance_id(root, args);
            let mut specs = load_specs(root);
            let index = specs
                .iter()
                .position(|spec| format!("agt_inst_{}", spec.id) == agent_id)
                .ok_or_else(|| "实例不存在".to_string())?;
            if specs[index].wire.is_empty() || !specs[index].wire.starts_with("inst_") {
                return Err("实例不存在".into());
            }
            let wire = specs[index].wire.clone();
            specs.remove(index);
            save_specs(root, &specs)?;
            events.push((
                EVENT_AGENT_CHANGED.into(),
                json!({"kind": "remove", "agent": wire}),
            ));
            events.push((EVENT_ACP_AGENTS.into(), json!({})));
            Ok(Value::Null)
        }
        "update_main_bot_settings" => {
            let Some(cfg) = args.get("settings").filter(|v| v.is_object()).cloned() else {
                return Err("settings 必须是对象".into());
            };
            let text_of = |key: &str| {
                cfg.get(key)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            let wire = text_of("agent_type");
            let provider_id = text_of("provider_id");
            let mut model = text_of("model");
            let mut auth_mode = cfg
                .get("auth_mode")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !provider_id.is_empty() && auth_mode.is_empty() {
                auth_mode = "api".into();
            }
            let mut next_cfg = cfg.clone();
            if !provider_id.is_empty() || auth_mode == "subscription" {
                let base = agents::pick(root, &wire)
                    .ok_or_else(|| "所选 Harness 不可用".to_string())?;
                let tool = if base.real_tool.is_empty() {
                    agents::wire_agent(&base)
                } else {
                    base.real_tool.clone()
                };
                validate_profile_auth(&tool, &auth_mode, &provider_id)?;
                if auth_mode == "api" {
                    let provider = validate_profile_provider(root, &tool, &provider_id)?;
                    if model.is_empty() {
                        model = provider.model.clone();
                    }
                    if model.is_empty() {
                        return Err("请选择模型".into());
                    }
                    validate_profile_model(&provider, &model)?;
                }
                // identity = tool\0provider\0model[\0subscription]，sha256 前 12 hex。
                let mut identity = format!("{tool}\x00{provider_id}\x00{model}");
                if auth_mode == "subscription" {
                    identity.push_str("\x00subscription");
                }
                use sha2::{Digest, Sha256};
                let digest = Sha256::digest(identity.as_bytes());
                let digest = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
                let spec_id = format!("main_{}_{}", base.real_tool, digest);
                let main_wire = format!("inst_main_{}_{}", base.real_tool, digest);
                let spec = InstanceSpec {
                    id: spec_id,
                    wire: main_wire.clone(),
                    name: format!("主智能体 · {} · {}", base.name, model),
                    real_tool: base.real_tool.clone(),
                    provider_id: provider_id.clone(),
                    default_model: model.clone(),
                    auth_mode: auth_mode.clone(),
                    base_agent_type: base_wire_of(&base),
                };
                // Upsert（同 spec id 覆盖）。
                let mut specs = load_specs(root);
                match specs.iter().position(|s| s.id == spec.id) {
                    Some(index) => specs[index] = spec.clone(),
                    None => specs.push(spec.clone()),
                }
                save_specs(root, &specs)?;
                next_cfg = json!({
                    "agent_type": main_wire,
                    "base_agent_type": base_wire_of(&base),
                    "provider_id": provider_id,
                    "model": model,
                    "auth_mode": auth_mode,
                });
                events.push(("main-bot://changed".into(), next_cfg.clone()));
                events.push((EVENT_ACP_AGENTS.into(), json!({})));
            } else {
                events.push(("main-bot://changed".into(), next_cfg.clone()));
            }
            // settings["get_main_bot_settings"] = cfg（store 全文档落盘）。
            write_main_bot_setting(root, &next_cfg)?;
            Ok(next_cfg)
        }
        _ => Err(format!("Go 移植尚未实现此接口：{command}")),
    }
}

fn auth_mode_of(args: &Value, has_auth: bool) -> &str {
    if has_auth {
        args.get("authMode").and_then(Value::as_str).unwrap_or("")
    } else {
        ""
    }
}

/// Go `instanceView`：重建注册表后取该 wire 的行。
fn instance_view(root: &Path, wire: &str) -> Result<Value, String> {
    for row in agents_rows(root) {
        if row.get("agent_type").and_then(Value::as_str) == Some(wire) {
            return Ok(row);
        }
    }
    Ok(json!({}))
}

/// Go `resolveInstanceID`：显式 agentId 优先，其次 inst_ 前缀的 wire。
fn resolve_instance_id(root: &Path, args: &Value) -> String {
    if let Some(id) = args.get("agentId").and_then(Value::as_str) {
        if !id.is_empty() {
            return id.to_string();
        }
    }
    if let Some(wire) = args.get("agentType").and_then(Value::as_str) {
        if wire.starts_with("inst_") {
            for agent in agents::registry(root) {
                if agent.wire == wire {
                    return agent.id.clone();
                }
            }
        }
    }
    String::new()
}

/// Go：`s.store.Settings["get_main_bot_settings"] = cfg` + saveLocked。
fn write_main_bot_setting(root: &Path, cfg: &Value) -> Result<(), String> {
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
        .insert("get_main_bot_settings".into(), cfg.clone());
    let bytes = goformat::marshal_store(&doc);
    let _guard = store_file::lock(&path)?;
    store_file::write_bytes(&path, &bytes)
}

fn txt(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-harness-inst-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    /// 内置 codex 供应商（create 的 base 与 profile 校验目标）。
    fn seed_provider(root: &Path) {
        let document = json!({
            "providers": [{
                "id": "prov1", "name": "P1", "agent": "codex", "url": "https://api.openai.com",
                "model": "gpt-5", "key": "sk-p1", "hasKey": true, "api": true,
                "models": [{"id": "gpt-5"}]
            }],
            "active": {}
        });
        std::fs::write(
            crate::backend::provider_catalog::api_providers_path(root),
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
    }

    fn call(root: &Path, command: &str, args: Value) -> (Result<Value, String>, Vec<(String, Value)>) {
        handle(root, command, &args).unwrap()
    }

    fn ok(root: &Path, command: &str, args: Value) -> Value {
        call(root, command, args).0.unwrap()
    }

    /// 本机未必装 codex/claude——registry 的内置可用性取决于探测；用 spec 直填
    /// 的方式保证「base 存在」这条路只在真实环境断言形状。
    #[test]
    fn list_shape_and_empty_by_default() {
        let root = temp_root("list");
        let (result, _) = call(&root, "codepluse_instance_list", json!({}));
        assert!(result.unwrap().is_array());
    }

    #[test]
    fn main_bot_requires_object_settings() {
        let root = temp_root("mainbot");
        assert_eq!(
            call(&root, "update_main_bot_settings", json!({"settings": "no"}))
                .0
                .unwrap_err(),
            "settings 必须是对象"
        );
        // 无 provider/subscription：cfg 原样写 settings，不建实例。
        let (result, events) = call(
            &root,
            "update_main_bot_settings",
            json!({"settings": {"agent_type": "codex"}}),
        );
        let value = result.unwrap();
        assert_eq!(value["agent_type"], "codex");
        assert!(events
            .iter()
            .any(|(channel, _)| channel == "main-bot://changed"));
        let doc =
            store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
                .unwrap()
                .unwrap();
        assert_eq!(
            doc["settings"]["get_main_bot_settings"]["agent_type"],
            "codex"
        );
    }

    #[test]
    fn instance_crud_rejects_missing_base() {
        let root = temp_root("crud");
        // 本机没有的执行端：base 不存在。
        let tool = if cfg!(target_os = "windows") {
            "qoder"
        } else {
            "workbuddy"
        };
        let (result, _) = call(
            &root,
            "codepluse_instance_create",
            json!({"name": "x", "realTool": tool}),
        );
        assert!(result
            .unwrap_err()
            .starts_with("本机未安装"));
    }

    #[test]
    fn update_and_delete_report_missing_instances() {
        let root = temp_root("missing");
        assert_eq!(
            call(
                &root,
                "codepluse_instance_update",
                json!({"agentId": "agt_inst_nope", "name": "x"})
            )
            .0
            .unwrap_err(),
            "实例不存在"
        );
        assert_eq!(
            call(
                &root,
                "codepluse_instance_delete",
                json!({"agentId": "agt_inst_nope"})
            )
            .0
            .unwrap_err(),
            "实例不存在"
        );
    }

    /// 真实生命周期（依赖本机装了 claude/codex 任一内置；CI 之外的开发机满足）。
    #[test]
    fn lifecycle_round_trip_on_installed_tool() {
        let root = temp_root("lifecycle");
        seed_provider(&root);
        let tool = if agents::pick(root.as_path(), "codex").is_some() {
            "codex"
        } else if agents::pick(root.as_path(), "claude").is_some() {
            "claude"
        } else {
            // 本机没有内置可克隆：跳过（形状已由其它用例覆盖）。
            return;
        };
        let (result, events) = call(
            &root,
            "codepluse_instance_create",
            json!({"name": " 二号 ", "realTool": tool, "providerId": "prov1",
                   "defaultModel": "gpt-5", "authMode": "api"}),
        );
        let view = result.unwrap();
        let wire = view["agent_type"].as_str().unwrap().to_string();
        assert!(wire.starts_with(&format!("inst_{tool}2")), "{wire}");
        assert!(events.iter().any(|(c, _)| c == EVENT_AGENT_CHANGED));
        // 落盘 spec。
        let specs = std::fs::read_to_string(instances_path(&root)).unwrap();
        assert!(specs.contains("prov1"), "{specs}");
        assert!(specs.contains("inst_"), "{specs}");

        // list 含该实例（非 inst_main_）。
        let list = ok(&root, "codepluse_instance_list", json!({}));
        let listed = list
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["agent_type"] == json!(wire))
            .expect("list 含新实例");
        assert_eq!(listed["name"], "二号");

        // update：改名。
        let updated = ok(
            &root,
            "codepluse_instance_update",
            json!({"agentType": wire, "name": "三号"}),
        );
        assert_eq!(updated["name"], "三号");

        // delete：remove 事件 + spec 消失。
        let (result, events) = call(
            &root,
            "codepluse_instance_delete",
            json!({"agentType": wire}),
        );
        assert_eq!(result.unwrap(), Value::Null);
        assert!(events
            .iter()
            .any(|(channel, payload)| channel == EVENT_AGENT_CHANGED
                && payload["kind"] == "remove"));
        assert!(!std::fs::read_to_string(instances_path(&root))
            .unwrap()
            .contains("prov1"));
    }
}
