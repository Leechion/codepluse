//! The group-C command surface: dispatch + the `acp_*` handlers, ported from
//! Go's `codeg/server.go` dispatch, `connections.go`, `preflight.go`,
//! `agentconfig.go`, `custom_launch.go` (install commands) and `agents()`
//! (server.go:148).
//!
//! Ownership split with `experts.rs`: `acp_list_agent_skills` and
//! `acp_list_agent_runtime_skills` stay in experts.rs (the skills machinery
//! lives there); everything else group-C answers here.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};

use crate::backend::acp::agents::{self, Agent};
use crate::backend::acp::connections::{EmitSink, Layer};
use crate::backend::acp::custom_install;
use crate::backend::acp::splits;
use crate::backend::acp::wire::{nil_if_empty, number, text};
use crate::backend::store_file;

/// The group-C commands this module answers (skills queries live in experts).
pub fn owns(command: &str) -> bool {
    crate::backend::acp::pi_trust::owns(command)
        || crate::backend::acp::pi_config::owns(command)
        || crate::backend::acp::diagnostics::owns(command)
        || crate::backend::acp::native_commands::owns(command)
        || matches!(
            command,
            "acp_connect"
                | "acp_disconnect"
                | "acp_prompt"
                | "acp_cancel"
                | "acp_fork"
                | "acp_get_session_snapshot"
                | "acp_get_session_snapshot_by_conversation"
                | "acp_find_connection_for_conversation"
                | "acp_respond_permission"
                | "acp_respond_task_split"
                | "acp_set_mode"
                | "acp_set_config_option"
                | "acp_stop_async_task"
                | "acp_touch_connection"
                | "acp_preflight"
                | "acp_list_connections"
                | "acp_list_agents"
                | "acp_get_agent_status"
                | "acp_goal_control"
                | "acp_describe_agent_options"
                | "acp_detect_agent_local_version"
                | "acp_update_agent_config"
                | "acp_update_agent_preferences"
                | "acp_reorder_agents"
                | "acp_update_agent_env"
                | "acp_answer_question"
                | "submit_session_feedback"
                | "acp_prepare_npx_agent"
                | "acp_uninstall_agent"
                | "acp_clear_binary_cache"
                | "acp_download_agent_binary"
        )
}

static LAYER: OnceLock<Arc<Layer>> = OnceLock::new();

/// 已建好的连接层（未被任何 acp 命令触发过时为空）——收尾批的
/// mcp 状态与连接退役在请求路径之外复用同一层。
pub fn layer_if_ready() -> Option<Arc<Layer>> {
    LAYER.get().cloned()
}

/// 启动期就把连接层建起来：工作任务的执行引擎（收尾批）要在请求路径之外
/// 直接调 connect/prompt/cancel，不能等某个 acp 命令先把层建出来。
pub fn init_shared_layer(root: PathBuf, base_url: String, sink: EmitSink) {
    let _ = LAYER.set(crate::backend::acp::connections::init_layer(
        root, base_url, sink,
    ));
}

