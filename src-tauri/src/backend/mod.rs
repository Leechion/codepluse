//! Rust backend front door.
//!
//! Commands already ported to Rust are answered here; everything else is
//! forwarded byte-for-byte to the Go sidecar. That is what lets the 334-command
//! surface move across one command at a time while the app stays fully usable.
//!
//! ```text
//! WebView -> 127.0.0.1:<proxy>   (this server)
//!                |-- /api/health          -> answered here
//!                |-- /api/<ported cmd>    -> answered here (providers.rs, ...)
//!                |-- /api/<other cmd>     -> forwarded to Go
//!                |-- /ws, /ws/events      -> relayed to Go
//!                +-- everything else      -> forwarded to Go (static UI)
//! ```
//!
//! Porting a command: implement it in a module here, add it to the module's
//! `handle` dispatch, then delete the Go handler.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Request, State,
    },
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;

pub mod acp;
pub mod turn_store;
pub mod exec_providers;
pub mod harness_connections;
pub mod harness_instances;
pub mod experts;
pub mod files;
pub mod folder_commands;
pub mod forge;
pub mod acp_registry;
pub mod git;
pub mod goformat;
pub mod net;
pub mod provider_catalog;
pub mod providers;
pub mod office;
pub mod automations;
pub mod custom_agents;
pub mod misc;
pub mod terminal;
pub mod workspace_state;
pub mod store_reads;
pub mod store_writes;
pub mod updater;
pub mod store_file;
pub mod browser;
pub mod computer;
pub mod worktask;
pub mod worktask_engine;

pub struct Config {
    /// Port the webview connects to (this server).
    pub listen_port: u16,
    /// Port the Go sidecar listens on.
    pub upstream_port: u16,
    /// Workspace root, shared with the sidecar (`.data/` lives here).
    pub root: PathBuf,
}

struct ProxyState {
    client: reqwest::Client,
    upstream_http: String,
    upstream_ws: String,
    root: PathBuf,
    /// The sidecar's port — the codeg-mcp companion endpoint lives here.
    upstream_port: u16,
    /// Sinks for frames this server synthesises. Ported commands that used to
    /// call Go's `emit` hand their events here (`emit_events`), and the relay
    /// forwards them into the client socket next to the upstream frames.
    sinks: Mutex<SinkBook>,
}

#[derive(Default)]
struct SinkBook {
    next_id: u64,
    list: Vec<(u64, mpsc::UnboundedSender<Message>)>,
}

/// Sends one frame to every connected WebSocket client. Closed sinks are
/// pruned; a full Go-side buffer drops events (`select default`) — ours is
/// unbounded, matching the observable behaviour that no client misses a frame
/// it stays connected for. The payload is canonicalised the way Go's
/// `json.Marshal` renders the same row (sorted keys, whole floats collapsed),
/// so the frame bytes match the sidecar's too.
fn emit_events(state: &ProxyState, events: &[(String, Value)]) {
    if events.is_empty() {
        return;
    }
    let mut book = state.sinks.lock().unwrap();
    for (channel, payload) in events {
        let frame = Message::text(
            json!({"channel": channel, "payload": goformat::canon(payload)}).to_string(),
        );
        book.list.retain(|(_, sink)| sink.send(frame.clone()).is_ok());
    }
}

/// 全局事件出口：请求路径之外的模块（terminal 读线程、automations 后台任务
/// 与调度器、workspace 轮询线程）拿不到 `&ProxyState`，serve() 时把
/// [`emit_events`] 注册进来，任何线程都能发帧（等价 Go 的 `s.emit`）。
static EMITTER: OnceLock<Arc<dyn Fn(&str, Value) + Send + Sync>> = OnceLock::new();

pub fn emit_event(channel: &str, payload: Value) {
    if let Some(emit) = EMITTER.get() {
        emit(channel, payload);
    }
}

