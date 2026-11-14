//! The agent registry, rebuilt from its persisted sources on demand.
//!
//! Go assembles the hub once at boot (`cmd/serve/wiring.go` main + `codeg.New`):
//! built-in CLI agents seeded by probing the machine, custom ACP commands from
//! `.data/acp-agents.json`, custom agents from `Store.CustomAgents`, harness
//! instances from `.data/harness-instances.json`, and finally the per-agent
//! enabled overlay from `Store.Agents`. Since the still-Go-served commands can
//! add agents at any time, the Rust side replays the same assembly per request
//! instead of caching a hub — the observable registry stays identical.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::backend::acp::custom_install::{self, custom_def, CustomAgentDef};
use crate::backend::acp::launch;
use crate::backend::acp::wire::text;
use crate::backend::store_file;

/// Mirror of Go's `hub.Agent` (hub.go:24) — only the fields the ACP paths read.
#[derive(Clone, Debug, Default)]
pub struct Agent {
    pub id: String,
    pub agent_type: String,
    pub name: String,
    pub enabled: bool,
    pub real: bool,
    pub real_tool: String,
    pub communication: String,
    pub endpoint: String,
    pub adapter_kind: String,
    pub acp_modes: std::collections::BTreeMap<String, String>,
    pub acp_version: String,
    pub wire: String,
    pub provider_id: String,
    pub default_model: String,
    pub auth_mode: String,
    pub base_agent_type: String,
    pub max_con: i64,
    pub status: String,
    pub load: f64,
    pub trust: i64,
    pub clearance: i64,
}

impl Agent {
    /// Go `CreateAgent` defaults (hub.go:158): enabled, online, clearance 1,
    /// max-concurrency 1.
    fn with_defaults(mut self) -> Self {
        if self.clearance == 0 {
            self.clearance = 1;
        }
        if self.max_con == 0 {
            self.max_con = 1;
        }
        self.enabled = true;
        if self.status.is_empty() {
            self.status = "online".into();
        }
        self
    }
}

/// Go `real_enabled` (state.go:29): `HARNESS_HUB_REAL` != "0".
pub fn real_enabled() -> bool {
    std::env::var("HARNESS_HUB_REAL").map(|v| v != "0").unwrap_or(true)
}

/// Go `wireAgent` (server.go:130).
pub fn wire_agent(a: &Agent) -> String {
    if !a.wire.is_empty() {
        return a.wire.clone();
    }
    match a.real_tool.as_str() {
        "dsh" => "deepseek".into(),
        "workbuddy" => "custom:workbuddy".into(),
        "claude" => "claude_code".into(),
        "codex" => "codex".into(),
        _ => {
            // 账号智能体（account:<id>）的公开名是 account_<id>。
            if let Some(rest) = a.real_tool.strip_prefix("account:") {
                return format!("account_{rest}");
            }
            format!("custom:{}", a.id)
        }
    }
}

/// 账号渠道的产品与显示名（上游 wiring.go 的播种表）。
pub const ACCOUNT_CHANNELS: [(&str, &str); 4] = [
    ("buddy", "WorkBuddy 国内版"),
    ("workbuddy", "WorkBuddy 国际版"),
    ("qoder", "Qoder"),
    ("qodercn", "Qoder CN"),
];

/// 账号智能体（上游 wiring.go）：协议进程与 ACP 适配器都在仓库内，
/// 装好 `bridges/account-channels` 才会出现在选择器里。
fn account_agents(root: &Path) -> Vec<Agent> {
    let mut out = Vec::new();
    for (channel_id, name) in ACCOUNT_CHANNELS {
        let tool = format!("account:{channel_id}");
        if launch::acp_installed_rooted(root, &tool).is_empty() {
            continue;
        }
        out.push(
            Agent {
                id: format!("agt_account_{channel_id}"),
                agent_type: "account-agent".into(),
                name: name.into(),
                enabled: true,
                real: true,
                real_tool: tool.clone(),
                wire: format!("account_{channel_id}"),
                auth_mode: "subscription".into(),
                communication: "acp".into(),
                endpoint: format!("acp://{tool}"),
                adapter_kind: "ACP".into(),
                acp_modes: std::collections::BTreeMap::from([
                    ("readonly".into(), "readonly".into()),
                    ("workspace".into(), "workspace".into()),
                    ("shell".into(), "readonly".into()),
                ]),
                ..Agent::default()
            }
            .with_defaults(),
        );
    }
    out
}

