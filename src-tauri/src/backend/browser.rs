//! Browser operations, ported from `server-go/internal/codeg/browser.go` +
//! `browser_cdp.go`.
//!
//! A minimal Chrome DevTools Protocol driver over WebSocket: launch (or
//! re-adopt) a local Chrome/Edge with `--remote-debugging-port`, attach flat
//! sessions per tab and evaluate inside isolated worlds for the browser-agent
//! bundle — the same shape as Go's `cdpBrowser`, with tokio-tungstenite in
//! place of gorilla/websocket. Handlers are async (the CDP round-trips are
//! awaited inline) and answer on the proxy's own dispatch path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Go's `text()`: only strings pass; a JSON number (like CDP's
/// `executionContextId`) fails the type assertion and yields "". Replicated
/// verbatim because the agent world id flows through it.
fn go_text(value: &Value) -> String {
    value.as_str().unwrap_or("").to_string()
}

/// Go `number()` for one JSON value.
fn int_of(value: &Value) -> i64 {
    match value {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "browser_capabilities"
            | "browser_list_tabs"
            | "browser_tabs"
            | "browser_open_tab"
            | "browser_close_tab"
            | "browser_navigate"
            | "browser_reload"
            | "browser_go_back"
            | "browser_stop"
            | "browser_page_capture"
            | "browser_agent_snapshot"
            | "browser_agent_act"
            | "browser_agent_console"
            | "browser_eval_decide"
            | "browser_find"
            | "browser_set_bounds"
            | "browser_freeze_frame"
            | "browser_pick_element"
            | "browser_pick_cancel"
            | "browser_set_host_rules"
            | "browser_clear_data"
            | "browser_open_devtools"
            | "browser_doc_open"
            | "browser_doc_state"
            | "browser_doc_set_mode"
            | "browser_agent_grant"
            | "browser_agent_capture"
            | "browser_answer_open_request"
    )
}

/// Command outcome: the HTTP body plus `browser://state` frames for the WS relay.
#[derive(Debug)]
pub struct Outcome {
    pub result: Value,
    pub events: Vec<(String, Value)>,
}

pub async fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Outcome, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args).await)
}

