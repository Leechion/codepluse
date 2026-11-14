//! Desktop settings that were still only exposed by the former sidecar.
//! Values share `codeg-workspace.json` with the workspace settings store.

use std::path::Path;

use serde_json::{json, Value};

use crate::backend::store_file;

const FILE: &str = "codeg-workspace.json";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "get_system_language_settings"
            | "update_system_language_settings"
            | "get_system_rendering_settings"
            | "update_system_rendering_settings"
            | "get_system_proxy_settings"
            | "update_system_proxy_settings"
            | "get_system_autostart_settings"
            | "update_system_autostart_settings"
            | "get_system_close_behavior_settings"
            | "update_system_close_behavior_settings"
            | "get_chat_command_prefix"
            | "set_chat_command_prefix"
            | "get_chat_event_filter"
            | "set_chat_event_filter"
            | "get_chat_event_webhooks"
            | "set_chat_event_webhooks"
            | "get_chat_event_top_level_only"
            | "set_chat_event_top_level_only"
            | "get_chat_message_language"
            | "set_chat_message_language"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

pub fn disable_hardware_acceleration(root: &Path) -> bool {
    setting(
        root,
        "get_system_rendering_settings",
        json!({"disable_hardware_acceleration":false}),
    )
    .ok()
    .and_then(|value| {
        value
            .get("disable_hardware_acceleration")
            .and_then(Value::as_bool)
    })
    .unwrap_or(false)
}

pub fn apply_proxy_from_store(root: &Path) -> Result<(), String> {
    let proxy = setting(
        root,
        "get_system_proxy_settings",
        json!({"enabled":false,"proxy_url":null,"no_proxy":null}),
    )?;
    let enabled = proxy.get("enabled").and_then(Value::as_bool) == Some(true);
    let proxy_url = proxy.get("proxy_url").and_then(Value::as_str);
    let no_proxy = proxy.get("no_proxy").and_then(Value::as_str);
    if enabled && proxy_url.is_none() {
        return Err("已启用代理，但没有设置代理地址".into());
    }
    crate::backend::net::configure_proxy(if enabled { proxy_url } else { None }, no_proxy)
}

fn store_path(root: &Path) -> std::path::PathBuf {
    root.join(".data").join(FILE)
}

fn read(root: &Path) -> Result<Value, String> {
    Ok(store_file::read_json(&store_path(root))?.unwrap_or_else(|| json!({})))
}

fn setting(root: &Path, key: &str, fallback: Value) -> Result<Value, String> {
    Ok(read(root)?
        .get("settings")
        .and_then(|settings| settings.get(key))
        .cloned()
        .unwrap_or(fallback))
}

fn save(root: &Path, key: &str, value: Value) -> Result<Value, String> {
    let path = store_path(root);
    let _lock = store_file::lock(&path)?;
    let mut data = store_file::read_json(&path)?.unwrap_or_else(|| json!({}));
    let document = data.as_object_mut().ok_or("工作区设置格式无效")?;
    let settings = document.entry("settings").or_insert_with(|| json!({}));
    let settings = settings.as_object_mut().ok_or("工作区设置格式无效")?;
    settings.insert(key.to_string(), value.clone());
    store_file::write_json(&path, &data)?;
    Ok(value)
}

