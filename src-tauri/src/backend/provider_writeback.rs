//! External provider write-back, ported from
//! `server-go/internal/providers/writeback.go` (plus `ccswitch.go`).
//!
//! A provider whose id starts with `native:`, `ccswitch:` or `store:` does not
//! live in `api-providers.json` — it lives in the CC Switch database, the
//! Codex/DSH native configuration, or the model-provider store. Editing one
//! therefore writes *through* to that source, which is what this module does.
//!
//! Go's shape is preserved step by step: the old record is resolved first
//! (an unknown id is an error), the key and the model list are inherited when
//! the request leaves them out, the write happens in the source's own format,
//! and finally the UI attributes (models / enabled / displayName) are mirrored
//! into `.data/provider-metadata.json` so the next read shows them again.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::backend::goformat;
use crate::backend::provider_catalog::{self, PModel, Provider};

/// Go's `externalProvider`.
pub fn is_external(id: &str) -> bool {
    id.starts_with("native:") || id.starts_with("ccswitch:") || id.starts_with("store:")
}

/// Go `saveExternal` (writeback.go:77).
///
/// `models` is `None` when the request did not carry the key at all — Go
/// distinguishes "absent" (inherit the old list) from "present but empty"
/// (keep the empty list), so the caller must pass the distinction through.
pub fn save_external(
    root: &Path,
    provider: &mut Provider,
    models: Option<&[PModel]>,
) -> Result<(), String> {
    let mut old = provider_catalog::find(root, &provider.id)?;
    if old.agent != provider.agent {
        return Err("不能更改供应商所属执行端".into());
    }
    let key_changed = !provider.key.is_empty();
    if !key_changed {
        provider.key = old.key.clone();
    }
    if provider.key.is_empty() {
        return Err("请输入 API 密钥".into());
    }
    match models {
        Some(list) => provider.models = list.to_vec(),
        None => provider.models = old.models.clone(),
    }
    if provider.enabled.is_none() {
        provider.enabled = old.enabled;
    }
    provider.source = old.source.clone();
    provider.cc_current = old.cc_current;
    old.has_key = !old.key.is_empty();

    let outcome = if provider.id.starts_with("ccswitch:") {
        if provider.agent != "claude" {
            Err("只有 Claude Code 使用 CC Switch".to_string())
        } else {
            write_cc_switch(provider, key_changed)
        }
    } else if provider.id.starts_with("native:codex:") {
        write_codex(provider, key_changed)
    } else if provider.id.starts_with("native:dsh:") {
        write_dsh(provider, key_changed)
    } else if provider.id.starts_with("store:") {
        write_store(root, provider)
    } else {
        Err("该来源暂不支持直接修改".to_string())
    };
    if let Err(error) = outcome {
        return Err(format!("保存供应商修改失败: {error}"));
    }
    write_metadata(root, provider)
}

/// Go `providerMetadata` (writeback.go:18): UI attributes keyed by the
/// original id — never a second copy of the provider or its key.
fn write_metadata(root: &Path, provider: &Provider) -> Result<(), String> {
    let path = root.join(".data").join("provider-metadata.json");
    let _guard = crate::backend::store_file::lock(&path)?;
    let mut data = crate::backend::store_file::read_json(&path)?
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();

    let mut entry = Map::new();
    // `json:"models"` has no omitempty: a nil list is written as null.
    let models = if provider.models.is_empty() {
        Value::Null
    } else {
        Value::Array(
            provider
                .models
                .iter()
                .map(|model| {
                    let mut row = Map::new();
                    row.insert("id".into(), json!(model.id));
                    if !model.name.is_empty() {
                        row.insert("name".into(), json!(model.name));
                    }
                    if model.enabled.is_some() {
                        row.insert("enabled".into(), json!(model.enabled));
                    }
                    Value::Object(row)
                })
                .collect(),
        )
    };
    entry.insert("models".into(), models);
    if let Some(enabled) = provider.enabled {
        entry.insert("enabled".into(), json!(enabled));
    }
    // Only the DSH source has no display name of its own to fall back on.
    if provider.id.starts_with("native:dsh:") {
        entry.insert("displayName".into(), json!(provider.name));
    }
    data.insert(provider.id.clone(), Value::Object(entry));

    // Go marshals `map[string]providerMetadata`: the outer keys are sorted,
    // the inner struct keeps its declared field order.
    let mut sorted: Vec<(String, Value)> = data.into_iter().collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let mut outer = Map::new();
    for (key, value) in sorted {
        outer.insert(key, value);
    }
    let bytes = goformat::marshal_pretty(&Value::Object(outer));
    replace_file(&path, &bytes)
}

