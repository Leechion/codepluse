//! Provider catalog: the `providers` package's read/assemble surface, ported
//! from `server-go/internal/providers/{providers,ccswitch,native,writeback}.go`
//! and `internal/codeg/providermodels.go`.
//!
//! Ported (5 commands): `codepluse_providers_list` (`providers.Public`),
//! `codepluse_provider_activate` (`providers.Activate`),
//! `ccswitch_provider_config`, `codepluse_provider_models`,
//! `list_provider_models`.
//!
//! NOT ported here, with reasons:
//! - `codepluse_connection_catalog` / `codepluse_connection_save` walk the
//!   Hub agent registry (`s.agents()`), which is the hub integration unit
//!   (⑥) — the TLS unlock does not make them portable.
//! - `acp_download_agent_binary` needs the ACP connection registry plus the
//!   prepare/download cache — ACP runtime unit (⑤).
//! - `saveExternal` / writeback (ccswitch/native file writes) belong to
//!   `codepluse_provider_save`, which stays in Go on purpose (migration doc
//!   批次 2) — only `applyMetadata`'s read path is ported here.
//!
//! Sources read live from this machine: `api-providers.json`,
//! `model-providers.json`, `provider-metadata.json`, the CC Switch SQLite
//! database, and the Claude/Codex/DSH native configs. Both backends read the
//! same machine state, which is what the dual-run compares.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::backend::net;
use crate::backend::providers as modelproviders;

// ── Provider ────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub(crate) struct Provider {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub url: String,
    pub model: String,
    pub key: String,
    pub has_key: bool,
    pub source: String,
    pub cc_current: bool,
    /// Go's `json:"-"` env map — populated only for ccswitch/native sources;
    /// api-providers.json never stores it.
    pub env: BTreeMap<String, String>,
    pub models: Vec<PModel>,
    pub protocol: String,
    pub api: bool,
    pub enabled: Option<bool>,
}

#[derive(Clone, Default)]
pub(crate) struct PModel {
    pub id: String,
    pub name: String,
    pub enabled: Option<bool>,
}

/// providers.go `IsAPI`.
/// Go providers.IsAPI（供实例/连接校验复用）。
pub(crate) fn is_api(p: &Provider) -> bool {
    let Some(parts) = parse_url(&p.url) else {
        return false;
    };
    !parts.host.is_empty()
        && (parts.scheme == "http" || parts.scheme == "https")
        && parts.userinfo.is_none()
        && parts.query.is_none()
        && parts.fragment.is_none()
        && !p.key.is_empty()
        && !p.key.starts_with("sk-ant-oat")
}

/// Minimal URL split covering what Go's `url.Parse` checks here: scheme,
/// optional userinfo, host, port, query, fragment.
pub(crate) struct UrlParts {
    scheme: String,
    userinfo: Option<String>,
    host: String,
    port: String,
    query: Option<String>,
    fragment: Option<String>,
}

impl UrlParts {
    fn host_is_non_empty(&self) -> bool {
        !self.host.is_empty()
    }
}

fn parse_url(input: &str) -> Option<UrlParts> {
    let trimmed = input.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    if scheme.is_empty() || !rest.starts_with(|c: char| !c.is_whitespace()) {
        // Go rejects schemes that do not start with a letter; keep it simple —
        // an empty scheme or missing authority yields None upstream anyway.
    }
    let (before_fragment, fragment) = match rest.split_once('#') {
        Some((a, b)) => (a, Some(b.to_string())),
        None => (rest, None),
    };
    let (before_query, query) = match before_fragment.split_once('?') {
        Some((a, b)) => (a, Some(b.to_string())),
        None => (before_fragment, None),
    };
    let (authority, _path) = match before_query.find('/') {
        Some(at) => (&before_query[..at], &before_query[at..]),
        None => (before_query, ""),
    };
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((user, host)) => (Some(user.to_string()), host),
        None => (None, authority),
    };
    // Host may be an IPv6 literal in brackets.
    let (host, port) = if let Some(end) = hostport.rfind(']') {
        let host = &hostport[..=end];
        let port = hostport[end + 1..].strip_prefix(':').unwrap_or("");
        (host.to_string(), port.to_string())
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.to_string()),
            None => (hostport.to_string(), String::new()),
        }
    };
    Some(UrlParts {
        scheme: scheme.to_lowercase(),
        userinfo,
        host,
        port,
        query,
        fragment,
    })
}

// ── api-providers.json (executors) ──────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
struct ApiData {
    #[serde(default)]
    providers: Vec<ApiProviderStored>,
    #[serde(default)]
    active: BTreeMap<String, String>,
}

#[derive(serde::Deserialize, Clone, Default)]
struct ApiProviderStored {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    agent: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    key: String,
    #[serde(rename = "hasKey", default)]
    has_key: bool,
    #[serde(default)]
    source: String,
    #[serde(rename = "ccCurrent", default)]
    cc_current: bool,
    #[serde(default)]
    models: Vec<ApiModelStored>,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    api: bool,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(serde::Deserialize, Clone, Default)]
struct ApiModelStored {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    enabled: Option<bool>,
}

impl ApiProviderStored {
    fn into_provider(self) -> Provider {
        Provider {
            id: self.id,
            name: self.name,
            agent: self.agent,
            url: self.url,
            model: self.model,
            has_key: self.has_key,
            key: self.key,
            source: self.source,
            cc_current: self.cc_current,
            api: self.api,
            models: self
                .models
                .into_iter()
                .map(|m| PModel { id: m.id, name: m.name, enabled: m.enabled })
                .collect(),
            protocol: self.protocol,
            enabled: self.enabled,
            ..Provider::default()
        }
    }
}

pub(crate) fn api_providers_path(root: &Path) -> PathBuf {
    root.join(".data").join("api-providers.json")
}

fn read_api(root: &Path) -> Result<ApiData, String> {
    match crate::backend::store_file::read_json(&api_providers_path(root))? {
        Some(value) => serde_json::from_value(value)
            .map_err(|e| format!("解析 api-providers.json 失败：{e}")),
        None => Ok(ApiData::default()),
    }
}

