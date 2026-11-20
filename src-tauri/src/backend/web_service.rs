//! Web 服务（本地 HTTP 服务）的状态、配置与端口探测 —— 补全批次。
//!
//! 前端 `codepluse-ui/src/lib/api.ts` 的 `getWebServerStatus` / `getWebServiceConfig` /
//! `probeWebServicePort` / `startWebServer` / `stopWebServer` /
//! `updateWebServiceConfig` 已经在调这 6 条命令，但原 Go sidecar 与 Rust 都没有
//! 实现过它们（Go 最终提交里搜不到这些字面量），所以契约只由调用点和
//! `types.ts` / `api.ts` 的返回结构决定：
//!
//! * `WebServerInfo` = `{ port: number, token: string, addresses: string[] }`，
//!   服务不可用时 `get_web_server_status` 返回 `null`。
//! * `WebServiceConfig` = `{ token: string|null, port: number|null, autoStart: boolean }`。
//! * `WebServicePortProbe` = `{ port: number, state: "free"|"occupied"|"unknown" }`。
//!
//! ## limited 说明（单进程自服务）
//!
//! 本项目 Phase 4 之后是**单进程自服务**：当前进程自己就是那个监听
//! `127.0.0.1:<port>` 的 HTTP 服务（见 `main.rs` 的启动段与 `mod.rs` 的
//! `backend::serve`），不存在第二个可以独立拉起/关停的 sidecar web 服务。因此：
//!
//! * `start_web_server` **不会**再绑定新端口，也不去碰当前进程已有的监听；
//!   它只把调用方给定的 `token` / `port` 记进 `.data/web-service-config.json`，
//!   再回显当前进程的监听信息。
//! * `stop_web_server` **不会**停止当前进程的监听（那等于把应用关掉），也不会
//!   杀进程、释放端口；按前端约定回 `null`。
//! * `get_web_server_status` 对配置端口（缺省取 `CODEPLUSE_PORT` / 8840）做
//!   **真实 TCP 探测**：可达才回 `WebServerInfo`，不可达回 `null`，不编造在跑
//!   的服务。
//! * `probe_web_service_port` 只做 `TcpStream::connect_timeout` 探测，**绝不绑定
//!   端口**；拒绝连接记 `free`，超时等其它错误记 `unknown`，都不是 `Err`。
//!
//! 配置落盘在 `root/.data/web-service-config.json`。

use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::backend::store_file;

const CONFIG_FILE: &str = "web-service-config.json";
/// 与 `main.rs` 的 `DEFAULT_PORT` 对齐：环境变量未给定时的本机端口。
const DEFAULT_PORT: u16 = 8840;
/// 端口探测只等这么久：本地回环，毫秒级即可区分在听 / 拒绝。
const PROBE_TIMEOUT: Duration = Duration::from_millis(600);

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "get_web_server_status"
            | "get_web_service_config"
            | "probe_web_service_port"
            | "start_web_server"
            | "stop_web_server"
            | "update_web_service_config"
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
        "get_web_server_status" => server_status(root),
        "get_web_service_config" => get_config(root),
        "probe_web_service_port" => probe_port(root, args),
        "start_web_server" => start_server(root, args),
        "stop_web_server" => stop_server(),
        "update_web_service_config" => update_config(root, args),
        _ => Err(format!("未知的 Web 服务命令：{command}")),
    }
}

// ── 配置落盘 ──────────────────────────────────────────────────────────────

fn config_path(root: &Path) -> PathBuf {
    root.join(".data").join(CONFIG_FILE)
}

fn read_config(root: &Path) -> Value {
    store_file::read_json(&config_path(root))
        .ok()
        .flatten()
        .unwrap_or_else(|| json!({}))
}

/// 当前进程实际监听的端口：与 `main.rs::backend_port` 同一取值顺序。
fn default_port() -> u16 {
    std::env::var("CODEPLUSE_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT)
}

fn configured_port(config: &Value) -> Option<u16> {
    config
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
}

fn resolve_port(config: &Value, explicit: Option<u16>) -> u16 {
    explicit.or_else(|| configured_port(config)).unwrap_or_else(default_port)
}

fn normalize_port(value: Option<&Value>) -> Result<Option<u16>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(raw) => {
            let port = raw
                .as_u64()
                .ok_or_else(|| "port 必须是数字或 null".to_string())?;
            if !(1..=65535).contains(&port) {
                return Err("port 必须在 1 到 65535 之间".into());
            }
            Ok(Some(port as u16))
        }
    }
}

fn normalize_token(value: Option<&Value>) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => {
            let token = raw.trim().to_string();
            Ok((!token.is_empty()).then_some(token))
        }
        Some(_) => Err("token 必须是字符串或 null".into()),
    }
}

/// 校验并规范化一份 `WebServiceConfig`；缺省值只在读取侧补，写入侧要求字段齐全。
fn normalize_config(config: &Value) -> Result<Value, String> {
    if !config.is_object() {
        return Err("config 必须是对象".into());
    }
    let token = normalize_token(config.get("token"))?;
    let port = normalize_port(config.get("port"))?;
    let auto_start = config
        .get("autoStart")
        .and_then(Value::as_bool)
        .ok_or_else(|| "autoStart 必须是布尔值".to_string())?;
    Ok(json!({ "token": token, "port": port, "autoStart": auto_start }))
}