/// Dispatch one group-C command. `sink` forwards WS frames into the front
/// door's relay; the layer is built once per process.
pub async fn dispatch(
    root: PathBuf,
    base_url: String,
    sink: EmitSink,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    let layer =
        LAYER.get_or_init(|| crate::backend::acp::connections::init_layer(root, base_url, sink));
    if let Some(result) = crate::backend::acp::pi_trust::handle(&layer.root, command, args) {
        return result;
    }
    if let Some(result) = crate::backend::acp::pi_config::handle(&layer.root, command, args) {
        return result;
    }
    if let Some(result) = crate::backend::acp::diagnostics::handle(&layer.root, command, args) {
        return result;
    }
    if crate::backend::acp::native_commands::owns(command) {
        let root = layer.root.clone();
        let command = command.to_string();
        let args = args.clone();
        return tokio::task::spawn_blocking(move || {
            crate::backend::acp::native_commands::handle(&root, &command, &args)
                .unwrap_or_else(|| Err("ACP 本地命令未注册".into()))
        })
        .await
        .map_err(|e| format!("ACP 本地命令失败：{e}"))?;
    }
    match command {
        "acp_connect" => Ok(json!(layer.connect(args).await?)),
        "acp_disconnect" => {
            layer.disconnect(&text(args.get("connectionId"))).await?;
            Ok(Value::Null)
        }
        "acp_prompt" => {
            layer.prompt(args).await?;
            Ok(Value::Null)
        }
        "acp_cancel" => {
            layer.cancel(&text(args.get("connectionId"))).await?;
            Ok(Value::Null)
        }
        "acp_touch_connection" => {
            let state = layer.state.lock().unwrap();
            Ok(json!(state
                .connections
                .contains_key(&text(args.get("connectionId")))))
        }
        "acp_list_connections" => Ok(json!(layer.connection_list())),
        "acp_get_session_snapshot" => Ok(layer.snapshot(&text(args.get("connectionId"))).await),
        "acp_get_session_snapshot_by_conversation" => {
            let cid = layer.find_connection_for_conversation(number(args.get("conversationId")));
            match cid {
                Some(cid) => Ok(layer.snapshot(&cid).await),
                None => Ok(Value::Null),
            }
        }
        "acp_find_connection_for_conversation" => {
            let cid = layer.find_connection_for_conversation(number(args.get("conversationId")));
            match cid {
                Some(cid) => {
                    let event_seq = layer
                        .snapshot(&cid)
                        .await
                        .get("event_seq")
                        .cloned()
                        .unwrap_or(Value::Null);
                    Ok(json!({"connection_id": cid, "event_seq": event_seq}))
                }
                None => Ok(Value::Null),
            }
        }
        "acp_respond_permission" => {
            let choice = text(args.get("optionId"));
            layer
                .runtime()
                .respond_permission(&text(args.get("requestId")), Some(&choice))?;
            Ok(Value::Null)
        }
        "submit_session_feedback" => Ok(layer.submit_feedback(args).await?),
        "acp_answer_question" => {
            layer.answer_question(args);
            Ok(Value::Null)
        }
        "acp_respond_task_split" => layer.respond_task_split(args).await,
        "acp_set_mode" => {
            let c = layer
                .connection(&text(args.get("connectionId")))
                .ok_or("连接不存在")?;
            let (runtime_id, cid) = {
                let guard = c.lock().unwrap();
                (guard.runtime_id.clone(), guard.id.clone())
            };
            layer
                .runtime()
                .set_mode(&runtime_id, &text(args.get("modeId")))
                .await?;
            layer.publish_session(&cid).await;
            Ok(Value::Null)
        }
        "acp_set_config_option" => {
            let c = layer
                .connection(&text(args.get("connectionId")))
                .ok_or("连接不存在")?;
            let key = text(args.get("configId"));
            let (runtime_id, cid) = {
                let guard = c.lock().unwrap();
                (guard.runtime_id.clone(), guard.id.clone())
            };
            layer
                .runtime()
                .configure(&runtime_id, &key, args.get("value").unwrap_or(&Value::Null))
                .await?;
            if key == "model" || key == "mode" {
                let mut guard = c.lock().unwrap();
                if key == "model" {
                    guard.options.model = text(args.get("value"));
                } else {
                    guard.options.mode = text(args.get("value"));
                }
            }
            layer.publish_session(&cid).await;
            Ok(Value::Null)
        }
        "acp_goal_control" => {
            let runtime_id = layer.runtime_id_of(&text(args.get("connectionId")))?;
            layer
                .runtime()
                .goal_control(&runtime_id, &text(args.get("action")))
                .await?;
            Ok(Value::Null)
        }
        "acp_stop_async_task" => {
            let runtime_id = layer.runtime_id_of(&text(args.get("connectionId")))?;
            Ok(layer
                .runtime()
                .stop_async_task(&runtime_id, &text(args.get("taskId")))
                .await?)
        }
        "acp_list_agents" => Ok(json!(agents_rows(&layer.root))),
        "acp_reorder_agents" => {
            reorder_agents(&layer.root, args)?;
            Ok(Value::Null)
        }
        "acp_get_agent_status" => {
            let want = text(args.get("agentType"));
            for row in agents_rows(&layer.root) {
                if text(row.get("agent_type")) == want {
                    return Ok(row);
                }
            }
            Ok(json!({
                "agent_type": want, "available": false, "enabled": false,
                "installed_version": Value::Null, "is_acp_adapter": true,
            }))
        }
        "acp_detect_agent_local_version" => {
            match agents::pick(&layer.root, &text(args.get("agentType"))) {
                Some(agent) => Ok(json!(agents::installed_version(&layer.root, &agent))),
                None => Ok(Value::Null),
            }
        }
        "acp_preflight" => preflight(&layer.root, &text(args.get("agentType"))),
        "acp_describe_agent_options" => describe_agent_options(&layer, args).await,
        "acp_update_agent_env" => {
            let live = update_agent_settings(&layer.root, &layer, args, true)?;
            Ok(json!(live))
        }
        "acp_update_agent_config" => {
            let live = update_agent_settings(&layer.root, &layer, args, false)?;
            Ok(json!(live))
        }
        "acp_update_agent_preferences" => {
            // Settings UI saves env, enablement and provider selection together
            // with optional provider-specific config. Persist both halves
            // through the same overlay writer, then report the active-session
            // count once (the value is stable across these two writes).
            let live = update_agent_settings(&layer.root, &layer, args, true)?;
            update_agent_settings(&layer.root, &layer, args, false)?;
            Ok(json!(live))
        }
        "acp_prepare_npx_agent"
        | "acp_uninstall_agent"
        | "acp_clear_binary_cache"
        | "acp_download_agent_binary" => custom_install_command(&layer, command, args).await,
        _ => Err(format!("Go 移植尚未实现此接口：{command}")),
    }
}

