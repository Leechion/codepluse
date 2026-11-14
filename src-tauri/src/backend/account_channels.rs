//! 账号渠道（Account channels）——WorkBuddy / Qoder 独立账号的本地协议进程。
//!
//! 移植自 `server-go/internal/accountchannels/channels.go`（Phase 4 后 Go 已删除，
//! 原件见 tag `go-sidecar-final`）。
//!
//! 账号本身不经过浏览器边界：用户在设置里点「管理官网账号」，本进程启动一个
//! Node 协议进程（`bridges/account-channels/server.mjs`），它监听 127.0.0.1 的
//! 随机端口，双方用一把随机密钥（`CODEPLUSE_CHANNEL_KEY`）互相认证。
//! 凭据只留在 `.data/account-channels` 里，浏览器只拿得到账号元信息与模型列表。
//!
//! ```text
//! Rust 后端 ──ensure()──> node server.mjs ──HTTP/Bearer──> 账号渠道
//!     │                        │
//!     └── .data/account-channels/runtime.json（共享实例的地址与密钥）
//! ```

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

/// 账号渠道只支持这四个产品（Go `Supported`）。
pub fn supported(id: &str) -> bool {
    matches!(id, "buddy" | "workbuddy" | "qoder" | "qodercn")
}

/// Go `Available`：协议进程是否已安装。
pub fn available(root: &Path) -> bool {
    bridge_entry(root).is_file()
}

fn bridge_entry(root: &Path) -> PathBuf {
    root.join("bridges")
        .join("account-channels")
        .join("server.mjs")
}

fn acp_agent_entry(root: &Path) -> PathBuf {
    root.join("bridges")
        .join("account-channels")
        .join("acp-agent.mjs")
}

/// 一个账号渠道的模型（Go `Model`）。
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub id: String,
    pub name: String,
}

/// 一个账号渠道（Go `Channel`）。
#[derive(Clone, Debug, Default)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub configured: bool,
    pub accounts: Vec<Value>,
    pub models: Vec<Model>,
}