/// Go `baseWire` (split.go:158): the public wire name of a built-in tool.
pub fn base_wire(tool: &str) -> String {
    match tool {
        "claude" => "claude_code".into(),
        "codex" => "codex".into(),
        "dsh" => "deepseek".into(),
        "workbuddy" => "custom:workbuddy".into(),
        _ => String::new(),
    }
}

fn json_string_map(value: Option<&Value>) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(map) = value.and_then(Value::as_object) {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    out
}

fn agent_from_json(raw: &Value) -> Agent {
    let map = raw.as_object().cloned().unwrap_or_default();
    let get = |key: &str| map.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    Agent {
        id: get("id"),
        agent_type: get("type"),
        name: get("name"),
        real_tool: get("realTool"),
        endpoint: get("endpoint"),
        acp_version: get("acpVersion"),
        wire: get("wire"),
        provider_id: get("providerId"),
        default_model: get("defaultModel"),
        auth_mode: get("authMode"),
        base_agent_type: get("baseAgentType"),
        max_con: crate::backend::acp::wire::number(map.get("maxConcurrency")),
        ..Agent::default()
    }
}

/// Go `loadCustomAcp` + `registerAcp` (wiring.go:122-215): `.data/
/// acp-agents.json` entries become real ACP agents with their own modes.
fn custom_acp_agents(root: &Path) -> Vec<Agent> {
    let Ok(Some(data)) = store_file::read_json(&root.join(".data").join("acp-agents.json")) else {
        return Vec::new();
    };
    let Some(entries) = data.as_array() else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|raw| {
            let mut a = agent_from_json(raw);
            a.real = true;
            a.communication = "acp".into();
            a.endpoint = format!("acp://custom/{}", a.id);
            a.adapter_kind = "ACP".into();
            let mut read_only = "plan".to_string();
            let mut workspace = "default".to_string();
            if let Some(v) = raw.get("readOnlyMode").and_then(Value::as_str) {
                if !v.is_empty() {
                    read_only = v.to_string();
                }
            }
            if let Some(v) = raw.get("workspaceMode").and_then(Value::as_str) {
                if !v.is_empty() {
                    workspace = v.to_string();
                }
            }
            a.acp_modes = std::collections::BTreeMap::from([
                ("readonly".into(), read_only.clone()),
                ("shell".into(), read_only),
                ("workspace".into(), workspace),
            ]);
            a.with_defaults()
        })
        .collect()
}

/// Go `loadCustomAgents` + `registerCustomAgent` (custom_agents.go:216-227):
/// `Store.CustomAgents` entries become `custom:<registryId>` wire agents.
fn custom_store_agents(root: &Path) -> Vec<Agent> {
    let Ok(Some(doc)) = store_file::read_json(&root.join("codeg-workspace.json")) else {
        return Vec::new();
    };
    let Some(defs) = doc.get("customAgents").and_then(Value::as_object) else {
        return Vec::new();
    };
    // Go iterates a map (random order); sort for a deterministic registry.
    let mut ids: Vec<&String> = defs.keys().collect();
    ids.sort();
    ids.into_iter()
        .filter_map(|rid| {
            let d = defs.get(rid)?;
            let name = d.get("name").and_then(Value::as_str).unwrap_or("");
            let version = d.get("version").and_then(Value::as_str).unwrap_or("");
            Some(
                Agent {
                    id: format!("agt_custom_{rid}"),
                    wire: format!("custom:{rid}"),
                    name: name.to_string(),
                    real: true,
                    communication: "acp".into(),
                    adapter_kind: "ACP".into(),
                    endpoint: format!("acp://custom/{rid}"),
                    acp_version: version.to_string(),
                    max_con: 4,
                    ..Agent::default()
                }
                .with_defaults(),
            )
        })
        .collect()
}

fn instance_specs(root: &Path) -> Vec<Value> {
    let Ok(Some(data)) = store_file::read_json(&root.join(".data").join("harness-instances.json"))
    else {
        return Vec::new();
    };
    data.as_array().cloned().unwrap_or_default()
}