/// providers package `write`: MarshalIndent with two spaces. serde_json's
/// pretty writer matches; field order comes from the struct declaration, and
/// the `active` map is a BTreeMap so its keys sort like Go's.
fn write_api(root: &Path, data: &ApiData) -> Result<(), String> {
    let owned: Vec<ApiProviderOwned> = data
        .providers
        .iter()
        .map(|p| ApiProviderOwned {
            id: p.id.clone(),
            name: p.name.clone(),
            agent: p.agent.clone(),
            url: p.url.clone(),
            model: p.model.clone(),
            key: p.key.clone(),
            has_key: p.has_key,
            source: p.source.clone(),
            cc_current: p.cc_current,
            models: p
                .models
                .iter()
                .map(|m| ApiModelOwned { id: m.id.clone(), name: m.name.clone(), enabled: m.enabled })
                .collect(),
            protocol: p.protocol.clone(),
            api: p.api,
            enabled: p.enabled,
        })
        .collect();
    let document = ApiDataDocument {
        providers: owned,
        active: data.active.clone(),
        cc_error: String::new(),
    };
    let value = serde_json::to_value(&document).map_err(|e| format!("序列化失败：{e}"))?;
    crate::backend::store_file::write_json(&api_providers_path(root), &value)
}

#[derive(serde::Serialize)]
struct ApiProviderOwned {
    id: String,
    name: String,
    agent: String,
    url: String,
    model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    key: String,
    #[serde(rename = "hasKey")]
    has_key: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    source: String,
    #[serde(skip_serializing_if = "cc_current_is_false_owned")]
    cc_current: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    models: Vec<ApiModelOwned>,
    #[serde(skip_serializing_if = "String::is_empty")]
    protocol: String,
    api: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
}
fn cc_current_is_false_owned(value: &bool) -> bool {
    !*value
}
#[derive(serde::Serialize)]
struct ApiModelOwned {
    id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
}
#[derive(serde::Serialize)]
struct ApiDataDocument {
    providers: Vec<ApiProviderOwned>,
    active: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    cc_error: String,
}

// ── small value helpers (Go's text/stringValue/object on any) ───────────────

fn value_text(value: Option<&Value>) -> String {
    value.and_then(|v| v.as_str()).unwrap_or("").to_string()
}

// ── CC Switch (providers/ccswitch.go) ───────────────────────────────────────

fn cc_switch_db_path() -> Result<PathBuf, String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|e| e.to_string())?;
    Ok(Path::new(&home).join(".cc-switch").join("cc-switch.db"))
}

/// ccswitch.go `CCSwitch`: read the live database without exposing keys into
/// anything but this process's memory. Missing database is an empty list.
fn cc_switch() -> Result<Vec<Provider>, String> {
    let path = cc_switch_db_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let connection = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| e.to_string())?;
    let mut statement = connection
        .prepare(
            "SELECT id,name,settings_config,is_current FROM providers \
             WHERE app_type = 'claude' ORDER BY COALESCE(sort_index, 2147483647), name",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = statement
        .query([])
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Ok(Some(row)) = rows.next() {
        let id: String = row.get(0).map_err(|e| e.to_string())?;
        let name: String = row.get(1).map_err(|e| e.to_string())?;
        let raw: String = row.get(2).map_err(|e| e.to_string())?;
        let current: bool = row.get(3).map_err(|e| e.to_string())?;
        let cfg: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("CC Switch 供应商配置无效: {e}"))?;
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        if let Some(entries) = cfg.get("env").and_then(|v| v.as_object()) {
            for (key, value) in entries {
                if key.starts_with("ANTHROPIC_") || key == "CLAUDE_CODE_SUBAGENT_MODEL" {
                    if let Some(text) = value.as_str() {
                        env.insert(key.clone(), text.to_string());
                    }
                }
            }
        }
        let mut model = env.get("ANTHROPIC_MODEL").cloned().unwrap_or_default();
        if model.is_empty() {
            model = value_text(cfg.get("model"));
        }
        let mut key = env.get("ANTHROPIC_AUTH_TOKEN").cloned().unwrap_or_default();
        if key.is_empty() {
            key = env.get("ANTHROPIC_API_KEY").cloned().unwrap_or_default();
        }
        let mut models = claude_models(&env, &model);
        let catalog_models: Vec<Value> = cfg
            .pointer("/modelCatalog/models")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if !catalog_models.is_empty() {
            models = Vec::new();
            for m in &catalog_models {
                let model_id = value_text(m.get("model"));
                if !model_id.is_empty() {
                    models.push(PModel { id: model_id, name: value_text(m.get("displayName")), enabled: None });
                }
            }
        }
        out.push(Provider {
            id: format!("ccswitch:{id}"),
            name,
            agent: "claude".into(),
            url: env.get("ANTHROPIC_BASE_URL").cloned().unwrap_or_default(),
            model,
            key: key.clone(),
            has_key: !key.is_empty(),
            source: "ccswitch".into(),
            cc_current: current,
            env,
            protocol: "anthropic".into(),
            models,
            ..Provider::default()
        });
    }
    Ok(out)
}

/// native.go `claudeModels`: model + the three default slots, deduped.
fn claude_models(env: &BTreeMap<String, String>, model: &str) -> Vec<PModel> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for id in [
        model.to_string(),
        env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").cloned().unwrap_or_default(),
        env.get("ANTHROPIC_DEFAULT_SONNET_MODEL").cloned().unwrap_or_default(),
        env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").cloned().unwrap_or_default(),
    ] {
        if !id.is_empty() && seen.insert(id.clone()) {
            out.push(PModel { id, name: String::new(), enabled: None });
        }
    }
    out
}

// ── native configs (providers/native.go) ────────────────────────────────────

