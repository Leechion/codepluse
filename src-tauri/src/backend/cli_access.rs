//! CLI 访问开关与电脑驱动 —— 补全批次（S8，3 条）。
//!
//! 前端 `codepluse-ui/src/lib/api.ts`（`cli_access_status` / `cli_access_set_enabled`）
//! 与 `codepluse-ui/src/lib/computer/computer-api.ts`（`computer_driver_install`）已经
//! 在调用这 3 条命令，但原 Go sidecar（提交 76712f5）与 Rust 都没有实现过它们
//! （全树搜不到字面量），契约只取自调用点参数名与 `types.ts` 的返回结构。
//!
//! 命令清单：
//! * `cli_access_status` —— 回 [`CliAccessStatus`]：开关是否落盘、端点是否在跑、
//!   端点文件路径、未运行原因。
//! * `cli_access_set_enabled` —— 翻转 CLI 访问开关并落 `.data/`，回显最新状态。
//! * `computer_driver_install` —— L3：**不**真的下载或安装系统合成输入驱动，
//!   按 [`DriverInfo`] 结构回一个明确的未安装说明。
//!
//! 限制（L3）：本构建没有随附 `codepluse-cli` 端点服务，开关打开后 `running` 仍为
//! `false`，`error` 说明原因；cua-driver 二进制不在仓库分发范围，安装命令只回
//! 状态，不触发下载、不改系统驱动。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::backend::store_file;

/// CLI 访问开关持久化文件（相对 `root/.data/`）。
const SETTINGS_FILE: &str = "cli-access.json";
/// 端点文件发布位置（本构建不实际写出，仅回显约定路径）。
const ENDPOINT_FILE: &str = "cli-endpoint.json";
/// 开关打开但端点服务未运行时的说明。
const NOT_RUNNING_ERROR: &str = "CLI 端点服务未在本构建中运行";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "cli_access_set_enabled" | "cli_access_status" | "computer_driver_install"
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
        "cli_access_status" => status(root),
        "cli_access_set_enabled" => set_enabled(root, args),
        "computer_driver_install" => driver_install(),
        _ => Err(format!("未知的 CLI/驱动命令：{command}")),
    }
}

// ── CLI 访问开关 ────────────────────────────────────────────────────────

fn settings_path(root: &Path) -> PathBuf {
    root.join(".data").join(SETTINGS_FILE)
}

fn endpoint_path(root: &Path) -> PathBuf {
    root.join(".data").join(ENDPOINT_FILE)
}

fn read_enabled(root: &Path) -> bool {
    store_file::read_json(&settings_path(root))
        .ok()
        .flatten()
        .and_then(|doc| doc.get("enabled").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn status_value(root: &Path, enabled: bool) -> Value {
    // 本构建没有端点服务进程：开关只落盘，running 恒为 false。
    let running = false;
    let error = if enabled && !running {
        Some(NOT_RUNNING_ERROR.to_string())
    } else {
        None
    };
    json!({
        "enabled": enabled,
        "running": running,
        "endpointFile": endpoint_path(root).to_string_lossy().into_owned(),
        "error": error,
    })
}

fn status(root: &Path) -> Result<Value, String> {
    Ok(status_value(root, read_enabled(root)))
}

fn set_enabled(root: &Path, args: &Value) -> Result<Value, String> {
    let enabled = args
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "enabled 必须是布尔值".to_string())?;
    let path = settings_path(root);
    let _lock = store_file::lock(&path)?;
    store_file::write_json(&path, &json!({ "enabled": enabled }))?;
    crate::backend::emit_event("cli-access://changed", json!({ "enabled": enabled }));
    Ok(status_value(root, enabled))
}

// ── 电脑驱动（L3，不真安装）──────────────────────────────────────────────

fn driver_install() -> Result<Value, String> {
    // 不下载、不安装、不改系统驱动；按 DriverInfo 结构回明确说明。
    Ok(json!({
        "version": "",
        "supported": false,
        "installed": [],
        "path": Value::Null,
        "task": Value::Null,
        "error": "本构建不分发合成输入驱动（cua-driver）：未下载或安装系统驱动",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-cli-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    #[test]
    fn owns_covers_the_three_documented_commands_only() {
        for command in ["cli_access_set_enabled", "cli_access_status", "computer_driver_install"] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("cli_tool_status"));
        assert!(!owns("computer_driver_info"));
        assert!(!owns("COMPUTER_DRIVER_INSTALL"));
    }

    #[test]
    fn status_defaults_to_disabled_with_the_contract_shape() {
        let root = temp_root("status-default");
        let status = call(&root, "cli_access_status", json!({})).unwrap();
        assert_eq!(status["enabled"], false);
        assert_eq!(status["running"], false);
        assert!(status["endpointFile"].as_str().unwrap().ends_with(ENDPOINT_FILE));
        assert!(status["error"].is_null());
    }

    #[test]
    fn set_enabled_persists_and_echoes_state_with_an_error_when_offline() {
        let root = temp_root("set-enabled");
        let on = call(&root, "cli_access_set_enabled", json!({ "enabled": true })).unwrap();
        assert_eq!(on["enabled"], true);
        assert_eq!(on["running"], false);
        assert_eq!(on["error"], NOT_RUNNING_ERROR);

        // 落盘后重新读回。
        let again = call(&root, "cli_access_status", json!({})).unwrap();
        assert_eq!(again["enabled"], true);

        let off = call(&root, "cli_access_set_enabled", json!({ "enabled": false })).unwrap();
        assert_eq!(off["enabled"], false);
        assert!(off["error"].is_null());
    }

    #[test]
    fn set_enabled_rejects_non_boolean_input() {
        let root = temp_root("set-bad");
        assert!(call(&root, "cli_access_set_enabled", json!({}))
            .unwrap_err()
            .contains("enabled 必须是布尔值"));
        assert!(call(
            &root,
            "cli_access_set_enabled",
            json!({ "enabled": "yes" })
        )
        .unwrap_err()
        .contains("enabled 必须是布尔值"));
    }

    #[test]
    fn driver_install_reports_unsupported_without_installing_anything() {
        let root = temp_root("driver");
        let info = call(&root, "computer_driver_install", json!({})).unwrap();
        assert_eq!(info["supported"], false);
        assert_eq!(info["installed"].as_array().unwrap().len(), 0);
        assert!(info["version"].as_str().unwrap().is_empty());
        assert!(info["path"].is_null());
        assert!(info["task"].is_null());
        assert!(info["error"].as_str().unwrap().contains("不分发"));
        // 没有在 .data 下落任何驱动记录。
        assert!(!endpoint_path(&root).exists());
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
