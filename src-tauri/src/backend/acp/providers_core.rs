//! Read side of Go's `internal/providers`, `internal/modelproviders` and the
//! CC Switch bridge — everything the launch chain needs to resolve which API
//! provider backs an agent. Write commands stay in `providers.rs` (the
//! management surface); this module never mutates provider files.
//!
//! Three provider sources resolve by id prefix, exactly like Go's `find`:
//! `native:` (scanned vendor configs), `store:` (`model-providers.json`) and
//! `ccswitch:` (the CC Switch app's SQLite DB), plus the plain ids stored in
//! `api-providers.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::backend::store_file;

/// Mirror of Go's `providers.Provider` (providers.go:15).
#[derive(Clone, Debug, Default)]
pub struct ProviderDef {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub url: String,
    pub model: String,
    pub key: String,
    pub has_key: bool,
    pub source: String,
    pub cc_current: bool,
    pub env: BTreeMap<String, String>,
    pub models: Vec<(String, String)>,
    pub protocol: String,
    pub native: Value,
    pub api: bool,
    pub enabled: Option<bool>,
}

pub fn user_dir(env: &str, suffix: &str) -> PathBuf {
    if let Ok(v) = std::env::var(env) {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    home().join(suffix)
}

/// `%USERPROFILE%` with a test seam (`HARNESS_HUB_FAKE_HOME`).
pub fn home() -> PathBuf {
    if let Ok(fake) = std::env::var("HARNESS_HUB_FAKE_HOME") {
        if !fake.is_empty() {
            return PathBuf::from(fake);
        }
    }
    std::env::var("USERPROFILE").map(PathBuf::from).unwrap_or_default()
}

/// Go `NativeHome` (auth.go): the vendor CLI's own config directory.
pub fn native_home(tool: &str) -> String {
    let dir = match tool {
        "claude" => user_dir("CLAUDE_CONFIG_DIR", ".claude"),
        "codex" => user_dir("CODEX_HOME", ".codex"),
        "dsh" => user_dir("DSH_HOME", ".dsh"),
        "workbuddy" => user_dir("CODEBUDDY_CONFIG_DIR", ".workbuddy"),
        "custom:qoder" | "qoder" => {
            if let Ok(dir) = std::env::var("QODER_CONFIG_DIR") {
                if !dir.is_empty() {
                    return dir;
                }
            }
            let base = std::env::var("QODER_CLI_HOME")
                .ok()
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(home);
            let name = std::env::var("QODER_CONFIG_DIR_NAME")
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| ".qoder".into());
            return base.join(name).to_string_lossy().to_string();
        }
        _ => return String::new(),
    };
    dir.to_string_lossy().to_string()
}

pub fn supports_subscription(tool: &str) -> bool {
    matches!(tool, "claude" | "codex" | "workbuddy" | "qoder" | "custom:qoder")
}