fn user_dir(env_key: &str, suffix: &str) -> PathBuf {
    if let Ok(value) = std::env::var(env_key) {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    Path::new(&home).join(suffix)
}

/// native.go `Native`: API configurations only; never touches the files.
fn native() -> Result<Vec<Provider>, String> {
    let (mut out, mut dsh_error) = match read_dsh(&user_dir("DSH_HOME", ".dsh")) {
        Ok(list) => (list, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    match read_codex(&user_dir("CODEX_HOME", ".codex")) {
        Ok(list) => out.extend(list),
        Err(error) => {
            if dsh_error.is_none() {
                dsh_error = Some(error);
            } else if let Some(existing) = dsh_error.as_mut() {
                existing.push('；');
                existing.push_str(&error);
            }
        }
    }
    match read_claude(&user_dir("CLAUDE_CONFIG_DIR", ".claude")) {
        Ok(list) => out.extend(list),
        Err(error) => {
            if dsh_error.is_none() {
                dsh_error = Some(error);
            } else if let Some(existing) = dsh_error.as_mut() {
                existing.push('；');
                existing.push_str(&error);
            }
        }
    }
    match dsh_error {
        Some(error) => Err(error),
        None => Ok(out),
    }
}

fn read_dsh(home: &Path) -> Result<Vec<Provider>, String> {
    let mut credentials_refs: BTreeMap<String, String> = BTreeMap::new();
    if let Ok(data) = std::fs::read_to_string(home.join(".credentials.yaml")) {
        if let Ok(value) = serde_yaml::from_str::<Value>(&data) {
            if let Some(refs) = value.get("refs").and_then(|v| v.as_object()) {
                for (key, value) in refs {
                    if let Some(text) = value.as_str() {
                        credentials_refs.insert(key.clone(), text.to_string());
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for profile in ["desktop", "acp"] {
        let file = home.join("profiles").join(profile).join("cordis.patch.yml");
        let Ok(data) = std::fs::read_to_string(&file) else {
            continue; // 缺文件 = 没有该 profile，与 Go 的 IsNotExist 一致
        };
        let yaml: serde_yaml::Value = serde_yaml::from_str(&data).map_err(|_| "DSH 配置格式无效".to_string())?;
        let plugins: Value = serde_json::to_value(yaml).map_err(|_| "DSH 配置格式无效".to_string())?;
        let Some(plugins) = plugins.as_array() else {
            return Err("DSH 配置格式无效".into());
        };
        let mut selected_provider = String::new();
        let mut selected_model = String::new();
        for plugin in plugins {
            let id = value_text(plugin.get("id"));
            if id == "agent-default-model" || id == "acp" {
                let config = plugin.get("config");
                selected_provider = value_text(config.and_then(|c| c.get("provider")));
                selected_model = value_text(config.and_then(|c| c.get("model")));
            }
        }
        for plugin in plugins {
            if value_text(plugin.get("id")) != "llm-pi-ai" {
                continue;
            }
            let Some(configs) = plugin
                .get("config")
                .and_then(|c| c.get("providers"))
                .and_then(|v| v.as_object())
            else {
                continue;
            };
            let mut ids: Vec<String> = configs.keys().cloned().collect();
            ids.sort();
            for id in ids {
                let cfg = configs.get(id.as_str());
                let base = value_text(cfg.as_ref().and_then(|c| c.get("baseURL")));
                if base.is_empty() {
                    continue;
                }
                let mut key = value_text(cfg.as_ref().and_then(|c| c.get("apiKey")));
                let reference = value_text(cfg.as_ref().and_then(|c| c.get("apiKeyEnv")));
                if !reference.is_empty() {
                    key = std::env::var(&reference).unwrap_or_default();
                    if key.is_empty() {
                        key = credentials_refs.get(&reference).cloned().unwrap_or_default();
                    }
                }
                let mut models = Vec::new();
                if let Some(raw) = cfg.as_ref().and_then(|c| c.get("models")).and_then(|v| v.as_array()) {
                    for value in raw {
                        let model_id = value_text(value.get("id"));
                        if !model_id.is_empty() {
                            models.push(PModel { id: model_id, name: value_text(value.get("name")), enabled: None });
                        }
                    }
                }
                let mut model = if id == selected_provider { selected_model.clone() } else { String::new() };
                if model.is_empty() {
                    if let Some(first) = models.first() {
                        model = first.id.clone();
                    }
                }
                let api = value_text(cfg.as_ref().and_then(|c| c.get("api")));
                let protocol = if api == "openai-responses" {
                    "openai-responses"
                } else if api.starts_with("anthropic") {
                    "anthropic"
                } else {
                    "openai"
                };
                let mut name = value_text(cfg.as_ref().and_then(|c| c.get("name")));
                if name.is_empty() {
                    name = format!("{id} · DSH {profile}");
                }
                out.push(Provider {
                    id: format!("native:dsh:{profile}:{id}"),
                    name,
                    agent: "dsh".into(),
                    url: base,
                    key: key.clone(),
                    has_key: !key.is_empty(),
                    source: "dsh".into(),
                    model,
                    models,
                    protocol: protocol.into(),
                    ..Provider::default()
                });
            }
        }
    }
    Ok(out)
}

fn read_claude(home: &Path) -> Result<Vec<Provider>, String> {
    let Ok(data) = std::fs::read_to_string(home.join("settings.json")) else {
        // Go: IsNotExist → nil, nil；其它读错误 → 读取 Claude 配置失败。
        return Ok(Vec::new());
    };
    let cfg: Value =
        serde_json::from_str(&data).map_err(|_| "Claude 配置格式无效".to_string())?;
    let env = |key: &str| {
        cfg.get("env")
            .and_then(|e| e.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let mut base = env("ANTHROPIC_BASE_URL");
    if base.is_empty() && !env("ANTHROPIC_API_KEY").is_empty() {
        base = "https://api.anthropic.com".into();
    }
    let mut key = env("ANTHROPIC_AUTH_TOKEN");
    if key.is_empty() {
        key = env("ANTHROPIC_API_KEY");
    }
    if base.is_empty() || key.is_empty() {
        return Ok(Vec::new());
    }
    let mut model = env("ANTHROPIC_MODEL");
    if model.is_empty() {
        model = value_text(cfg.get("model"));
    }
    let env_map: BTreeMap<String, String> = cfg
        .get("env")
        .and_then(|v| v.as_object())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let models = claude_models(&env_map, &model);
    Ok(vec![Provider {
        id: "native:claude:settings".into(),
        name: "Claude 原生 API 配置".into(),
        agent: "claude".into(),
        url: base,
        model,
        key: key.clone(),
        has_key: true,
        source: "claude".into(),
        protocol: "anthropic".into(),
        env: env_map,
        models,
        ..Provider::default()
    }])
}

fn read_codex(home: &Path) -> Result<Vec<Provider>, String> {
    let Ok(data) = std::fs::read_to_string(home.join("config.toml")) else {
        return Ok(Vec::new());
    };
    let mut auth: Value = match std::fs::read_to_string(home.join("auth.json")) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    if !auth.is_object() {
        auth = json!({});
    }
    let toml_cfg: toml::Value = toml::from_str(&data).map_err(|_| "Codex 配置格式无效".to_string())?;
    let cfg: Value = serde_json::to_value(toml_cfg).map_err(|_| "Codex 配置格式无效".to_string())?;
    let text_of = |value: Option<&Value>| value_text(value);
    let selected = text_of(cfg.get("model_provider"));
    let providers_map = cfg
        .get("model_providers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut names: Vec<String> = providers_map.keys().cloned().collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let settings = providers_map.get(&name).cloned().unwrap_or(Value::Null);
        let base = text_of(settings.get("base_url"));
        if base.is_empty() {
            continue;
        }
        let mut key = std::env::var(text_of(settings.get("env_key"))).unwrap_or_default();
        if key.is_empty() {
            key = text_of(settings.get("experimental_bearer_token"));
        }
        if key.is_empty() && name == selected {
            key = text_of(auth.get("OPENAI_API_KEY"));
        }
        let mut model = text_of(settings.get("model"));
        if name == selected {
            model = text_of(cfg.get("model"));
        }
        let label = {
            let from_settings = text_of(settings.get("name"));
            if from_settings.is_empty() { name.clone() } else { from_settings }
        };
        let mut models = Vec::new();
        if !model.is_empty() {
            models.push(PModel { id: model.clone(), name: String::new(), enabled: None });
        }
        out.push(Provider {
            id: format!("native:codex:provider:{name}"),
            name: label,
            source: "codex".into(),
            agent: "codex".into(),
            url: base,
            model,
            key: key.clone(),
            has_key: !key.is_empty(),
            cc_current: name == selected,
            protocol: "openai-responses".into(),
            models,
            ..Provider::default()
        });
    }
    Ok(out)
}

// ── model-providers store adapter (native.go ModelProviders) ────────────────

fn model_providers(root: &Path) -> Result<Vec<Provider>, String> {
    let rows = modelproviders::list_infos(root)?;
    let mut out = Vec::new();
    for row in rows {
        let mut tool = row.agent_type.clone();
        match tool.as_str() {
            "claude_code" => tool = "claude".into(),
            "deepseek" => tool = "dsh".into(),
            _ => {}
        }
        if tool != "claude" && tool != "codex" && tool != "dsh" {
            continue;
        }
        let key = modelproviders::key_by_id(root, row.id)?;
        let model = row.model.clone().unwrap_or_default();
        let protocol = if tool == "claude" { "anthropic" } else { "openai" };
        out.push(Provider {
            id: format!("store:{}", row.id),
            name: row.name.clone(),
            agent: tool,
            url: row.api_url.clone(),
            key: key.clone(),
            has_key: !key.is_empty(),
            source: "codepluse".into(),
            model,
            protocol: protocol.into(),
            ..Provider::default()
        });
    }
    Ok(out)
}

// ── provider metadata (writeback.go applyMetadata) ──────────────────────────

fn external_provider(id: &str) -> bool {
    id.starts_with("native:") || id.starts_with("ccswitch:") || id.starts_with("store:")
}

/// writeback.go `applyMetadata`: UI attributes (models/enabled/displayName)
/// keyed by the original id; never duplicates a provider or its key.
fn apply_metadata(root: &Path, provider: &mut Provider) -> Result<(), String> {
    if !external_provider(&provider.id) {
        return Ok(());
    }
    let path = root.join(".data").join("provider-metadata.json");
    let Some(value) = crate::backend::store_file::read_json(&path)? else {
        return Ok(());
    };
    let Some(entry) = value.get(&provider.id) else {
        return Ok(());
    };
    if let Some(models) = entry.get("models").and_then(|v| v.as_array()) {
        // Go: `if m.Models != nil` — the key being present replaces, even
        // when the array is empty; a missing key keeps the original.
        provider.models = models
            .iter()
            .map(|m| PModel {
                id: value_text(m.get("id")),
                name: value_text(m.get("name")),
                enabled: m.get("enabled").and_then(|v| v.as_bool()),
            })
            .collect();
    }
    provider.enabled = entry.get("enabled").and_then(|v| v.as_bool());
    let display_name = value_text(entry.get("displayName"));
    if !display_name.is_empty() {
        provider.name = display_name;
    }
    Ok(())
}

// ── find / Public / Activate ────────────────────────────────────────────────

/// providers.go `Find` → `find` + applyMetadata.
pub(crate) fn find(root: &Path, id: &str) -> Result<Provider, String> {
    let mut provider = find_raw(root, id)?;
    apply_metadata(root, &mut provider)?;
    Ok(provider)
}

fn find_raw(root: &Path, id: &str) -> Result<Provider, String> {
    if id.starts_with("native:") {
        let list = native()?;
        for provider in &list {
            if id == "native:codex:config" && provider.agent == "codex" && provider.cc_current {
                return Ok(provider.clone());
            }
            if provider.id == id {
                return Ok(provider.clone());
            }
        }
        return Err("原生供应商已不存在，请重新读取配置".into());
    }
    if id.starts_with("store:") {
        for provider in model_providers(root)? {
            if provider.id == id {
                return Ok(provider);
            }
        }
        return Err("已保存的模型供应商不存在".into());
    }
    if id.starts_with("ccswitch:") {
        if id.starts_with("ccswitch:codex:") {
            return Err("Codex 不再从 CC Switch 读取，请选择 Codex 原生配置中的供应商".into());
        }
        let list = cc_switch().map_err(|e| format!("读取 CC Switch 失败: {e}"))?;
        for provider in list {
            if provider.id == id {
                if !provider.has_key {
                    return Err("该 CC Switch 供应商没有 API 密钥，请先在 CC Switch 中切换到它".into());
                }
                return Ok(provider);
            }
        }
        return Err("供应商不存在".into());
    }
    for stored in &read_api(root)?.providers {
        if stored.id == id {
            let mut provider = stored.clone().into_provider();
            provider.has_key = !provider.key.is_empty();
            return Ok(provider);
        }
    }
    Err("供应商不存在".into())
}

/// providers.go `Public`: executors + CC Switch + saved stores + native
/// (non-claude), with UI metadata applied and every secret stripped.
pub(crate) fn public(root: &Path) -> Result<Value, String> {
    let api_data = read_api(root)?;
    let mut providers: Vec<Provider> = api_data
        .providers
        .into_iter()
        .map(|stored| stored.into_provider())
        .collect();
    let mut cc_error = String::new();
    match cc_switch() {
        Ok(list) => providers.extend(list),
        Err(error) => cc_error = format!("无法读取 CC Switch 数据库: {error}"),
    }
    match model_providers(root) {
        Ok(list) => providers.extend(list),
        Err(_) => cc_error.push_str(" 无法读取已保存的模型供应商"),
    }
    match native() {
        Ok(list) => {
            for provider in list {
                if provider.agent != "claude" {
                    providers.push(provider);
                }
            }
        }
        Err(error) => {
            cc_error.push(' ');
            cc_error.push_str(&error);
        }
    }
    let mut rows = Vec::new();
    for mut provider in providers {
        apply_metadata(root, &mut provider)?;
        provider.api = is_api(&provider);
        if let Some(parts) = parse_url(&provider.url) {
            if parts.userinfo.is_some() || parts.query.is_some() || parts.fragment.is_some() {
                provider.url = String::new();
            }
        }
        provider.has_key = provider.has_key || !provider.key.is_empty();
        provider.key = String::new();
        provider.env.clear();
        rows.push(provider_public_json(&provider));
    }
    // Data 的字段序：providers、active、ccError（omitempty）。
    let mut document = Map::new();
    document.insert("providers".into(), Value::Array(rows));
    document.insert("active".into(), serde_json::to_value(api_data.active).unwrap_or(json!({})));
    if !cc_error.is_empty() {
        document.insert("ccError".into(), json!(cc_error));
    }
    Ok(Value::Object(document))
}

// ── Activate ────────────────────────────────────────────────────────────────

fn activate(root: &Path, agent: &str, id: &str) -> Result<Value, String> {
    if agent != "claude" && agent != "dsh" {
        return Err("执行端无效".into());
    }
    let mut data = read_api(root)?;
    if !id.is_empty() {
        let mut ok = data
            .providers
            .iter()
            .any(|p| p.id == id && p.agent == agent);
        if !ok && agent == "claude" && id.starts_with("ccswitch:") {
            for provider in cc_switch()? {
                if provider.id == id && (provider.has_key || provider.cc_current) {
                    ok = true;
                }
            }
        }
        if !ok {
            return Err("供应商不存在".into());
        }
    }
    data.active.insert(agent.to_string(), id.to_string());
    write_api(root, &data)?;
    Ok(Value::Null)
}

/// ccswitch_provider_config（server.go）：掩码回显，明文不出本机。
fn ccswitch_provider_config(root: &Path, id: &str) -> Result<Value, String> {
    if !id.starts_with("ccswitch:") {
        return Err("仅支持 CC Switch 供应商".into());
    }
    let provider = find(root, id)?;
    let mut key = provider
        .env
        .get("ANTHROPIC_AUTH_TOKEN")
        .cloned()
        .unwrap_or_default();
    if key.is_empty() {
        key = provider.env.get("ANTHROPIC_API_KEY").cloned().unwrap_or_default();
    }
    Ok(json!({
        "url": provider.env.get("ANTHROPIC_BASE_URL").cloned().unwrap_or_default(),
        "apiKey": mask_secret(&key),
        "model": provider.model,
        "hasKey": !key.is_empty(),
    }))
}

/// secrets.go `maskSecret`：前 3 位 + … + 后 4 位；≤8 位全掩码。
fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let runes: Vec<char> = value.chars().collect();
    if runes.len() <= 8 {
        return "********".into();
    }
    let head: String = runes[..3].iter().collect();
    let tail: String = runes[runes.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// Public 响应里的供应商行（Go Provider 结构体序，omitempty 生效）。
pub(crate) fn provider_public_json(provider: &Provider) -> Value {
    // Go's `Provider` struct field order — the JSON key order is part of the
    // wire contract the dual-run compares byte for byte.
    let mut row = Map::new();
    row.insert("id".into(), json!(provider.id));
    row.insert("name".into(), json!(provider.name));
    row.insert("agent".into(), json!(provider.agent));
    row.insert("url".into(), json!(provider.url));
    row.insert("model".into(), json!(provider.model));
    row.insert("hasKey".into(), json!(provider.has_key));
    if !provider.source.is_empty() {
        row.insert("source".into(), json!(provider.source));
    }
    if provider.cc_current {
        row.insert("ccCurrent".into(), json!(true));
    }
    if !provider.models.is_empty() {
        let mut models = Vec::new();
        for model in &provider.models {
            let mut item = Map::new();
            item.insert("id".into(), json!(model.id));
            if !model.name.is_empty() {
                item.insert("name".into(), json!(model.name));
            }
            if model.enabled.is_some() {
                item.insert("enabled".into(), json!(model.enabled));
            }
            models.push(Value::Object(item));
        }
        row.insert("models".into(), Value::Array(models));
    }
    if !provider.protocol.is_empty() {
        row.insert("protocol".into(), json!(provider.protocol));
    }
    row.insert("api".into(), json!(provider.api));
    if provider.enabled.is_some() {
        row.insert("enabled".into(), json!(provider.enabled));
    }
    Value::Object(row)
}

// ── 模型目录（providermodels.go）────────────────────────────────────────────

/// outboundProxy：环境变量优先，其次 Windows 用户代理设置（注册表）。
fn outbound_proxy() -> String {
    if let Ok(value) = std::env::var("HTTPS_PROXY") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return value;
        }
    }
    if let Ok(value) = std::env::var("HTTP_PROXY") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return value;
        }
    }
    let Ok(output) = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyServer",
        ])
        .output()
    else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if !line.contains("ProxyServer") {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            break;
        }
        let server = fields[fields.len() - 1].to_string();
        if server.is_empty() {
            return String::new();
        }
        if !server.contains('=') {
            if !server.contains("://") {
                return format!("http://{server}");
            }
            return server;
        }
        for part in server.split(';') {
            let lowered = part.to_lowercase();
            if let Some(rest) = lowered.strip_prefix("https=") {
                return part[part.len() - rest.len()..].trim_start_matches("//").to_string();
            }
        }
        for part in server.split(';') {
            let lowered = part.to_lowercase();
            if let Some(rest) = lowered.strip_prefix("http=") {
                return part[part.len() - rest.len()..].trim_start_matches("//").to_string();
            }
        }
        return String::new();
    }
    String::new()
}

/// providermodels.go `sameHost`：scheme 忽略，host 相同且（双方都给了端口时）
/// 端口相同。
pub(crate) fn same_host(a: &str, b: &str) -> bool {
    let (Some(ua), Some(ub)) = (parse_url(a), parse_url(b)) else {
        return false;
    };
    if !ua.host_is_non_empty() || !ub.host_is_non_empty() {
        return false;
    }
    if !ua.host.eq_ignore_ascii_case(&ub.host) {
        return false;
    }
    if !ua.port.is_empty() && !ub.port.is_empty() {
        return ua.port == ub.port;
    }
    true
}

/// providermodels.go `modelListProviderURL`：密钥所附带的落点。
fn model_list_provider_url(root: &Path, key_source: &Value) -> Result<String, String> {
    match value_text(key_source.get("kind")).as_str() {
        "executor" | "ccswitch" => {
            let provider = find(root, &value_text(key_source.get("providerId")))?;
            if !provider.url.trim().is_empty() {
                return Ok(provider.url);
            }
            Ok(provider.env.get("ANTHROPIC_BASE_URL").cloned().unwrap_or_default())
        }
        "provider" => {
            let id = number_of(key_source.get("providerId"));
            if id == 0 {
                return Err("providerId 无效".into());
            }
            for info in modelproviders::list_infos(root)? {
                if info.id == id {
                    return Ok(info.api_url);
                }
            }
            Err("供应商不存在".into())
        }
        _ => Ok(String::new()),
    }
}

/// providermodels.go `resolveModelListKey`。
fn resolve_model_list_key(root: &Path, key_source: &Value) -> Result<String, String> {
    match value_text(key_source.get("kind")).as_str() {
        "executor" => Ok(find(root, &value_text(key_source.get("providerId")))?.key),
        "inline" => Ok(value_text(key_source.get("apiKey"))),
        "provider" => {
            let id = number_of(key_source.get("providerId"));
            if id == 0 {
                return Err("providerId 无效".into());
            }
            modelproviders::key_by_id(root, id)
        }
        "ccswitch" => {
            let provider = find(root, &value_text(key_source.get("providerId")))?;
            let mut key = provider.env.get("ANTHROPIC_AUTH_TOKEN").cloned().unwrap_or_default();
            if key.is_empty() {
                key = provider.env.get("ANTHROPIC_API_KEY").cloned().unwrap_or_default();
            }
            Ok(key)
        }
        kind => Err(format!("暂不支持该密钥来源: {kind}")),
    }
}

fn number_of(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

/// server.go `harnessProviderModels`：本地配置 + 远程目录合并。
fn harness_provider_models(root: &Path, id: &str) -> Result<Value, String> {
    let provider = find(root, id)?;
    if !is_api(&provider) {
        return Err("请选择 API 供应商，官方订阅不提供此配置".into());
    }
    let mut models: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let disabled: Vec<String> = provider
        .models
        .iter()
        .filter(|m| m.enabled == Some(false))
        .map(|m| m.id.clone())
        .collect();
    let mut add = |models: &mut Vec<Value>, seen: &mut Vec<String>, id: &str, name: &str| {
        if !id.is_empty() && !seen.iter().any(|s| s == id) && !disabled.iter().any(|d| d == id) {
            seen.push(id.to_string());
            let mut row = json!({ "id": id });
            if !name.is_empty() {
                row["displayName"] = json!(name);
            }
            models.push(row);
        }
    };
    for model in &provider.models {
        add(&mut models, &mut seen, &model.id, &model.name);
    }
    add(&mut models, &mut seen, &provider.model, "");
    if provider.source == "dsh" && !models.is_empty() {
        return Ok(json!({"models": models, "source": "native"}));
    }
    let mut protocol = provider.protocol.clone();
    if protocol == "openai-responses" {
        protocol = "openai".into();
    }
    if protocol.is_empty() {
        protocol = if provider.agent == "claude" { "anthropic".into() } else { "openai".into() };
    }
    let mut base = provider.url.trim_end_matches('/').to_string();
    if protocol == "anthropic" {
        base = base.strip_suffix("/v1").unwrap_or(&base).to_string();
    }
    let mut params = json!({
        "protocol": protocol,
        "baseUrl": base,
        "keySource": {"kind": "executor", "providerId": id},
    });
    if !provider.env.get("ANTHROPIC_AUTH_TOKEN").cloned().unwrap_or_default().is_empty() {
        params["keyHeader"] = json!("bearer");
    }
    let mut warning = String::new();
    match list_provider_models(root, &json!({"params": params})) {
        Ok(raw) => {
            if let Some(remote) = raw.get("models").and_then(|v| v.as_array()) {
                for model in remote {
                    let id = value_text(model.get("id"));
                    let name = value_text(model.get("displayName"));
                    add(&mut models, &mut seen, &id, &name);
                }
            }
        }
        Err(_) => {
            warning = "供应商模型目录暂不可用，已显示本地配置中的模型；也可填写模型 ID。".into();
        }
    }
    Ok(json!({"models": models, "source": "provider", "warning": warning}))
}

/// fetchguard.go 的地址分类器（inline 密钥的 SSRF 防护）：环回/内网/链路本地/
/// 多播/保留段全部拒绝。Rust 侧先解析并分类，再发请求（Go 在 dial 时做）。
fn blocked_fetch_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_unspecified()
                || v4.is_link_local()
                || v4.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xC0) == 64)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (o[1] & 0xFE) == 18)
                || (o[0] & 0xF0) == 240
        }
        std::net::IpAddr::V6(v6) => {
            let segments = v6.segments();
            if matches!(segments[..5], [0, 0, 0, 0, 0xFFFF]) {
                return blocked_fetch_ip(&std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                    (segments[6] >> 8) as u8,
                    (segments[6] & 0xFF) as u8,
                    (segments[7] >> 8) as u8,
                    (segments[7] & 0xFF) as u8,
                )));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (segments[0] & 0xFE00) == 0xFC00
                || (segments[0] & 0xFFC0) == 0xFE80
                || (segments[0] == 0x2001 && segments[1] == 0x0DB8)
        }
    }
}

