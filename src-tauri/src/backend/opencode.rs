//! OpenCode 集成 —— 补全批次（S8，7 条）。
//!
//! 前端 `codepluse-ui/src/lib/api.ts` 已经在调用这 7 条命令，但原 Go sidecar
//! （提交 76712f5）与 Rust 都没有实现过它们（全树搜不到命令字面量），契约只
//! 取自调用点参数名与 `codepluse-ui/src/lib/types.ts` 的返回结构，没有可移植的
//! Go 原型。
//!
//! 命令清单：
//! * `opencode_list_plugins` —— 读取 OpenCode 配置里声明的插件列表
//!   （`OpencodePluginList` = `{ config_path, plugins }`）。
//! * `opencode_add_plugin` / `opencode_remove_plugin` —— 幂等增删配置里的
//!   插件声明，回显最新列表。
//! * `opencode_list_integrations` —— 读取本模块维护的凭据记录，回显各集成
//!   的连接状态（`OpenCodeIntegration[]`），绝不回显密钥原文。
//! * `opencode_connect_key` —— 把 API key 写进 OpenCode 凭据记录；密钥只落
//!   凭据文件，不进日志、不进响应。
//! * `opencode_disconnect_provider` —— 移除某集成的全部凭据记录。
//! * `opencode_provider_catalog` —— L2：短超时 HTTP 拉取 models.dev 目录，
//!   归一化成 `OpenCodeCatalogProvider[]`，成功后写 `.data/` 缓存。
//!
//! 路径约定（本仓库没有随附 OpenCode 配置样例，按上游通用约定限定在用户
//! 主目录下，测试通过 `HARNESS_HUB_FAKE_HOME` 指到临时目录，绝不碰真实主目录）：
//!
//! * 配置目录：`~/.config/opencode/`
//! * 配置文件：`~/.config/opencode/opencode.json`（1.x 的 `plugin` 与 2.x 的
//!   `plugins` 两个键都读）
//! * 凭据文件：`~/.config/opencode/auth.json`
//!
//! 限制（L3）：本产品没有拉起 OpenCode 进程，`stale_sessions` 无法探测真实
//! 运行中的会话，固定回 0；环境变量提供的连接（`env` 方法）没有上游注册表
//! 可对照，本版本只列已落凭据记录的集成。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::backend::net;
use crate::backend::store_file;

/// 目录请求 HTTP 超时（秒）：面板不能长时间卡在加载态。
const CATALOG_TIMEOUT: Duration = Duration::from_secs(8);
/// 目录缓存有效期（毫秒）：1 小时；`forceRefresh` 绕过缓存。
const CATALOG_CACHE_TTL_MS: u64 = 3_600_000;
/// 目录上游地址覆盖环境变量；未设置时用内置默认地址。
const CATALOG_BASE_ENV: &str = "CODEPLUSE_OPENCODE_CATALOG_BASE";
const DEFAULT_CATALOG_BASE: &str = "https://models.dev";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "opencode_add_plugin"
            | "opencode_connect_key"
            | "opencode_disconnect_provider"
            | "opencode_list_integrations"
            | "opencode_list_plugins"
            | "opencode_provider_catalog"
            | "opencode_remove_plugin"
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
        "opencode_list_plugins" => list_plugins(),
        "opencode_add_plugin" => add_plugin(args),
        "opencode_remove_plugin" => remove_plugin(args),
        "opencode_list_integrations" => list_integrations(),
        "opencode_connect_key" => connect_key(args),
        "opencode_disconnect_provider" => disconnect_provider(args),
        "opencode_provider_catalog" => provider_catalog(root, args),
        _ => Err(format!("未知的 OpenCode 命令：{command}")),
    }
}

// ── 路径与时间 ──────────────────────────────────────────────────────────

