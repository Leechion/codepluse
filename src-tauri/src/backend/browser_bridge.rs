//! 浏览器扩展桥 —— 补全批次（S8，2 条）。
//!
//! 前端 `codeg-ui/src/lib/browser/browser-bridge.ts` 已经在调用这 2 条命令，
//! 但原 Go sidecar（提交 76712f5）与 Rust 都没有实现过它们（全树搜不到字面量），
//! 契约只取自调用点参数名与 `browser-bridge.ts` 的 `BridgeStatus` /
//! `BridgeGrant` 结构，没有可移植的 Go 原型。
//!
//! 命令清单：
//! * `browser_bridge_status` —— 探测本机桥接监听端口，回确定形态的
//!   [`BridgeStatus`]；连不上就给 `enabled: false` 的离线结构，绝不伪造 ready。
//! * `browser_bridge_open` —— 为一个 loopback 目标地址申请桥接授权（grant）；
//!   没有可达桥接监听时返回中文错误，不编造 grant。
//!
//! 限制（L3）：本构建的工作台是桌面应用，浏览器标签直连宿主，不需要桥；
//! 桥接监听由 web 模式的服务端进程绑定，本模块只做端口探测与授权拼装。
//! 候选端口由环境变量 `CODEPLUSE_BRIDGE_PORTS`（逗号分隔）给出；未配置或全部
//! 拒绝连接时，`enabled` 为 `false`。

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use url::Url;

/// 端口探测超时：桥接端口就在本机，拒绝连接应立刻返回。
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);
/// 候选端口覆盖环境变量。
const PORTS_ENV: &str = "CODEPLUSE_BRIDGE_PORTS";
const PUBLIC_HOST_ENV: &str = "CODEPLUSE_BRIDGE_PUBLIC_HOST";
const HOST_PATTERN_ENV: &str = "CODEPLUSE_BRIDGE_HOST_PATTERN";
/// 桥接入口路径（与前端 `bridgeEntryUrl` 拼的 `entryPath` 对齐）。
const ENTRY_PATH: &str = "/__codeg_bridge/entry";

pub fn owns(command: &str) -> bool {
    matches!(command, "browser_bridge_open" | "browser_bridge_status")
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn run(_root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "browser_bridge_status" => status(),
        "browser_bridge_open" => open(args),
        _ => Err(format!("未知的浏览器桥命令：{command}")),
    }
}

// ── 探测 ────────────────────────────────────────────────────────────────

fn probe_open(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok()
}