/// Go `replaceFile` (writeback.go:58): write a sibling temp file, then rename
/// it over the target so a reader never sees a half-written file.
fn replace_file(path: &Path, data: &[u8]) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let temp = dir.join(format!(".codepluse-save-{}", goformat::hex_id()));
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp).map_err(|e| e.to_string())?;
        file.write_all(data).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        e.to_string()
    })
}

// ── CC Switch (SQLite) ──────────────────────────────────────────────────────

/// Go `writeCCSwitch` (writeback.go:144): update the provider row in
/// `~/.cc-switch/cc-switch.db`, keeping the rest of its settings intact.
fn write_cc_switch(provider: &Provider, key_changed: bool) -> Result<(), String> {
    let path = cc_switch_db_path()?;
    // `rw`: never create a replacement database if the source disappeared.
    let mut connection =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|e| e.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    let id = provider.id.trim_start_matches("ccswitch:").to_string();
    let raw: String = transaction
        .query_row(
            "SELECT settings_config FROM providers WHERE id=? AND app_type='claude'",
            [&id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut config: Value =
        serde_json::from_str(&raw).map_err(|_| "CC Switch 配置格式无效".to_string())?;
    if !config.is_object() {
        return Err("CC Switch 配置格式无效".into());
    }

    {
        let map = config.as_object_mut().expect("对象");
        let mut env = map
            .get("env")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        env.insert("ANTHROPIC_BASE_URL".into(), json!(provider.url));
        env.insert("ANTHROPIC_MODEL".into(), json!(provider.model));
        if key_changed {
            let has_token = env.contains_key("ANTHROPIC_AUTH_TOKEN");
            let has_api_key = env.contains_key("ANTHROPIC_API_KEY");
            if has_token || !has_api_key {
                env.insert("ANTHROPIC_AUTH_TOKEN".into(), json!(provider.key));
            }
            if has_api_key {
                env.insert("ANTHROPIC_API_KEY".into(), json!(provider.key));
            }
        }
        map.insert("env".into(), Value::Object(env));
        if map.contains_key("model") {
            map.insert("model".into(), json!(provider.model));
        }
    }

    // modelCatalog.models: keep the rows we do not touch, refresh ours.
    {
        let map = config.as_object_mut().expect("对象");
        let mut catalog = map
            .get("modelCatalog")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let mut existing: std::collections::HashMap<String, Map<String, Value>> =
            std::collections::HashMap::new();
        if let Some(rows) = catalog.get("models").and_then(|value| value.as_array()) {
            for row in rows {
                if let Some(object) = row.as_object() {
                    let key = object
                        .get("model")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string();
                    existing.insert(key, object.clone());
                }
            }
        }
        let mut models = Vec::new();
        for model in &provider.models {
            let mut row = existing.get(&model.id).cloned().unwrap_or_default();
            row.insert("model".into(), json!(model.id));
            row.insert("displayName".into(), json!(model.name));
            models.push(Value::Object(row));
        }
        catalog.insert("models".into(), Value::Array(models));
        map.insert("modelCatalog".into(), Value::Object(catalog));
    }

    // Go marshals the config map: keys sorted, HTML escaped.
    let bytes = goformat::marshal_preserve(&goformat::canon(&config));
    transaction
        .execute(
            "UPDATE providers SET name=?, settings_config=? WHERE id=? AND app_type='claude'",
            rusqlite::params![provider.name, String::from_utf8_lossy(&bytes), id],
        )
        .map_err(|e| e.to_string())?;
    transaction.commit().map_err(|e| e.to_string())
}

fn cc_switch_db_path() -> Result<PathBuf, String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|e| e.to_string())?;
    Ok(Path::new(&home).join(".cc-switch").join("cc-switch.db"))
}

// ── Codex (config.toml) ─────────────────────────────────────────────────────