pub(crate) fn host_is_safe(host: &str) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    if host.eq_ignore_ascii_case("localhost") {
        return Err(format!("目标地址不允许访问：{host}"));
    }
    let addresses: Vec<std::net::SocketAddr> = match format!("{host}:0").to_socket_addrs() {
        Ok(list) => list.collect(),
        Err(error) => return Err(error.to_string()),
    };
    let tried = addresses
        .iter()
        .any(|address| !blocked_fetch_ip(&address.ip()));
    if !tried {
        return Err(format!("目标地址不允许访问：{host}"));
    }
    Ok(())
}

/// providermodels.go `listProviderModels`：三协议模型目录拉取。
fn list_provider_models(root: &Path, args: &Value) -> Result<Value, String> {
    let Some(params) = args.get("params").filter(|v| v.is_object()) else {
        return Err("缺少 params".into());
    };
    let protocol = value_text(params.get("protocol"));
    let base_url = value_text(params.get("baseUrl")).trim().trim_end_matches('/').to_string();
    if base_url.is_empty() || (!base_url.starts_with("http://") && !base_url.starts_with("https://")) {
        return Err("API 地址必须是 http(s) 地址".into());
    }
    let key_source = params.get("keySource").cloned().unwrap_or(Value::Null);
    let key = resolve_model_list_key(root, &key_source)?;
    // 存储过的凭据只能发往存储时的主机：请求体指定 baseUrl 会把本接口变成
    // 凭据外带原语。只比较主机（调用方仍控制路径前缀）。
    let key_kind = value_text(key_source.get("kind"));
    if !key_kind.is_empty() && key_kind != "inline" {
        let stored = model_list_provider_url(root, &key_source)?;
        if !same_host(&base_url, &stored) {
            return Err("API 地址与密钥来源的供应商不一致".into());
        }
    }
    let (list_url, mut headers) = match protocol.as_str() {
        "openai" => (
            format!("{base_url}/models"),
            vec![("Authorization".to_string(), format!("Bearer {key}"))],
        ),
        "anthropic" => {
            let mut headers = Vec::new();
            if !key.trim().is_empty() {
                headers.push(("x-api-key".to_string(), key.clone()));
            }
            headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
            (format!("{base_url}/v1/models"), headers)
        }
        "gemini" => (
            format!("{base_url}/v1beta/models"),
            vec![("x-goog-api-key".to_string(), key.clone())],
        ),
        other => return Err(format!("暂不支持该协议: {other}")),
    };
    // 非默认密钥传递方式：覆盖协议默认头。
    match value_text(params.get("keyHeader")).as_str() {
        "bearer" => {
            headers.retain(|(name, _)| name != "x-api-key" && name != "x-goog-api-key");
            headers.push(("Authorization".to_string(), format!("Bearer {key}")));
        }
        "x_api_key" => {
            headers.retain(|(name, _)| name != "Authorization");
            headers.push(("x-api-key".to_string(), key.clone()));
        }
        "x_goog_api_key" => {
            headers.retain(|(name, _)| name != "x-api-key" && name != "Authorization");
            headers.push(("x-goog-api-key".to_string(), key.clone()));
        }
        _ => {}
    }
    // 出站客户端：inline 密钥先分类目标地址并禁用代理；loopback 目标不走代理；
    // 其余目标优先注册表代理，缺省回落环境代理（Go 的 DefaultTransport 语义）。
    let client = match key_kind.as_str() {
        "inline" => {
            let parts = parse_url(&list_url).ok_or_else(|| "无法访问模型列表: 无效地址".to_string())?;
            host_is_safe(&parts.host).map_err(|e| format!("无法访问模型列表: {e}"))?;
            net::no_proxy_client().clone()
        }
        _ => {
            let parts = parse_url(&base_url).ok_or_else(|| "无法访问模型列表: 无效地址".to_string())?;
            if parts.host.eq_ignore_ascii_case("localhost")
                || parts
                    .host
                    .parse::<std::net::IpAddr>()
                    .map(|ip| ip.is_loopback())
                    .unwrap_or(false)
            {
                net::no_proxy_client().clone()
            } else {
                let proxy = outbound_proxy();
                if proxy.is_empty() {
                    net::client().clone()
                } else {
                    net::proxied_client(&proxy)?
                }
            }
        }
    };
    let mut request = client.get(&list_url).timeout(std::time::Duration::from_secs(12));
    for (name, value) in &headers {
        if !value.is_empty() {
            request = request.header(name, value);
        }
    }
    let response = request.send().map_err(|e| format!("无法访问模型列表: {e}"))?;
    let status = response.status().as_u16();
    let body = response.bytes().map_err(|e| e.to_string())?;
    if status >= 400 {
        let mut snippet = String::from_utf8_lossy(&body).trim().to_string();
        if snippet.len() > 200 {
            snippet.truncate(200);
        }
        return Err(format!("模型列表请求失败（HTTP {status}）: {snippet}"));
    }
    let payload: Value =
        serde_json::from_slice(&body).map_err(|e| format!("模型列表响应无效: {e}"))?;
    let mut models: Vec<Value> = Vec::new();
    if protocol == "gemini" {
        for model in payload
            .get("models")
            .and_then(|v| v.as_array())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
        {
            let id = value_text(model.get("name")).trim_start_matches("models/").to_string();
            if id.is_empty() {
                continue;
            }
            let mut row = json!({ "id": id });
            let display = value_text(model.get("displayName"));
            if !display.is_empty() {
                row["displayName"] = json!(display);
            }
            models.push(row);
        }
    } else {
        for model in payload
            .get("data")
            .and_then(|v| v.as_array())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
        {
            let id = value_text(model.get("id"));
            if id.is_empty() {
                continue;
            }
            let mut row = json!({ "id": id });
            let display = value_text(model.get("display_name"));
            if !display.is_empty() {
                row["displayName"] = json!(display);
            }
            if let Some(window) = model.get("context_window").and_then(|v| v.as_i64()) {
                if window != 0 {
                    row["contextWindow"] = json!(window);
                }
            }
            models.push(row);
        }
    }
    Ok(json!({"url": list_url, "models": models}))
}