fn read_key_for_update(command: &str) -> String {
    format!("get_{}", command.trim_start_matches("update_"))
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "get_system_language_settings" => {
            setting(root, command, json!({"mode":"manual", "language":"zh_cn"}))
        }
        "update_system_language_settings" => {
            let settings = args.get("settings").ok_or("语言设置无效")?;
            let mode = settings.get("mode").and_then(Value::as_str).unwrap_or("");
            let language = settings
                .get("language")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !matches!(mode, "system" | "manual")
                || !matches!(
                    language,
                    "en" | "zh_cn" | "zh_tw" | "ja" | "ko" | "es" | "de" | "fr" | "pt" | "ar"
                )
            {
                return Err("语言设置无效".into());
            }
            save(
                root,
                &read_key_for_update(command),
                json!({"mode":mode,"language":language}),
            )
        }
        "get_system_rendering_settings" => setting(
            root,
            command,
            json!({"disable_hardware_acceleration":false}),
        ),
        "update_system_rendering_settings" => {
            let disabled = args
                .get("settings")
                .and_then(|settings| settings.get("disable_hardware_acceleration"))
                .and_then(Value::as_bool)
                .ok_or("图形渲染设置无效")?;
            save(
                root,
                &read_key_for_update(command),
                json!({"disable_hardware_acceleration":disabled}),
            )
        }
        "get_system_proxy_settings" => setting(
            root,
            command,
            json!({"enabled":false,"proxy_url":null,"no_proxy":null}),
        ),
        "update_system_proxy_settings" => {
            let settings = args.get("settings").ok_or("代理设置无效")?;
            let enabled = settings
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or("代理设置无效")?;
            let proxy_url = match settings.get("proxy_url") {
                None | Some(Value::Null) => Value::Null,
                Some(Value::String(url)) if valid_proxy_url(url) => json!(url),
                _ => return Err("代理地址无效".into()),
            };
            let no_proxy = match settings.get("no_proxy") {
                None | Some(Value::Null) => Value::Null,
                Some(Value::String(value)) => json!(value),
                _ => return Err("代理绕过列表无效".into()),
            };
            if enabled && proxy_url.is_null() {
                return Err("启用代理时必须填写代理地址".into());
            }
            let updated = save(
                root,
                &read_key_for_update(command),
                json!({"enabled":enabled,"proxy_url":proxy_url,"no_proxy":no_proxy}),
            )?;
            crate::backend::net::configure_proxy(
                if enabled {
                    updated.get("proxy_url").and_then(Value::as_str)
                } else {
                    None
                },
                updated.get("no_proxy").and_then(Value::as_str),
            )?;
            Ok(updated)
        }
        "get_system_autostart_settings" => Ok(json!({"enabled": autostart_enabled()?})),
        "update_system_autostart_settings" => {
            let enabled = args
                .get("settings")
                .and_then(|settings| settings.get("enabled"))
                .and_then(Value::as_bool)
                .ok_or("开机自启设置无效")?;
            set_autostart(enabled)?;
            Ok(json!({"enabled": autostart_enabled()?}))
        }
        "get_system_close_behavior_settings" => {
            let saved = setting(root, command, json!({"behavior":"exit"}))?;
            Ok(json!({
                "behavior": saved.get("behavior").and_then(Value::as_str).unwrap_or("exit"),
                "tray_available": false,
            }))
        }
        "update_system_close_behavior_settings" => {
            let behavior = args.get("behavior").and_then(Value::as_str).unwrap_or("");
            if !matches!(behavior, "ask" | "minimize" | "exit") {
                return Err("关闭行为无效".into());
            }
            let saved = save(
                root,
                "get_system_close_behavior_settings",
                json!({"behavior":behavior}),
            )?;
            Ok(json!({
                "behavior": saved.get("behavior").and_then(Value::as_str).unwrap_or("exit"),
                "tray_available": false,
            }))
        }
        "get_chat_command_prefix" => setting(root, command, json!("/")),
        "set_chat_command_prefix" => {
            let prefix = args
                .get("prefix")
                .and_then(Value::as_str)
                .ok_or("命令前缀必须是文本")?;
            if prefix.chars().count() > 8 || prefix.chars().any(char::is_control) {
                return Err("命令前缀不能超过 8 个字符或包含控制字符".into());
            }
            save(root, "get_chat_command_prefix", json!(prefix)).map(|_| Value::Null)
        }
        "get_chat_event_filter" => setting(root, command, Value::Null),
        "set_chat_event_filter" => {
            let filter = match args.get("filter") {
                None | Some(Value::Null) => Value::Null,
                Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
                    json!(items)
                }
                _ => return Err("事件筛选必须是字符串数组或 null".into()),
            };
            save(root, "get_chat_event_filter", filter).map(|_| Value::Null)
        }
        "get_chat_event_webhooks" => setting(root, command, json!([])),
        "set_chat_event_webhooks" => {
            let webhooks = args
                .get("webhooks")
                .and_then(Value::as_array)
                .ok_or("Webhook 设置必须是数组")?;
            for webhook in webhooks {
                let url = webhook.get("url").and_then(Value::as_str).unwrap_or("");
                let parsed = url::Url::parse(url).map_err(|_| "Webhook 地址无效")?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err("Webhook 地址必须使用 HTTP 或 HTTPS".into());
                }
                if !webhook.get("enabled").is_some_and(Value::is_boolean) {
                    return Err("Webhook enabled 必须是布尔值".into());
                }
            }
            save(root, "get_chat_event_webhooks", json!(webhooks)).map(|_| Value::Null)
        }
        "get_chat_event_top_level_only" => setting(root, command, json!(true)),
        "set_chat_event_top_level_only" => {
            let enabled = args
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or("enabled 必须是布尔值")?;
            save(root, "get_chat_event_top_level_only", json!(enabled)).map(|_| Value::Null)
        }
        "get_chat_message_language" => setting(root, command, json!("zh-CN")),
        "set_chat_message_language" => {
            let language = args
                .get("language")
                .and_then(Value::as_str)
                .ok_or("消息语言必须是文本")?;
            if language.trim().is_empty() || language.len() > 32 {
                return Err("消息语言无效".into());
            }
            save(root, "get_chat_message_language", json!(language)).map(|_| Value::Null)
        }
        _ => unreachable!("owned command must have a handler"),
    }
}