/// POSTs still-Go-owned commands and waits (bounded): Go kills the terminals
/// inline before answering a canvas delete, so that ordering stays visible to
/// the client. A stuck sidecar must not stall us, hence the timeout.
async fn forward_commands(state: &ProxyState, forwards: &[(String, Value)]) {
    for (command, args) in forwards {
        // 终端已归 Rust（G 组）：画布删除的 terminal_kill 直接杀本地会话，
        // Go 侧的同名会话已不存在。
        if command == "terminal_kill" {
            if let Some(tid) = args.get("terminalId").and_then(|v| v.as_str()) {
                let tid = tid.to_string();
                let _ =
                    tokio::task::spawn_blocking(move || terminal::kill_for_canvas(&tid)).await;
            }
            continue;
        }
        let client = state.client.clone();
        let url = format!("{}/api/{command}", state.upstream_http);
        let body = serde_json::to_vec(args).unwrap_or_default();
        let request = async {
            let _ = client
                .post(url)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await;
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), request).await;
    }
}

/// Starts the front door. Returns once the listener is bound.
pub async fn serve(cfg: Config) -> std::io::Result<()> {
    // The work task engine still lives in the sidecar; its handlers learn the
    // upstream address here so Rust-owned commands can hand work back to it.
    worktask::set_upstream(format!("http://127.0.0.1:{}", cfg.upstream_port));
    worktask::set_board_root(cfg.root.clone());
    let state = Arc::new(ProxyState {
        client: reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(std::io::Error::other)?,
        upstream_http: format!("http://127.0.0.1:{}", cfg.upstream_port),
        upstream_ws: format!("ws://127.0.0.1:{}/ws/events", cfg.upstream_port),
        root: cfg.root.clone(),
        upstream_port: cfg.upstream_port,
        sinks: Mutex::new(SinkBook::default()),
    });

    // 事件出口 + 定时任务调度器（30 秒一轮，等价 Go 的 TickAutomations）。
    let emitter_state = Arc::clone(&state);
    let _ = EMITTER.set(Arc::new(move |channel: &str, payload: Value| {
        emit_events(&emitter_state, &[(channel.to_string(), payload)]);
    }));
    // ACP 连接层在启动期就绪：任务引擎在请求路径之外直接驱动它。
    let layer_state = Arc::clone(&state);
    acp::commands::init_shared_layer(
        cfg.root.clone(),
        cfg.upstream_port,
        Arc::new(move |channel: &str, payload: &Value| {
            emit_events(&layer_state, &[(channel.to_string(), payload.clone())]);
        }),
    );
    worktask::spawn_scheduler(cfg.root.clone());
    automations::spawn_scheduler(automations::Bridge {
        client: reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(std::io::Error::other)?,
        self_http: format!("http://127.0.0.1:{}", cfg.listen_port),
        root: cfg.root,
    });

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/{command}", post(api_dispatch))
        .route("/ws", get(ws_proxy))
        .route("/ws/events", get(ws_proxy))
        .fallback(any(forward))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", cfg.listen_port)).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Health is proxied rather than synthesised: the UI and the sidecar's own
/// probes read fields from it (`codeg_version`, `data`, …), and a hand-built
/// body drifts the moment the Go side adds one. The only marker this layer
/// adds is a response header, so the body stays byte-identical to Go's.
async fn health(State(st): State<Arc<ProxyState>>) -> Response {
    match st
        .client
        .get(format!("{}/api/health", st.upstream_http))
        .send()
        .await
    {
        Ok(upstream) => {
            let status = upstream.status();
            let body = upstream.bytes().await.unwrap_or_default();
            let mut response = Response::new(axum::body::Body::from(body));
            *response.status_mut() = status;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            response
                .headers_mut()
                .insert("x-codepluse-proxy", HeaderValue::from_static("rust"));
            response
        }
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": err.to_string() })),
        )
            .into_response(),
    }
}

