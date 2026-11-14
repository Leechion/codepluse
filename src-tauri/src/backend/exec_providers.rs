//! Executor-side provider store (`api-providers.json`).
//!
//! Ported: `codepluse_provider_delete`, `codepluse_provider_save`.
//!
//! Both are pure read-modify-write of this one file — Go re-reads it on every
//! call, so a Rust implementation cannot lose updates that Go is holding in
//! memory. The sibling commands stay in Go on purpose:
//!
//! - `codepluse_providers_list` merges CC Switch entries (`ccError`, masked keys)
//! - `codepluse_provider_activate` accepts `ccswitch:<id>` after reading Claude's
//!   own configuration
//! - `codepluse_provider_models` performs a network call with the stored key
//!
//! `save` with an external id (`native:` / `ccswitch:` / `store:`) is *not*
//! handled here: Go writes those through to their source profile, so the
//! dispatcher forwards them (`handle` returns `None`).
//!
//! Writes use `serde_json::Value` with `preserve_order`, and new entries are
//! inserted in the same field order Go's struct emits, so the file stays
//! byte-comparable between the two implementations.

use std::path::Path;

use serde_json::{Map, Value};

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
/// by the sidecar, not by this file.
fn is_external(id: &str) -> bool {
    id.starts_with("native:") || id.starts_with("ccswitch:") || id.starts_with("store:")
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
        if models.as_array().map(|list| !list.is_empty()).unwrap_or(false) {
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

pub fn save(root: &Path, args: &Value) -> Result<Value, String> {
    // --- handler-level normalization (provider_configuration.go) ---
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

    let models: Option<Value> = match args.get("models") {
        None => None,
        Some(raw) => {
            if !raw.is_array() {
                return Err("模型列表格式无效".into());
            }
            Some(raw.clone())
        }
    };

    if protocol.is_empty() {
        protocol = match agent.as_str() {
            "claude" => "anthropic".into(),
            "codex" => "openai-responses".into(),
            _ => "openai".into(),
        };
    }
    let known = matches!(protocol.as_str(), "openai" | "openai-responses" | "anthropic");
    if !known
        || (agent == "codex" && protocol != "openai-responses" && protocol != "openai")
        || (agent == "claude" && protocol != "anthropic")
    {
        return Err("该 Harness 不支持此 API 格式".into());
    }

    // --- providers.SaveConfiguration ---
    let name = name.trim().to_string();
    let url = url.trim().trim_end_matches('/').to_string();
    let mut model = model.trim().to_string();
    if model.is_empty() {
        if let Some(models) = models.as_ref().and_then(|m| m.as_array()) {
            if let Some(first) = models.first() {
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
    let path = file(root);
    let _guard = crate::backend::store_file::lock(&path)?;
    let mut data = crate::backend::store_file::read_json(&path)?
        .unwrap_or_else(|| serde_json::json!({"providers": [], "active": {}}));
    let providers = data
        .get_mut("providers")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| "供应商配置格式无效".to_string())?;

    let existing = if id.is_empty() {
        None
    } else {
        providers
            .iter()
            .position(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
    };

    let entry = match existing {
        Some(index) => {
            let old = &providers[index];
            if old.get("agent").and_then(|v| v.as_str()).unwrap_or("") != agent {
                return Err("不能更改供应商所属执行端".into());
            }
            if key.is_empty() {
                key = old
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            let models = match models {
                Some(value) => Some(value),
                None => old.get("models").cloned(),
            };
            let entry = build_entry(
                &id,
                &name,
                &agent,
                &url,
                &model,
                &key,
                models.as_ref(),
                &protocol,
                enabled,
            );
            providers[index] = Value::Object(entry.clone());
            entry
        }
        None => {
            if !id.is_empty() {
                return Err("供应商不存在".into());
            }
            if key.is_empty() {
                return Err("请输入 API 密钥".into());
            }
            let new_id = hex_id();
            let entry = build_entry(
                &new_id,
                &name,
                &agent,
                &url,
                &model,
                &key,
                models.as_ref(),
                &protocol,
                enabled,
            );
            providers.push(Value::Object(entry.clone()));
            entry
        }
    };

    crate::backend::store_file::write_json(&path, &data)?;

    // --- post-processing: hasKey before the key is stripped ---
    let mut response = entry;
    response.insert("hasKey".into(), Value::Bool(!key.is_empty()));
    response.insert("api".into(), Value::Bool(is_api(&url, &key)));
    response.remove("key");
    Ok(Value::Object(response))
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

/// `None` means "not mine" and the caller forwards to Go.
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
            // External sources are written through by the sidecar.
            if is_external(&id) {
                return None;
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

    fn seed(root: &Path) {
        let store = json!({
            "providers": [
                {"id": "abc", "name": "A", "agent": "claude", "url": "https://a.dev", "model": "m", "hasKey": true, "api": true, "future_field": {"x": 1}},
                {"id": "ccswitch:zzz", "name": "Z", "agent": "claude", "url": "https://z.dev", "model": "m", "hasKey": true, "ccCurrent": true}
            ],
            "active": {"claude": "abc", "dsh": "keep"}
        });
        fs::write(file(root), serde_json::to_vec_pretty(&store).unwrap()).unwrap();
    }

    fn read_store(root: &Path) -> Value {
        serde_json::from_slice(&fs::read(file(root)).unwrap()).unwrap()
    }

    #[test]
    fn delete_removes_provider_and_active_pointer() {
        let root = temp_root("del");
        seed(&root);
        delete(&root, &json!({"id": "abc"})).unwrap();
        let store = read_store(&root);
        let ids: Vec<&str> = store["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["ccswitch:zzz"]);
        assert!(store["active"].get("claude").is_none());
        assert_eq!(store["active"]["dsh"], "keep");
    }

    #[test]
    fn delete_reports_missing_provider() {
        let root = temp_root("missing");
        seed(&root);
        assert_eq!(
            delete(&root, &json!({"id": "nope"})).unwrap_err(),
            "供应商不存在"
        );
    }

    #[test]
    fn save_creates_provider_and_hides_key() {
        let root = temp_root("create");
        seed(&root);
        let saved = save(
            &root,
            &json!({"name":"new","agent":"codex","url":"https://api.dev/","model":"gpt","key":"sk-abcdefghijk"}),
        )
        .unwrap();
        assert_eq!(saved["id"].as_str().unwrap().len(), 24);
        assert_eq!(saved["url"], "https://api.dev");
        assert_eq!(saved["protocol"], "openai-responses");
        assert_eq!(saved["hasKey"], true);
        assert_eq!(saved["api"], true);
        assert!(saved.get("key").is_none(), "响应不能带明文密钥");

        let store = read_store(&root);
        assert_eq!(store["providers"].as_array().unwrap().len(), 3);
        // Stored entry keeps the key on disk, Go parity.
        assert_eq!(store["providers"][2]["key"], "sk-abcdefghijk");
    }

    #[test]
    fn save_update_keeps_key_and_rejects_agent_change() {
        let root = temp_root("update");
        seed(&root);
        let saved = save(&root, &json!({"id":"abc","name":"A2","agent":"claude","url":"https://a.dev","model":"m2","key":"sk-newkey12345"})).unwrap();
        assert_eq!(saved["name"], "A2");
        assert_eq!(saved["hasKey"], true);
        // 未传 key 时必须沿用旧值，否则编辑表单会悄悄清空凭据。
        let again = save(&root, &json!({"id":"abc","name":"A3","agent":"claude","url":"https://a.dev","model":"m2"})).unwrap();
        assert_eq!(again["hasKey"], true);
        let store = read_store(&root);
        assert_eq!(store["providers"][0]["key"], "sk-newkey12345");
        let err = save(
            &root,
            &json!({"id":"abc","name":"A3","agent":"codex","url":"https://a.dev","model":"m"}),
        )
        .unwrap_err();
        assert_eq!(err, "不能更改供应商所属执行端");
    }

    #[test]
    fn save_validation_messages() {
        let root = temp_root("valid");
        seed(&root);
        assert_eq!(
            save(&root, &json!({"name":"x","agent":"claude","url":"https://a.dev","model":"m","protocol":"openai"}))
                .unwrap_err(),
            "该 Harness 不支持此 API 格式"
        );
        assert_eq!(
            save(&root, &json!({"name":"x","agent":"claude","url":"ftp://a.dev","model":"m","key":"k"})).unwrap_err(),
            "请填写名称、模型和有效的 HTTP(S) 地址，并选择执行端"
        );
        assert_eq!(
            save(&root, &json!({"name":"x","agent":"dsh","url":"https://a.dev","model":"m","key":""})).unwrap_err(),
            "请输入 API 密钥"
        );
        assert_eq!(
            save(&root, &json!({"id":"ghost","name":"x","agent":"claude","url":"https://a.dev","model":"m"})).unwrap_err(),
            "供应商不存在"
        );
    }

    #[test]
    fn save_forwards_external_ids() {
        let root = temp_root("external");
        seed(&root);
        assert!(handle(&root, "codepluse_provider_save", &json!({"id":"ccswitch:zzz"})).is_none());
    }
}