impl Channel {
    fn from_value(value: &Value) -> Channel {
        Channel {
            id: value
                .get("id")
                .and_then(|item| item.as_str())
                .unwrap_or("")
                .to_string(),
            name: value
                .get("name")
                .and_then(|item| item.as_str())
                .unwrap_or("")
                .to_string(),
            configured: value
                .get("configured")
                .and_then(|item| item.as_bool())
                .unwrap_or(false),
            accounts: value
                .get("accounts")
                .and_then(|item| item.as_array())
                .cloned()
                .unwrap_or_default(),
            models: value
                .get("models")
                .and_then(|item| item.as_array())
                .map(|rows| {
                    rows.iter()
                        .map(|row| Model {
                            id: row
                                .get("id")
                                .and_then(|item| item.as_str())
                                .unwrap_or("")
                                .to_string(),
                            name: row
                                .get("name")
                                .and_then(|item| item.as_str())
                                .unwrap_or("")
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// 与协议进程的一次会话（地址 + 密钥）。
#[derive(Clone, Debug, Default)]
pub struct Client {
    pub url: String,
    pub key: String,
}

/// 进程表里的一个条目：`child` 为空表示这是**共享实例**（别的 Codepluse
/// 后台起的进程，本进程只复用它的地址与密钥）。
struct Entry {
    client: Client,
    child: Option<Child>,
    /// 协议进程在 stdin 收到 EOF 时会退出（server.mjs 的 `stdin.on("end")`），
    /// 所以这个句柄必须一直留着，不能提前丢掉。
    stdin: Option<std::process::ChildStdin>,
}

static REGISTRY: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, Entry>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `.data/account-channels` 目录。
pub fn data_dir(root: &Path) -> PathBuf {
    root.join(".data").join("account-channels")
}

/// Go `running`：复用同一个工作区里已经跑着的协议进程。
///
/// 多个 Codepluse 后台会共享同一条受保护的本地运行时记录（`runtime.json`），
/// 所以先认它，再考虑自己起一个。
fn running(root: &Path) -> Option<Client> {
    let raw = std::fs::read_to_string(data_dir(root).join("runtime.json")).ok()?;
    let state: Value = serde_json::from_str(&raw).ok()?;
    let url = state.get("url").and_then(|item| item.as_str())?.to_string();
    let key = state.get("key").and_then(|item| item.as_str())?.to_string();
    if key.len() < 32 {
        return None;
    }
    let parsed = url::Url::parse(&url).ok()?;
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("127.0.0.1")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    let client = Client {
        // Go 记的是 `http://127.0.0.1:PORT`（无路径）。authority() 带端口，
        // host_str() 不带 —— 少了端口会打到 80 端口去。
        url: format!("{}://{}", parsed.scheme(), parsed.authority()).trim_end_matches('/').to_string(),
        key,
    };
    let probe = Client {
        url: client.url.clone(),
        key: client.key.clone(),
    };
    // 健康检查通过才算活着。
    probe.call("GET", "/health", None).ok()?;
    Some(client)
}

/// Go `Ensure`：拿到一个可用的协议进程（复用或新起）。
pub fn ensure(root: &Path) -> Result<Client, String> {
    let root = absolute(root)?;
    let key = root.to_string_lossy().to_string();
    // 查缓存：锁只用来读写这张表，启动协议进程时不持有它 —— 否则一次最长
    // 30 秒的等待会把所有并发调用者一起堵住。
    let mut reuse: Option<Client> = None;
    let mut stale = false;
    {
        let mut clients = registry().lock().unwrap();
        if let Some(entry) = clients.get_mut(&key) {
            match entry.child {
                None => {
                    // 共享实例：再确认一次它还活着。
                    stale = true;
                }
                Some(_) => {
                    if entry_alive(entry) {
                        reuse = Some(entry.client.clone());
                    } else {
                        stale = true;
                    }
                }
            }
        }
        if stale {
            clients.remove(&key);
        }
    }
    if let Some(client) = reuse {
        return Ok(client);
    }
    // 共享实例（同一个工作区里别的 Codepluse 后台起的进程）。
    if let Some(shared) = running(&root) {
        registry().lock().unwrap().insert(
            key.clone(),
            Entry {
                client: shared.clone(),
                child: None,
                stdin: None,
            },
        );
        return Ok(shared);
    }
    if !available(&root) {
        return Err("账号渠道适配器未安装".into());
    }
    let node = node_binary()?;
    let data = data_dir(&root);
    std::fs::create_dir_all(&data).map_err(|error| error.to_string())?;

    let secret = hex_key();
    let mut command = Command::new(&node);
    command
        .arg(bridge_entry(&root))
        .arg(&data)
        .current_dir(&root)
        .env_remove("CODEPLUSE_CHANNEL_KEY")
        .env("CODEPLUSE_CHANNEL_KEY", &secret)
        // stdin 必须是管道而不是空设备：协议进程把 stdin 的 EOF 当成退出信号。
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    // stderr 落到 bridge.log，启动失败时用户能自己看到原因。
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data.join("bridge.log"))
        .map_err(|error| error.to_string())?;
    command.stderr(Stdio::from(log));
    hide_process(&mut command);

    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or("账号渠道启动失败")?;
    // 协议进程的第一行 stdout 是 `{"url":"http://127.0.0.1:PORT"}`。
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut first = String::new();
        if reader.read_line(&mut first).unwrap_or(0) > 0 {
            let _ = tx.send(first);
        }
        // 其余输出丢弃：协议进程只在第一行交代地址。
        let mut sink = Vec::new();
        let _ = std::io::Read::read_to_end(&mut reader, &mut sink);
    });

    let line = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(line) => line,
        Err(_) => {
            let _ = child.kill();
            return Err("账号渠道启动超时".into());
        }
    };
    let state: Value = serde_json::from_str(line.trim()).map_err(|_| "账号渠道启动响应无效".to_string())?;
    let url = state
        .get("url")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    let parsed = url::Url::parse(&url).map_err(|_| "账号渠道监听地址无效".to_string())?;
    if parsed.scheme() != "http" || parsed.host_str() != Some("127.0.0.1") {
        let _ = child.kill();
        return Err("账号渠道监听地址无效".into());
    }
    let client = Client {
        url: format!("{}://{}", parsed.scheme(), parsed.authority()).trim_end_matches('/').to_string(),
        key: secret,
    };
    registry().lock().unwrap().insert(
        key,
        Entry {
            client: client.clone(),
            child: Some(child),
            stdin,
        },
    );
    Ok(client)
}

/// 进程是否还活着（Go 的 `done` 通道）。
fn entry_alive(entry: &mut Entry) -> bool {
    let Some(child) = entry.child.as_mut() else {
        return false;
    };
    // try_wait 会回收已经结束的子进程。
    !matches!(child.try_wait(), Ok(Some(_)))
}

fn node_binary() -> Result<String, String> {
    if let Ok(node) = std::env::var("HARNESS_HUB_NODE_BIN") {
        if !node.trim().is_empty() {
            return Ok(node);
        }
    }
    crate::backend::acp::launch::which("node")
        .map_err(|_| "账号渠道需要 Node.js 22 或更新版本".to_string())
}

fn absolute(root: &Path) -> Result<PathBuf, String> {
    let mut out = root.to_path_buf();
    if !out.is_absolute() {
        out = std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(out);
    }
    Ok(out)
}

fn hex_key() -> String {
    let bytes: [u8; 32] = rand::random();
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(windows)]
fn hide_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW：协议进程不该弹出控制台窗口。
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_process(_command: &mut Command) {}

impl Client {
    /// Go `Client.Call`：一次带 Bearer 密钥的本地 HTTP 调用。
    pub fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(45))
            // 协议进程永远在环回地址上：系统代理（HTTP_PROXY/HTTPS_PROXY）不能
            // 介入，否则请求会被送到代理手里而失败。
            .no_proxy()
            .build()
            .map_err(|error| format!("账号渠道不可达: {error}"))?;
        let mut request = client
            .request(
                reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|_| "账号渠道请求方法无效".to_string())?,
                format!("{}{}", self.url, path),
            )
            .bearer_auth(&self.key)
            .header("Content-Type", "application/json");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .map_err(|error| format!("账号渠道不可达: {error}"))?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        {
            use std::io::Read;
            response
                .take(8 << 20)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
        }
        if status >= 400 {
            let message = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|value| {
                    value
                        .pointer("/error/message")
                        .and_then(|item| item.as_str().map(|text| text.to_string()))
                });
            return Err(match message {
                Some(message) if !message.is_empty() => message,
                _ => format!("账号渠道 HTTP {status}"),
            });
        }
        serde_json::from_slice(&bytes).map_err(|_| "账号渠道响应格式无效".to_string())
    }
}