/// Command entry point. Rust answers what it owns; the rest goes upstream.
async fn api_dispatch(
    Path(command): Path<String>,
    State(st): State<Arc<ProxyState>>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, 128 << 20).await {
        Ok(bytes) => bytes,
        Err(err) => return fail(format!("读取请求体失败：{err}")),
    };

    // Go caps POST bodies at 2MB (MaxBytesReader) and answers with its
    // invalid_input envelope; mirrored here so owned commands enforce the
    // same contract (forwarded ones hit the sidecar's own cap regardless).
    if body.len() > 2 << 20 {
        let mut response = go_json(json!({
            "code": "invalid_input",
            "message": "http: request body too large",
        }));
        *response.status_mut() = StatusCode::BAD_REQUEST;
        return response;
    }

    let args: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&body).unwrap_or_else(|_| json!({}))
    };

    // The ACP layer is stateful (driver pool, connections): dispatch is async
    // and owns its dispatch table. `turn_in_progress` maps to Go's 409.
    if acp::owns(&command) {
        let root = st.root.clone();
        let upstream_port = st.upstream_port;
        let state_for_sink = st.clone();
        let sink: acp::connections::EmitSink = Arc::new(move |channel, payload| {
            emit_events(&state_for_sink, &[(channel.to_string(), payload.clone())]);
        });
        return match acp::commands::dispatch(root, upstream_port, sink, &command, &args).await {
            Ok(value) => go_json(value),
            Err(message) if message == "turn_in_progress" => {
                let mut response = go_json(json!({
                    "code": "turn_in_progress",
                    "message": "A turn is already in progress",
                }));
                *response.status_mut() = StatusCode::CONFLICT;
                response
            }
            Err(message) => fail(message),
        };
    }
    if let Some(outcome) = providers::handle(&st.root, &command, &args) {
        return match outcome {
            Ok(value) => go_json(value),
            Err(message) => fail(message),
        };
    }
    if let Some(outcome) = exec_providers::handle(&st.root, &command, &args) {
        return match outcome {
            Ok(value) => go_json(value),
            Err(message) => fail(message),
        };
    }
    // updater / forge / provider_catalog / acp_registry do blocking HTTPS via
    // `net.rs` — they must run on the blocking pool, never on an async worker.
    if updater::owns(&command)
        || forge::owns(&command)
        || provider_catalog::owns(&command)
        || acp_registry::owns(&command)
    {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome =
            tokio::task::spawn_blocking(move || {
                updater::handle(&root, &owned, &owned_args)
                    .or_else(|| forge::handle(&root, &owned, &owned_args))
                    .or_else(|| provider_catalog::handle(&root, &owned, &owned_args))
                    .or_else(|| acp_registry::handle(&root, &owned, &owned_args))


            })
            .await;
        return match outcome {
            Ok(Some(Ok(value))) => go_json(value),
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("命令执行失败：{join}")),
        };
    }
    if let Some(outcome) = office::handle(&st.root, &command, &args) {
        return match outcome {
            Ok(value) => go_json(value),
            Err(message) => fail(message),
        };
    }
    if let Some(outcome) = experts::handle(&st.root, &command, &args) {
        return match outcome {
            Ok(value) => go_json(value),
            Err(message) => fail(message),
        };
    }
    if let Some(outcome) = files::handle(&st.root, &command, &args) {
        return match outcome {
            Ok(value) => go_json(value),
            Err(message) => fail(message),
        };
    }
    if let Some(outcome) = store_reads::handle(&st.root, &command, &args) {
        return match outcome {
            Ok(value) => go_json(value),
            Err(message) => fail(message),
        };
    }
    // Store writes return events (and Go-bound side calls) on top of the HTTP
    // answer; the frames reach the same sockets Go's `emit` used to feed. The
    // handlers do blocking file I/O under a cross-process lock, so they run on
    // the blocking pool like git — a parked worker must not stall the relay.
    if store_writes::owns(&command) || folder_commands::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            if store_writes::owns(&owned) {
                store_writes::handle(&root, &owned, &owned_args)
                    .map(|o| (o.result, o.events, o.forward))
            } else {
                folder_commands::handle(&root, &owned, &owned_args)
                    .map(|r| (r, Vec::new(), Vec::new()))
            }
        })
        .await;
        let (result, events, forward) = match outcome {
            Ok(Some(broken_out)) => broken_out,
            Ok(None) => {
                return forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await;
            }
            Err(join) => return fail(format!("存储命令执行失败：{join}")),
        };
        emit_events(&st, &events);
        if !forward.is_empty() {
            forward_commands(&st, &forward).await;
        }
        return match result {
            Ok(value) => go_json(value),
            Err(message) => fail(message),
        };
    }
    // git fetch/push can legitimately take tens of seconds; keep the runtime
    // (and the WS relay) responsive while the CLI does its work.
    if git::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            git::handle(&root, &owned, &owned_args)
        })
        .await;
        return match outcome {
            Ok(Some(Ok(value))) => go_json(value),
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("git 任务执行失败：{join}")),
        };
    }

    // ── G/H/I 批次（terminal / automations / workspace_state / 杂项）────────

    // G 终端：ConPTY 会话在本进程；spawn/write/resize 毫秒级，内联执行。
    if let Some(outcome) = terminal::handle(&st.root, &command, &args) {
        return match outcome {
            Ok(value) => go_json(value),
            Err(message) => fail(message),
        };
    }
    // git_commit：除 git 外还要发 commit 成功事件，走 git.rs 的专用入口。
    if command == "git_commit" {
        let root = st.root.clone();
        let owned_args = args.clone();
        let outcome =
            tokio::task::spawn_blocking(move || git::commit_command(&root, &owned_args)).await;
        return match outcome {
            Ok(Ok((value, events))) => {
                emit_events(&st, &events);
                go_json(value)
            }
            Ok(Err(message)) => fail(message),
            Err(join) => fail(format!("git 任务执行失败：{join}")),
        };
    }
    // pick_directory 弹模态文件夹对话框，用户可能停几分钟——必须阻塞池。
    if misc::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome =
            tokio::task::spawn_blocking(move || misc::handle(&root, &owned, &owned_args)).await;
        return match outcome {
            Ok(Some(Ok(value))) => go_json(value),
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("命令执行失败：{join}")),
        };
    }
    // 收尾批：会话历史（SQLite）+ token 用量——带 conversation://changed 事件。
    if turn_store::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome =
            tokio::task::spawn_blocking(move || turn_store::handle(&root, &owned, &owned_args))
                .await;
        return match outcome {
            Ok(Some(outcome)) => {
                emit_events(&st, &outcome.events);
                match outcome.result {
                    Ok(value) => go_json(value),
                    Err(message) => fail(message),
                }
            }
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("命令执行失败：{join}")),
        };
    }
    // 收尾批：任务执行引擎（claim/merge 后台跑、discard 同步 git 清理）。
    if worktask_engine::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            worktask_engine::handle(&root, &owned, &owned_args)
        })
        .await;
        return match outcome {
            Ok(result) => match result {
                Ok(value) => go_json(value),
                Err(message) => fail(message),
            },
            Err(join) => fail(format!("引擎命令执行失败：{join}")),
        };
    }
    // 收尾批：执行端实例 CRUD + 主智能体设置（带 agent://changed 事件）。
    if harness_instances::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            harness_instances::handle(&root, &owned, &owned_args)
        })
        .await;
        return match outcome {
            Ok(Some((result, events))) => {
                emit_events(&st, &events);
                match result {
                    Ok(value) => go_json(value),
                    Err(message) => fail(message),
                }
            }
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("命令执行失败：{join}")),
        };
    }
    // 收尾批：连接目录/保存 + MCP 服务状态。
    if harness_connections::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            harness_connections::handle(&root, &owned, &owned_args)
        })
        .await;
        return match outcome {
            Ok(Some(outcome)) => {
                emit_events(&st, &outcome.events);
                match outcome.result {
                    Ok(value) => go_json(value),
                    Err(message) => fail(message),
                }
            }
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("命令执行失败：{join}")),
        };
    }
    // custom agents 只读面（acp_current_platform / acp_list_custom_agents）。
    if custom_agents::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            custom_agents::handle(&root, &owned, &owned_args)
        })
        .await;
        return match outcome {
            Ok(Some(Ok(value))) => go_json(value),
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("命令执行失败：{join}")),
        };
    }
    // workspace 状态流：全树扫描 + git 子进程（秒级）——阻塞池。
    if workspace_state::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            workspace_state::handle(&root, &owned, &owned_args)
        })
        .await;
        return match outcome {
            Ok(Some(Ok(value))) => go_json(value),
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("工作区扫描失败：{join}")),
        };
    }
    // 定时任务：文件 IO 小，但 run_now 要把运行任务挂回 tokio —— 阻塞池 +
    // Handle。响应是 Go struct 字段序，走 go_json_struct（canon 会打散它）。
    if automations::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let runtime = tokio::runtime::Handle::current();
        let outcome = tokio::task::spawn_blocking(move || {
            automations::handle(&root, &owned, &owned_args, &runtime)
        })
        .await;
        return match outcome {
            Ok(Some(Ok(value))) => go_json_struct(value),
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("定时任务执行失败：{join}")),
        };
    }
    // Work tasks own `.data/codeg-worktasks.json`; the run engine stays in the
    // sidecar, reached through internal trigger commands (see worktask.rs).
    // File I/O, git and the blocking trigger POSTs run on the blocking pool.
    // Responses are Go struct field order — go_json_struct, not go_json.
    if worktask::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome =
            tokio::task::spawn_blocking(move || worktask::handle(&root, &owned, &owned_args))
                .await;
        return match outcome {
            Ok(Some(Ok(outcome))) => {
                emit_events(&st, &outcome.events);
                go_json_struct(outcome.result)
            }
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("任务命令执行失败：{join}")),
        };
    }
    // Browser commands await the CDP WebSocket inline — async I/O, no
    // blocking pool needed (browser.rs mirrors Go's gorilla/websocket client).
    if browser::owns(&command) {
        return match browser::handle(&st.root, &command, &args).await {
            Some(Ok(outcome)) => {
                emit_events(&st, &outcome.events);
                go_json(outcome.result)
            }
            Some(Err(message)) => fail(message),
            None => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
        };
    }
    // Window enumeration and PrintWindow capture are quick but synchronous
    // FFI calls; keep them off the async workers like the file stores.
    if computer::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome =
            tokio::task::spawn_blocking(move || computer::handle(&root, &owned, &owned_args))
                .await;
        return match outcome {
            Ok(Some(Ok(outcome))) => {
                emit_events(&st, &outcome.events);
                go_json(outcome.result)
            }
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("电脑操作命令执行失败：{join}")),
        };
    }

    forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await
}