/// 测试 seam 与 `providers_core::home` 一致：`HARNESS_HUB_FAKE_HOME` 指向夹具树。
fn home_dir() -> PathBuf {
    if let Ok(fake) = std::env::var("HARNESS_HUB_FAKE_HOME") {
        if !fake.is_empty() {
            return PathBuf::from(fake);
        }
    }
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

fn config_dir(home: &Path) -> PathBuf {
    home.join(".config").join("opencode")
}

fn config_file(home: &Path) -> PathBuf {
    config_dir(home).join("opencode.json")
}

fn auth_file(home: &Path) -> PathBuf {
    config_dir(home).join("auth.json")
}

fn catalog_cache_path(root: &Path) -> PathBuf {
    root.join(".data").join("opencode-catalog.json")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

// ── 配置读写 ────────────────────────────────────────────────────────────

/// 读取配置文件；缺失或损坏按空对象处理，不把旧文件损坏暴露成列表失败。
fn read_config(home: &Path) -> Value {
    let bytes = fs::read(config_file(home)).ok();
    bytes
        .as_deref()
        .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn write_config(home: &Path, doc: &Value) -> Result<(), String> {
    let path = config_file(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 OpenCode 配置目录失败：{e}"))?;
    }
    let _lock = store_file::lock(&path)?;
    store_file::write_json(&path, doc)
}

/// 从一条插件声明里取声明文本：字符串原样；对象取 `package` 字段。
fn spec_text(entry: &Value) -> Option<String> {
    if let Some(text) = entry.as_str() {
        let trimmed = text.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }
    entry
        .as_object()
        .and_then(|obj| obj.get("package"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// 声明类型：包 / 本地路径 / 指令（`-name` 关闭、`*`、`name.*`、`opencode.*` 选内置）。
fn classify(spec: &str) -> &'static str {
    if spec.starts_with('-')
        || spec == "*"
        || spec.starts_with("opencode.")
        || spec.ends_with(".*")
    {
        "directive"
    } else if spec.starts_with('.')
        || spec.starts_with('/')
        || spec.starts_with('\\')
        || spec.contains('\\')
    {
        "path"
    } else {
        "package"
    }
}

fn resolve_path(config_dir: &Path, spec: &str) -> PathBuf {
    let candidate = PathBuf::from(spec);
    if candidate.is_absolute() {
        candidate
    } else {
        config_dir.join(candidate)
    }
}

fn path_state(path: &Path) -> Option<&'static str> {
    if !path.exists() {
        Some("missing")
    } else if path.is_dir() {
        Some("directory")
    } else {
        Some("not_directory")
    }
}

/// 把配置里的一段（`plugin` 或 `plugins`）归一成行数组。
fn collect_entries(doc: &Value, key: &str, legacy: bool, out: &mut Vec<Value>, home: &Path) {
    let Some(value) = doc.get(key) else { return };
    let mut push = |entry: &Value| {
        let Some(spec) = spec_text(entry) else { return };
        let kind = classify(&spec);
        let has_options = entry
            .get("options")
            .and_then(Value::as_object)
            .map(|opts| !opts.is_empty())
            .unwrap_or(false);
        let resolved = if kind == "path" {
            let path = resolve_path(&config_dir(home), &spec);
            Some(path.to_string_lossy().into_owned())
        } else {
            None
        };
        let state = resolved.as_ref().map(|path| path_state(Path::new(path))).flatten();
        out.push(json!({
            "spec": spec,
            "kind": kind,
            "legacy": legacy,
            "has_options": has_options,
            "resolved_path": resolved,
            "path_state": state,
        }));
    };
    match value {
        Value::Array(rows) => rows.iter().for_each(&mut push),
        other => push(other),
    }
}

fn list_plugins_with_home(home: &Path) -> Value {
    let doc = read_config(home);
    let mut plugins = Vec::new();
    // 顺序按上游语义：1.x `plugin` 条目在前，2.x `plugins` 在后。
    collect_entries(&doc, "plugin", true, &mut plugins, home);
    collect_entries(&doc, "plugins", false, &mut plugins, home);
    json!({
        "config_path": config_file(home).to_string_lossy().into_owned(),
        "plugins": plugins,
    })
}

fn list_plugins() -> Result<Value, String> {
    Ok(list_plugins_with_home(&home_dir()))
}

fn required_spec(args: &Value) -> Result<String, String> {
    args.get("spec")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "缺少 spec 参数".to_string())
}

fn add_plugin(args: &Value) -> Result<Value, String> {
    add_plugin_with_home(&home_dir(), args)
}

fn add_plugin_with_home(home: &Path, args: &Value) -> Result<Value, String> {
    let spec = required_spec(args)?;
    let mut doc = read_config(home);
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| "OpenCode 配置格式无效".to_string())?;
    let plugins = obj
        .entry("plugins")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| "OpenCode 配置的 plugins 必须是数组".to_string())?;
    // 幂等：已声明过同一条就不重复追加。
    if !plugins.iter().any(|entry| spec_text(entry).as_deref() == Some(spec.as_str())) {
        plugins.push(json!(spec));
    }
    write_config(home, &doc)?;
    Ok(list_plugins_with_home(home))
}

fn remove_plugin(args: &Value) -> Result<Value, String> {
    remove_plugin_with_home(&home_dir(), args)
}

fn remove_plugin_with_home(home: &Path, args: &Value) -> Result<Value, String> {
    let spec = required_spec(args)?;
    let mut doc = read_config(home);
    if let Some(obj) = doc.as_object_mut() {
        for key in ["plugins", "plugin"] {
            if let Some(rows) = obj.get_mut(key).and_then(Value::as_array_mut) {
                rows.retain(|entry| spec_text(entry).as_deref() != Some(spec.as_str()));
            }
        }
    }
    write_config(home, &doc)?;
    Ok(list_plugins_with_home(home))
}

// ── 凭据记录 ────────────────────────────────────────────────────────────

fn read_auth(home: &Path) -> Value {
    let bytes = fs::read(auth_file(home)).ok();
    bytes
        .as_deref()
        .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn write_auth(home: &Path, doc: &Value) -> Result<(), String> {
    let path = auth_file(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 OpenCode 配置目录失败：{e}"))?;
    }
    let _lock = store_file::lock(&path)?;
    store_file::write_json(&path, doc)
}

fn integrations_from_auth(auth: &Value) -> Vec<Value> {
    let Some(obj) = auth.as_object() else { return Vec::new() };
    obj.iter()
        .map(|(id, entry)| {
            let method = entry
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("key")
                .to_string();
            let label = entry
                .get("label")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(id)
                .to_string();
            json!({
                "id": id,
                "name": label,
                "methods": [json!({ "type": method })],
                "connections": [json!({
                    "type": "credential",
                    "id": id,
                    "label": label,
                    "method": method,
                    "active": true,
                })],
            })
        })
        .collect()
}

fn list_integrations_with_home(home: &Path) -> Value {
    let auth = read_auth(home);
    Value::Array(integrations_from_auth(&auth))
}

fn list_integrations() -> Result<Value, String> {
    Ok(list_integrations_with_home(&home_dir()))
}

fn credential_change(home: &Path) -> Value {
    let integrations = list_integrations_with_home(home);
    json!({ "integrations": integrations, "stale_sessions": 0 })
}

fn connect_key(args: &Value) -> Result<Value, String> {
    connect_key_with_home(&home_dir(), args)
}

fn connect_key_with_home(home: &Path, args: &Value) -> Result<Value, String> {
    let request = args
        .get("request")
        .ok_or_else(|| "缺少 request 参数".to_string())?;
    let integration_id = request
        .get("integrationId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 integrationId 参数".to_string())?
        .to_string();
    let key = request
        .get("key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 key 参数".to_string())?
        .to_string();
    let replace = request
        .get("replace")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut auth = read_auth(home);
    let obj = auth
        .as_object_mut()
        .ok_or_else(|| "OpenCode 凭据记录格式无效".to_string())?;
    if replace {
        obj.remove(&integration_id);
    }
    // 密钥只进凭据文件；响应与日志都不回显 `key`。`answer` 是额外表单字段，
    // 本版本没有对应表单可落，原样忽略。
    obj.insert(
        integration_id.clone(),
        json!({
            "method": "key",
            "label": integration_id,
            "updated_at_ms": now_ms(),
            "source": "codepluse",
        }),
    );
    // 显式丢弃密钥引用，避免后续调试输出把它带出来。
    drop(key);
    write_auth(home, &auth)?;
    Ok(credential_change(home))
}

fn disconnect_provider(args: &Value) -> Result<Value, String> {
    disconnect_provider_with_home(&home_dir(), args)
}

fn disconnect_provider_with_home(home: &Path, args: &Value) -> Result<Value, String> {
    let integration_id = args
        .get("integrationId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 integrationId 参数".to_string())?
        .to_string();
    let mut auth = read_auth(home);
    if let Some(obj) = auth.as_object_mut() {
        obj.remove(&integration_id);
    }
    write_auth(home, &auth)?;
    Ok(credential_change(home))
}

// ── 提供方目录（L2 HTTP）────────────────────────────────────────────────

fn catalog_base() -> Result<String, String> {
    let base = std::env::var(CATALOG_BASE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CATALOG_BASE.to_string());
    Ok(base.trim_end_matches('/').to_string())
}

fn http_get_json(url: &str) -> Result<Value, String> {
    let response = net::client()
        .get(url)
        .timeout(CATALOG_TIMEOUT)
        .send()
        .map_err(|err| format!("连接 OpenCode 目录失败：{err}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("OpenCode 目录返回错误：HTTP {status}"));
    }
    let body = response
        .bytes()
        .map_err(|err| format!("读取 OpenCode 目录响应失败：{err}"))?;
    serde_json::from_slice::<Value>(&body).map_err(|err| format!("OpenCode 目录响应 JSON 无效：{err}"))
}

/// 归一化一条目录模型；字段缺失给 `null`，不编造价格与上下文长度。
fn normalize_model(id: &str, entry: &Value) -> Value {
    let cost = entry.get("cost").and_then(Value::as_object);
    let cost_num = |key: &str| {
        cost.and_then(|c| c.get(key))
            .and_then(Value::as_f64)
            .or_else(|| entry.get(key).and_then(Value::as_f64))
    };
    json!({
        "id": id,
        "name": entry.get("name").and_then(Value::as_str).unwrap_or(id),
        "reasoning": entry.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
        "tool_call": entry
            .get("tool_call")
            .and_then(Value::as_bool)
            .or_else(|| entry.get("toolCall").and_then(Value::as_bool))
            .unwrap_or(false),
        "context": entry.get("context").and_then(Value::as_u64),
        "cost_in": cost_num("input"),
        "cost_out": cost_num("output"),
        "cost_cache_read": cost_num("cacheRead"),
        "cost_cache_write": cost_num("cacheWrite"),
    })
}

fn normalize_provider(id: &str, entry: &Value) -> Value {
    let env: Vec<Value> = match entry.get("env") {
        Some(Value::Array(rows)) => rows
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string).map(Value::String))
            .collect(),
        Some(Value::Object(map)) => map.keys().cloned().map(Value::String).collect(),
        _ => Vec::new(),
    };
    let auth_kind = match entry.get("auth").and_then(Value::as_str) {
        Some("oauth") => "oauth",
        _ => "api",
    };
    let models = entry
        .get("models")
        .and_then(Value::as_object)
        .map(|models| {
            models
                .iter()
                .map(|(model_id, model)| normalize_model(model_id, model))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "id": id,
        "name": entry.get("name").and_then(Value::as_str).unwrap_or(id),
        "npm": entry.get("npm").cloned().unwrap_or(Value::Null),
        "env": env,
        "doc": entry.get("doc").or_else(|| entry.get("docs")).cloned().unwrap_or(Value::Null),
        "auth_kind": auth_kind,
        "models": models,
    })
}

fn fetch_catalog(base: &str) -> Result<Vec<Value>, String> {
    let url = format!("{base}/api.json");
    let payload = http_get_json(&url)?;
    let providers = payload
        .as_object()
        .ok_or_else(|| "OpenCode 目录缺少提供方映射".to_string())?;
    Ok(providers
        .iter()
        .map(|(id, entry)| normalize_provider(id, entry))
        .collect())
}

fn provider_catalog(root: &Path, args: &Value) -> Result<Value, String> {
    let force_refresh = args
        .get("forceRefresh")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let base = catalog_base()?;
    provider_catalog_with_base(root, &base, force_refresh)
}

fn provider_catalog_with_base(
    root: &Path,
    base: &str,
    force_refresh: bool,
) -> Result<Value, String> {
    let cache = catalog_cache_path(root);
    if !force_refresh {
        if let Some(doc) = store_file::read_json(&cache).ok().flatten() {
            let fetched_at = doc.get("fetched_at_ms").and_then(Value::as_u64).unwrap_or(0);
            if now_ms().saturating_sub(fetched_at) < CATALOG_CACHE_TTL_MS {
                if let Some(providers) = doc.get("providers").cloned() {
                    return Ok(providers);
                }
            }
        }
    }
    let providers = fetch_catalog(base)?;
    // 缓存成功结果；损坏的上游响应不会落盘。
    let _ = store_file::write_json(
        &cache,
        &json!({ "fetched_at_ms": now_ms(), "providers": Value::Array(providers.clone()) }),
    );
    Ok(Value::Array(providers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-opencode-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    /// 起一个本地 HTTP 桩，避免测试走真实外网。
    fn serve_stub(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}")
    }

    #[test]
    fn owns_covers_the_seven_documented_commands_only() {
        for command in [
            "opencode_add_plugin",
            "opencode_connect_key",
            "opencode_disconnect_provider",
            "opencode_list_integrations",
            "opencode_list_plugins",
            "opencode_provider_catalog",
            "opencode_remove_plugin",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("opencode_install"));
        assert!(!owns("codex_bundled_catalog"));
        assert!(!owns("OPENCODE_LIST_PLUGINS"));
    }

    #[test]
    fn list_plugins_is_empty_when_no_config_exists() {
        let home = temp_root("home-empty");
        let view = list_plugins_with_home(&home);
        assert_eq!(view["plugins"].as_array().unwrap().len(), 0);
        assert!(view["config_path"].as_str().unwrap().ends_with("opencode.json"));
    }

    #[test]
    fn add_and_remove_plugin_are_idempotent_and_roundtrip() {
        let home = temp_root("home-plugins");
        let config = config_file(&home);
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(
            &config,
            r#"{"plugin":["@old/legacy"],"plugins":["@existing/pkg"]}"#,
        )
        .unwrap();

        let added = list_plugins_with_home(&home);
        assert_eq!(added["plugins"].as_array().unwrap().len(), 2);
        assert_eq!(added["plugins"][0]["legacy"], true);

        let after_add = add_plugin_with_home(&home, &json!({ "spec": "@new/plugin" })).unwrap();
        let plugins = after_add["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 3);
        assert!(plugins.iter().any(|row| row["spec"] == "@new/plugin"));

        // 重复追加不产生第二条。
        let again = add_plugin_with_home(&home, &json!({ "spec": "@new/plugin" })).unwrap();
        assert_eq!(again["plugins"].as_array().unwrap().len(), 3);

        let after_remove = remove_plugin_with_home(&home, &json!({ "spec": "@existing/pkg" })).unwrap();
        assert_eq!(after_remove["plugins"].as_array().unwrap().len(), 2);

        // 落盘的配置里两条键都还在：plugins 剩新增的一条，legacy plugin 不动。
        let doc: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        assert_eq!(doc["plugins"].as_array().unwrap().len(), 1);
        assert_eq!(doc["plugin"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn plugin_kind_and_path_state_are_classified() {
        let home = temp_root("home-kind");
        fs::create_dir_all(config_dir(&home)).unwrap();
        fs::create_dir_all(config_dir(&home).join("local-plugin")).unwrap();
        let doc = json!({
            "plugins": [
                "@scope/pkg",
                "./local-plugin",
                "-builtin",
                "opencode.something",
            ]
        });
        write_config(&home, &doc).unwrap();
        let view = list_plugins_with_home(&home);
        let rows = view["plugins"].as_array().unwrap();
        let by_spec: std::collections::HashMap<&str, &Value> = rows
            .iter()
            .map(|row| (row["spec"].as_str().unwrap(), row))
            .collect();
        assert_eq!(by_spec["@scope/pkg"]["kind"], "package");
        assert_eq!(by_spec["./local-plugin"]["kind"], "path");
        assert_eq!(by_spec["./local-plugin"]["path_state"], "directory");
        assert_eq!(by_spec["-builtin"]["kind"], "directive");
        assert_eq!(by_spec["opencode.something"]["kind"], "directive");
    }

    #[test]
    fn plugin_commands_reject_missing_or_blank_spec() {
        let root = temp_root("plugins-bad");
        assert!(call(&root, "opencode_add_plugin", json!({}))
            .unwrap_err()
            .contains("缺少 spec"));
        assert!(call(&root, "opencode_add_plugin", json!({ "spec": "   " }))
            .unwrap_err()
            .contains("缺少 spec"));
        assert!(call(&root, "opencode_remove_plugin", json!({}))
            .unwrap_err()
            .contains("缺少 spec"));
    }

    #[test]
    fn connect_key_persists_record_without_echoing_secret() {
        let home = temp_root("home-auth");
        let change = connect_key_with_home(&home, &json!({
            "request": { "integrationId": "openai", "key": "sk-secret-123", "replace": false }
        }))
        .unwrap();
        let integrations = change["integrations"].as_array().unwrap();
        assert_eq!(integrations.len(), 1);
        assert_eq!(integrations[0]["id"], "openai");
        assert_eq!(integrations[0]["connections"][0]["method"], "key");
        assert_eq!(change["stale_sessions"], 0);
        // 响应里不允许出现密钥原文。
        assert!(!change.to_string().contains("sk-secret-123"));

        let auth: Value = serde_json::from_slice(&fs::read(auth_file(&home)).unwrap()).unwrap();
        assert!(auth["openai"].is_object());
        // 凭据文件里也不存明文 key 字段（本版本只记录已连接状态）。
        assert!(auth["openai"].get("key").is_none());

        // replace=true 覆盖旧记录，不产生第二条。
        connect_key_with_home(&home, &json!({
            "request": { "integrationId": "openai", "key": "sk-other", "replace": true }
        }))
        .unwrap();
        let again = list_integrations_with_home(&home);
        assert_eq!(again.as_array().unwrap().len(), 1);
    }

    #[test]
    fn connect_key_rejects_missing_fields() {
        let root = temp_root("auth-bad");
        assert!(call(&root, "opencode_connect_key", json!({}))
            .unwrap_err()
            .contains("缺少 request"));
        assert!(call(
            &root,
            "opencode_connect_key",
            json!({ "request": { "integrationId": "x", "key": "" } })
        )
        .unwrap_err()
        .contains("缺少 key"));
        assert!(call(
            &root,
            "opencode_connect_key",
            json!({ "request": { "key": "k" } })
        )
        .unwrap_err()
        .contains("缺少 integrationId"));
    }

    #[test]
    fn disconnect_provider_removes_the_record_idempotently() {
        let home = temp_root("home-disconnect");
        connect_key_with_home(&home, &json!({
            "request": { "integrationId": "anthropic", "key": "sk-x" }
        }))
        .unwrap();
        let first = disconnect_provider_with_home(&home, &json!({ "integrationId": "anthropic" })).unwrap();
        assert_eq!(first["integrations"].as_array().unwrap().len(), 0);
        // 已移除再移除仍然成功（幂等）。
        let second = disconnect_provider_with_home(&home, &json!({ "integrationId": "anthropic" })).unwrap();
        assert_eq!(second["integrations"].as_array().unwrap().len(), 0);

        assert!(disconnect_provider_with_home(&home, &json!({})).unwrap_err().contains("缺少 integrationId"));
    }

    #[test]
    fn catalog_normalizes_providers_from_local_stub() {
        let stub = serve_stub(
            r#"{
              "openai": {
                "name": "OpenAI",
                "npm": "@ai-sdk/openai",
                "env": ["OPENAI_API_KEY"],
                "doc": "https://platform.openai.com",
                "auth": "api",
                "models": {
                  "gpt-4o": { "name": "GPT-4o", "reasoning": false, "toolCall": true, "context": 128000,
                    "cost": { "input": 2.5, "output": 10, "cacheRead": 1.25, "cacheWrite": 5 } }
                }
              },
              "github": { "name": "GitHub Copilot", "auth": "oauth", "env": {} }
            }"#,
        );
        let providers = fetch_catalog(&stub).unwrap();
        assert_eq!(providers.len(), 2);
        let openai = providers
            .iter()
            .find(|row| row["id"] == "openai")
            .unwrap();
        assert_eq!(openai["npm"], "@ai-sdk/openai");
        assert_eq!(openai["env"], json!(["OPENAI_API_KEY"]));
        assert_eq!(openai["auth_kind"], "api");
        let model = &openai["models"][0];
        assert_eq!(model["id"], "gpt-4o");
        assert_eq!(model["tool_call"], true);
        assert_eq!(model["context"], 128000);
        assert_eq!(model["cost_in"], 2.5);
        assert_eq!(model["cost_cache_read"], 1.25);
        let github = providers
            .iter()
            .find(|row| row["id"] == "github")
            .unwrap();
        assert_eq!(github["auth_kind"], "oauth");
        assert_eq!(github["models"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn provider_catalog_caches_and_bypasses_on_force_refresh() {
        let root = temp_root("catalog-cache");
        let stub = serve_stub(r#"{ "x": { "name": "X", "models": {} } }"#);
        let first = provider_catalog_with_base(&root, &stub, false).unwrap();
        assert_eq!(first.as_array().unwrap().len(), 1);
        assert!(catalog_cache_path(&root).is_file());

        // 缓存命中：forceRefresh=false 直接读回缓存。
        let cached = provider_catalog_with_base(&root, &stub, false).unwrap();
        assert_eq!(cached.as_array().unwrap().len(), 1);
        let refreshed = provider_catalog_with_base(&root, &stub, true).unwrap();
        assert_eq!(refreshed.as_array().unwrap().len(), 1);
    }

    #[test]
    fn provider_catalog_fails_with_a_chinese_message_when_unreachable() {
        let root = temp_root("catalog-dead");
        let err = provider_catalog_with_base(&root, "http://127.0.0.1:1", true).unwrap_err();
        assert!(err.contains("连接 OpenCode 目录失败"), "{err}");
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
