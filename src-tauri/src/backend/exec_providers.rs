//! Executor-side provider store (`api-providers.json`) plus the external
//! write-back path.
//!
//! Ported: `codepluse_provider_delete`, `codepluse_provider_save`.
//!
//! `save` has two destinations:
//! - a plain id → the `api-providers.json` record (this file),
//! - an external id (`native:` / `ccswitch:` / `store:`) → the source the
//!   record came from: the CC Switch database, the Codex/DSH native config
//!   or the model-provider store (`provider_writeback.rs`).
//!
//! Both share one validation chain (Go `saveProviderConfiguration` →
//! `providers.SaveConfiguration`), so an external edit is rejected by exactly
//! the same rules as a local one.
//!
//! The sibling commands stay in Go on purpose:
//!
//! - `codepluse_providers_list` merges CC Switch entries (`ccError`, masked keys)
//! - `codepluse_provider_activate` accepts `ccswitch:<id>` after reading Claude's
//!   own configuration
//! - `codepluse_provider_models` performs a network call with the stored key
//!
//! Writes use `serde_json::Value` with `preserve_order`, and new entries are
//! inserted in the same field order Go's struct emits, so the file stays
//! byte-comparable between the two implementations.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::backend::provider_catalog::{PModel, Provider};
use crate::backend::provider_writeback;

pub const FILE: &str = "api-providers.json";

fn file(root: &Path) -> std::path::PathBuf {
    root.join(".data").join(FILE)
}

fn text(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Go's `externalProvider`: ids that point at another store are written through
/// to that store instead of this file.
fn is_external(id: &str) -> bool {
    provider_writeback::is_external(id)
}

/// The subset of `url.Parse` + the checks in our code paths: absolute http(s),
/// no userinfo, no query, no fragment, non-empty host.
fn valid_url(raw: &str) -> bool {
    if raw.is_empty() || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    if raw.contains('?') || raw.contains('#') {
        return false;
    }
    let lower = raw.to_ascii_lowercase();
    let rest = match lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
    {
        Some(rest) => rest,
        None => return false,
    };
    let authority = match rest.find('/') {
        Some(index) => &rest[..index],
        None => rest,
    };
    !authority.is_empty() && !authority.contains('@')
}

/// Go's `providers.IsAPI`.
fn is_api(url: &str, key: &str) -> bool {
    valid_url(url) && !key.is_empty() && !key.starts_with("sk-ant-oat")
}

fn hex_id() -> String {
    use std::fmt::Write;
    let bytes: [u8; 12] = rand::random();
    let mut out = String::with_capacity(24);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Builds one provider record in the same field order as Go's `Provider` struct.
#[allow(clippy::too_many_arguments)]
fn build_entry(
    id: &str,
    name: &str,
    agent: &str,
    url: &str,
    model: &str,
    key: &str,
    models: Option<&Value>,
    protocol: &str,
    enabled: Option<bool>,
) -> Map<String, Value> {
    let mut entry = Map::new();
    entry.insert("id".into(), Value::String(id.into()));
    entry.insert("name".into(), Value::String(name.into()));
    entry.insert("agent".into(), Value::String(agent.into()));
    entry.insert("url".into(), Value::String(url.into()));
    entry.insert("model".into(), Value::String(model.into()));
    if !key.is_empty() {
        entry.insert("key".into(), Value::String(key.into()));
    }
    // The handler never sets API on input, so the stored value is false; the
    // response recomputes it.
    entry.insert("hasKey".into(), Value::Bool(false));
    entry.insert("source".into(), Value::String("codepluse".into()));
    if let Some(models) = models {
        if models
            .as_array()
            .map(|list| !list.is_empty())
            .unwrap_or(false)
        {
            entry.insert("models".into(), models.clone());
        }
    }
    if !protocol.is_empty() {
        entry.insert("protocol".into(), Value::String(protocol.into()));
    }
    entry.insert("api".into(), Value::Bool(false));
    if let Some(enabled) = enabled {
        entry.insert("enabled".into(), Value::Bool(enabled));
    }
    entry
}

/// The normalised request, shared by the local and the external path.
///
/// Go's `saveProviderConfiguration` builds a `providers.Provider` from the
/// request, then `providers.SaveConfiguration` normalises and validates it.
pub(crate) struct SaveInput {
    pub id: String,
    pub name: String,
    pub agent: String,
    pub url: String,
    pub model: String,
    pub key: String,
    pub protocol: String,
    pub enabled: Option<bool>,
    /// `None` when the request carried no `models` key at all — Go keeps the
    /// difference between "absent" (inherit) and "present but empty" (clear).
    pub models: Option<Vec<PModel>>,
    /// The raw `models` value, kept for the local store (it stores the
    /// request's own JSON).
    pub models_raw: Option<Value>,
}

/// Go `saveProviderConfiguration` + `providers.SaveConfiguration` (the parts
/// before the store is chosen).
pub(crate) fn normalize(args: &Value) -> Result<SaveInput, String> {
    let mut id = text(args, "id");
    if id.is_empty() {
        id = text(args, "sourceId");
    }
    let name = text(args, "name");
    let agent = text(args, "agent");
    let url = text(args, "url");
    let model = text(args, "model");
    let mut key = text(args, "key");
    let mut protocol = text(args, "protocol");
    let enabled = args.get("enabled").and_then(|v| v.as_bool());

    let models_raw: Option<Value> = match args.get("models") {
        None => None,
        Some(raw) => {
            if !raw.is_array() {
                return Err("模型列表格式无效".into());
            }
            Some(raw.clone())
        }
    };
    let models: Option<Vec<PModel>> = models_raw.as_ref().map(|raw| {
        raw.as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| PModel {
                        id: row
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: row
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        enabled: row.get("enabled").and_then(|v| v.as_bool()),
                    })
                    .collect()
            })
            .unwrap_or_default()
    });

    if protocol.is_empty() {
        protocol = match agent.as_str() {
            "claude" => "anthropic".into(),
            "codex" => "openai-responses".into(),
            _ => "openai".into(),
        };
    }
    let known = matches!(
        protocol.as_str(),
        "openai" | "openai-responses" | "anthropic"
    );
    if !known
        || (agent == "codex" && protocol != "openai-responses" && protocol != "openai")
        || (agent == "claude" && protocol != "anthropic")
    {
        return Err("该 Harness 不支持此 API 格式".into());
    }

    // providers.SaveConfiguration
    let name = name.trim().to_string();
    let url = url.trim().trim_end_matches('/').to_string();
    let mut model = model.trim().to_string();
    if model.is_empty() {
        if let Some(rows) = models_raw.as_ref().and_then(|m| m.as_array()) {
            if let Some(first) = rows.first() {
                model = first
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
        }
    }
    let agent_ok = matches!(agent.as_str(), "claude" | "dsh" | "codex");
    if name.is_empty() || model.is_empty() || !agent_ok || !valid_url(&url) {
        return Err("请填写名称、模型和有效的 HTTP(S) 地址，并选择执行端".into());
    }
    if key.is_empty() {
        key = String::new();
    }

    Ok(SaveInput {
        id,
        name,
        agent,
        url,
        model,
        key,
        protocol,
        enabled,
        models,
        models_raw,
    })
}