/// Go's `respond` envelope (server.go:120): compact JSON with Go's HTML
/// escaping and a trailing newline, plus the same two headers. Byte parity
/// holds even for `null` bodies, where a naive `Json(...)` diverges. Values
/// are canonicalised first: Go marshals `map[string]any`, so its key order is
/// sorted and whole floats lose the decimal point.
fn go_json(value: Value) -> Response {
    let mut body = goformat::marshal_preserve(&goformat::canon(&value));
    body.push(b'\n');
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::from(body))
        .expect("静态部件构造不可能失败")
}

/// Go's command error envelope, reproduced for parity.
fn fail(message: String) -> Response {
    let mut response = go_json(json!({ "code": "task_execution_failed", "message": message }));
    *response.status_mut() = StatusCode::BAD_REQUEST;
    response
}

/// 同 [`go_json`]，但不做键排序：automations 的响应是 Go struct（字段序 =
/// 声明序），canon 的递归排序会把字段序打散。嵌入的 map 值已在模块内单独
/// canon 过（Go 对 struct 里的 map 字段同样按键排序）。
fn go_json_struct(value: Value) -> Response {
    let mut body = goformat::marshal_preserve(&value);
    body.push(b'\n');
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::from(body))
        .expect("静态部件构造不可能失败")
}