async fn run(root: &Path, command: &str, a: &Value) -> Result<Outcome, String> {
    match command {
        "browser_capabilities" => Ok(Outcome {
            result: json!({
                "supported": true,
                "agent": true,
                "browser": !find_browser().is_empty(),
            }),
            events: Vec::new(),
        }),
        "browser_list_tabs" | "browser_tabs" => {
            let browser = ensure_browser(root).await?;
            let tabs = browser.list_tabs().await?;
            let out: Vec<Value> = tabs
                .iter()
                .enumerate()
                .map(|(i, tab)| {
                    json!({
                        "id": i.to_string(),
                        "target_id": tab.target_id,
                        "url": tab.url,
                        "title": tab.title,
                    })
                })
                .collect();
            Ok(Outcome {
                result: json!({ "tabs": out }),
                events: Vec::new(),
            })
        }
        "browser_open_tab" => {
            let browser = ensure_browser(root).await?;
            let raw_url = go_text(&a["url"]);
            let raw_url = if raw_url.is_empty() {
                "about:blank"
            } else {
                &raw_url
            };
            let result = browser
                .call("", "Target.createTarget", json!({ "url": raw_url }))
                .await?;
            let target_id = result["targetId"].clone();
            Ok(Outcome {
                result: json!({ "target_id": target_id }),
                events: vec![(
                    "browser://state".into(),
                    json!({ "kind": "opened", "targetId": target_id }),
                )],
            })
        }
        "browser_close_tab" => {
            let browser = ensure_browser(root).await?;
            let tab = browser.tab_by_ref(&go_text(&a["tabId"])).await?;
            browser
                .call(
                    "",
                    "Target.closeTarget",
                    json!({ "targetId": tab.target_id }),
                )
                .await?;
            Ok(Outcome {
                result: Value::Null,
                events: vec![(
                    "browser://state".into(),
                    json!({ "kind": "closed", "targetId": tab.target_id }),
                )],
            })
        }
        "browser_navigate" => {
            let browser = ensure_browser(root).await?;
            let mut tab = browser.tab_by_ref(&go_text(&a["tabId"])).await?;
            browser.attach(&mut tab).await?;
            browser
                .call(
                    &tab.session_id,
                    "Page.navigate",
                    json!({ "url": go_text(&a["url"]) }),
                )
                .await?;
            Ok(Outcome {
                result: Value::Null,
                events: Vec::new(),
            })
        }
        "browser_reload" => browser_simple(root, a, "Page.reload").await,
        "browser_go_back" => browser_simple(root, a, "Page.navigateToHistoryEntry").await,
        "browser_stop" => Ok(Outcome {
            result: Value::Null,
            events: Vec::new(),
        }),
        "browser_page_capture" => {
            let browser = ensure_browser(root).await?;
            let mut tab = browser.tab_by_ref(&go_text(&a["tabId"])).await?;
            browser.attach(&mut tab).await?;
            let result = browser
                .call(
                    &tab.session_id,
                    "Page.captureScreenshot",
                    json!({ "format": "png" }),
                )
                .await?;
            Ok(Outcome {
                result: json!({ "mime_type": "image/png", "base64": result["data"].clone() }),
                events: Vec::new(),
            })
        }
        "browser_agent_snapshot" => {
            let browser = ensure_browser(root).await?;
            let mut tab = browser.tab_by_ref(&go_text(&a["tabId"])).await?;
            let bundle = agent_bundle(root)?;
            browser.ensure_agent_world(&mut tab, &bundle).await?;
            let max_chars = int_of(&a["maxChars"]);
            let expression = if max_chars > 0 {
                format!("JSON.stringify(__codegAgent.snapshot({{maxChars:{max_chars}}})) || null")
            } else {
                "JSON.stringify(__codegAgent.snapshot({})) || null".to_string()
            };
            let result = browser
                .call(
                    &tab.session_id,
                    "Runtime.evaluate",
                    json!({
                        "contextId": tab.world_id,
                        "expression": expression,
                        "returnByValue": true,
                    }),
                )
                .await?;
            Ok(Outcome {
                result: parse_stringified(&result),
                events: Vec::new(),
            })
        }
        "browser_agent_act" => {
            let browser = ensure_browser(root).await?;
            let mut tab = browser.tab_by_ref(&go_text(&a["tabId"])).await?;
            let bundle = agent_bundle(root)?;
            browser.ensure_agent_world(&mut tab, &bundle).await?;
            // Go marshals the request map, so keys come out sorted; canon
            // reproduces that for the embedded JS payload.
            let payload = serde_json::to_string(&crate::backend::goformat::canon(&a["request"]))
                .unwrap_or_default();
            let expression = format!(
                "JSON.stringify(__codegAgent.act({payload}) || {{ok:false,error:'no result'}})"
            );
            let result = browser
                .call(
                    &tab.session_id,
                    "Runtime.evaluate",
                    json!({
                        "contextId": tab.world_id,
                        "expression": expression,
                        "returnByValue": true,
                        "userGesture": true,
                    }),
                )
                .await?;
            Ok(Outcome {
                result: parse_stringified(&result),
                events: Vec::new(),
            })
        }
        // Go's console buffer is never populated (the events channel has no
        // reader), so the map lookup is always nil → `entries: null`.
        "browser_agent_console" => {
            let browser = ensure_browser(root).await?;
            browser.tab_by_ref(&go_text(&a["tabId"])).await?;
            Ok(Outcome {
                result: json!({ "entries": Value::Null }),
                events: Vec::new(),
            })
        }
        "browser_eval_decide" => {
            let browser = ensure_browser(root).await?;
            let mut tab = browser.tab_by_ref(&go_text(&a["tabId"])).await?;
            browser.attach(&mut tab).await?;
            let result = browser
                .call(
                    &tab.session_id,
                    "Runtime.evaluate",
                    json!({ "expression": go_text(&a["expression"]), "returnByValue": true }),
                )
                .await?;
            Ok(Outcome {
                result: json!({
                    "value": result["result"]["value"].clone(),
                    "type": result["result"]["type"].clone(),
                }),
                events: Vec::new(),
            })
        }
        "browser_find" => Ok(Outcome {
            result: json!({ "matches": [] }),
            events: Vec::new(),
        }),
        "browser_set_bounds"
        | "browser_freeze_frame"
        | "browser_pick_element"
        | "browser_pick_cancel"
        | "browser_set_host_rules"
        | "browser_clear_data"
        | "browser_open_devtools"
        | "browser_doc_open"
        | "browser_doc_state"
        | "browser_doc_set_mode"
        | "browser_agent_grant"
        | "browser_agent_capture"
        | "browser_answer_open_request" => Err(format!("浏览器命令 {command} 暂未实现")),
        _ => Err(format!("未知的浏览器命令：{command}")),
    }
}