pub fn save(root: &Path, args: &Value) -> Result<Value, String> {
    let input = normalize(args)?;
    let path = file(root);
    let _guard = crate::backend::store_file::lock(&path)?;
    let mut data = crate::backend::store_file::read_json(&path)?
        .unwrap_or_else(|| serde_json::json!({"providers": [], "active": {}}));
    let providers = data
        .get_mut("providers")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| "供应商配置格式无效".to_string())?;

    let existing = if input.id.is_empty() {
        None
    } else {
        providers
            .iter()
            .position(|p| p.get("id").and_then(|v| v.as_str()) == Some(input.id.as_str()))
    };

    let entry = match existing {
        Some(index) => {
            let old = &providers[index];
            if old.get("agent").and_then(|v| v.as_str()).unwrap_or("") != input.agent {
                return Err("不能更改供应商所属执行端".into());
            }
            let mut key = input.key.clone();
            if key.is_empty() {
                key = old
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            let models = match input.models_raw.clone() {
                Some(value) => Some(value),
                None => old.get("models").cloned(),
            };
            let entry = build_entry(
                &input.id,
                &input.name,
                &input.agent,
                &input.url,
                &input.model,
                &key,
                models.as_ref(),
                &input.protocol,
                input.enabled,
            );
            providers[index] = Value::Object(entry.clone());
            entry
        }
        None => {
            if !input.id.is_empty() {
                return Err("供应商不存在".into());
            }
            if input.key.is_empty() {
                return Err("请输入 API 密钥".into());
            }
            let new_id = hex_id();
            let entry = build_entry(
                &new_id,
                &input.name,
                &input.agent,
                &input.url,
                &input.model,
                &input.key,
                input.models_raw.as_ref(),
                &input.protocol,
                input.enabled,
            );
            providers.push(Value::Object(entry.clone()));
            entry
        }
    };

    crate::backend::store_file::write_json(&path, &data)?;

    // --- post-processing: hasKey before the key is stripped ---
    let mut response = entry;
    let key = response
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    response.insert("hasKey".into(), Value::Bool(!key.is_empty()));
    response.insert("api".into(), Value::Bool(is_api(&input.url, &key)));
    response.remove("key");
    Ok(Value::Object(response))
}

