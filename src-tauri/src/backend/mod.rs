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
use std::sync::{Arc, OnceLock};

use axum::{
    body::Bytes,
    extract::{ws::WebSocketUpgrade, Path, Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};

pub mod account_channels;
pub mod acp;
pub mod acp_registry;
pub mod agent_tool_settings;
pub mod automations;
pub mod background;
pub mod browser;
pub mod computer;
pub mod custom_agents;
pub mod desktop_settings;
pub mod events_ws;
pub mod exec_providers;
pub mod experts;
pub mod files;
pub mod folder_commands;
pub mod folder_links;
pub mod forge;
pub mod git;
pub mod goformat;
pub mod harness_connections;
pub mod harness_instances;
pub mod mcp_companion;
pub mod misc;
pub mod net;
pub mod office;
pub mod provider_catalog;
pub mod provider_writeback;
pub mod providers;
pub mod store_file;
pub mod store_reads;
pub mod store_writes;
pub mod terminal;
pub mod turn_store;
pub mod updater;
pub mod webapp;
pub mod workspace_state;
pub mod worktask;
pub mod worktask_engine;

pub struct Config {
    /// Port the webview connects to (this server).
    pub listen_port: u16,
    /// Port the Go sidecar listens on.
    /// Port of the Go sidecar — `None` once the sidecar is gone (Phase 4).
    pub upstream_port: Option<u16>,
    /// Workspace root, shared with the sidecar (`.data/` lives here).
    pub root: PathBuf,
}

struct ProxyState {
    client: reqwest::Client,
    /// The sidecar's address, if one is still around.
    upstream_http: Option<String>,
    root: PathBuf,
    /// The port this server listens on — the guard compares it with `Origin`.
    listen_port: u16,
}

/// Sends one frame to every connected WebSocket client. Closed sinks are
/// pruned; a full Go-side buffer drops events (`select default`) — ours is
/// unbounded, matching the observable behaviour that no client misses a frame
/// it stays connected for. The payload is canonicalised the way Go's
/// `json.Marshal` renders the same row (sorted keys, whole floats collapsed),
/// so the frame bytes match the sidecar's too.
fn emit_events(_state: &ProxyState, events: &[(String, Value)]) {
    for (channel, payload) in events {
        events_ws::broadcast(channel, goformat::canon(payload));
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
                let _ = tokio::task::spawn_blocking(move || terminal::kill_for_canvas(&tid)).await;
            }
            continue;
        }
        let client = state.client.clone();
        let base = match state.upstream_http.as_deref() {
            Some(base) => base,
            // No sidecar: the command must be owned by Rust.
            None => continue,
        };
        let url = format!("{base}/api/{command}");
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
    git::load_settings(&cfg.root);
    if let Err(error) = desktop_settings::apply_proxy_from_store(&cfg.root) {
        eprintln!("[backend] 忽略无效的应用代理设置：{error}");
    }
    // The work task engine still lives in the sidecar; its handlers learn the
    // upstream address here so Rust-owned commands can hand work back to it.
    let upstream_http = cfg
        .upstream_port
        .map(|port| format!("http://127.0.0.1:{port}"));
    if let Some(base) = upstream_http.clone() {
        worktask::set_upstream(base);
    }
    worktask::set_board_root(cfg.root.clone());
    let state = Arc::new(ProxyState {
        client: reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(std::io::Error::other)?,
        upstream_http,
        root: cfg.root.clone(),
        listen_port: cfg.listen_port,
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
        format!("http://127.0.0.1:{}", cfg.listen_port),
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

    let app = app_router(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", cfg.listen_port)).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn app_router(state: Arc<ProxyState>) -> Router {
    Router::new()
        // The browser's credential discovery posts here before opening its
        // event socket. A GET-only route strands connected agents in the UI.
        .route("/api/health", get(health).post(health))
        .route("/api/upload_attachment", any(upload_attachment))
        .route("/api/codeg_mcp", any(companion))
        .route("/api/{command}", any(api_dispatch))
        .route("/ws", get(ws_events))
        .route("/ws/events", get(ws_events))
        .fallback(any(static_or_dev))
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(state, guard_layer))
}

/// Health is answered locally (Phase 3): the sidecar is gone, and the row Go
/// used to synthesise is reproduced verbatim in `webapp::health`.
async fn health() -> Response {
    webapp::health()
}

/// Go `attachments.go` `uploadAttachment` — the multipart body is read here
/// so the 20MB + 1MB cap matches Go's `MaxBytesReader`.
async fn upload_attachment(State(st): State<Arc<ProxyState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, webapp::MAX_ATTACHMENT_REQUEST + 1024).await {
        Ok(bytes) => bytes,
        Err(err) => return fail(format!("读取请求体失败：{err}")),
    };
    webapp::upload_attachment(&st.root, &parts.method, &parts.headers, &body)
}

/// Go `delegation.go` `companionRequest`: the MCP companion endpoint. The
/// caller token must match one of the live ACP callers; the tool call itself
/// is dispatched by `mcp_companion::dispatch`.
async fn companion(State(st): State<Arc<ProxyState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    // Go answers a non-POST companion request with 405 before the token check.
    if parts.method != Method::POST {
        return webapp::companion_method_not_allowed();
    }
    let body = match axum::body::to_bytes(body, 256 << 10).await {
        Ok(bytes) => bytes,
        Err(err) => return fail(format!("读取请求体失败：{err}")),
    };
    let token = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_start_matches("Bearer ").to_string())
        .unwrap_or_default();
    let Some(layer) = acp::commands::layer_if_ready() else {
        return webapp::companion_unauthorized();
    };
    let Some(caller) = layer.caller_for_token(&token) else {
        return webapp::companion_unauthorized();
    };
    let request: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => return webapp::companion_bad_request(&err.to_string()),
    };
    let name = request
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let arguments = request.get("arguments").cloned().unwrap_or(json!({}));
    let root = st.root.clone();
    match mcp_companion::dispatch(&root, Some(&layer), Some(&caller), &name, &arguments).await {
        Ok(value) => webapp::companion_ok(&value),
        Err(message) => webapp::companion_bad_request(&message),
    }
}