// ----------------------------- agents() rows -----------------------------

/// Go `mergeAgentOverlay` (agentconfig.go:143): fold the persisted overlay row
/// onto an agent-list entry.
fn merge_agent_overlay(overlay: &Value, wire: &str, row: &mut Value) {
    let Some(ov) = overlay.get(wire) else { return };
    if let Some(env) = ov.get("env") {
        if env.is_object() {
            row["env"] = env.clone();
        }
    }
    for key in [
        "config_json",
        "codex_auth_json",
        "codex_config_toml",
        "codex_model_catalog",
        "system_instructions_content",
        "system_instructions_file_path",
        "model_provider_id",
    ] {
        if let Some(v) = ov.get(key) {
            row[key] = v.clone();
        }
    }
}

/// Go `agents()` (server.go:148): the settings-page agent list, overlay-merged.
pub fn agents_rows(root: &Path) -> Vec<Value> {
    let doc = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
        .ok()
        .flatten()
        .unwrap_or(Value::Null);
    let overlay = doc.get("agents").cloned().unwrap_or(json!({}));
    let mut out: Vec<Value> = Vec::new();
    for agent in agents::registry(root) {
        if !agent.real {
            continue;
        }
        let wire = agents::wire_agent(&agent);
        let available = agents::agent_available(root, &agent);
        let mut row = json!({
            "agent_type": wire,
            "name": agent.name,
            "description": format!("ACP · {}", agent.name),
            "available": available,
            "enabled": agent.enabled,
            "registry_id": wire,
            "registry_version": Value::Null,
            "skills_capable": true,
            "supports_custom_version": false,
            "distribution_type": "system",
            "is_acp_adapter": !agent.real_tool.is_empty(),
            "custom_source": Value::Null,
            "installed_version": agents::installed_version(root, &agent),
            "sort_order": out.len(),
            "env": {},
            "config_json": Value::Null,
            "config_file_path": Value::Null,
            "codex_auth_json": Value::Null,
            "codex_config_toml": Value::Null,
            "codex_model_catalog": Value::Null,
            "system_instructions_content": Value::Null,
            "system_instructions_file_path": Value::Null,
            "model_provider_id": Value::Null,
            "host_tools_agent_mode": true,
            // 实例只认 `inst_` 前缀：账号智能体的 wire 是 account_<id>，
            // 它不是实例（上游 server.go 的同一处改动）。
            "instance": agent.wire.starts_with("inst_"),
            "real_tool": agent.real_tool,
            "provider_id": agent.provider_id,
            "default_model": agent.default_model,
        });
        merge_agent_overlay(&overlay, &wire, &mut row);
        // Go injects the collaboration MCP alongside each harness's native
        // file/command tools.
        row["host_tools_supports_delegation"] = json!(true);
        row["auth_mode"] = json!(agent.auth_mode);
        row["base_agent_type"] = json!(agent.base_agent_type);
        if let Some(d) = agents::custom_def_for_agent(root, &agent) {
            let (available, _) =
                agents::custom_agent_available(root, &agent).unwrap_or((false, true));
            row["available"] = json!(available);
            row["instance"] = json!(agent.wire.starts_with("inst_"));
            row["description"] = json!(d.description);
            row["registry_id"] = json!(d.registry_id);
            row["registry_version"] = Value::Null;
            if d.source == "registry" && !d.version.is_empty() {
                row["registry_version"] = json!(d.version);
            }
            row["custom_source"] = json!(d.source);
            row["icon_url"] = d.icon_url.clone().map(Value::String).unwrap_or(Value::Null);
            row["distribution_type"] = json!(d.kind);
            row["is_acp_adapter"] = json!(false);
            row["skills_capable"] = json!(d.skills_shared_store || d.skills_dir.is_some());
            row["host_tools_supports_delegation"] = json!(d.supports_mcp);
            row["installed_version"] = custom_install::custom_installed_version(root, &d);
        }
        out.push(row);
    }
    let order = doc
        .get("settings")
        .and_then(|settings| settings.get("acp_agent_order"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .enumerate()
                .map(|(index, item)| (item.to_string(), index))
                .collect::<std::collections::HashMap<_, _>>()
        })
        .unwrap_or_default();
    out.sort_by_key(|row| {
        let wire = text(row.get("agent_type"));
        order.get(&wire).copied().unwrap_or(usize::MAX)
    });
    for (index, row) in out.iter_mut().enumerate() {
        row["sort_order"] = json!(index);
    }
    out
}