// ── owns / handle ───────────────────────────────────────────────────────────

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "codepluse_providers_list"
            | "codepluse_provider_activate"
            | "ccswitch_provider_config"
            | "codepluse_provider_models"
            | "list_provider_models"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "codepluse_providers_list" => public(root),
        "codepluse_provider_activate" => activate(root, &txt(args, "agent"), &txt(args, "id")),
        "ccswitch_provider_config" => ccswitch_provider_config(root, &txt(args, "id")),
        "codepluse_provider_models" => harness_provider_models(root, &txt(args, "providerId")),
        "list_provider_models" => list_provider_models(root, args),
        _ => Err(format!("Go 移植尚未实现此接口：{command}")),
    }
}

fn txt(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-provider-catalog-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    /// 一站式 mock 网关：/models 与 /v1/models 都回 openai 形状。
    fn serve_gateway(payload: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}")
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    fn seed_api_provider(root: &Path, url: &str) {
        // Go Provider 结构体序 + MarshalIndent 两空格（providers 包 write）。
        let document = json!({
            "providers": [{
                "id": "abc123", "name": "网关", "agent": "claude", "url": url,
                "model": "glm-4.7", "key": "sk-gw-test", "hasKey": true,
                "models": [{"id": "glm-4.7", "name": "GLM"}, {"id": "glm-4.6", "name": "", "enabled": false}],
                "protocol": "anthropic", "api": false
            }],
            "active": {"claude": "abc123"}
        });
        std::fs::write(
            api_providers_path(root),
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn providers_list_merges_sources_and_strips_keys() {
        let root = temp_root("list");
        seed_api_provider(root.as_path(), "https://gw.example/v1");
        // model-providers.json：一个 store 供应商（providers.rs 的格式）。
        let store = json!({"nextId": 2, "entries": [{
            "id": 1, "name": "存下", "api_url": "https://api.openai.com", "api_key": "sk-store",
            "api_key_masked": "", "agent_type": "codex", "model": "gpt-5",
            "created_at": "2026-11-13T00:00:00Z", "updated_at": "2026-11-13T00:00:00Z", "key": "sk-store"
        }]});
        std::fs::write(
            root.join(".data").join("model-providers.json"),
            serde_json::to_vec_pretty(&store).unwrap(),
        )
        .unwrap();
        let out = call(&root, "codepluse_providers_list", json!({})).unwrap();
        let providers = out["providers"].as_array().unwrap();
        // executor + store（openai agent 原样）——ccswitch/native 取决于本机。
        let executor = providers
            .iter()
            .find(|p| p["id"] == "abc123")
            .expect("executor 供应商在列表里");
        assert_eq!(executor["name"], "网关");
        assert_eq!(executor["hasKey"], true, "存了 key 的供应商回 hasKey");
        assert_eq!(executor["api"], true, "http(s)+host+非 oat key → api");
        assert!(executor.get("key").is_none(), "响应绝不带明文 key");
        assert!(executor.get("env").is_none(), "env 是 Go 的 json:\"-\"");
        let stored = providers
            .iter()
            .find(|p| p["id"] == "store:1")
            .expect("store 供应商在列表里");
        assert_eq!(stored["agent"], "codex");
        assert_eq!(stored["hasKey"], true);
        assert_eq!(stored["source"], "codepluse");
        assert_eq!(stored["protocol"], "openai");
        // ccswitch 段：读取错误时写进 ccError，正常时缺省。
        if let Some(value) = out.get("ccError") {
            assert!(value.as_str().is_some(), "ccError 是字符串");
        }
    }

    #[test]
    fn activate_rejects_unknown_and_persists() {
        let root = temp_root("activate");
        seed_api_provider(root.as_path(), "https://gw.example/v1");
        assert_eq!(
            call(&root, "codepluse_provider_activate", json!({"agent": "codex", "id": "abc123"})).unwrap_err(),
            "执行端无效"
        );
        assert_eq!(
            call(&root, "codepluse_provider_activate", json!({"agent": "dsh", "id": "nope"})).unwrap_err(),
            "供应商不存在"
        );
        call(&root, "codepluse_provider_activate", json!({"agent": "claude", "id": "abc123"})).unwrap();
        let raw = std::fs::read_to_string(api_providers_path(&root)).unwrap();
        assert!(raw.contains("\"claude\": \"abc123\""), "{raw}");
        // 落盘仍是两空格缩进（Go MarshalIndent）。
        assert!(raw.starts_with("{\n  \"providers\": ["), "{raw}");
    }

    #[test]
    fn provider_models_merges_local_and_remote() {
        let gateway = serve_gateway(
            r#"{"data":[{"id":"glm-4.7","display_name":"GLM"},{"id":"glm-air","display_name":"Air","context_window":128000}]}"#,
        );
        let root = temp_root("models");
        // url 指向本机 mock 网关（keySource=executor → 非 inline → 无代理直连 loopback）。
        seed_api_provider(root.as_path(), &gateway);
        let out = call(
            &root,
            "codepluse_provider_models",
            json!({"providerId": "abc123"}),
        )
        .unwrap();
        assert_eq!(out["source"], "provider");
        let ids: Vec<&str> = out["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        // 本地两条（glm-4.6 被禁用）+ 远程新增 glm-air。
        assert_eq!(ids, vec!["glm-4.7", "glm-air"]);
        assert!(!ids.contains(&"glm-4.6"), "禁用的模型即使远程出现也不会回来");
    }

    #[test]
    fn list_provider_models_validates_and_parses() {
        let root = temp_root("list-models");
        seed_api_provider(root.as_path(), "https://gw.example/v1");
        // host 不一致：凭据不外带。
        assert_eq!(
            call(&root, "list_provider_models", json!({"params": {
                "protocol": "openai", "baseUrl": "https://evil.example",
                "keySource": {"kind": "executor", "providerId": "abc123"}
            }})).unwrap_err(),
            "API 地址与密钥来源的供应商不一致"
        );
        // 协议不支持。
        assert_eq!(
            call(&root, "list_provider_models", json!({"params": {
                "protocol": "coze", "baseUrl": "https://gw.example", "keySource": {"kind": "inline", "apiKey": "k"}
            }})).unwrap_err(),
            "暂不支持该协议: coze"
        );
        // inline + 内网目标：SSRF 防护。
        assert!(call(&root, "list_provider_models", json!({"params": {
            "protocol": "openai", "baseUrl": "http://127.0.0.1:9", "keySource": {"kind": "inline", "apiKey": "k"}
        }})).unwrap_err()
        .contains("目标地址不允许访问"));
        // 缺 params。
        assert_eq!(call(&root, "list_provider_models", json!({})).unwrap_err(), "缺少 params");
    }

    #[test]
    fn same_host_ignores_scheme_and_requires_both_ports() {
        assert!(same_host("https://GW.example:7897/v1", "http://gw.example:7897"));
        assert!(same_host("https://gw.example/x", "http://gw.example:80/y"));
        assert!(!same_host("https://gw.example:1", "https://gw.example:2"));
        assert!(!same_host("https://gw.example", "https://other.example"));
        assert!(!same_host("", "https://gw.example"));
    }

    #[test]
    fn parse_url_splits_what_go_checks() {
        let parts = parse_url("https://user:pw@host.example:8443/p?a=b#f").unwrap();
        assert_eq!(parts.scheme, "https");
        assert_eq!(parts.userinfo.as_deref(), Some("user:pw"));
        assert_eq!(parts.host, "host.example");
        assert_eq!(parts.port, "8443");
        assert_eq!(parts.query.as_deref(), Some("a=b"));
        assert_eq!(parts.fragment.as_deref(), Some("f"));
        let bare = parse_url("https://host-only").unwrap();
        assert_eq!(bare.host, "host-only");
        assert!(bare.port.is_empty());
    }

    #[test]
    fn mask_secret_matches_secrets_go() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("short"), "********");
        assert_eq!(mask_secret("123456789"), "123…6789");
    }

    #[test]
    fn only_catalog_commands_are_owned() {
        let root = temp_root("owns");
        assert!(handle(&root, "list_model_providers", &json!({})).is_none());
        assert!(handle(&root, "codepluse_provider_save", &json!({})).is_none());
        assert!(handle(&root, "codepluse_connection_catalog", &json!({})).is_none());
    }
}