fn string_at(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn object_at(value: &Value, key: &str) -> Value {
    value.get(key).cloned().unwrap_or(Value::Null)
}

/// Go `Account` (auth.go): login metadata with no secrets in it.
pub fn account(tool: &str) -> Value {
    let source = native_home(tool);
    let supported = supports_subscription(tool);
    let mut info = serde_json::json!({
        "supported": supported,
        "configured": false,
        "source": source,
        "models": [],
    });
    match tool {
        "codex" => {
            info["loginCommand"] = serde_json::json!("codex login");
            let path = Path::new(&source).join("auth.json");
            if let Ok(body) = std::fs::read(&path) {
                if let Ok(auth) = serde_json::from_slice::<Value>(&body) {
                    let tokens = object_at(&auth, "tokens");
                    if !string_at(&tokens, "access_token").is_empty() {
                        info["configured"] = serde_json::json!(true);
                    }
                }
            }
            let cache_path = Path::new(&source).join("models_cache.json");
            if let Ok(body) = std::fs::read(&cache_path) {
                if let Ok(cache) = serde_json::from_slice::<Value>(&body) {
                    let mut models = Vec::new();
                    for m in cache.get("models").and_then(Value::as_array).into_iter().flatten() {
                        let slug = string_at(m, "slug");
                        if !slug.is_empty() {
                            models.push(serde_json::json!({
                                "id": slug, "name": string_at(m, "display_name"),
                            }));
                        }
                    }
                    info["models"] = Value::Array(models);
                }
            }
        }
        "claude" => {
            info["loginCommand"] = serde_json::json!("claude auth login");
            let path = Path::new(&source).join(".credentials.json");
            if let Ok(body) = std::fs::read(&path) {
                if let Ok(auth) = serde_json::from_slice::<Value>(&body) {
                    let oauth = object_at(&auth, "claudeAiOauth");
                    if !string_at(&oauth, "accessToken").is_empty() {
                        info["configured"] = serde_json::json!(true);
                    }
                }
            }
            if std::env::var("CLAUDE_CODE_OAUTH_TOKEN").map(|v| !v.is_empty()).unwrap_or(false) {
                info["configured"] = serde_json::json!(true);
            }
        }
        "workbuddy" | "qoder" | "custom:qoder" => {
            info["managed"] = serde_json::json!(true);
        }
        _ => {}
    }
    info
}

/// Go `AccountVersion` (auth.go): hash of the native credential file.
pub fn account_version(tool: &str) -> String {
    use sha2::Digest;
    let name = if tool == "claude" { ".credentials.json" } else { "auth.json" };
    let mut body = std::fs::read(Path::new(&native_home(tool)).join(name)).unwrap_or_default();
    if tool == "claude" {
        if let Ok(token) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
            body.extend_from_slice(token.as_bytes());
        }
    }
    let mut hasher = sha2::Sha256::new();
    hasher.update(&body);
    format!("{:x}", hasher.finalize())
}

/// Go `ClearAPIEnvironment` (auth.go): drop every API credential variable so a
/// subscription profile can't inherit stale keys.
pub fn clear_api_environment(env: &[String]) -> Vec<String> {
    env.iter()
        .filter(|entry| {
            let key = entry.split('=').next().unwrap_or("").to_uppercase();
            !(key.starts_with("ANTHROPIC_")
                || key == "CLAUDE_CODE_SUBAGENT_MODEL"
                || key == "OPENAI_API_KEY"
                || key == "OPENAI_BASE_URL"
                || key == "CODEX_API_KEY"
                || key == "CODEPLUSE_CODEX_API_KEY"
                || key == "CODEPLUSE_DSH_API_KEY"
                || key == "CODEBUDDY_API_KEY"
                || key == "CODEBUDDY_BASE_URL")
        })
        .cloned()
        .collect()
}

/// Go `IsAPI` (providers.go:38): a usable HTTP(S) endpoint with a real key.
pub fn is_api(p: &ProviderDef) -> bool {
    let Ok(u) = url::Url::parse(&p.url) else { return false };
    !p.url.is_empty()
        && (u.scheme() == "http" || u.scheme() == "https")
        && u.host_str().is_some()
        && u.username().is_empty()
        && u.query().is_none()
        && u.fragment().is_none()
        && !p.key.is_empty()
        && !p.key.starts_with("sk-ant-oat")
}

fn claude_models(env: &BTreeMap<String, String>, model: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for id in [
        model,
        env.get("ANTHROPIC_DEFAULT_OPUS_MODEL").map(String::as_str).unwrap_or(""),
        env.get("ANTHROPIC_DEFAULT_SONNET_MODEL").map(String::as_str).unwrap_or(""),
        env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL").map(String::as_str).unwrap_or(""),
    ] {
        if !id.is_empty() && seen.insert(id.to_string()) {
            out.push((id.to_string(), String::new()));
        }
    }
    out
}