fn configured_ports() -> Vec<u16> {
    std::env::var(PORTS_ENV)
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter_map(|part| part.trim().parse::<u16>().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn status_with(open_ports: Vec<u16>) -> Value {
    let enabled = !open_ports.is_empty();
    json!({
        "enabled": enabled,
        "ports": open_ports,
        "publicHost": env_string(PUBLIC_HOST_ENV),
        "hostPattern": env_string(HOST_PATTERN_ENV),
    })
}

fn status() -> Result<Value, String> {
    let open: Vec<u16> = configured_ports()
        .into_iter()
        .filter(|port| probe_open(*port))
        .collect();
    Ok(status_with(open))
}

// ── 授权 ────────────────────────────────────────────────────────────────

/// 与前端 `isLoopbackHost` 对齐的精简判定：localhost / *.localhost、::1、
/// ::、0.0.0.0 与整个 127/8。
fn is_loopback_host(hostname: &str) -> bool {
    let host = hostname.trim().trim_matches(&['[', ']'] as &[_]).to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if host == "::1" || host == "::" || host == "0.0.0.0" {
        return true;
    }
    let mut parts = host.split('.');
    if let (Some(first), Some(second), Some(third), Some(fourth)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        if parts.next().is_none() {
            if let (Ok(a), Ok(_b), Ok(_c), Ok(_d)) = (
                first.parse::<u8>(),
                second.parse::<u8>(),
                third.parse::<u8>(),
                fourth.parse::<u8>(),
            ) {
                return a == 127;
            }
        }
    }
    false
}

fn open_with(open_ports: &[u16], args: &Value) -> Result<Value, String> {
    let _tab_id = args
        .get("tabId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 tabId 参数".to_string())?
        .to_string();
    let url_raw = args
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 url 参数".to_string())?;

    let parsed = Url::parse(url_raw).map_err(|_| "url 不是合法地址".to_string())?;
    if parsed.scheme() != "http" {
        return Err("桥接只携带本机 http 地址".into());
    }
    if !is_loopback_host(parsed.host_str().unwrap_or_default()) {
        return Err("桥接目标不是本机 loopback 地址".into());
    }

    let Some(&bridge_port) = open_ports.first() else {
        return Err("浏览器桥未启用：本机没有可达的桥接监听端口".into());
    };

    let target_port = parsed.port().unwrap_or(80);
    let mut path = parsed.path().to_string();
    if let Some(query) = parsed.query() {
        path = format!("{path}?{query}");
    }
    Ok(json!({
        "targetPort": target_port,
        "bridgePort": bridge_port,
        "bridgeHost": Value::Null,
        "entryPath": ENTRY_PATH,
        "publicHost": env_string(PUBLIC_HOST_ENV),
        "path": path,
    }))
}

fn open(args: &Value) -> Result<Value, String> {
    let open: Vec<u16> = configured_ports()
        .into_iter()
        .filter(|port| probe_open(*port))
        .collect();
    open_with(&open, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-bridge-{tag}-{}-{}",
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
    fn owns_covers_the_two_documented_commands_only() {
        for command in ["browser_bridge_open", "browser_bridge_status"] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("browser_bridge_close"));
        assert!(!owns("browser_open"));
        assert!(!owns("BROWSER_BRIDGE_STATUS"));
    }

    #[test]
    fn status_is_off_with_an_empty_deterministic_shape_when_no_listener() {
        let status = status_with(Vec::new());
        assert_eq!(status["enabled"], false);
        assert_eq!(status["ports"].as_array().unwrap().len(), 0);
        assert!(status["publicHost"].is_null());
        assert!(status["hostPattern"].is_null());
    }

    #[test]
    fn status_reports_open_ports_from_a_local_stub() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let status = status_with(vec![port]);
        assert_eq!(status["enabled"], true);
        assert_eq!(status["ports"], json!([port]));
    }

    #[test]
    fn open_builds_a_grant_for_a_reachable_loopback_target() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let bridge_port = listener.local_addr().unwrap().port();
        let grant = open_with(
            &[bridge_port],
            &json!({
                "url": "http://127.0.0.1:3000/install?tab=1",
                "tabId": "tab-1",
            }),
        )
        .unwrap();
        assert_eq!(grant["targetPort"], 3000);
        assert_eq!(grant["bridgePort"], bridge_port);
        assert_eq!(grant["entryPath"], ENTRY_PATH);
        assert_eq!(grant["path"], "/install?tab=1");
        assert!(grant["bridgeHost"].is_null());
    }

    #[test]
    fn open_rejects_missing_bad_or_non_loopback_inputs() {
        let _root = temp_root("open-bad");
        let ports = vec![];
        // 缺参。
        assert!(open_with(&ports, &json!({})).unwrap_err().contains("缺少 tabId"));
        assert!(open_with(&ports, &json!({ "tabId": "t" }))
            .unwrap_err()
            .contains("缺少 url"));
        // 非 http、非 loopback。
        assert!(open_with(
            &ports,
            &json!({ "url": "https://example.com", "tabId": "t" })
        )
        .unwrap_err()
        .contains("http"));
        assert!(open_with(
            &ports,
            &json!({ "url": "http://192.168.1.10:3000", "tabId": "t" })
        )
        .unwrap_err()
        .contains("loopback"));
        // 没有可达监听：不编造 grant。
        let err = open_with(
            &ports,
            &json!({ "url": "http://127.0.0.1:3000", "tabId": "t" }),
        )
        .unwrap_err();
        assert!(err.contains("浏览器桥未启用"), "{err}");
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
