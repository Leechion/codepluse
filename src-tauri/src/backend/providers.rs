//! Provider store — first subsystem ported off the Go backend (Phase 2 pilot).
//!
//! Mirrors `server-go/internal/modelproviders`: same file, same JSON shape,
//! same validation messages and masking rules. The Go side keeps working on the
//! same file, so both implementations can be A/B compared and rolled back.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub const FILE: &str = "model-providers.json";

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Info {
    pub id: i64,
    pub name: String,
    pub api_url: String,
    /// Always empty in responses; `omitempty` on the Go side.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    #[serde(default)]
    pub api_key_masked: String,
    #[serde(default)]
    pub agent_type: String,
    pub model: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Entry {
    #[serde(flatten)]
    pub info: Info,
    #[serde(default)]
    pub key: String,
}

#[derive(Serialize, Deserialize)]
struct Store {
    #[serde(rename = "nextId")]
    next_id: i64,
    #[serde(default)]
    entries: Vec<Entry>,
}

impl Default for Store {
    fn default() -> Self {
        Store {
            next_id: 1,
            entries: Vec::new(),
        }
    }
}

fn file(root: &Path) -> PathBuf {
    root.join(".data").join(FILE)
}

fn read(root: &Path) -> Result<Store, String> {
    let value = match crate::backend::store_file::read_json(&file(root))? {
        Some(value) => value,
        None => return Ok(Store::default()),
    };
    let mut store: Store =
        serde_json::from_value(value).map_err(|e| format!("解析供应商配置失败：{e}"))?;
    if store.next_id < 1 {
        store.next_id = 1;
    }
    Ok(store)
}

fn write(root: &Path, store: &Store) -> Result<(), String> {
    let value = serde_json::to_value(store).map_err(|e| format!("序列化供应商配置失败：{e}"))?;
    crate::backend::store_file::write_json(&file(root), &value)
}

/// Mirrors the Go mask: first/last four characters, fully masked when short.
fn mask(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "•".repeat(chars.len());
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

fn public(entry: &Entry) -> Info {
    let mut info = entry.info.clone();
    info.api_key = String::new();
    info.api_key_masked = mask(&entry.key);
    info
}

/// modelproviders.KeyByID: the plaintext key for local calls that must send it
/// (model catalog fetches). Never leaves this process except to the provider.
pub fn key_by_id(root: &Path, id: i64) -> Result<String, String> {
    let store = read(root)?;
    for entry in &store.entries {
        if entry.info.id == id {
            return Ok(entry.key.clone());
        }
    }
    Err("供应商不存在".into())
}

/// modelproviders.List for callers that need the stored rows (URL/model per
/// id), used by the provider catalog assembly.
pub fn list_infos(root: &Path) -> Result<Vec<Info>, String> {
    Ok(read(root)?
        .entries
        .iter()
        .map(|entry| entry.info.clone())
        .collect())
}

fn validate(name: &str, api_url: &str, agent_type: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("供应商名称不能为空".into());
    }
    let url = api_url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("API 地址必须是 http(s) 地址".into());
    }
    if agent_type.trim().is_empty() {
        return Err("请选择智能体类型".into());
    }
    Ok(())
}

/// Go writes `time.RFC3339`, which is second precision. `time`'s formatter
/// keeps subseconds, so they are stripped explicitly — otherwise the stored
/// timestamps differ from the sidecar's and every parity check sees a delta.
fn now() -> String {
    let stamp = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap_or_else(|_| OffsetDateTime::now_utc());
    stamp.format(&Rfc3339).unwrap_or_default()
}