/// Go `profileBase` (harness_connections.go:9) resolved against `agents`.
fn profile_base<'a>(agents: &'a [Arc<Agent>], tool: &str, base: &str) -> Option<&'a Arc<Agent>> {
    let want = if !base.is_empty() {
        base.to_string()
    } else {
        let wire = base_wire(tool);
        if wire.is_empty() {
            tool.to_string()
        } else {
            wire
        }
    };
    agents.iter().find(|a| {
        a.enabled && a.real && (wire_agent(a) == want || a.id == want)
    })
}

/// Go `loadInstances` + `instanceAgent` (instances.go:40-120): named copies of
/// a built-in pinning their own provider/model; entries whose tool vanished
/// are skipped.
fn instance_agents(root: &Path, agents: &mut Vec<Arc<Agent>>) {
    for spec in instance_specs(root) {
        let get = |key: &str| text(spec.get(key));
        let real_tool = get("realTool");
        let base_agent_type = get("baseAgentType");
        let Some(base) = profile_base(agents, &real_tool, &base_agent_type) else {
            continue;
        };
        let id = get("id");
        let existing = format!("agt_inst_{id}");
        if agents.iter().any(|a| a.id == existing) {
            continue;
        }
        let mut clone = (**base).clone();
        let mut name = get("name");
        let wire = get("wire");
        if name.trim().is_empty() {
            name = wire.clone();
        }
        clone.id = existing;
        clone.name = name.trim().to_string();
        clone.wire = wire;
        clone.provider_id = get("providerId");
        clone.default_model = get("defaultModel");
        clone.auth_mode = get("authMode");
        clone.base_agent_type = wire_agent(&base);
        clone.endpoint = format!("acp://{real_tool}/{id}");
        clone.load = 0.0;
        clone.max_con = 4;
        clone.status = "online".into();
        agents.push(Arc::new(clone));
    }
}

/// Go boot seeding (wiring.go:554-574): one agent per installed built-in tool.
fn builtin_agents(root: &Path) -> Vec<Agent> {
    if !real_enabled() {
        return Vec::new();
    }
    let type_name = |tool: &str| match tool {
        "codex" => "codex",
        "claude" => "claude-code",
        "dsh" => "deepseek",
        "workbuddy" => "workbuddy",
        _ => "",
    };
    let display = |tool: &str| match tool {
        "codex" => "Codex",
        "claude" => "Claude Code",
        "dsh" => "DeepSeek (DSH)",
        "workbuddy" => "WorkBuddy",
        _ => "",
    };
    let mut out = Vec::new();
    for tool in ["codex", "claude", "dsh", "workbuddy"] {
        let entry = launch::acp_installed_rooted(root, tool);
        if entry.is_empty() {
            continue;
        }
        let mut a = Agent {
            id: format!("agt_{tool}_01"),
            agent_type: type_name(tool).into(),
            name: display(tool).into(),
            real: true,
            real_tool: tool.into(),
            acp_version: launch::adapter_version(&entry),
            communication: "acp".into(),
            endpoint: format!("acp://{tool}"),
            adapter_kind: "ACP".into(),
            ..Agent::default()
        };
        if tool == "workbuddy" {
            a.acp_modes = std::collections::BTreeMap::from([
                ("readonly".into(), "plan".into()),
                ("workspace".into(), "default".into()),
                ("shell".into(), "plan".into()),
            ]);
        }
        out.push(a.with_defaults());
    }
    out
}

/// Go `applyEnabledOverrides` (agentconfig.go:102): the settings-page
/// enable/disable choices re-applied onto the registry.
fn apply_enabled_overrides(root: &Path, agents: &mut [Arc<Agent>]) {
    let Ok(Some(doc)) = store_file::read_json(&root.join("codeg-workspace.json")) else {
        return;
    };
    let Some(overlays) = doc.get("agents").and_then(Value::as_object) else {
        return;
    };
    for a in agents.iter_mut() {
        if !a.real {
            continue;
        }
        if let Some(enabled) = overlays
            .get(&wire_agent(a))
            .and_then(|ov| ov.get("enabled"))
            .and_then(Value::as_bool)
        {
            let agent = Arc::make_mut(a);
            agent.enabled = enabled;
        }
    }
}