fn valid_proxy_url(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https" | "socks5") && url.host_str().is_some()
}

#[cfg(windows)]
fn autostart_value() -> Result<Option<String>, String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};

    let mut size = 0u32;
    let query = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            w!("Codepluse"),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if query.is_err() || size < 2 {
        return Ok(None);
    }
    let mut value = vec![0u16; size as usize / 2];
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            w!("Codepluse"),
            RRF_RT_REG_SZ,
            None,
            Some(value.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if read.is_err() {
        return Ok(None);
    }
    let end = value.iter().position(|c| *c == 0).unwrap_or(value.len());
    let value = String::from_utf16_lossy(&value[..end]);
    Ok(Some(value.trim_matches('"').to_string()))
}

#[cfg(windows)]
fn set_autostart(enabled: bool) -> Result<(), String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "Codepluse\0".encode_utf16().collect();
    if !enabled {
        let mut key = windows::Win32::System::Registry::HKEY::default();
        let open = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                Some(0),
                KEY_SET_VALUE,
                &mut key,
            )
        };
        if open.is_err() {
            return Ok(());
        }
        let status = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };
        let _ = unsafe { RegCloseKey(key) };
        // ERROR_FILE_NOT_FOUND is idempotent success.
        if status.0 != 0 && status.0 != 2 {
            return Err(format!("删除开机启动项失败：{status:?}"));
        }
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut value: Vec<u16> = format!("\"{}\"", exe.display())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut key = windows::Win32::System::Registry::HKEY::default();
    unsafe {
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            PWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        );
        if status.0 != 0 {
            return Err(format!("创建开机启动项失败：{status:?}"));
        }
        let bytes = std::slice::from_raw_parts(
            value.as_mut_ptr().cast::<u8>(),
            value.len() * std::mem::size_of::<u16>(),
        );
        let status = RegSetValueExW(key, PCWSTR(name.as_ptr()), Some(0), REG_SZ, Some(bytes));
        let _ = RegCloseKey(key);
        if status.0 != 0 {
            return Err(format!("保存开机启动项失败：{status:?}"));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn autostart_value() -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(not(windows))]
fn set_autostart(_enabled: bool) -> Result<(), String> {
    Err("此平台暂不支持设置开机自启".into())
}

fn autostart_enabled() -> Result<bool, String> {
    let Some(value) = autostart_value()? else {
        return Ok(false);
    };
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    Ok(std::path::Path::new(&value) == exe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "codepluse-desktop-settings-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".data")).unwrap();
        root
    }

    #[test]
    fn language_and_rendering_settings_round_trip() {
        let root = root();
        let language = handle(
            &root,
            "update_system_language_settings",
            &json!({"settings":{"mode":"manual","language":"en"}}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(language, json!({"mode":"manual","language":"en"}));
        assert_eq!(
            handle(&root, "get_system_language_settings", &json!({}))
                .unwrap()
                .unwrap(),
            language
        );

        let rendering = handle(
            &root,
            "update_system_rendering_settings",
            &json!({"settings":{"disable_hardware_acceleration":true}}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            handle(&root, "get_system_rendering_settings", &json!({}))
                .unwrap()
                .unwrap(),
            rendering
        );
    }

    #[test]
    fn invalid_proxy_and_language_settings_are_rejected() {
        let root = root();
        assert!(handle(
            &root,
            "update_system_language_settings",
            &json!({"settings":{"mode":"other","language":"en"}}),
        )
        .unwrap()
        .is_err());
        assert!(handle(
            &root,
            "update_system_proxy_settings",
            &json!({"settings":{"enabled":true,"proxy_url":"file:///tmp"}}),
        )
        .unwrap()
        .is_err());
    }
}