fn opt(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn list(root: &Path) -> Result<Value, String> {
    let store = read(root)?;
    Ok(serde_json::to_value(
        store.entries.iter().map(public).collect::<Vec<_>>(),
    )
    .map_err(|e| e.to_string())?)
}

pub fn create(root: &Path, args: &Value) -> Result<Value, String> {
    let name = opt(args, "name").unwrap_or_default();
    let api_url = opt(args, "apiUrl").unwrap_or_default();
    let api_key = opt(args, "apiKey").unwrap_or_default();
    let agent_type = opt(args, "agentType").unwrap_or_default();
    let model = opt(args, "model");

    validate(&name, &api_url, &agent_type)?;
    if api_key.trim().is_empty() {
        return Err("API 密钥不能为空".into());
    }

    let _guard = crate::backend::store_file::lock(&file(root))?;
    let mut store = read(root)?;
    let stamp = now();
    let entry = Entry {
        info: Info {
            id: store.next_id,
            name: name.trim().to_string(),
            api_url: api_url.trim().trim_end_matches('/').to_string(),
            api_key: String::new(),
            api_key_masked: String::new(),
            agent_type: agent_type.trim().to_string(),
            model,
            created_at: stamp.clone(),
            updated_at: stamp,
        },
        key: api_key.trim().to_string(),
    };
    store.next_id += 1;
    store.entries.push(entry.clone());
    write(root, &store)?;
    // Go returns the raw Info here, so `api_key_masked` stays empty on create;
    // the UI re-reads the list right after and gets the mask from there.
    Ok(serde_json::to_value(&entry.info).map_err(|e| e.to_string())?)
}

pub fn update(root: &Path, args: &Value) -> Result<Value, String> {
    let id = args.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let _guard = crate::backend::store_file::lock(&file(root))?;
    let mut store = read(root)?;
    let index = store
        .entries
        .iter()
        .position(|e| e.info.id == id)
        .ok_or_else(|| "供应商不存在".to_string())?;

    {
        let entry = &mut store.entries[index];
        if let Some(name) = opt(args, "name") {
            entry.info.name = name.trim().to_string();
        }
        if let Some(url) = opt(args, "apiUrl") {
            entry.info.api_url = url.trim().trim_end_matches('/').to_string();
        }
        if let Some(agent_type) = opt(args, "agentType") {
            entry.info.agent_type = agent_type.trim().to_string();
        }
        if let Some(model) = opt(args, "model") {
            entry.info.model = Some(model.trim().to_string());
        }
        // Empty string clears the key, matching the Go contract.
        if let Some(key) = opt(args, "apiKey") {
            entry.key = key.trim().to_string();
        }
        validate(
            &entry.info.name,
            &entry.info.api_url,
            &entry.info.agent_type,
        )?;
        entry.info.updated_at = now();
    }

    write(root, &store)?;
    let provider = public(&store.entries[index]);
    Ok(json!({ "provider": provider, "affectedRunningSessions": 0 }))
}

pub fn delete(root: &Path, args: &Value) -> Result<Value, String> {
    let id = args.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let _guard = crate::backend::store_file::lock(&file(root))?;
    let mut store = read(root)?;
    let index = store
        .entries
        .iter()
        .position(|e| e.info.id == id)
        .ok_or_else(|| "供应商不存在".to_string())?;
    store.entries.remove(index);
    write(root, &store)?;
    Ok(Value::Null)
}

/// Dispatches the four commands this module owns. `None` means "not mine" and
/// the caller forwards to the Go backend.
pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    match command {
        "list_model_providers" => Some(list(root)),
        "create_model_provider" => Some(create(root, args)),
        "update_model_provider" => Some(update(root, args)),
        "delete_model_provider" => Some(delete(root, args)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-providers-{tag}-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn timestamps_match_go_rfc3339_second_precision() {
        let stamp = now();
        assert!(stamp.ends_with('Z'), "应为 UTC：{stamp}");
        assert_eq!(stamp.len(), 20, "秒级 RFC3339 固定 20 字符：{stamp}");
        assert!(!stamp.contains('.'), "不能带亚秒：{stamp}");
    }

    #[test]
    fn mask_matches_go_rules() {
        assert_eq!(mask(""), "");
        assert_eq!(mask("abc"), "•••");
        assert_eq!(mask("12345678"), "••••••••");
        assert_eq!(mask("123456789"), "1234…6789");
    }

    #[test]
    fn create_list_update_delete_roundtrip() {
        let root = temp_root("crud");
        let created = create(
            &root,
            &json!({"name":"测试","apiUrl":"https://api.example.com/","apiKey":"sk-1234567890","agentType":"claude"}),
        )
        .unwrap();
        assert_eq!(created["name"], "测试");
        assert_eq!(created["api_url"], "https://api.example.com");
        // Go parity: create returns the raw Info, so the mask is empty here and
        // arrives on the next list call.
        assert_eq!(created["api_key_masked"], "");

        let listed = list(&root).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["api_key_masked"], "sk-1…7890");

        // The plaintext key must never appear in a response.
        assert!(!listed.to_string().contains("sk-1234567890"));

        let updated = update(&root, &json!({"id":1,"name":"改名"})).unwrap();
        assert_eq!(updated["provider"]["name"], "改名");
        assert_eq!(updated["affectedRunningSessions"], 0);

        delete(&root, &json!({"id":1})).unwrap();
        assert_eq!(list(&root).unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn validation_and_missing_provider() {
        let root = temp_root("errors");
        let err = create(
            &root,
            &json!({"name":"","apiUrl":"https://x.dev","apiKey":"k","agentType":"claude"}),
        )
        .unwrap_err();
        assert_eq!(err, "供应商名称不能为空");

        let err = create(
            &root,
            &json!({"name":"a","apiUrl":"ftp://x","apiKey":"k","agentType":"claude"}),
        )
        .unwrap_err();
        assert_eq!(err, "API 地址必须是 http(s) 地址");

        let err = update(&root, &json!({"id":99,"name":"x"})).unwrap_err();
        assert_eq!(err, "供应商不存在");
    }

    #[test]
    fn reads_existing_go_store() {
        let root = temp_root("legacy");
        fs::create_dir_all(root.join(".data")).unwrap();
        fs::write(
            file(&root),
            br#"{"nextId":7,"entries":[{"id":3,"name":"legacy","api_url":"https://a.dev","api_key_masked":"","agent_type":"claude","model":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","key":"abcd1234efgh"}]}"#,
        )
        .unwrap();
        let listed = list(&root).unwrap();
        assert_eq!(listed[0]["id"], 3);
        assert_eq!(listed[0]["api_key_masked"], "abcd…efgh");
        // A new entry continues from the stored counter, not from len(entries).
        let created = create(
            &root,
            &json!({"name":"n","apiUrl":"https://b.dev","apiKey":"kkkkkkkkkk","agentType":"claude"}),
        )
        .unwrap();
        assert_eq!(created["id"], 7);
    }
}