/// Static assets (or the `next dev` proxy when one is configured).
async fn static_or_dev(State(st): State<Arc<ProxyState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, 128 << 20).await {
        Ok(bytes) => bytes,
        Err(err) => return fail(format!("读取请求体失败：{err}")),
    };
    if let Some(target) = webapp::dev_frontend() {
        return dev_proxy(&st, &target, parts.method, &parts.uri, &parts.headers, body).await;
    }
    let path = parts.uri.path().to_string();
    webapp::static_asset(&st.root, &path, &parts.headers)
}

/// Go's dev proxy: non-API traffic goes to the `next dev` server so the
/// running WebView keeps its hot reload. Unreachable target → 502.
async fn dev_proxy(
    st: &ProxyState,
    target: &str,
    method: Method,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let url = format!("{}{}", target.trim_end_matches('/'), path);
    let mut outgoing = HeaderMap::new();
    webapp::copy_headers(headers, &mut outgoing);
    outgoing.remove(header::HOST);
    outgoing.remove(header::ORIGIN);
    outgoing.remove(header::CONTENT_LENGTH);
    let mut builder = st.client.request(method, &url);
    for (name, value) in outgoing.iter() {
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
        Err(err) => webapp::dev_unavailable(target, &err.to_string()),
    }
}

/// Go `guard.go` `checkLocalRequest`: a browser page on any origin must not
/// be able to drive the terminal, git and file-system commands.
async fn guard_layer(
    State(st): State<Arc<ProxyState>>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    if let Err(reason) = webapp::guard(req.headers(), &host, st.listen_port, &method, &path) {
        return webapp::denied(reason);
    }
    next.run(req).await
}