/// Go `writeCodex` (writeback.go:223): update one entry of
/// `model_providers` in the Codex configuration.
fn write_codex(provider: &Provider, key_changed: bool) -> Result<(), String> {
    let home = codex_home();
    let file = home.join("config.toml");
    let raw = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    let mut config: toml::Value = toml::from_str(&raw).map_err(|e| e.to_string())?;

    let mut name = provider
        .id
        .trim_start_matches("native:codex:")
        .trim_start_matches("provider:")
        .to_string();
    let selected = config
        .as_table()
        .and_then(|table| table.get("model_provider"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if provider.id == "native:codex:config" {
        name = selected.clone();
    }
    let entry = config
        .as_table()
        .and_then(|table| table.get("model_providers"))
        .and_then(|value| value.as_table())
        .and_then(|table| table.get(&name))
        .and_then(|value| value.as_table())
        .cloned();
    let Some(mut entry) = entry else {
        return Err("Codex 供应商已不存在".into());
    };
    entry.insert("name".into(), toml::Value::String(provider.name.clone()));
    entry.insert("base_url".into(), toml::Value::String(provider.url.clone()));
    entry.insert("model".into(), toml::Value::String(provider.model.clone()));
    entry.insert("wire_api".into(), toml::Value::String("responses".into()));
    if selected == name {
        if let Some(table) = config.as_table_mut() {
            table.insert("model".into(), toml::Value::String(provider.model.clone()));
        }
    }
    if key_changed {
        entry.remove("env_key");
        entry.insert(
            "experimental_bearer_token".into(),
            toml::Value::String(provider.key.clone()),
        );
    }
    if let Some(table) = config.as_table_mut() {
        if let Some(providers) = table
            .get_mut("model_providers")
            .and_then(|value| value.as_table_mut())
        {
            providers.insert(name.as_str().into(), toml::Value::Table(entry));
        }
    }
    let rendered = toml::to_string(&config).map_err(|e| e.to_string())?;
    replace_file(&file, rendered.as_bytes())
}

fn codex_home() -> PathBuf {
    match std::env::var("CODEX_HOME") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => home_dir().join(".codex"),
    }
}

// ── DSH (cordis.patch.yml) ─────────────────────────────────────────────────

/// Go `writeDSH` (writeback.go:258): update the `llm-pi-ai` plugin entry of
/// the DSH profile's patch file.
fn write_dsh(provider: &Provider, key_changed: bool) -> Result<(), String> {
    let rest = provider.id.trim_start_matches("native:dsh:");
    let mut pieces = rest.splitn(2, ':');
    let scope = pieces.next().unwrap_or_default();
    let name = pieces.next().unwrap_or_default();
    if (scope != "desktop" && scope != "acp") || name.is_empty() {
        return Err("DSH 供应商标识无效".into());
    }
    let home = dsh_home();
    let file = home.join("profiles").join(scope).join("cordis.patch.yml");
    let raw = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    let mut plugins: Vec<serde_yaml::Value> =
        serde_yaml::from_str(&raw).map_err(|e| e.to_string())?;

    let mut found = false;
    for plugin in plugins.iter_mut() {
        let Some(object) = plugin.as_mapping_mut() else {
            continue;
        };
        let id = object
            .get(serde_yaml::Value::String("id".into()))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let Some(config) = object
            .get_mut(serde_yaml::Value::String("config".into()))
            .and_then(|value| value.as_mapping_mut())
        else {
            continue;
        };
        match id.as_str() {
            "llm-pi-ai" => {
                let Some(providers) = config
                    .get_mut(serde_yaml::Value::String("providers".into()))
                    .and_then(|value| value.as_mapping_mut())
                else {
                    continue;
                };
                let Some(entry) = providers
                    .get_mut(serde_yaml::Value::String(name.to_string()))
                    .and_then(|value| value.as_mapping_mut())
                else {
                    continue;
                };
                found = true;
                entry.insert(
                    serde_yaml::Value::String("baseURL".into()),
                    serde_yaml::Value::String(provider.url.clone()),
                );
                let api = match provider.protocol.as_str() {
                    "openai" => "openai-completions",
                    "openai-responses" => "openai-responses",
                    "anthropic" => "anthropic-messages",
                    other => other,
                };
                entry.insert(
                    serde_yaml::Value::String("api".into()),
                    serde_yaml::Value::String(api.to_string()),
                );
                if key_changed {
                    entry.remove(serde_yaml::Value::String("apiKeyEnv".into()));
                    entry.insert(
                        serde_yaml::Value::String("apiKey".into()),
                        serde_yaml::Value::String(provider.key.clone()),
                    );
                }
                // models: keep the rows we do not touch, refresh ours.
                let mut existing: std::collections::HashMap<String, serde_yaml::Value> =
                    std::collections::HashMap::new();
                if let Some(rows) = entry
                    .get(serde_yaml::Value::String("models".into()))
                    .and_then(|value| value.as_sequence())
                {
                    for row in rows {
                        if let Some(mapping) = row.as_mapping() {
                            let key = mapping
                                .get(serde_yaml::Value::String("id".into()))
                                .and_then(|value| value.as_str())
                                .unwrap_or("")
                                .to_string();
                            existing.insert(key, row.clone());
                        }
                    }
                }
                let mut models = Vec::new();
                for model in &provider.models {
                    let mut row = existing
                        .get(&model.id)
                        .and_then(|value| value.as_mapping().cloned())
                        .map(serde_yaml::Mapping::from_iter)
                        .unwrap_or_default();
                    row.insert(
                        serde_yaml::Value::String("id".into()),
                        serde_yaml::Value::String(model.id.clone()),
                    );
                    row.insert(
                        serde_yaml::Value::String("name".into()),
                        serde_yaml::Value::String(model.name.clone()),
                    );
                    models.push(serde_yaml::Value::Mapping(row));
                }
                entry.insert(
                    serde_yaml::Value::String("models".into()),
                    serde_yaml::Value::Sequence(models),
                );
            }
            "agent-default-model" | "acp" => {
                let current = config
                    .get(serde_yaml::Value::String("provider".into()))
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if current == name {
                    config.insert(
                        serde_yaml::Value::String("model".into()),
                        serde_yaml::Value::String(provider.model.clone()),
                    );
                }
            }
            _ => {}
        }
    }
    if !found {
        return Err("DSH 供应商已不存在".into());
    }
    // Go's yaml.v3 sorts the keys of every mapping it marshals; serde_yaml
    // keeps the insertion order. Sorting here makes the two outputs match
    // (only the indentation width differs, which the parser ignores).
    let sorted: Vec<serde_yaml::Value> = plugins.into_iter().map(sort_yaml).collect();
    let rendered = serde_yaml::to_string(&sorted).map_err(|e| e.to_string())?;
    replace_file(&file, rendered.as_bytes())
}

/// Rebuilds a YAML tree with every mapping's keys in sorted order.
fn sort_yaml(value: serde_yaml::Value) -> serde_yaml::Value {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            let mut sorted: Vec<(serde_yaml::Value, serde_yaml::Value)> =
                mapping.into_iter().collect();
            sorted.sort_by(|left, right| key_text(&left.0).cmp(&key_text(&right.0)));
            let mut out = serde_yaml::Mapping::new();
            for (key, item) in sorted {
                out.insert(key, sort_yaml(item));
            }
            serde_yaml::Value::Mapping(out)
        }
        serde_yaml::Value::Sequence(items) => {
            serde_yaml::Value::Sequence(items.into_iter().map(sort_yaml).collect())
        }
        other => other,
    }
}