async fn forward(State(st): State<Arc<ProxyState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, 128 << 20).await {
        Ok(bytes) => bytes,
        Err(err) => return fail(format!("读取请求体失败：{err}")),
    };
    forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await
}

/// Forwards to the sidecar. Hop headers are dropped: the Go server validates
/// Origin against its own port, and a proxied request would otherwise look
/// foreign and get 403.
async fn forward_parts(
    st: &ProxyState,
    method: Method,
    uri: &axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let url = format!("{}{}", st.upstream_http, path);

    let mut headers = headers;
    for name in [
        header::HOST,
        header::ORIGIN,
        header::CONNECTION,
        header::CONTENT_LENGTH,
        header::ACCEPT_ENCODING,
    ] {
        headers.remove(&name);
    }

    let mut builder = st.client.request(method, &url);
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    if !body.is_empty() {
        builder = builder.body(body);
    }

    match builder.send().await {
        Ok(upstream) => {
            let status = upstream.status();
            let upstream_headers = upstream.headers().clone();
            let body = upstream.bytes().await.unwrap_or_default();
            let mut response = Response::new(axum::body::Body::from(body));
            *response.status_mut() = status;
            for (name, value) in upstream_headers.iter() {
                if name == header::TRANSFER_ENCODING || name == header::CONNECTION {
                    continue;
                }
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(name.as_str().as_bytes()),
                    HeaderValue::from_bytes(value.as_bytes()),
                ) {
                    response.headers_mut().append(name, value);
                }
            }
            response
        }
        Err(err) => (StatusCode::BAD_GATEWAY, format!("后台不可达：{err}")).into_response(),
    }
}