fn get_config(root: &Path) -> Result<Value, String> {
    let stored = read_config(root);
    let token = stored
        .get("token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let port = configured_port(&stored);
    let auto_start = stored
        .get("autoStart")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Ok(json!({ "token": token, "port": port, "autoStart": auto_start }))
}

fn update_config(root: &Path, args: &Value) -> Result<Value, String> {
    let config = args
        .get("config")
        .ok_or_else(|| "缺少 config 参数".to_string())?;
    let normalized = normalize_config(config)?;
    let path = config_path(root);
    let _guard = store_file::lock(&path)?;
    store_file::write_json(&path, &normalized)?;
    Ok(normalized)
}

// ── 端口探测 ────────────────────────────────────────────────────────────

/// 只连不绑：探测 `127.0.0.1:port` 是否在听。
fn probe_state(port: u16) -> &'static str {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    match TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
        Ok(_) => "occupied",
        Err(err) => {
            // Windows 上部分拒绝类错误没有被标准库映射到 ErrorKind，按 OS 错误码
            // 再判一次：拒绝 / 复位 / 主机或网络不可达都记 free；超时等记 unknown。
            let free_os_code = matches!(
                err.raw_os_error(),
                Some(10061) | Some(10054) | Some(10065) | Some(10051)
            );
            let free_kind = matches!(
                err.kind(),
                ErrorKind::ConnectionRefused
                    | ErrorKind::ConnectionReset
                    | ErrorKind::HostUnreachable
                    | ErrorKind::NetworkUnreachable
            );
            if free_os_code || free_kind {
                "free"
            } else {
                "unknown"
            }
        }
    }
}

fn probe_port(root: &Path, args: &Value) -> Result<Value, String> {
    let explicit = normalize_port(args.get("port"))?;
    let config = read_config(root);
    let port = resolve_port(&config, explicit);
    Ok(json!({ "port": port, "state": probe_state(port) }))
}

// ── 服务状态与启停 ───────────────────────────────────────────────────────

fn loopback_addresses(port: u16) -> Vec<String> {
    vec![format!("http://127.0.0.1:{port}")]
}

fn status_info(root: &Path) -> Result<Value, String> {
    let config = read_config(root);
    let port = resolve_port(&config, None);
    if probe_state(port) != "occupied" {
        // 没有在跑的服务就回 null，前端据此给出空地址列表，而不是猜一个 URL。
        return Ok(Value::Null);
    }
    let token = config
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(json!({
        "port": port,
        "token": token,
        "addresses": loopback_addresses(port),
    }))
}

fn server_status(root: &Path) -> Result<Value, String> {
    status_info(root)
}

fn start_server(root: &Path, args: &Value) -> Result<Value, String> {
    // limited：单进程自服务，这里不新建监听、不改当前进程的端口；调用方给的
    // token / port 只作为启动意图落盘，供后续读取与探测使用。
    let path = config_path(root);
    let _guard = store_file::lock(&path)?;
    let mut data = store_file::read_json(&path)?
        .unwrap_or_else(|| json!({ "autoStart": true }));

    if let Some(token) = normalize_token(args.get("token"))? {
        data["token"] = Value::String(token);
    }
    if let Some(port) = normalize_port(args.get("port"))? {
        data["port"] = Value::from(port);
    }
    // `host` 被前端作为参数传入，但本进程固定监听 127.0.0.1，不落地。
    if data.get("autoStart").is_none() {
        data["autoStart"] = Value::Bool(true);
    }
    store_file::write_json(&path, &data)?;

    let port = resolve_port(&data, None);
    let token = data
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(json!({
        "port": port,
        "token": token,
        "addresses": loopback_addresses(port),
    }))
}