fn reorder_agents(root: &Path, args: &Value) -> Result<(), String> {
    let items = args
        .get("agentTypes")
        .and_then(Value::as_array)
        .ok_or("智能体排序必须是数组")?;
    let mut seen = std::collections::HashSet::new();
    let mut order = Vec::with_capacity(items.len());
    for item in items {
        let value = item.as_str().ok_or("智能体类型必须是文本")?;
        if value.trim().is_empty() || !seen.insert(value.to_string()) {
            return Err("智能体排序包含空值或重复项".into());
        }
        order.push(value.to_string());
    }
    let path = root.join(".data").join("codeg-workspace.json");
    let _guard = store_file::lock(&path)?;
    let mut doc = store_file::read_json(&path)?.unwrap_or_else(|| json!({}));
    let object = doc.as_object_mut().ok_or("工作区存储格式无效")?;
    let settings = object.entry("settings").or_insert_with(|| json!({}));
    settings
        .as_object_mut()
        .ok_or("工作区设置格式无效")?
        .insert("acp_agent_order".into(), json!(order));
    store_file::write_bytes(&path, &crate::backend::goformat::marshal_store(&doc))
}

// ----------------------------- preflight -----------------------------

/// Go `adapterIdentity` (preflight.go:98).
fn adapter_identity(
    real_tool: &str,
) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    match real_tool {
        "claude" => Some((
            "claude-agent-acp",
            "claude",
            "Claude Code CLI",
            "https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp",
        )),
        "codex" => Some((
            "codex-acp",
            "codex",
            "Codex CLI",
            "https://www.npmjs.com/package/@agentclientprotocol/codex-acp",
        )),
        _ => None,
    }
}

