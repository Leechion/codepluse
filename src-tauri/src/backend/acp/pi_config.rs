//! Pi's native global agent configuration commands.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::backend::store_file;

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "acp_load_pi_config" | "acp_update_pi_config" | "acp_validate_pi_command"
    )
}

pub fn handle(_root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(match command {
        "acp_load_pi_config" => load_config(),
        "acp_update_pi_config" => update_config(args),
        "acp_validate_pi_command" => validate_command(args),
        _ => unreachable!("owned command must have a handler"),
    })
}

fn pi_agent_dir() -> PathBuf {
    super::pi_trust::agent_dir()
}

fn read_value(path: &Path) -> Result<Value, String> {
    Ok(store_file::read_json(path)?.unwrap_or_else(|| json!({})))
}

fn object_at<'a>(root: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>, String> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| "Pi 配置格式无效".to_string())?;
    let entry = object.entry(key.to_string()).or_insert_with(|| json!({}));
    entry
        .as_object_mut()
        .ok_or_else(|| "Pi 配置格式无效".to_string())
}

fn load_config() -> Result<Value, String> {
    let dir = pi_agent_dir();
    let settings = read_value(&dir.join("settings.json"))?;
    let auth = read_value(&dir.join("auth.json"))?;
    let models = read_value(&dir.join("models.json"))?;
    let providers = models
        .get("providers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut custom_providers = Vec::new();
    for (id, provider) in providers {
        let rows = provider
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let models = rows
            .iter()
            .filter_map(|row| {
                let id = row.get("id")?.as_str()?;
                let reasoning = row.get("reasoning").and_then(Value::as_bool);
                let thinking_level_map = row
                    .get("thinkingLevelMap")
                    .or_else(|| row.get("thinking_level_map"))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                Some(json!({
                    "id": id,
                    "reasoning": reasoning,
                    "thinkingLevelMap": thinking_level_map,
                }))
            })
            .collect::<Vec<_>>();
        custom_providers.push(json!({
            "id": id,
            "baseUrl": provider.get("baseUrl").and_then(Value::as_str).unwrap_or(""),
            "api": provider.get("api").and_then(Value::as_str).unwrap_or("openai-completions"),
            "models": models,
        }));
    }
    custom_providers.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let auth_providers = auth
        .as_object()
        .map(|map| {
            let mut names: Vec<_> = map.keys().cloned().collect();
            names.sort();
            names
        })
        .unwrap_or_default();
    Ok(json!({
        "defaultProvider": settings.get("defaultProvider").cloned().unwrap_or(Value::Null),
        "defaultModel": settings.get("defaultModel").cloned().unwrap_or(Value::Null),
        "defaultThinkingLevel": settings.get("defaultThinkingLevel").cloned().unwrap_or(Value::Null),
        "authProviders": auth_providers,
        "customProviders": custom_providers,
    }))
}

fn update_config(args: &Value) -> Result<Value, String> {
    let provider = args
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if provider.is_empty() || model.is_empty() {
        return Err("Pi 提供商和模型不能为空".into());
    }
    let dir = pi_agent_dir();
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建 Pi 配置目录失败：{error}"))?;

    let settings_path = dir.join("settings.json");
    let _settings_lock = store_file::lock(&settings_path)?;
    let mut settings = read_value(&settings_path)?;
    let settings_object = settings
        .as_object_mut()
        .ok_or("Pi settings.json 必须是对象")?;
    settings_object.insert("defaultProvider".into(), json!(provider));
    settings_object.insert("defaultModel".into(), json!(model));
    if let Some(level) = args.get("thinkingLevel").and_then(Value::as_str) {
        settings_object.insert("defaultThinkingLevel".into(), json!(level));
    }

    let api_key = args.get("apiKey").and_then(Value::as_str).unwrap_or("");
    let auth_path = dir.join("auth.json");
    let _auth_lock = if !api_key.is_empty() {
        Some(store_file::lock(&auth_path)?)
    } else {
        None
    };
    let mut auth = read_value(&auth_path)?;
    if !api_key.is_empty() {
        if let Some(object) = auth.as_object_mut() {
            object.insert(
                provider.to_string(),
                json!({"type":"api_key","key":api_key}),
            );
        } else {
            return Err("Pi auth.json 必须是对象".into());
        }
    }

    let base_url = args
        .get("customBaseUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let models_path = dir.join("models.json");
    let _models_lock = if !base_url.is_empty() {
        Some(store_file::lock(&models_path)?)
    } else {
        None
    };
    let mut models_doc = read_value(&models_path)?;
    if !base_url.is_empty() {
        let parsed = url::Url::parse(base_url).map_err(|_| "自定义 Pi 提供商地址无效")?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err("自定义 Pi 提供商必须是 HTTP 或 HTTPS 地址".into());
        }
        let api = args
            .get("customApi")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("openai-completions");
        if !matches!(
            api,
            "openai-completions"
                | "openai-responses"
                | "anthropic-messages"
                | "google-generative-ai"
        ) {
            return Err("Pi 自定义提供商协议无效".into());
        }
        let providers = object_at(&mut models_doc, "providers")?;
        let current = providers
            .get(provider)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut model_ids = args
            .get("customModels")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| {
                current
                    .get("models")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            });
        if !model_ids.iter().any(|id| id == model) {
            model_ids.push(model.to_string());
        }
        model_ids.sort();
        model_ids.dedup();
        let reason = args.get("modelReasoning");
        let model_rows: Vec<Value> = model_ids
            .into_iter()
            .map(|id| {
                let mut row = current
                    .get("models")
                    .and_then(Value::as_array)
                    .and_then(|rows| {
                        rows.iter()
                            .find(|row| row.get("id").and_then(Value::as_str) == Some(id.as_str()))
                    })
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_else(|| {
                        Map::from_iter([("id".into(), json!(id)), ("name".into(), json!(id))])
                    });
                if id == model {
                    if let Some(reason) = reason.and_then(Value::as_object) {
                        if let Some(enabled) = reason.get("reasoning").and_then(Value::as_bool) {
                            row.insert("reasoning".into(), json!(enabled));
                        }
                        if let Some(map) = reason.get("thinkingLevelMap").and_then(Value::as_object)
                        {
                            row.insert("thinkingLevelMap".into(), json!(map));
                        }
                    }
                }
                Value::Object(row)
            })
            .collect();
        providers.insert(
            provider.to_string(),
            json!({"baseUrl":base_url,"api":api,"models":model_rows}),
        );
    }

    // Each file is individually replaced atomically; validation is complete
    // before any write so malformed input never leaves a half-updated set.
    store_file::write_json(&settings_path, &settings)?;
    if !api_key.is_empty() {
        store_file::write_json(&auth_path, &auth)?;
    }
    if !base_url.is_empty() {
        store_file::write_json(&models_path, &models_doc)?;
    }
    Ok(Value::Null)
}