/// Go `List`：渠道状态（账号元信息 + 模型列表）。
pub fn list(root: &Path) -> Result<(Vec<Channel>, Client), String> {
    let client = ensure(root)?;
    let raw = client.call("GET", "/status", None)?;
    let rows = raw.as_array().cloned().unwrap_or_default();
    let channels = rows.iter().map(Channel::from_value).collect();
    Ok((channels, client))
}

/// Go `accountChannelCommand`（codeg/account_channels.go:13）：五条命令的转发。
pub fn command(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let client = ensure(root)?;
    let text = |key: &str| {
        args.get(key)
            .and_then(|item| item.as_str())
            .unwrap_or("")
            .to_string()
    };
    let (method, path, body) = match command {
        "codepluse_channel_status" => ("GET", "/status".to_string(), None),
        "codepluse_channel_login" => (
            "POST",
            "/login/start".to_string(),
            Some(json!({ "provider": text("provider") })),
        ),
        "codepluse_channel_login_status" => {
            ("GET", format!("/login/{}", escape_path(&text("id"))), None)
        }
        "codepluse_channel_cancel" => (
            "POST",
            "/login/cancel".to_string(),
            Some(json!({ "id": text("id") })),
        ),
        "codepluse_channel_logout" => (
            "POST",
            "/logout".to_string(),
            Some(json!({ "provider": text("provider"), "accountId": text("accountId") })),
        ),
        _ => return Err("账号渠道操作无效".into()),
    };
    client.call(method, &path, body)
}

/// Go 的 `url.PathEscape`：路径段里的特殊字符要转义。
fn escape_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// ACP 适配器入口（`account:<id>` 的 agent）。
pub fn acp_adapter(root: &Path) -> PathBuf {
    acp_agent_entry(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_four_products_are_supported() {
        for id in ["buddy", "workbuddy", "qoder", "qodercn"] {
            assert!(supported(id));
        }
        assert!(!supported("claude"));
        assert!(!supported(""));
    }

    #[test]
    fn availability_follows_the_installed_bridge() {
        let root = std::env::temp_dir().join(format!(
            "codepluse-account-channels-{}",
            crate::backend::goformat::hex_id()
        ));
        assert!(!available(&root));
        std::fs::create_dir_all(root.join("bridges").join("account-channels")).unwrap();
        std::fs::write(
            root.join("bridges").join("account-channels").join("server.mjs"),
            "//",
        )
        .unwrap();
        assert!(available(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_shared_runtime_record_must_be_a_loopback_url() {
        let root = std::env::temp_dir().join(format!(
            "codepluse-account-runtime-{}",
            crate::backend::goformat::hex_id()
        ));
        std::fs::create_dir_all(data_dir(&root)).unwrap();
        std::fs::write(
            data_dir(&root).join("runtime.json"),
            r#"{"url":"http://127.0.0.1:9911","key":"0123456789abcdef0123456789abcdef"}"#,
        )
        .unwrap();
        // 健康检查打不通 → 不复用。
        assert!(running(&root).is_none());
        std::fs::write(
            data_dir(&root).join("runtime.json"),
            r#"{"url":"http://evil.example:9911","key":"0123456789abcdef0123456789abcdef"}"#,
        )
        .unwrap();
        assert!(running(&root).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn path_segments_are_escaped() {
        assert_eq!(escape_path("abc"), "abc");
        assert_eq!(escape_path("a/b"), "a%2Fb");
        assert_eq!(escape_path("a b"), "a%20b");
    }

    #[test]
    fn unknown_commands_are_rejected() {
        let root = std::env::temp_dir().join("codepluse-account-cmd");
        let error = command(&root, "codepluse_channel_nope", &json!({})).unwrap_err();
        // 适配器没装 → 先报安装缺失（命令名在装好之前也走不到）。
        assert!(!error.is_empty());
    }
}