fn key_text(key: &serde_yaml::Value) -> String {
    match key {
        serde_yaml::Value::String(text) => text.clone(),
        other => serde_yaml::to_string(other).unwrap_or_default(),
    }
}

fn dsh_home() -> PathBuf {
    match std::env::var("DSH_HOME") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => home_dir().join(".dsh"),
    }
}

fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

// ── model-provider store ────────────────────────────────────────────────────

/// Go `modelproviders.Update(root, id, &name, &url, &key, nil, &model)`.
fn write_store(root: &Path, provider: &Provider) -> Result<(), String> {
    let id: i64 = provider
        .id
        .trim_start_matches("store:")
        .parse()
        .map_err(|_| "供应商编号无效".to_string())?;
    let args = json!({
        "id": id,
        "name": provider.name,
        "apiUrl": provider.url,
        "apiKey": provider.key,
        "model": provider.model,
    });
    crate::backend::providers::update(root, &args).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_ids_are_recognised() {
        assert!(is_external("native:codex:provider:x"));
        assert!(is_external("ccswitch:abc"));
        assert!(is_external("store:12"));
        assert!(!is_external("plain-id"));
    }

    #[test]
    fn metadata_is_written_with_go_shape() {
        let root = std::env::temp_dir().join(format!("codepluse-writeback-{}", goformat::hex_id()));
        std::fs::create_dir_all(root.join(".data")).unwrap();
        let provider = Provider {
            id: "ccswitch:abc".into(),
            name: "CC".into(),
            models: vec![PModel {
                id: "m1".into(),
                name: "M1".into(),
                enabled: None,
            }],
            enabled: Some(true),
            ..Provider::default()
        };
        write_metadata(&root, &provider).unwrap();
        let raw =
            std::fs::read_to_string(root.join(".data").join("provider-metadata.json")).unwrap();
        assert_eq!(
            raw,
            "{\n  \"ccswitch:abc\": {\n    \"models\": [\n      {\n        \"id\": \"m1\",\n        \"name\": \"M1\"\n      }\n    ],\n    \"enabled\": true\n  }\n}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