/// The registry, in Go's boot order: built-ins → custom ACP commands →
/// custom agents → instances, then the enabled overlay.
pub fn registry(root: &Path) -> Vec<Arc<Agent>> {
    let mut agents: Vec<Arc<Agent>> = builtin_agents(root)
        .into_iter()
        .chain(account_agents(root))
        .map(Arc::new)
        .collect();
    agents.extend(custom_acp_agents(root).into_iter().map(Arc::new));
    agents.extend(custom_store_agents(root).into_iter().map(Arc::new));
    instance_agents(root, &mut agents);
    apply_enabled_overrides(root, &mut agents);
    agents
}

/// Go `pick` (server.go:121): first enabled real agent matching the wire or
/// the registry id.
pub fn pick(root: &Path, agent_type: &str) -> Option<Arc<Agent>> {
    registry(root).into_iter().find(|a| {
        a.enabled && a.real && (wire_agent(a) == agent_type || a.id == agent_type)
    })
}

/// Go `agentByType` (agentconfig.go:91): same match ignoring `enabled`.
pub fn agent_by_type(root: &Path, agent_type: &str) -> Option<Arc<Agent>> {
    registry(root).into_iter().find(|a| {
        a.real && (wire_agent(a) == agent_type || a.id == agent_type)
    })
}

/// Registry lookup for other modules (`experts.rs` skills guard).
pub fn agent_exists(root: &Path, agent_type: &str) -> bool {
    agent_by_type(root, agent_type).is_some()
}