/// Go `ModelProviders` (native.go): `model-providers.json` rows as providers.
pub fn model_providers(root: &Path) -> Result<Vec<ProviderDef>, String> {
    let data = store_file::read_json(&root.join(".data").join("model-providers.json"))?
        .unwrap_or(Value::Null);
    let mut out = Vec::new();
    for row in data.get("entries").and_then(Value::as_array).into_iter().flatten() {
        let agent_type = string_at(row, "agent_type");
        let tool = match agent_type.as_str() {
            "claude_code" => "claude",
            "deepseek" => "dsh",
            other => other,
        };
        if !matches!(tool, "claude" | "codex" | "dsh") {
            continue;
        }
        let key = string_at(row, "key");
        let model = row.get("model").and_then(Value::as_str).unwrap_or("").to_string();
        let protocol = if tool == "claude" { "anthropic" } else { "openai" };
        out.push(ProviderDef {
            id: format!("store:{}", crate::backend::acp::wire::number(row.get("id"))),
            name: string_at(row, "name"),
            agent: tool.into(),
            url: string_at(row, "api_url"),
            key: key.clone(),
            has_key: !key.is_empty(),
            source: "codepluse".into(),
            model,
            protocol: protocol.into(),
            ..ProviderDef::default()
        });
    }
    Ok(out)
}

/// Go `CCSwitch` (ccswitch.go): the CC Switch app's live SQLite DB, read-only.
pub fn ccswitch() -> Result<Vec<ProviderDef>, String> {
    let path = home().join(".cc-switch").join("cc-switch.db");
    read_ccswitch(&path)
}

fn read_ccswitch(path: &Path) -> Result<Vec<ProviderDef>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("无法读取 CC Switch 数据库: {e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT id,name,settings_config,is_current FROM providers \
             WHERE app_type = 'claude' ORDER BY COALESCE(sort_index, 2147483647), name",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt
        .query([])
        .map_err(|e| e.to_string())?;
    let mut out: Vec<ProviderDef> = Vec::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let id: String = row.get(0).map_err(|e| e.to_string())?;
        let name: String = row.get(1).map_err(|e| e.to_string())?;
        let raw: String = row.get(2).map_err(|e| e.to_string())?;
        let current: bool = row.get(3).map_err(|e| e.to_string())?;
        let cfg: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("CC Switch 供应商配置无效: {e}"))?;
        let mut env = BTreeMap::new();
        for (k, v) in cfg.get("env").and_then(Value::as_object).into_iter().flatten() {
            if k.starts_with("ANTHROPIC_") || k == "CLAUDE_CODE_SUBAGENT_MODEL" {
                if let Some(s) = v.as_str() {
                    env.insert(k.clone(), s.to_string());
                }
            }
        }
        let mut model = env.get("ANTHROPIC_MODEL").cloned().unwrap_or_default();
        if model.is_empty() {
            model = string_at(&cfg, "model");
        }
        let mut key = env.get("ANTHROPIC_AUTH_TOKEN").cloned().unwrap_or_default();
        if key.is_empty() {
            key = env.get("ANTHROPIC_API_KEY").cloned().unwrap_or_default();
        }
        let mut models = claude_models(&env, &model);
        let catalog = cfg
            .get("modelCatalog")
            .and_then(|c| c.get("models"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !catalog.is_empty() {
            models = Vec::new();
            for m in &catalog {
                let model_id = string_at(m, "model");
                if !model_id.is_empty() {
                    models.push((model_id, string_at(m, "displayName")));
                }
            }
        }
        out.push(ProviderDef {
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
            ..ProviderDef::default()
        });
    }
    Ok(out)
}