/// WebSocket passthrough. The subprotocol must be echoed back: the browser
/// rejects an upgrade whose server did not select one of the offered protocols.
async fn ws_proxy(
    ws: WebSocketUpgrade,
    State(st): State<Arc<ProxyState>>,
    headers: HeaderMap,
) -> Response {
    let requested = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_string());

    let upgrade = match requested {
        Some(protocol) if !protocol.is_empty() => ws.protocols([protocol]),
        _ => ws,
    };

    upgrade.on_upgrade(move |socket| async move {
        if let Err(err) = relay(socket, st.clone()).await {
            eprintln!("[proxy] websocket relay ended: {err}");
        }
    })
}

async fn relay(client: WebSocket, st: Arc<ProxyState>) -> Result<(), String> {
    let (upstream, _) = tokio_tungstenite::connect_async(&st.upstream_ws)
        .await
        .map_err(|e| format!("连接上游失败：{e}"))?;
    let (mut client_tx, mut client_rx) = client.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();

    // Client-bound frames come from two places: the upstream relay and
    // `emit_events`. The injector channel joins them into one writer loop.
    let (inject_tx, mut inject_rx) = mpsc::unbounded_channel::<Message>();
    let sink_id = {
        let mut book = st.sinks.lock().unwrap();
        book.next_id += 1;
        let id = book.next_id;
        book.list.push((id, inject_tx.clone()));
        id
    };
    let outcome = loop {
        tokio::select! {
            incoming = client_rx.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if upstream_tx.send(UpstreamMessage::Text(text.to_string())).await.is_err() { break Ok(()); }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        if upstream_tx.send(UpstreamMessage::Binary(bytes.to_vec())).await.is_err() { break Ok(()); }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = upstream_tx.send(UpstreamMessage::Ping(payload.to_vec())).await;
                    }
                    Some(Ok(Message::Pong(payload))) => {
                        let _ = upstream_tx.send(UpstreamMessage::Pong(payload.to_vec())).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break Ok(()),
                    Some(Err(err)) => break Err(err.to_string()),
                }
            }
            outgoing = upstream_rx.next() => {
                match outgoing {
                    Some(Ok(UpstreamMessage::Text(text))) => {
                        if client_tx.send(Message::Text(text.into())).await.is_err() { break Ok(()); }
                    }
                    Some(Ok(UpstreamMessage::Binary(bytes))) => {
                        if client_tx.send(Message::Binary(Bytes::from(bytes))).await.is_err() { break Ok(()); }
                    }
                    Some(Ok(UpstreamMessage::Ping(payload))) => {
                        let _ = client_tx.send(Message::Ping(Bytes::from(payload))).await;
                    }
                    Some(Ok(UpstreamMessage::Pong(payload))) => {
                        let _ = client_tx.send(Message::Pong(Bytes::from(payload))).await;
                    }
                    Some(Ok(UpstreamMessage::Close(_))) | None => break Ok(()),
                    // Raw frames never surface from a read stream.
                    Some(Ok(UpstreamMessage::Frame(_))) => {}
                    Some(Err(err)) => break Err(err.to_string()),
                }
            }
            injected = inject_rx.recv() => {
                match injected {
                    Some(frame) => {
                        if client_tx.send(frame).await.is_err() { break Ok(()); }
                    }
                    // The registry never drops a live sink; losing it means
                    // shutdown, so end the relay instead of spinning on None.
                    None => break Ok(()),
                }
            }
        }
    };
    st.sinks.lock().unwrap().list.retain(|(id, _)| *id != sink_id);
    outcome
}