/// Go `installedVersion` (server.go:203).
pub fn installed_version(root: &Path, a: &Agent) -> String {
    if !a.acp_version.is_empty() {
        return a.acp_version.clone();
    }
    let name = match a.real_tool.as_str() {
        "claude" => "claude-agent-acp",
        _ => "codex-acp",
    };
    if a.real_tool.is_empty() {
        return "local".into();
    }
    let home = home_dir();
    for base in [
        home.join("AppData").join("Roaming").join("npm").join("node_modules"),
        root.join("node_modules"),
    ] {
        let path = base
            .join("@agentclientprotocol")
            .join(name)
            .join("package.json");
        let Ok(body) = std::fs::read(&path) else { continue };
        let Ok(pkg) = serde_json::from_slice::<Value>(&body) else { continue };
        if let Some(v) = pkg.get("version").and_then(Value::as_str) {
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    "local".into()
}

/// Go `CustomAgentAvailable` verdict for the row builders: `Some((available,
/// known))` — `None` when the agent is not a custom definition at all.
pub fn custom_agent_available(root: &Path, a: &Agent) -> Option<(bool, bool)> {
    let d = custom_def_for_agent(root, a)?;
    Some(match custom_install::available_verdict(root, &d) {
        Some(available) => (available, true),
        None => (false, true), // invalid definition: known but not launchable
    })
}

/// Go `customDefForAgent` (custom_agents.go:127): the instance's base wire or
/// the agent's own wire resolves the custom definition, if any.
pub fn custom_def_for_agent(root: &Path, a: &Agent) -> Option<CustomAgentDef> {
    let mut wire = wire_agent(a);
    if !a.base_agent_type.is_empty() {
        wire = a.base_agent_type.clone();
    }
    custom_def(root, &wire)
}

/// Go `agentAvailable` (wiring.go:361): a live probe, not the registration
/// flag — the agent is available only when its backing launch actually exists.
pub fn agent_available(root: &Path, a: &Agent) -> bool {
    if custom_def_for_agent(root, a).is_some() {
        return custom_agent_available(root, a).map(|(available, _)| available).unwrap_or(false);
    }
    // custom ACP command instances (acp-agents.json): command must still exist
    let wire = wire_agent(a);
    if let Some(entry) = custom_acp_launch_entry(root, &a.id) {
        return !entry.command.is_empty() && std::path::Path::new(&entry.command).is_file();
    }
    let _ = wire;
    if a.real_tool.is_empty() {
        return false;
    }
    // 探测要带工作区根：账号智能体的适配器在仓库的 bridges/ 下，npm 全局包
    // 也可能只装在工作区里（与 Go 的 acpInstalled 一致）。
    !launch::acp_installed_rooted(root, &a.real_tool).is_empty()
}

/// One `.data/acp-agents.json` entry, consumed by the launch path.
#[derive(Clone, Debug, Default)]
pub struct CustomAcpEntry {
    pub command: String,
    pub args: Vec<String>,
}

pub fn custom_acp_launch_entry(root: &Path, id: &str) -> Option<CustomAcpEntry> {
    let data = store_file::read_json(&root.join(".data").join("acp-agents.json")).ok()??;
    for raw in data.as_array()? {
        if text(raw.get("id")) == id {
            let args = raw
                .get("args")
                .and_then(Value::as_array)
                .map(|list| list.iter().map(|v| text(Some(v))).collect())
                .unwrap_or_default();
            return Some(CustomAcpEntry {
                command: text(raw.get("command")),
                args,
            });
        }
    }
    None
}

/// `%USERPROFILE%` with a test seam: `HARNESS_HUB_FAKE_HOME` points probes at a
/// fixture tree (Go tests run with the real HOME; the fixture tests set this).
pub fn home_dir() -> PathBuf {
    crate::backend::acp::providers_core::home()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acp-agents-{tag}-{}",
            crate::backend::goformat::hex_id()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn write_store(root: &Path, custom_agents: Value, overlays: Value) {
        let doc = json!({"customAgents": custom_agents, "agents": overlays, "next": 1});
        fs::write(root.join("codeg-workspace.json"), doc.to_string()).unwrap();
    }

    #[test]
    fn wire_agent_maps_real_tools_and_instances() {
        let mut a = Agent::default();
        a.real_tool = "claude".into();
        assert_eq!(wire_agent(&a), "claude_code");
        a.real_tool = "codex".into();
        assert_eq!(wire_agent(&a), "codex");
        a.real_tool = "dsh".into();
        assert_eq!(wire_agent(&a), "deepseek");
        a.real_tool = "workbuddy".into();
        assert_eq!(wire_agent(&a), "custom:workbuddy");
        a.real_tool = String::new();
        a.id = "agt_x".into();
        assert_eq!(wire_agent(&a), "custom:agt_x");
        a.wire = "inst_claude2".into();
        assert_eq!(wire_agent(&a), "inst_claude2");
        assert_eq!(base_wire("claude"), "claude_code");
        assert_eq!(base_wire("dsh"), "deepseek");
        assert_eq!(base_wire("nope"), "");
    }

    #[test]
    fn builtin_seed_probes_machine_and_sets_defaults() {
        let root = temp_root("builtin");
        let agents = registry(&root);
        // Only the tools installed on this machine appear; every present one
        // carries the CreateAgent defaults and the ACP endpoint shape.
        for a in &agents {
            if a.id.starts_with("agt_") && !a.id.starts_with("agt_custom") && !a.id.starts_with("agt_inst") {
                assert!(a.enabled && a.real && a.status == "online");
                assert_eq!(a.endpoint, format!("acp://{}", a.real_tool));
                assert_eq!(a.adapter_kind, "ACP");
                if a.real_tool == "workbuddy" {
                    assert_eq!(a.acp_modes.get("readonly").map(String::as_str), Some("plan"));
                    assert_eq!(a.acp_modes.get("workspace").map(String::as_str), Some("default"));
                }
            }
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn custom_and_instance_sources_assemble_in_boot_order() {
        let root = temp_root("assemble");
        write_store(
            &root,
            json!({"myagent": {"registryId": "myagent", "name": "My Agent", "version": "1.2.3"}}),
            json!({"codex": {"enabled": false}}),
        );
        fs::write(
            root.join(".data").join("harness-instances.json"),
            json!([{"id": "abc123", "wire": "inst_claude2", "name": "Claude 2号", "realTool": "claude",
                    "providerId": "", "defaultModel": "glm-5", "authMode": "", "baseAgentType": "claude_code"}])
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join(".data").join("acp-agents.json"),
            json!([{"id": "agt_acp_x", "name": "Custom ACP", "type": "acp", "command": "node",
                    "args": ["x.js"], "readOnlyMode": "", "workspaceMode": "workspace-write"}])
            .to_string(),
        )
        .unwrap();

        let agents = registry(&root);
        let find = |pred: &dyn Fn(&Agent) -> bool| agents.iter().find(|a| pred(a)).cloned();

        // custom agent from the store
        let custom = find(&|a: &Agent| a.wire == "custom:myagent").expect("custom agent");
        assert_eq!(custom.id, "agt_custom_myagent");
        assert_eq!(custom.name, "My Agent");
        assert_eq!(custom.acp_version, "1.2.3");
        assert_eq!(custom.max_con, 4);
        assert_eq!(custom.endpoint, "acp://custom/myagent");

        // instance cloned from its base (claude built-in may be absent on this
        // machine — the spec is skipped in that case, matching Go).
        if let Some(inst) = find(&|a: &Agent| a.wire == "inst_claude2") {
            assert_eq!(inst.id, "agt_inst_abc123");
            assert_eq!(inst.name, "Claude 2号");
            assert_eq!(inst.default_model, "glm-5");
            assert_eq!(inst.base_agent_type, "claude_code");
            assert_eq!(inst.endpoint, "acp://claude/abc123");
            assert_eq!(inst.max_con, 4);
            assert_eq!(inst.real_tool, "claude");
        } else {
            assert!(
                find(&|a: &Agent| a.real_tool == "claude").is_none(),
                "instance must be skipped exactly when the base is absent"
            );
        }

        // custom ACP command entry
        if let Some(acp) = find(&|a: &Agent| a.id == "agt_acp_x") {
            assert!(acp.real);
            assert_eq!(acp.acp_modes.get("workspace").map(String::as_str), Some("workspace-write"));
            assert_eq!(acp.acp_modes.get("readonly").map(String::as_str), Some("plan"));
        }

        // enabled overlay flips the codex built-in
        if let Some(codex) = find(&|a: &Agent| a.real_tool == "codex") {
            assert!(!codex.enabled, "overlay enabled=false must flip the built-in");
            assert!(pick(&root, "codex").is_none());
            assert!(agent_by_type(&root, "codex").is_some(), "agentByType ignores enabled");
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn installed_version_probes_npm_tree_and_falls_back() {
        let root = temp_root("version");
        let mut a = Agent::default();
        a.real_tool = "claude".into();
        // The probe walks global npm first, then the workspace tree; whichever
        // answers, the result is a concrete version string (this machine has
        // the adapter installed, so "local" is not assertable here).
        assert!(!installed_version(&root, &a).is_empty());
        a.real_tool = String::new();
        assert_eq!(installed_version(&root, &a), "local");
        a.acp_version = "9.9.9".into();
        assert_eq!(installed_version(&root, &a), "9.9.9");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn account_agents_are_seeded_when_the_bridge_is_installed() {
        let root = temp_root("account-agents");
        // 没装协议进程 → 选择器里没有账号智能体。
        assert!(account_agents(&root).is_empty());
        let dir = root.join("bridges").join("account-channels");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("acp-agent.mjs"), "//").unwrap();
        let seeded = account_agents(&root);
        assert_eq!(seeded.len(), 4);
        assert_eq!(seeded[0].real_tool, "account:buddy");
        assert_eq!(seeded[0].wire, "account_buddy");
        assert_eq!(seeded[0].auth_mode, "subscription");
        assert_eq!(seeded[0].endpoint, "acp://account:buddy");
        assert_eq!(seeded[0].acp_modes.get("shell").map(|s| s.as_str()), Some("readonly"));
        assert_eq!(
            seeded.iter().map(|a| a.wire.clone()).collect::<Vec<_>>(),
            vec![
                "account_buddy",
                "account_workbuddy",
                "account_qoder",
                "account_qodercn"
            ]
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn account_tools_map_to_their_public_wire() {
        let mut a = Agent::default();
        a.real_tool = "account:workbuddy".into();
        assert_eq!(wire_agent(&a), "account_workbuddy");
        a.wire = "inst_main_account_workbuddy_abc".into();
        assert_eq!(wire_agent(&a), "inst_main_account_workbuddy_abc");
    }
}