/// Go `adapterLookup` (preflight.go:152): installed adapter version + package
/// directory, in the same search order as installedVersion.
fn adapter_lookup(root: &Path, real_tool: &str) -> Option<(String, PathBuf)> {
    let name = match real_tool {
        "claude" => "claude-agent-acp",
        "codex" => "codex-acp",
        _ => return None,
    };
    for base in [
        agents::home_dir()
            .join("AppData")
            .join("Roaming")
            .join("npm")
            .join("node_modules"),
        root.join("node_modules"),
    ] {
        let pkg_dir = base.join("@agentclientprotocol").join(name);
        if let Ok(body) = std::fs::read(pkg_dir.join("package.json")) {
            if let Ok(pkg) = serde_json::from_slice::<Value>(&body) {
                let version = pkg
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                return Some((version, pkg_dir));
            }
        }
    }
    None
}

/// Go `nativeConfigDir` (preflight.go:137).
fn native_config_dir(adapter_name: &str) -> String {
    let home = agents::home_dir();
    if adapter_name == "claude-agent-acp" {
        home.join(".claude").to_string_lossy().to_string()
    } else {
        home.join(".codex").to_string_lossy().to_string()
    }
}

/// Go `preflight` (preflight.go:27): the settings panel's launch-readiness
/// probe for one agent.
pub fn preflight(root: &Path, agent_type: &str) -> Result<Value, String> {
    let Some(ag) = agents::pick(root, agent_type) else {
        return Ok(json!({
            "agent_type": agent_type,
            "agent_name": agent_type,
            "passed": false,
            "checks": [{"check_id": "system_cli", "label": "Launch target", "status": "fail",
                        "message": "此智能体未注册或已停用。", "fixes": []}],
            "adapter": Value::Null,
        }));
    };
    let mut adapter: Value = Value::Null;
    let mut checks: Vec<Value> = Vec::new();
    if let Some((name, native_cmd, native_label, docs_url)) = adapter_identity(&ag.real_tool) {
        match adapter_lookup(root, &ag.real_tool) {
            Some((version, entry_dir)) => {
                let mut pkg = format!("@agentclientprotocol/{name}");
                if !version.is_empty() && version != "local" {
                    pkg.push_str(&format!("@{version}"));
                }
                let native_path = crate::backend::acp::launch::resolve_bin(native_cmd);
                adapter = json!({
                    "adapter_package": pkg,
                    "adapter_cmd": name,
                    "adapter_installed": true,
                    "native_cmd": native_cmd,
                    "native_label": native_label,
                    "native_path": if native_path.is_empty() { Value::Null } else { json!(native_path) },
                    "native_required": false,
                    "shared_config_dir": native_config_dir(name),
                    "docs_url": docs_url,
                });
                checks.push(json!({
                    "check_id": "system_cli", "label": "Launch target", "status": "pass",
                    "message": format!("找到 ACP 适配器 {}，启动时将运行它。", entry_dir.to_string_lossy()),
                    "fixes": [],
                }));
            }
            None => {
                let pkg = format!("@agentclientprotocol/{name}");
                adapter = json!({
                    "adapter_package": pkg,
                    "adapter_cmd": name,
                    "adapter_installed": false,
                    "native_cmd": native_cmd,
                    "native_label": native_label,
                    "native_path": match crate::backend::acp::launch::resolve_bin(native_cmd) {
                        p if p.is_empty() => Value::Null,
                        p => json!(p),
                    },
                    "native_required": false,
                    "shared_config_dir": native_config_dir(name),
                    "docs_url": docs_url,
                });
                checks.push(json!({
                    "check_id": "system_cli", "label": "Launch target", "status": "fail",
                    "message": format!("尚未安装 ACP 适配器 @agentclientprotocol/{name}，下方可安装。"),
                    "fixes": [{"label": "了解更多", "kind": "open_url", "payload": docs_url}],
                }));
            }
        }
    } else {
        let available = agents::agent_available(root, &ag);
        checks.push(json!({
            "check_id": "system_cli", "label": "Launch target",
            "status": if available { "pass" } else { "fail" },
            "message": if available { "运行环境已就绪。" } else { "未找到可启动的运行环境。" },
            "fixes": [],
        }));
    }
    let passed = agents::agent_available(root, &ag);
    Ok(json!({
        "agent_type": agents::wire_agent(&ag),
        "agent_name": ag.name,
        "passed": passed,
        "checks": checks,
        "adapter": adapter,
    }))
}