/// The external path: the record lives in CC Switch / Codex / DSH / the model
/// provider store, so the edit is written through to that source.
pub fn save_external(root: &Path, args: &Value) -> Result<Value, String> {
    let input = normalize(args)?;
    if input.id.is_empty() {
        return Err("该来源暂不支持直接修改".into());
    }
    let mut provider = Provider {
        id: input.id.clone(),
        name: input.name.clone(),
        agent: input.agent.clone(),
        url: input.url.clone(),
        model: input.model.clone(),
        key: input.key.clone(),
        protocol: input.protocol.clone(),
        enabled: input.enabled,
        models: input.models.clone().unwrap_or_default(),
        ..Provider::default()
    };
    provider_writeback::save_external(root, &mut provider, input.models.as_deref())?;

    // Go answers with the Provider struct: hasKey and api recomputed, the key
    // and the env map dropped.
    provider.has_key = !provider.key.is_empty();
    provider.api = crate::backend::provider_catalog::is_api(&provider);
    Ok(crate::backend::provider_catalog::provider_public_json(
        &provider,
    ))
}

pub fn delete(root: &Path, args: &Value) -> Result<Value, String> {
    let id = text(args, "id");
    let path = file(root);
    let _guard = crate::backend::store_file::lock(&path)?;
    let Some(mut data) = crate::backend::store_file::read_json(&path)? else {
        return Err("供应商不存在".into());
    };
    let providers = data
        .get_mut("providers")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| "供应商配置格式无效".to_string())?;

    let index = providers
        .iter()
        .position(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        .ok_or_else(|| "供应商不存在".to_string())?;

    let agent = providers[index]
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    providers.remove(index);

    if let Some(active) = data.get_mut("active").and_then(|v| v.as_object_mut()) {
        if active.get(&agent).and_then(|v| v.as_str()) == Some(id.as_str()) {
            active.remove(&agent);
        }
    }

    crate::backend::store_file::write_json(&path, &data)?;
    Ok(Value::Null)
}

/// `None` means "not mine" and the caller forwards to the sidecar.
pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    match command {
        "codepluse_provider_delete" => Some(delete(root, args)),
        "codepluse_provider_save" => {
            let id = {
                let id = text(args, "id");
                if id.is_empty() {
                    text(args, "sourceId")
                } else {
                    id
                }
            };
            // External sources are written through to their own store.
            if is_external(&id) {
                return Some(save_external(root, args));
            }
            Some(save(root, args))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-exec-providers-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    #[test]
    fn create_and_update_round_trip() {
        let root = temp_root("create");
        let created = save(
            &root,
            &json!({"name": "p", "agent": "claude", "url": "https://x.dev", "model": "m", "key": "k"}),
        )
        .unwrap();
        let id = created.get("id").unwrap().as_str().unwrap().to_string();
        assert_eq!(created.get("hasKey").unwrap(), &json!(true));
        assert_eq!(created.get("api").unwrap(), &json!(true));
        assert!(created.get("key").is_none());

        let updated = save(
            &root,
            &json!({"id": id, "name": "p2", "agent": "claude", "url": "https://x.dev", "model": "m"}),
        )
        .unwrap();
        assert_eq!(updated.get("name").unwrap(), &json!("p2"));
        // The key was inherited from the stored record.
        assert_eq!(updated.get("hasKey").unwrap(), &json!(true));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn protocol_rules_match_go() {
        let root = temp_root("protocol");
        let err = save(
            &root,
            &json!({"name": "p", "agent": "claude", "url": "https://x.dev", "model": "m", "key": "k", "protocol": "openai"}),
        )
        .unwrap_err();
        assert_eq!(err, "该 Harness 不支持此 API 格式");

        let err = save(
            &root,
            &json!({"name": "", "agent": "claude", "url": "not-a-url", "model": ""}),
        )
        .unwrap_err();
        assert_eq!(err, "请填写名称、模型和有效的 HTTP(S) 地址，并选择执行端");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn external_ids_take_the_writeback_path() {
        let root = temp_root("external");
        // The record does not exist in any source → the same error Go reports.
        let err = save_external(
            &root,
            &json!({"id": "ccswitch:nope", "name": "p", "agent": "claude", "url": "https://x.dev", "model": "m", "key": "k"}),
        )
        .unwrap_err();
        assert!(!err.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn models_absent_is_not_the_same_as_empty() {
        let absent = normalize(&json!({"name": "p", "agent": "claude", "url": "https://x.dev", "model": "m", "key": "k"})).unwrap();
        assert!(absent.models.is_none());
        let empty = normalize(&json!({"name": "p", "agent": "claude", "url": "https://x.dev", "model": "m", "key": "k", "models": []})).unwrap();
        assert_eq!(empty.models.map(|list| list.len()), Some(0));
    }
}