/// Shared tail for one-shot page commands (`Page.reload`, `Page.
/// navigateToHistoryEntry`). Go passes a nil params row, so the wire message
/// carries `"params": null` — kept for byte parity, even though the history
/// entry call is rejected by Chrome exactly like it is upstream.
async fn browser_simple(root: &Path, a: &Value, method: &str) -> Result<Outcome, String> {
    let browser = ensure_browser(root).await?;
    let mut tab = browser.tab_by_ref(&go_text(&a["tabId"])).await?;
    browser.attach(&mut tab).await?;
    browser.call(&tab.session_id, method, Value::Null).await?;
    Ok(Outcome {
        result: Value::Null,
        events: Vec::new(),
    })
}

/// Runtime.evaluate answers a stringified payload; an unparsable body yields
/// `null` exactly like Go's failed `json.Unmarshal`.
fn parse_stringified(result: &Value) -> Value {
    let value = result["result"]["value"].as_str().unwrap_or("");
    serde_json::from_str(value).unwrap_or(Value::Null)
}

// ----------------------------- agent bundle -----------------------------

const AGENT_BUNDLE_REL: &str = "codeg-ui/browser-agent/agent.bundle.js";

static BUNDLE: Mutex<Option<String>> = Mutex::new(None);

/// Reads (and caches forever, like Go's `browser.bundle`) the built agent bundle.
fn agent_bundle(root: &Path) -> Result<String, String> {
    if let Some(bundle) = BUNDLE.lock().unwrap().clone() {
        return Ok(bundle);
    }
    let path = root.join(AGENT_BUNDLE_REL);
    let data = std::fs::read_to_string(&path).map_err(|err| {
        format!(
            "浏览器 agent 脚本未构建（codeg-ui/browser-agent/agent.bundle.js）：open {}: {}",
            path.display(),
            err
        )
    })?;
    *BUNDLE.lock().unwrap() = Some(data.clone());
    Ok(data)
}

// ----------------------------- CDP driver -----------------------------

#[derive(Clone, Debug)]
struct CdpTab {
    session_id: String,
    target_id: String,
    url: String,
    title: String,
    world_id: String,
    bundle_on: bool,
}

struct Cdp {
    /// The browser process, kept alive for as long as the connection lives
    /// (Go keeps `cmd` on the struct for the same reason; dropping would not
    /// kill it, but the handle documents ownership).
    #[allow(dead_code)]
    child: Option<std::process::Child>,
    sender: mpsc::UnboundedSender<String>,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    closed: Arc<AtomicBool>,
    #[allow(dead_code)]
    user_data_dir: PathBuf,
}