fn validate_command(args: &Value) -> Result<Value, String> {
    let raw = args
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        return Ok(json!({"found":false,"resolvedPath":Value::Null,"version":Value::Null}));
    }
    let requested = Path::new(raw);
    let resolved = if requested.components().count() > 1 || requested.is_absolute() {
        requested
            .is_file()
            .then(|| std::fs::canonicalize(requested).unwrap_or_else(|_| requested.to_path_buf()))
    } else {
        find_on_path(raw)
    };
    let Some(path) = resolved else {
        return Ok(json!({"found":false,"resolvedPath":Value::Null,"version":Value::Null}));
    };
    let version = run_version(&path);
    Ok(json!({
        "found": true,
        "resolvedPath": path.to_string_lossy(),
        "version": version,
    }))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let mut names = vec![name.to_string()];
    #[cfg(windows)]
    if Path::new(name).extension().is_none() {
        names.extend([
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
        ]);
    }
    for dir in std::env::split_paths(&path) {
        for candidate in &names {
            let candidate = dir.join(candidate);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn run_version(path: &Path) -> Option<String> {
    #[cfg(windows)]
    let output = {
        use std::os::windows::process::CommandExt;
        let mut command = std::process::Command::new(path);
        command.arg("--version").creation_flags(0x0800_0000);
        command.output()
    };
    #[cfg(not(windows))]
    let output = std::process::Command::new(path).arg("--version").output();
    let output = output.ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout.lines().next().unwrap_or(&stdout).to_string())
    }
}