// ----------------------------- describe / update -----------------------------

/// Go `acp_describe_agent_options` (server.go:519): connect a probe session
/// and report its modes/config options/prompt capabilities.
async fn describe_agent_options(layer: &Arc<Layer>, args: &Value) -> Result<Value, String> {
    let agent = agents::pick(&layer.root, &text(args.get("agentType"))).ok_or("执行端不可用")?;
    let cwd = crate::backend::acp::connections::resolve_workdir(
        &layer.root,
        &text(args.get("workingDir")),
    )?;
    let permission = if agent.real_tool == "dsh" {
        "workspace"
    } else {
        "readonly"
    };
    let rid = layer
        .runtime()
        .connect(
            &agent,
            crate::backend::acp::runtime::Options {
                session_key: format!("probe:{}", crate::backend::goformat::hex_id()),
                permission: permission.into(),
                workdir: cwd.to_string_lossy().to_string(),
                ..Default::default()
            },
        )
        .await?;
    let view = layer.runtime_view(&rid).await;
    Ok(json!({
        "modes": crate::backend::acp::wire::mode_wire(view.get("modes").unwrap_or(&Value::Null)),
        "config_options": crate::backend::acp::wire::config_wire(&view),
        "available_commands": [],
        "unapplied_config_ids": [],
        "prompt_capabilities": layer.runtime().prompt_capabilities(&rid),
    }))
}

/// Go `updateAgentSettings` (agentconfig.go:25): write the env or config half
/// of the per-agent overlay; return the live-session count.
fn update_agent_settings(
    root: &Path,
    layer: &Arc<Layer>,
    args: &Value,
    is_env: bool,
) -> Result<i64, String> {
    let agent_type = text(args.get("agentType"));
    let ag = agents::agent_by_type(root, &agent_type)
        .ok_or_else(|| format!("智能体不存在：{agent_type}"))?;
    let wire = agents::wire_agent(&ag);

    let path = root.join(".data").join("codeg-workspace.json");
    let guard = store_file::lock(&path)?;
    let mut doc = store_file::read_json(&path)?.unwrap_or(json!({}));
    let agents_map = doc
        .as_object_mut()
        .ok_or("工作区存储格式无效")?
        .entry("agents".to_string())
        .or_insert_with(|| json!({}));
    let ov = agents_map
        .as_object_mut()
        .unwrap()
        .entry(wire.clone())
        .or_insert_with(|| json!({}));
    let ov = ov.as_object_mut().unwrap();
    if is_env {
        let env = args.get("env").cloned().unwrap_or(Value::Null);
        let mut env_map = serde_json::Map::new();
        if let Some(map) = env.as_object() {
            for (k, v) in map {
                let value = match v {
                    Value::String(s) => Value::String(s.clone()),
                    other => Value::String(other.to_string()),
                };
                env_map.insert(k.clone(), value);
            }
        }
        ov.insert("env".into(), Value::Object(env_map));
        if let Some(enabled) = args.get("enabled").and_then(Value::as_bool) {
            ov.insert("enabled".into(), json!(enabled));
        }
        if let Some(mp) = args.get("modelProviderId") {
            let value = match mp {
                Value::Number(n) => json!(n.as_f64().map(|f| f.trunc() as i64).unwrap_or(0)),
                Value::Null => Value::Null,
                _ => Value::Null,
            };
            ov.insert("model_provider_id".into(), value);
        }
    } else {
        for (payload, key) in [
            ("configJson", "config_json"),
            ("codexAuthJson", "codex_auth_json"),
            ("codexConfigToml", "codex_config_toml"),
            ("codexModelCatalog", "codex_model_catalog"),
            ("systemInstructionsContent", "system_instructions_content"),
            (
                "systemInstructionsFilePath",
                "system_instructions_file_path",
            ),
        ] {
            if let Some(v) = args.get(payload) {
                ov.insert(key.into(), json!(text(Some(v))));
            }
        }
    }
    let body = crate::backend::goformat::marshal_store(&doc);
    store_file::write_bytes(&path, &body)?;
    drop(guard);

    // live = connections on that wire, excluding warm ones
    let mut live = 0i64;
    let state = layer.state.lock().unwrap();
    for conn in state.connections.values() {
        let c = conn.lock().unwrap();
        if c.agent_type == wire && !c.warm {
            live += 1;
        }
    }
    Ok(live)
}