impl Cdp {
    fn alive(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    /// Builds the wire message; params `null` keeps byte parity with Go's nil
    /// Row, and the key order (id, method, params, sessionId) matches Go's
    /// sorted map marshal.
    fn build_message(&self, id: i64, session_id: &str, method: &str, params: &Value) -> String {
        let mut message = json!({ "id": id, "method": method, "params": params });
        if !session_id.is_empty() {
            message["sessionId"] = json!(session_id);
        }
        message.to_string()
    }

    /// Sends one CDP command and waits for its result (30s budget, like Go).
    async fn call(&self, session_id: &str, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self
            .sender
            .send(self.build_message(id, session_id, method, &params))
            .is_err()
        {
            self.pending.lock().unwrap().remove(&id);
            return Err("浏览器连接已关闭".into());
        }
        let response = match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(response)) => response,
            // The reader task dropped the waiter: the connection is gone.
            Ok(Err(_)) => {
                self.pending.lock().unwrap().remove(&id);
                return Err("浏览器连接中断".into());
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(format!("{method} 超时"));
            }
        };
        if let Some(error) = response["error"].as_object() {
            let message = error.get("message").and_then(|m| m.as_str()).unwrap_or("");
            return Err(format!("{method}：{message}"));
        }
        Ok(response["result"].clone())
    }

    /// Lists page targets via `Target.getTargets`.
    async fn list_tabs(&self) -> Result<Vec<CdpTab>, String> {
        let result = self.call("", "Target.getTargets", Value::Null).await?;
        let mut out = Vec::new();
        if let Some(infos) = result["targetInfos"].as_array() {
            for info in infos {
                if info["type"].as_str() != Some("page") {
                    continue;
                }
                out.push(CdpTab {
                    session_id: String::new(),
                    target_id: go_text(&info["targetId"]),
                    url: go_text(&info["url"]),
                    title: go_text(&info["title"]),
                    world_id: String::new(),
                    bundle_on: false,
                });
            }
        }
        Ok(out)
    }

    /// Attaches (on demand) a flat session for the tab.
    async fn attach(&self, tab: &mut CdpTab) -> Result<(), String> {
        if !tab.session_id.is_empty() {
            return Ok(());
        }
        let result = self
            .call(
                "",
                "Target.attachToTarget",
                json!({ "targetId": tab.target_id, "flatten": true }),
            )
            .await?;
        tab.session_id = go_text(&result["sessionId"]);
        Ok(())
    }

    /// Creates the isolated world and installs the agent bundle.
    async fn ensure_agent_world(&self, tab: &mut CdpTab, bundle: &str) -> Result<(), String> {
        self.attach(tab).await?;
        if tab.bundle_on && !tab.world_id.is_empty() {
            return Ok(());
        }
        let result = self
            .call(
                &tab.session_id,
                "Page.createIsolatedWorld",
                json!({ "targetId": tab.target_id }),
            )
            .await?;
        tab.world_id = go_text(&result["executionContextId"]);
        self.call(
            &tab.session_id,
            "Runtime.evaluate",
            json!({
                "contextId": tab.world_id,
                "expression": bundle,
                "returnByValue": true,
            }),
        )
        .await
        .map_err(|err| format!("注入浏览器 agent 失败：{err}"))?;
        tab.bundle_on = true;
        Ok(())
    }

    /// Resolves a tab ref from the frontend (target id or list index).
    async fn tab_by_ref(&self, reference: &str) -> Result<CdpTab, String> {
        let tabs = self.list_tabs().await?;
        for (i, tab) in tabs.into_iter().enumerate() {
            if tab.target_id == reference || i.to_string() == reference {
                return Ok(tab);
            }
        }
        Err(format!("标签页不存在：{reference}"))
    }
}

/// Registry for the process-wide browser, held across the launch await like
/// Go's `browserMu`.
static BROWSER: tokio::sync::Mutex<Option<Arc<Cdp>>> = tokio::sync::Mutex::const_new(None);

/// Launches (or reuses) the local browser with CDP enabled. The profile lives
/// under the project's `.data` (the temp directory is not writable under this
/// machine's ACLs) and the chosen port is persisted so a restart re-adopts the
/// running browser instead of spawning another.
async fn ensure_browser(root: &Path) -> Result<Arc<Cdp>, String> {
    let mut guard = BROWSER.lock().await;
    if let Some(browser) = guard.as_ref() {
        if browser.alive() {
            return Ok(browser.clone());
        }
    }
    let browser = acquire_browser(root).await?;
    *guard = Some(browser.clone());
    Ok(browser)
}