/// Command entry point. Rust answers what it owns; the rest goes upstream.
async fn api_dispatch(
    Path(command): Path<String>,
    State(st): State<Arc<ProxyState>>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();

    // Go answers a non-POST command with 405 before it looks at the body.
    if parts.method != Method::POST {
        return webapp::error_envelope(StatusCode::METHOD_NOT_ALLOWED, "invalid_input", "Use POST");
    }
    // The content-type contract is enforced by the guard middleware (Go
    // `cmd/serve/guard.go`), which sees every request.
    // Background images are base64 JSON. Their normal 64 MiB asset cap needs
    // about 86 MiB on the wire; all other commands retain Go's 2 MiB limit.
    let command_body_limit = if command == "background_set" {
        90 * 1024 * 1024
    } else {
        webapp::MAX_COMMAND_BODY
    };
    let body = match axum::body::to_bytes(body, command_body_limit + 1024).await {
        Ok(bytes) => bytes,
        Err(err) => return fail(format!("读取请求体失败：{err}")),
    };

    // Go caps POST bodies at 2MB (MaxBytesReader) and answers with its
    // invalid_input envelope; mirrored here so owned commands enforce the
    // same contract (forwarded ones hit the sidecar's own cap regardless).
    if body.len() > command_body_limit {
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
        match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(err) => {
                let mut response = go_json(json!({
                    "code": "invalid_input",
                    "message": go_decode_message(&err),
                }));
                *response.status_mut() = StatusCode::BAD_REQUEST;
                return response;
            }
        }
    };

    if background::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let outcome =
            tokio::task::spawn_blocking(move || background::handle(&root, &owned, &args)).await;
        return match outcome {
            Ok(Some(Ok(value))) => go_json(value),
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => fail("背景图片命令未注册".into()),
            Err(join) => fail(format!("背景图片命令执行失败：{join}")),
        };
    }

    // The ACP layer is stateful (driver pool, connections): dispatch is async
    // and owns its dispatch table. `turn_in_progress` maps to Go's 409.
    if acp::owns(&command) {
        let root = st.root.clone();
        let upstream_port = st
            .upstream_http
            .as_deref()
            .and_then(|base| {
                base.rsplit(':')
                    .next()
                    .and_then(|port| port.parse::<u16>().ok())
            })
            .unwrap_or_default();
        let state_for_sink = st.clone();
        let sink: acp::connections::EmitSink = Arc::new(move |channel, payload| {
            emit_events(&state_for_sink, &[(channel.to_string(), payload.clone())]);
        });
        let base_url = format!("http://127.0.0.1:{}", st.listen_port);
        return match acp::commands::dispatch(root, base_url, sink, &command, &args).await {
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
        // Go returns structs here (`[]Info` / `Info`), whose field order is
        // part of the wire shape; `update_model_provider` returns a Row (a
        // map), whose keys Go sorts.
        let shape = if command == "update_model_provider" {
            go_json
        } else {
            go_json_struct
        };
        return match outcome {
            Ok(value) => shape(value),
            Err(message) => fail(message),
        };
    }
    if let Some(outcome) = exec_providers::handle(&st.root, &command, &args) {
        // `codepluse_provider_save` answers with the Provider struct.
        return match outcome {
            Ok(value) => go_json_struct(value),
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
        let outcome = tokio::task::spawn_blocking(move || {
            updater::handle(&root, &owned, &owned_args)
                .or_else(|| forge::handle(&root, &owned, &owned_args))
                .or_else(|| provider_catalog::handle(&root, &owned, &owned_args))
                .or_else(|| acp_registry::handle(&root, &owned, &owned_args))
        })
        .await;
        return match outcome {
            Ok(Some(Ok(value))) => {
                // `codepluse_providers_list` answers with the Data struct; the
                // sibling commands answer with a Row (a map Go sorts).
                if command == "codepluse_providers_list" {
                    go_json_struct(value)
                } else {
                    go_json(value)
                }
            }
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
    if folder_links::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome =
            tokio::task::spawn_blocking(move || folder_links::handle(&root, &owned, &owned_args))
                .await;
        return match outcome {
            Ok(Some(Ok(outcome))) => {
                emit_events(&st, &outcome.events);
                go_json(outcome.result)
            }
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("文件夹链接命令失败：{join}")),
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
        let outcome =
            tokio::task::spawn_blocking(move || git::handle(&root, &owned, &owned_args)).await;
        return match outcome {
            Ok(Some(Ok(value))) => go_json(value),
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("git 任务执行失败：{join}")),
        };
    }

    // 账号渠道（WorkBuddy / Qoder 独立账号）：命令直接转发给本地协议进程，
    // 响应保持它自己的键序（Go 侧返回的是原始字节）。
    if command.starts_with("codepluse_channel_") {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            account_channels::command(&root, &owned, &owned_args)
        })
        .await;
        return match outcome {
            Ok(Ok(value)) => go_json_struct(value),
            Ok(Err(message)) => fail(message),
            Err(join) => fail(format!("账号渠道命令执行失败：{join}")),
        };
    }

    // Agent-tool settings are shared by the settings UI and the status-bar MCP
    // controls. Persist them in the workspace store and relay committed
    // updates to every open window.
    if agent_tool_settings::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            agent_tool_settings::handle(&root, &owned, &owned_args)
        })
        .await;
        return match outcome {
            Ok(Some(Ok(outcome))) => {
                emit_events(&st, &outcome.events);
                go_json(outcome.result)
            }
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("智能体工具设置失败：{join}")),
        };
    }

    // Desktop preferences were formerly supplied by Go's desktop layer.
    // Persist and read them locally; registry writes stay off the async worker.
    if desktop_settings::owns(&command) {
        let root = st.root.clone();
        let owned = command.clone();
        let owned_args = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            desktop_settings::handle(&root, &owned, &owned_args)
        })
        .await;
        return match outcome {
            Ok(Some(Ok(value))) => go_json(value),
            Ok(Some(Err(message))) => fail(message),
            Ok(None) => forward_parts(&st, parts.method, &parts.uri, parts.headers, body).await,
            Err(join) => fail(format!("桌面设置命令失败：{join}")),
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
        let outcome =
            tokio::task::spawn_blocking(move || custom_agents::handle(&root, &owned, &owned_args))
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
            tokio::task::spawn_blocking(move || worktask::handle(&root, &owned, &owned_args)).await;
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
            tokio::task::spawn_blocking(move || computer::handle(&root, &owned, &owned_args)).await;
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
        // 本地应答标记：双跑据此断言「这条命令真由 Rust 接管」，否则
        // 前门转发给 Go 也会和 Go 侧逐字节相同（假绿）。
        .header("x-codepluse-backend", "rust")
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
    // Without a sidecar there is nothing to forward to: every command is
    // owned by Rust, so reaching this point means an unknown command.
    let base = match st.upstream_http.as_deref() {
        Some(base) => base,
        None => {
            return fail(format!("命令未实现：{}", path.trim_start_matches("/api/")));
        }
    };
    let url = format!("{base}{path}");

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

/// The event WebSocket (Go `websocket.go`), implemented locally in
/// `events_ws.rs`.
async fn ws_events(
    ws: WebSocketUpgrade,
    State(st): State<Arc<ProxyState>>,
    headers: HeaderMap,
) -> Response {
    events_ws::handle(ws, headers, st.listen_port).await
}

/// Go's JSON decoder error texts, so a malformed body answers with the same
/// message the sidecar used to send.
fn go_decode_message(err: &serde_json::Error) -> String {
    let text = err.to_string();
    if err.is_eof() {
        return "unexpected EOF".to_string();
    }
    if let Some(rest) = text.split(" at line ").next() {
        return rest.to_string();
    }
    text
}

#[cfg(test)]
mod transport_contract_tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn browser_health_discovery_and_event_socket_work_on_the_same_router() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();
        let state = Arc::new(ProxyState {
            client: client.clone(),
            upstream_http: None,
            root: std::env::temp_dir(),
            listen_port: port,
        });
        let server = tokio::spawn(async move {
            axum::serve(listener, app_router(state)).await.unwrap();
        });
        let base = format!("http://127.0.0.1:{port}");
        let get = client
            .get(format!("{base}/api/health"))
            .send()
            .await
            .unwrap();
        let post = client
            .post(format!("{base}/api/health"))
            .header("Origin", &base)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        let get_status = get.status();
        let post_status = post.status();
        let get_body = get.json::<Value>().await.unwrap();
        let post_body = post.json::<Value>().await.unwrap();
        let (mut ws, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws/events"))
                .await
                .unwrap();
        let ready = tokio::time::timeout(std::time::Duration::from_secs(3), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let ready: Value = serde_json::from_str(ready.to_text().unwrap()).unwrap();
        let _ = ws.close(None).await;
        server.abort();
        assert_eq!(get_status.as_u16(), 200);
        assert_eq!(
            post_status.as_u16(),
            200,
            "browser discovery must accept POST"
        );
        assert_eq!(post_body, get_body);
        assert_eq!(ready["channel"], "__ready__");
    }
}