// ----------------------------- custom install commands -----------------------------

async fn custom_install_command(
    layer: &Arc<Layer>,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    let agent_type = text(args.get("agentType"));
    let wire = if agent_type.starts_with("custom:") {
        agent_type.clone()
    } else {
        format!("custom:{agent_type}")
    };
    let d = custom_install::custom_def(&layer.root, &wire)
        .ok_or_else(|| format!("自定义智能体不存在：{}", d_name(&wire)))?;
    let task_id = text(args.get("taskId"));
    layer.emit_event(
        "app://agent-install",
        &json!({"task_id": task_id, "agent_type": format!("custom:{}", d.registry_id),
                "kind": "started", "payload": format!("正在准备 {}", d.name)}),
    );
    // npm/uv/archive installers block for minutes (Go runs them on a
    // goroutine) — the whole body runs on the blocking pool.
    let outcome: Result<Value, String> = tokio::task::spawn_blocking({
        let layer = layer.clone();
        let d = d.clone();
        let args = args.clone();
        let command = command.to_string();
        move || {
            if !text(args.get("version")).is_empty() && text(args.get("version")) != d.version {
                return Err(
                    "自定义智能体的版本由发行版 spec 固定，请先编辑配置中的包版本或下载地址".into(),
                );
            }
            // Go: `command == uninstall || command == clear_binary_cache || a["cleanFirst"] == true`
            // ——下载链路只在显式 cleanFirst 时清缓存。
            let clean = command != "acp_download_agent_binary"
                && (command == "acp_prepare_npx_agent"
                    || args
                        .get("cleanFirst")
                        .and_then(Value::as_bool)
                        .unwrap_or(false));
            if command == "acp_uninstall_agent" || command == "acp_clear_binary_cache" || clean {
                // disconnect the agent's connections first; a busy one blocks
                let mut cids: Vec<String> = Vec::new();
                let mut busy = false;
                {
                    let state = layer.state.lock().unwrap();
                    for conn in state.connections.values() {
                        let c = conn.lock().unwrap();
                        if c.agent_type == format!("custom:{}", d.registry_id) {
                            if c.busy {
                                busy = true;
                            }
                            cids.push(c.id.clone());
                        }
                    }
                }
                if busy {
                    return Err("此智能体仍在执行任务，暂不能清除安装目录".into());
                }
                for cid in &cids {
                    let layer = layer.clone();
                    let cid = cid.clone();
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async move {
                            layer.disconnect(&cid).await?;
                            Ok::<(), String>(())
                        })
                    })?;
                }
                custom_install::validate_custom_def(&d)?;
                let dir = custom_install::custom_cache_dir(&layer.root, &d);
                if dir.exists() {
                    std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
                }
                if command == "acp_uninstall_agent" || command == "acp_clear_binary_cache" {
                    return Ok(Value::Null);
                }
            }
            let launch = custom_install::prepare_custom(&layer.root, &d)?;
            if command == "acp_prepare_npx_agent" {
                if !launch.version.is_empty() {
                    return Ok(json!(launch.version));
                }
                return Ok(json!("local"));
            }
            Ok(Value::Null)
        }
    })
    .await
    .map_err(|e| format!("命令执行失败：{e}"))?;
    let result = outcome;
    let (kind, message) = match &result {
        Ok(_) => ("completed", "已完成".to_string()),
        Err(err) => ("failed", err.clone()),
    };
    layer.emit_event(
        "app://agent-install",
        &json!({"task_id": task_id, "agent_type": format!("custom:{}", d.registry_id),
                "kind": kind, "payload": message}),
    );
    layer.emit_event("app://acp-agents-updated", &json!({}));
    result
}

fn d_name(wire: &str) -> String {
    wire.trim_start_matches("custom:").to_string()
}