/// `GET /json/version` on the devtools endpoint (800ms budget, like Go).
async fn browser_version_port(port: &str) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(800))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(format!("http://127.0.0.1:{port}/json/version"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    // reqwest's `json` feature is off; parse the body ourselves.
    let body = response.text().await.map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

async fn acquire_browser(root: &Path) -> Result<Arc<Cdp>, String> {
    let exe = find_browser();
    if exe.is_empty() {
        return Err("未找到 Chrome 或 Edge，请安装后重试".into());
    }
    let user_data_dir = root.join(".data").join("browser-profile");
    let _ = std::fs::create_dir_all(&user_data_dir);
    let port_file = root.join(".data").join("browser-port.txt");

    // Re-adopt a browser we launched earlier.
    if let Ok(raw) = std::fs::read_to_string(&port_file) {
        let port = raw.trim().to_string();
        if !port.is_empty() {
            if let Ok(version) = browser_version_port(&port).await {
                if let Some(ws) = version["webSocketDebuggerUrl"].as_str() {
                    if !ws.is_empty() {
                        if let Ok(cdp) =
                            connect_cdp(ws.to_string(), user_data_dir.clone(), None).await
                        {
                            return Ok(Arc::new(cdp));
                        }
                    }
                }
            }
        }
    }

    // Pick a free debugging port and launch.
    for port in 9337..=9341u16 {
        let port_text = port.to_string();
        if browser_version_port(&port_text).await.is_ok() {
            continue; // something else owns this port
        }
        let mut child = match std::process::Command::new(&exe)
            .arg(format!("--remote-debugging-port={port}"))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(format!("--user-data-dir={}", user_data_dir.display()))
            .arg("--window-size=1280,900")
            .arg("about:blank")
            .spawn()
        {
            Ok(child) => child,
            Err(err) => return Err(format!("启动浏览器失败：{err}")),
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut ws_url = String::new();
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Ok(version) = browser_version_port(&port_text).await {
                if let Some(ws) = version["webSocketDebuggerUrl"].as_str() {
                    if !ws.is_empty() {
                        ws_url = ws.to_string();
                        let _ = std::fs::write(&port_file, port_text.as_bytes());
                        break;
                    }
                }
            }
        }
        if ws_url.is_empty() {
            let _ = child.kill();
            continue;
        }
        match connect_cdp(ws_url, user_data_dir.clone(), Some(child)).await {
            Ok(cdp) => return Ok(Arc::new(cdp)),
            Err(err) => return Err(err),
        }
    }
    Err("浏览器调试端口未就绪，请关闭已打开的 Edge/Chrome 后重试".into())
}

/// Opens the debug WebSocket and starts the reader/writer tasks. Tungstenite's
/// default `max_message_size` is 64 MiB — the same cap as Go's `SetReadLimit`.
async fn connect_cdp(
    ws_url: String,
    user_data_dir: PathBuf,
    child: Option<std::process::Child>,
) -> Result<Cdp, String> {
    let (socket, _response) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .map_err(|e| format!("连接浏览器调试通道失败：{e}"))?;
    let (mut sink, mut stream) = socket.split();
    let (sender, mut receiver) = mpsc::unbounded_channel::<String>();
    let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let closed = Arc::new(AtomicBool::new(false));

    tokio::spawn(async move {
        while let Some(text) = receiver.recv().await {
            if sink.send(WsMessage::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    let read_pending = pending.clone();
    let read_closed = closed.clone();
    tokio::spawn(async move {
        while let Some(message) = stream.next().await {
            let text = match message {
                Ok(WsMessage::Text(text)) => text.to_string(),
                Ok(_) => continue,
                Err(_) => break,
            };
            let parsed: Value = match serde_json::from_str(&text) {
                Ok(value) => value,
                Err(_) => continue,
            };
            // Responses complete the pending call; events are dropped (Go
            // buffers them into a channel that nothing drains).
            if let Some(id) = parsed["id"].as_i64() {
                if let Some(waiter) = read_pending.lock().unwrap().remove(&id) {
                    let _ = waiter.send(parsed);
                }
            }
        }
        read_closed.store(true, Ordering::SeqCst);
        read_pending.lock().unwrap().clear();
    });

    Ok(Cdp {
        child,
        sender,
        next_id: AtomicI64::new(0),
        pending,
        closed,
        user_data_dir,
    })
}

/// Locates a Chrome/Edge executable, mirroring Go's candidate order
/// (filepath.Join collapses an empty base to just the tail).
fn find_browser() -> String {
    let program_files = std::env::var("ProgramFiles").unwrap_or_default();
    let program_files_x86 = std::env::var("ProgramFiles(x86)").unwrap_or_default();
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let candidates = [
        join_prog_dir(&program_files, r"Google\Chrome\Application\chrome.exe"),
        join_prog_dir(&program_files_x86, r"Google\Chrome\Application\chrome.exe"),
        join_prog_dir(&local_app_data, r"Google\Chrome\Application\chrome.exe"),
        join_prog_dir(&program_files_x86, r"Microsoft\Edge\Application\msedge.exe"),
        join_prog_dir(&program_files, r"Microsoft\Edge\Application\msedge.exe"),
    ];
    for path in candidates {
        if path.is_empty() {
            continue;
        }
        if std::fs::metadata(&path).is_ok() {
            return path;
        }
    }
    String::new()
}

fn join_prog_dir(base: &str, tail: &str) -> String {
    if base.is_empty() {
        return tail.to_string();
    }
    format!("{base}\\{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cdp() -> Cdp {
        let (sender, _receiver) = mpsc::unbounded_channel();
        Cdp {
            child: None,
            sender,
            next_id: AtomicI64::new(0),
            pending: Arc::new(Mutex::new(HashMap::new())),
            closed: Arc::new(AtomicBool::new(false)),
            user_data_dir: PathBuf::from("unused"),
        }
    }

    #[test]
    fn browser_owns_covers_the_switch() {
        for command in [
            "browser_capabilities",
            "browser_list_tabs",
            "browser_tabs",
            "browser_open_tab",
            "browser_close_tab",
            "browser_navigate",
            "browser_reload",
            "browser_go_back",
            "browser_stop",
            "browser_page_capture",
            "browser_agent_snapshot",
            "browser_agent_act",
            "browser_agent_console",
            "browser_eval_decide",
            "browser_find",
            "browser_set_bounds",
            "browser_freeze_frame",
            "browser_pick_element",
            "browser_pick_cancel",
        ] {
            assert!(owns(command), "{command} 应归 browser 模块");
        }
        assert!(!owns("browser_nope"));
    }

    #[test]
    fn wire_message_matches_go_shape() {
        let cdp = cdp();
        // nil params → "params": null；带 session 时补 sessionId。
        assert_eq!(
            cdp.build_message(7, "", "Target.getTargets", &Value::Null),
            r#"{"id":7,"method":"Target.getTargets","params":null}"#
        );
        assert_eq!(
            cdp.build_message(8, "S1", "Page.navigate", &json!({"url":"https://x"})),
            r#"{"id":8,"method":"Page.navigate","params":{"url":"https://x"},"sessionId":"S1"}"#
        );
    }

    #[test]
    fn go_text_mirrors_go_type_assertion() {
        assert_eq!(go_text(&json!("abc")), "abc");
        assert_eq!(
            go_text(&json!(1234)),
            "",
            "CDP 的数字过不了 Go 的 string 断言"
        );
        assert_eq!(go_text(&json!(null)), "");
        assert_eq!(go_text(&json!(true)), "");
    }

    #[test]
    fn parse_stringified_handles_null_and_junk() {
        assert_eq!(
            parse_stringified(&json!({"result": {"value": "{\"a\":1}"}})),
            json!({"a":1})
        );
        assert_eq!(
            parse_stringified(&json!({"result": {"value": "null"}})),
            Value::Null
        );
        assert_eq!(
            parse_stringified(&json!({"result": {"value": ""}})),
            Value::Null
        );
        assert_eq!(parse_stringified(&json!({"result": {}})), Value::Null);
    }

    #[test]
    fn act_payload_is_canonicalized_like_go_marshal() {
        let request = json!({"b": 2, "a": {"d": [1, 2.0], "c": "x"}});
        let payload = serde_json::to_string(&crate::backend::goformat::canon(&request)).unwrap();
        assert_eq!(payload, r#"{"a":{"c":"x","d":[1,2]},"b":2}"#);
    }

    #[tokio::test]
    async fn stub_errors_match_go() {
        let args = json!({});
        assert_eq!(
            run(Path::new("."), "browser_pick_element", &args)
                .await
                .unwrap_err(),
            "浏览器命令 browser_pick_element 暂未实现"
        );
        assert_eq!(
            run(Path::new("."), "browser_set_bounds", &args)
                .await
                .unwrap_err(),
            "浏览器命令 browser_set_bounds 暂未实现"
        );
        let out = run(Path::new("."), "browser_find", &args).await.unwrap();
        assert_eq!(out.result, json!({"matches": []}));
        let out = run(Path::new("."), "browser_stop", &args).await.unwrap();
        assert_eq!(out.result, Value::Null);
    }

    #[tokio::test]
    async fn capabilities_shape() {
        let out = run(Path::new("."), "browser_capabilities", &json!({}))
            .await
            .unwrap();
        assert_eq!(out.result["supported"], json!(true));
        assert_eq!(out.result["agent"], json!(true));
        assert!(out.result["browser"].is_boolean());
    }
}