fn stop_server() -> Result<Value, String> {
    // limited：见模块注释。当前进程的监听就是应用本体，停止它等于退出应用，
    // 因此这里不停止监听、不杀进程、不释放端口；前端约定无返回值，回 null。
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::net::TcpListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-web-service-{tag}-{}-{}",
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

    #[test]
    fn owns_covers_the_six_documented_commands_only() {
        for command in [
            "get_web_server_status",
            "get_web_service_config",
            "probe_web_service_port",
            "start_web_server",
            "stop_web_server",
            "update_web_service_config",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("get_recent_logs"));
        assert!(!owns("web_service_restart"));
        assert!(!owns("Start_Web_Server"));
    }

    #[test]
    fn config_defaults_to_null_token_null_port_and_autostart() {
        let root = temp_root("config-default");
        let view = call(&root, "get_web_service_config", json!({})).unwrap();
        assert!(view["token"].is_null());
        assert!(view["port"].is_null());
        assert_eq!(view["autoStart"], true);
    }

    #[test]
    fn update_config_persists_and_echoes_contract_shape() {
        let root = temp_root("config-write");
        let echoed = call(
            &root,
            "update_web_service_config",
            json!({"config": {"token": "secret-token", "port": 3080, "autoStart": false}}),
        )
        .unwrap();
        assert_eq!(echoed["token"], "secret-token");
        assert_eq!(echoed["port"], 3080);
        assert_eq!(echoed["autoStart"], false);

        let view = call(&root, "get_web_service_config", json!({})).unwrap();
        assert_eq!(view["token"], "secret-token");
        assert_eq!(view["port"], 3080);
        assert_eq!(view["autoStart"], false);
        assert!(config_path(&root).is_file());
    }

    #[test]
    fn update_config_rejects_bad_input() {
        let root = temp_root("config-bad");
        assert!(call(&root, "update_web_service_config", json!({}))
            .unwrap_err()
            .contains("缺少 config"));
        assert!(call(
            &root,
            "update_web_service_config",
            json!({"config": {"token": 123, "port": null, "autoStart": true}})
        )
        .unwrap_err()
        .contains("token 必须是字符串"));
        assert!(call(
            &root,
            "update_web_service_config",
            json!({"config": {"token": null, "port": 0, "autoStart": true}})
        )
        .unwrap_err()
        .contains("port 必须在 1 到 65535"));
        assert!(call(
            &root,
            "update_web_service_config",
            json!({"config": {"token": null, "port": 70000, "autoStart": true}})
        )
        .unwrap_err()
        .contains("port 必须在 1 到 65535"));
        assert!(call(
            &root,
            "update_web_service_config",
            json!({"config": {"token": null, "port": null, "autoStart": "yes"}})
        )
        .unwrap_err()
        .contains("autoStart 必须是布尔值"));
        assert!(call(&root, "update_web_service_config", json!({"config": []}))
            .unwrap_err()
            .contains("config 必须是对象"));
    }

    #[test]
    fn probe_reports_occupied_for_a_listening_port_and_free_for_a_dead_one() {
        let root = temp_root("probe");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let occupied = call(&root, "probe_web_service_port", json!({"port": port})).unwrap();
        assert_eq!(occupied["port"], port);
        assert_eq!(occupied["state"], "occupied");

        // 先占一个临时端口再立刻释放：本机无人监听 → 不可达（拒绝记 free，
        // 被过滤驱动丢包记 unknown），总之不是 occupied。
        let dead_port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let free = call(&root, "probe_web_service_port", json!({"port": dead_port})).unwrap();
        assert_eq!(free["port"], dead_port);
        assert_ne!(free["state"], "occupied");
        assert!(free["state"] == "free" || free["state"] == "unknown");

        let bad = call(&root, "probe_web_service_port", json!({"port": 70000})).unwrap_err();
        assert!(bad.contains("port 必须在 1 到 65535"));
        drop(listener);
    }

    #[test]
    fn status_is_null_when_the_port_is_dead_and_info_when_listening() {
        let root = temp_root("status");
        // 先把配置指到一个死端口：探测不可达 → null。
        call(
            &root,
            "update_web_service_config",
            json!({"config": {"token": null, "port": 1, "autoStart": true}}),
        )
        .unwrap();
        let off = call(&root, "get_web_server_status", json!({})).unwrap();
        assert!(off.is_null());

        // 再起一个桩监听，配置指向它 → 回 WebServerInfo。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        call(
            &root,
            "update_web_service_config",
            json!({"config": {"token": "webhook-token", "port": port, "autoStart": true}}),
        )
        .unwrap();
        let on = call(&root, "get_web_server_status", json!({})).unwrap();
        assert_eq!(on["port"], port);
        assert_eq!(on["token"], "webhook-token");
        let addresses = on["addresses"].as_array().unwrap();
        assert_eq!(addresses.len(), 1);
        assert!(addresses[0].as_str().unwrap().contains(&format!(":{port}")));
        drop(listener);
    }

    #[test]
    fn start_records_intent_and_stop_is_a_noop_without_touching_the_listener() {
        let root = temp_root("start-stop");
        let started = call(
            &root,
            "start_web_server",
            json!({"port": 3080, "host": "127.0.0.1", "token": "webhook-token"}),
        )
        .unwrap();
        assert_eq!(started["port"], 3080);
        assert_eq!(started["token"], "webhook-token");
        assert!(started["addresses"].as_array().unwrap().len() == 1);

        // 意图已落盘；读取配置能看到。
        let config = call(&root, "get_web_service_config", json!({})).unwrap();
        assert_eq!(config["port"], 3080);
        assert_eq!(config["token"], "webhook-token");

        // stop 不杀进程、不停监听：回 null，配置原样保留。
        let stopped = call(&root, "stop_web_server", json!({})).unwrap();
        assert!(stopped.is_null());
        let config_after = call(&root, "get_web_service_config", json!({})).unwrap();
        assert_eq!(config_after["port"], 3080);

        // 非法端口在 start 里同样被拒。
        assert!(call(
            &root,
            "start_web_server",
            json!({"port": 0, "token": null})
        )
        .unwrap_err()
        .contains("port 必须在 1 到 65535"));
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