fn read_dsh(home: &Path) -> Result<Vec<ProviderDef>, String> {
    let mut refs = BTreeMap::new();
    if let Ok(data) = std::fs::read(home.join(".credentials.yaml")) {
        if let Ok(credentials) = serde_yaml::from_slice::<Value>(&data) {
            for (k, v) in credentials.get("refs").and_then(Value::as_object).into_iter().flatten() {
                if let Some(s) = v.as_str() {
                    refs.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    let mut out = Vec::new();
    for profile in ["desktop", "acp"] {
        let data = match std::fs::read(home.join("profiles").join(profile).join("cordis.patch.yml")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("读取 DSH 配置失败".into()),
            Ok(data) => data,
        };
        let plugins: Vec<Value> = match serde_yaml::from_slice(&data) {
            Err(_) => return Err("DSH 配置格式无效".into()),
            Ok(serde_yaml::Value::Sequence(items)) => items.into_iter().map(convert_yaml).collect(),
            Ok(_) => return Err("DSH 配置格式无效".into()),
        };
        let (mut selected_provider, mut selected_model) = (String::new(), String::new());
        for p in &plugins {
            let id = string_at(p, "id");
            if id == "agent-default-model" || id == "acp" {
                let config = object_at(p, "config");
                selected_provider = string_at(&config, "provider");
                selected_model = string_at(&config, "model");
            }
        }
        for plugin in &plugins {
            if string_at(plugin, "id") != "llm-pi-ai" {
                continue;
            }
            let configs = object_at(plugin, "config")
                .get("providers")
                .cloned()
                .unwrap_or(Value::Null);
            let Some(configs) = configs.as_object() else { continue };
            let mut ids: Vec<&String> = configs.keys().collect();
            ids.sort();
            for id in ids {
                let cfg = configs.get(id).cloned().unwrap_or(Value::Null);
                let base = string_at(&cfg, "baseURL");
                if base.is_empty() {
                    continue;
                }
                let mut key = string_at(&cfg, "apiKey");
                let env_ref = string_at(&cfg, "apiKeyEnv");
                if !env_ref.is_empty() {
                    key = std::env::var(&env_ref).unwrap_or_default();
                    if key.is_empty() {
                        key = refs.get(&env_ref).cloned().unwrap_or_default();
                    }
                }
                let mut models: Vec<(String, String)> = Vec::new();
                if let Some(raw) = cfg.get("models").and_then(Value::as_array) {
                    for v in raw {
                        let mid = string_at(v, "id");
                        if !mid.is_empty() {
                            models.push((mid, string_at(v, "name")));
                        }
                    }
                }
                let mut model = if *id == selected_provider { selected_model.clone() } else { String::new() };
                if model.is_empty() {
                    model = models.first().map(|(id, _)| id.clone()).unwrap_or_default();
                }
                let api = string_at(&cfg, "api");
                let protocol = if api == "openai-responses" {
                    "openai-responses"
                } else if api.starts_with("anthropic") {
                    "anthropic"
                } else {
                    "openai"
                };
                let mut name = string_at(&cfg, "name");
                if name.is_empty() {
                    name = format!("{id} · DSH {profile}");
                }
                out.push(ProviderDef {
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
                    native: cfg,
                    ..ProviderDef::default()
                });
            }
        }
    }
    Ok(out)
}

fn read_claude(home: &Path) -> Result<Vec<ProviderDef>, String> {
    let data = match std::fs::read(home.join("settings.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("读取 Claude 配置失败".into()),
        Ok(data) => data,
    };
    let cfg: Value = serde_json::from_slice(&data).map_err(|_| "Claude 配置格式无效")?;
    let env_map = cfg.get("env").and_then(Value::as_object).cloned().unwrap_or_default();
    let env_of = |k: &str| env_map.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut base = env_of("ANTHROPIC_BASE_URL");
    if base.is_empty() && !env_of("ANTHROPIC_API_KEY").is_empty() {
        base = "https://api.anthropic.com".into();
    }
    let mut key = env_of("ANTHROPIC_AUTH_TOKEN");
    if key.is_empty() {
        key = env_of("ANTHROPIC_API_KEY");
    }
    if base.is_empty() || key.is_empty() {
        return Ok(Vec::new());
    }
    let mut model = env_of("ANTHROPIC_MODEL");
    if model.is_empty() {
        model = string_at(&cfg, "model");
    }
    let mut env = BTreeMap::new();
    for (k, v) in &env_map {
        if let Some(s) = v.as_str() {
            env.insert(k.clone(), s.to_string());
        }
    }
    let models = claude_models(&env, &model);
    Ok(vec![ProviderDef {
        id: "native:claude:settings".into(),
        name: "Claude 原生 API 配置".into(),
        agent: "claude".into(),
        url: base,
        model,
        key,
        has_key: true,
        source: "claude".into(),
        protocol: "anthropic".into(),
        env,
        models,
        ..ProviderDef::default()
    }])
}

fn read_codex(home: &Path) -> Result<Vec<ProviderDef>, String> {
    let data = match std::fs::read(home.join("config.toml")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("读取 Codex 配置失败".into()),
        Ok(data) => data,
    };
    let mut auth = Value::Null;
    if let Ok(body) = std::fs::read(home.join("auth.json")) {
        auth = serde_json::from_slice(&body).unwrap_or(Value::Null);
    }
    let text = std::str::from_utf8(&data).map_err(|_| "Codex 配置格式无效".to_string())?;
    let cfg: toml::Value = toml::from_str(text).map_err(|_| "Codex 配置格式无效".to_string())?;
    let cfg = convert_toml(cfg);
    let selected = string_at(&cfg, "model_provider");
    let providers = object_at(&cfg, "model_providers");
    let Some(providers) = providers.as_object() else { return Ok(Vec::new()) };
    let mut names: Vec<&String> = providers.keys().collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let settings = providers.get(name).cloned().unwrap_or(Value::Null);
        let base = string_at(&settings, "base_url");
        if base.is_empty() {
            continue;
        }
        let mut key = std::env::var(string_at(&settings, "env_key")).unwrap_or_default();
        if key.is_empty() {
            key = string_at(&settings, "experimental_bearer_token");
        }
        if key.is_empty() && *name == selected {
            key = string_at(&auth, "OPENAI_API_KEY");
        }
        let mut model = string_at(&settings, "model");
        if *name == selected {
            model = string_at(&cfg, "model");
        }
        let mut label = string_at(&settings, "name");
        if label.is_empty() {
            label = name.clone();
        }
        let mut models = Vec::new();
        if !model.is_empty() {
            models.push((model.clone(), String::new()));
        }
        out.push(ProviderDef {
            id: format!("native:codex:provider:{name}"),
            name: label,
            source: "codex".into(),
            agent: "codex".into(),
            url: base,
            model,
            key,
            has_key: true,
            cc_current: *name == selected,
            protocol: "openai-responses".into(),
            models,
            native: settings,
            ..ProviderDef::default()
        });
    }
    Ok(out)
}

/// Go `Native` (native.go): scan the vendor CLI configs for API providers.
pub fn native() -> Result<Vec<ProviderDef>, String> {
    let mut out = read_dsh(&user_dir("DSH_HOME", ".dsh"))?;
    out.extend(read_codex(&user_dir("CODEX_HOME", ".codex"))?);
    out.extend(read_claude(&user_dir("CLAUDE_CONFIG_DIR", ".claude"))?);
    Ok(out)
}

fn read_api_providers(root: &Path) -> Result<(Vec<Value>, BTreeMap<String, String>), String> {
    let data = store_file::read_json(&root.join(".data").join("api-providers.json"))?
        .unwrap_or(Value::Null);
    let providers = data
        .get("providers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut active = BTreeMap::new();
    for (k, v) in data.get("active").and_then(Value::as_object).into_iter().flatten() {
        if let Some(s) = v.as_str() {
            active.insert(k.clone(), s.to_string());
        }
    }
    Ok((providers, active))
}

fn provider_from_json(raw: &Value) -> ProviderDef {
    let get = |k: &str| raw.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    ProviderDef {
        id: get("id"),
        name: get("name"),
        agent: get("agent"),
        url: get("url"),
        model: get("model"),
        key: get("key"),
        has_key: raw.get("hasKey").and_then(Value::as_bool).unwrap_or(false),
        source: get("source"),
        cc_current: raw.get("ccCurrent").and_then(Value::as_bool).unwrap_or(false),
        env: {
            let mut m = BTreeMap::new();
            for (k, v) in raw.get("env").and_then(Value::as_object).into_iter().flatten() {
                if let Some(s) = v.as_str() {
                    m.insert(k.clone(), s.to_string());
                }
            }
            m
        },
        models: raw
            .get("models")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .map(|m| (string_at(m, "id"), string_at(m, "name")))
                    .collect()
            })
            .unwrap_or_default(),
        protocol: get("protocol"),
        native: raw.get("native").cloned().unwrap_or(Value::Null),
        api: raw.get("api").and_then(Value::as_bool).unwrap_or(false),
        enabled: raw.get("enabled").and_then(Value::as_bool),
    }
}

fn external_provider(id: &str) -> bool {
    id.starts_with("native:") || id.starts_with("ccswitch:") || id.starts_with("store:")
}

fn apply_metadata(root: &Path, p: &mut ProviderDef) -> Result<(), String> {
    if !external_provider(&p.id) {
        return Ok(());
    }
    let data = store_file::read_json(&root.join(".data").join("provider-metadata.json"))?
        .unwrap_or(Value::Null);
    if let Some(m) = data.get(&p.id) {
        if let Some(models) = m.get("models").and_then(Value::as_array) {
            p.models = models
                .iter()
                .map(|x| (string_at(x, "id"), string_at(x, "name")))
                .collect();
        }
        if let Some(enabled) = m.get("enabled").and_then(Value::as_bool) {
            p.enabled = Some(enabled);
        }
        let display = string_at(m, "displayName");
        if !display.is_empty() {
            p.name = display;
        }
    }
    Ok(())
}

/// Go `find` (providers.go:247).
pub fn find(root: &Path, id: &str) -> Result<ProviderDef, String> {
    let mut p = find_inner(root, id)?;
    apply_metadata(root, &mut p)?;
    Ok(p)
}

fn find_inner(root: &Path, id: &str) -> Result<ProviderDef, String> {
    if let Some(rest) = id.strip_prefix("native:") {
        let list = native()?;
        for p in list {
            if id == "native:codex:config" && p.agent == "codex" && p.cc_current {
                return Ok(p);
            }
            if p.id == id {
                return Ok(p);
            }
        }
        let _ = rest;
        return Err("原生供应商已不存在，请重新读取配置".into());
    }
    if id.starts_with("store:") {
        for p in model_providers(root)? {
            if p.id == id {
                return Ok(p);
            }
        }
        return Err("已保存的模型供应商不存在".into());
    }
    if id.starts_with("ccswitch:") {
        if id.starts_with("ccswitch:codex:") {
            return Err("Codex 不再从 CC Switch 读取，请选择 Codex 原生配置中的供应商".into());
        }
        let cc = ccswitch().map_err(|e| format!("读取 CC Switch 失败: {e}"))?;
        for p in cc {
            if p.id == id {
                if !p.has_key {
                    return Err("该 CC Switch 供应商没有 API 密钥，请先在 CC Switch 中切换到它".into());
                }
                return Ok(p);
            }
        }
        return Err("供应商不存在".into());
    }
    let (providers, _) = read_api_providers(root)?;
    for raw in providers {
        if raw.get("id").and_then(Value::as_str) == Some(id) {
            let mut p = provider_from_json(&raw);
            p.has_key = !p.key.is_empty();
            return Ok(p);
        }
    }
    Err("供应商不存在".into())
}

/// Go `Active` (providers.go:308): the provider actually backing an agent
/// right now, with Go's `configured` bool.
pub fn active(root: &Path, agent: &str) -> Result<(ProviderDef, bool), String> {
    let (providers, active_map) = read_api_providers(root)?;
    let chosen = active_map.get(agent).cloned().unwrap_or_default();
    if agent == "claude" && (chosen.is_empty() || chosen.starts_with("ccswitch:")) {
        let cc = ccswitch().map_err(|e| format!("读取 CC Switch 失败: {e}"))?;
        for p in cc {
            if p.id == chosen || (chosen.is_empty() && p.cc_current) {
                if !chosen.is_empty() && !p.has_key && !p.cc_current {
                    return Err("该 CC Switch 供应商没有 API 密钥，请先在 CC Switch 中切换到它".into());
                }
                return Ok((p, true));
            }
        }
        if !chosen.is_empty() {
            return Err("所选 CC Switch 供应商不存在".into());
        }
    }
    for raw in providers {
        let p = provider_from_json(&raw);
        if p.agent == agent && p.id == chosen {
            return Ok((p, true));
        }
    }
    Ok((ProviderDef::default(), false))
}

/// Convert a `toml::Value` tree into `serde_json::Value` (numbers → f64/i64).
pub fn convert_toml(value: toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::Number(i.into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Array(items) => Value::Array(items.into_iter().map(convert_toml).collect()),
        toml::Value::Table(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k, convert_toml(v));
            }
            Value::Object(out)
        }
        toml::Value::Datetime(s) => Value::String(s.to_string()),
    }
}

/// Convert a `serde_yaml::Value` tree into `serde_json::Value`.
pub fn convert_yaml(value: serde_yaml::Value) -> Value {
    match value {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Number(i.into())
            } else {
                serde_json::Number::from_f64(n.as_f64().unwrap_or(0.0))
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }
        }
        serde_yaml::Value::String(s) => Value::String(s),
        serde_yaml::Value::Sequence(items) => Value::Array(items.into_iter().map(convert_yaml).collect()),
        serde_yaml::Value::Mapping(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let key = match k {
                    serde_yaml::Value::String(s) => s,
                    other => serde_yaml::to_string(&other).unwrap_or_default().trim().to_string(),
                };
                out.insert(key, convert_yaml(v));
            }
            Value::Object(out)
        }
        serde_yaml::Value::Tagged(tagged) => convert_yaml(tagged.value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acp-prov-{tag}-{}",
            crate::backend::goformat::hex_id()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    #[test]
    fn clear_api_environment_drops_only_credentials() {
        let env = vec![
            "PATH=C:\\x".to_string(),
            "ANTHROPIC_BASE_URL=https://a".to_string(),
            "claude_code_subagent_model=m".to_string(),
            "OPENAI_API_KEY=k".to_string(),
            "CODEPLUSE_DSH_API_KEY=k2".to_string(),
            "NODE_OPTIONS=1".to_string(),
        ];
        assert_eq!(
            clear_api_environment(&env),
            vec!["PATH=C:\\x".to_string(), "NODE_OPTIONS=1".to_string()]
        );
    }

    #[test]
    fn is_api_rejects_non_http_and_keyless() {
        let mut p = ProviderDef::default();
        p.url = "https://api.example.com".into();
        p.key = "sk-abc".into();
        assert!(is_api(&p));
        p.key = "sk-ant-oat01-x".into();
        assert!(!is_api(&p));
        p.key = "sk-abc".into();
        p.url = "user:pass@host".into();
        assert!(!is_api(&p));
        p.url = String::new();
        assert!(!is_api(&p));
    }

    #[test]
    fn find_resolves_plain_and_store_ids() {
        let root = temp_root("find");
        fs::write(
            root.join(".data").join("api-providers.json"),
            serde_json::json!({
                "providers": [{"id": "abc", "name": "P1", "agent": "claude", "url": "https://x",
                               "model": "m", "key": "k"}],
                "active": {"claude": "abc"}
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join(".data").join("model-providers.json"),
            serde_json::json!({"nextId": 2, "entries": [
                {"id": 1, "name": "S1", "api_url": "https://s", "api_key_masked": "",
                 "agent_type": "claude_code", "model": null, "created_at": "", "updated_at": "",
                 "key": "sk"}
            ]})
            .to_string(),
        )
        .unwrap();

        let p = find(&root, "abc").unwrap();
        assert_eq!(p.agent, "claude");
        assert!(p.has_key);
        assert!(find(&root, "missing").is_err());

        let (active_p, configured) = active(&root, "claude").unwrap();
        assert!(configured);
        assert_eq!(active_p.id, "abc");

        let stored = model_providers(&root).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, "store:1");
        assert_eq!(stored[0].key, "sk");
        assert_eq!(stored[0].protocol, "anthropic");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn native_home_env_overrides_win() {
        // USERPROFILE-based default (no env override on this machine unless set).
        let saved = std::env::var("CODEX_HOME").ok();
        std::env::remove_var("CODEX_HOME");
        let home = home();
        assert!(native_home("codex").ends_with(".codex"));
        assert!(native_home("claude").ends_with(".claude"));
        std::env::set_var("CODEX_HOME", "X:\\custom");
        assert_eq!(native_home("codex"), "X:\\custom");
        if let Some(v) = saved {
            std::env::set_var("CODEX_HOME", v);
        } else {
            std::env::remove_var("CODEX_HOME");
        }
        let _ = home;
    }
}
