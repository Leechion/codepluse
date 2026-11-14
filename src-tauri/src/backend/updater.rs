//! Update surface, ported from `server-go/internal/codeg/updater.go`.
//!
//! Ported: `app_update_status`, `app_update_state`, `perform_app_update`,
//! `rollback_app`, `restart_app` (constant answers) and `check_app_update`
//! (the optional manifest fetch — TLS arrived with `native-tls`).
//!
//! The network call is blocking, like Go's `http.Client{Timeout: 15s}`, and
//! must run on the blocking pool (see the dispatch in `mod.rs`).

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::backend::net;

pub const VERSION: &str = "0.42.0-go";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "check_app_update"
            | "app_update_status"
            | "app_update_state"
            | "perform_app_update"
            | "rollback_app"
            | "restart_app"
    )
}

pub fn handle(_root: &Path, command: &str, _args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(match command {
        "check_app_update" => check_update(),
        "app_update_status" => Ok(json!({
            "currentVersion": VERSION,
            "selfUpdateSupported": false,
            "capability": "unsupported",
            "runtime": "standalone",
            "rollbackAvailable": false,
            "liveProgress": false,
        })),
        "app_update_state" => Ok(json!({"seq": 0, "status": "idle"})),
        "perform_app_update" => Err("本构建不内置更新通道，请从发布页手动更新".into()),
        "rollback_app" => Err("本构建不内置回滚：保留旧版本目录即可手动回退".into()),
        "restart_app" => Err("请关闭窗口后手动重启（后台服务继续运行）".into()),
        _ => unreachable!(),
    })
}

/// updater.go's "no update" row: `update` is an explicit null there.
fn no_update() -> Value {
    json!({
        "currentVersion": VERSION,
        "update": Value::Null,
        "selfUpdateSupported": false,
        "capability": "unsupported",
        "runtime": "standalone",
        "restartDelayMs": 0,
        "rollbackAvailable": false,
        "liveProgress": false,
    })
}

/// updater.go `updaterCheck`: compare the running version against the optional
/// manifest URL in `HARNESS_HUB_UPDATE_MANIFEST`. Field extraction goes through
/// `Value` because Go's decode turns missing/null fields into empty strings.
fn check_update() -> Result<Value, String> {
    let manifest_url = std::env::var("HARNESS_HUB_UPDATE_MANIFEST")
        .unwrap_or_default()
        .trim()
        .to_string();
    check_update_with(&manifest_url)
}

fn check_update_with(manifest_url: &str) -> Result<Value, String> {
    if manifest_url.is_empty() {
        return Ok(no_update());
    }
    let response = net::get(manifest_url, Duration::from_secs(15))
        .map_err(|e| format!("无法读取更新清单：{e}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("更新清单返回 {status}"));
    }
    let body = response.bytes().map_err(|e| e.to_string())?;
    let manifest: Value =
        serde_json::from_slice(&body).map_err(|e| format!("更新清单格式无效：{e}"))?;
    let field = |key: &str| {
        manifest
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let version = field("version");
    if version.is_empty() || version == VERSION {
        return Ok(no_update());
    }
    Ok(json!({
        "currentVersion": VERSION,
        "selfUpdateSupported": false,
        "update": {
            "version": version,
            "body": field("body"),
            "date": field("date"),
            "url": field("url"),
        },
        "capability": "unsupported",
        "runtime": "standalone",
        "restartDelayMs": 0,
        "rollbackAvailable": false,
        "liveProgress": false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn call(command: &str) -> Result<Value, String> {
        handle(Path::new("."), command, &json!({})).unwrap()
    }

    #[test]
    fn status_and_state_are_constant() {
        assert_eq!(call("app_update_status").unwrap()["currentVersion"], VERSION);
        assert_eq!(call("app_update_status").unwrap()["capability"], "unsupported");
        assert_eq!(call("app_update_state").unwrap(), json!({"seq": 0, "status": "idle"}));
    }

    #[test]
    fn unavailable_channels_explain_themselves() {
        assert_eq!(
            call("perform_app_update").unwrap_err(),
            "本构建不内置更新通道，请从发布页手动更新"
        );
        assert_eq!(
            call("rollback_app").unwrap_err(),
            "本构建不内置回滚：保留旧版本目录即可手动回退"
        );
        assert_eq!(
            call("restart_app").unwrap_err(),
            "请关闭窗口后手动重启（后台服务继续运行）"
        );
    }

    #[test]
    fn check_without_manifest_reports_current() {
        assert_eq!(check_update_with(""), Ok(no_update()));
    }

    /// 本地 HTTP 清单服务器：验证成功路径与版本相等路径（Go 用 http.Get，明文
    /// HTTP 也走同一条路）。
    fn serve_manifest(payload: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
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
        format!("http://{address}/manifest.json")
    }

    #[test]
    fn check_with_newer_version_offers_update() {
        let url = serve_manifest(
            r#"{"version":"0.43.0-go","body":"notes","date":"2026-11-14","url":"https://dl/x"}"#,
        );
        let out = check_update_with(&url).unwrap();
        assert_eq!(out["update"]["version"], "0.43.0-go");
        assert_eq!(out["update"]["body"], "notes");
        assert_eq!(out["currentVersion"], VERSION);
    }

    #[test]
    fn check_with_same_version_or_empty_reports_current() {
        for payload in [
            r#"{"version":"0.42.0-go"}"#,
            r#"{"date":"2026-11-14"}"#,
        ] {
            let url = serve_manifest(payload);
            assert_eq!(check_update_with(&url).unwrap(), no_update(), "{payload}");
        }
    }

    #[test]
    fn check_with_missing_manifest_reports_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // 先读请求再应答，否则早到的 FIN 会让客户端看到连接错误。
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        assert_eq!(
            check_update_with(&format!("http://{address}/gone")).unwrap_err(),
            "更新清单返回 404"
        );
    }
}
