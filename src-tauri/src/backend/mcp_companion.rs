//! The `codeg-mcp` companion: the stdio MCP server the agents spawn, and the
//! tool surface it exposes.
//!
//! Ported from `server-go/internal/codeg/companion.go` (the JSON-RPC stdio
//! server and the tool definitions), `delegation_profiles.go`
//! (`delegationAgents`) and `webfetch.go` (`fetchURL`).
//!
//! The agent side:
//! ```text
//! codepluse.exe mcp-codeg <endpoint> <token>
//! ```
//! speaks MCP over stdio and forwards every `tools/call` to `<endpoint>`
//! (`/api/codeg_mcp`) with the caller's bearer token. The server side
//! ([`dispatch`]) answers the tool call.
//!
//! The collaboration tools (`delegate_to_agent`, `resume_delegation`,
//! `get_delegation_status`, `cancel_delegation`, `send_agent_message`,
//! `propose_task_split`, `check_user_feedback`, `ask_user_question`) resolve
//! the live layer through the caller's bearer token: `ask_user_question`
//! parks on the connection until the user answers (the HTTP request blocks,
//! like Go), and `propose_task_split` returns after the card is raised.

use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

/// Go `ToolDefinitions` (companion.go:15) — the tool list the agent sees.
pub fn tool_definitions() -> Vec<Value> {
    fn property(kind: &str, description: &str) -> Value {
        json!({ "type": kind, "description": description })
    }
    fn tool(name: &str, description: &str, required: &[&str], properties: Value) -> Value {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {
                "type": "object",
                "required": required,
                "properties": properties,
            }
        })
    }
    vec![
        tool(
            "list_agents",
            "列出子智能体配置中可委派的子智能体；每项含 assistant_id、名称、agent_type（它配置的执行端）与 model。委派时传 assistant_id。",
            &[],
            json!({}),
        ),
        tool(
            "resume_delegation",
            "在原子对话和原生会话中继续已有任务，适用于中断、失败、取消或追加工作。先用 check_user_feedback 获取原 task_id；不要重新委派同一任务。",
            &["task_id"],
            json!({
                "task_id": property("string", "已有任务 ID"),
                "task": property("string", "可选，继续任务的补充要求"),
            }),
        ),
        tool(
            "delegate_to_agent",
            "异步委派完整、自包含的子任务，立即返回 task_id。子代理没有你的对话上下文。可多次调用并行分工，再查询状态收集结果。优先传 list_agents 返回的 assistant_id（由该子智能体配置决定执行端与模型）；用户点名某个子智能体时用它的 assistant_id。",
            &["task"],
            json!({
                "assistant_id": property("string", "list_agents 返回的 assistant_id；传入后由该子智能体配置决定执行端与模型"),
                "agent_type": property("string", "可选，未传 assistant_id 时直接指定执行端"),
                "task": property("string", "完整任务、相关路径、约束、预期结果"),
                "working_dir": property("string", "可选工作目录，默认继承父会话"),
                "model": property("string", "可选，目标 Harness 支持的模型 ID"),
                "task_id": property("string", "可选，指定已有任务以复用原子对话"),
                "new_task": property("boolean", "明确创建不同的新任务；继续中断任务时不要设置"),
            }),
        ),
        tool(
            "get_delegation_status",
            "收集一个或多个子任务的真实结果。省略 wait_ms 为即时快照，正数最多等待 60000ms，0 等待到完成或需要用户授权。任务耗时正常时继续等待，不要假称完成。",
            &["task_ids"],
            json!({
                "task_ids": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                "wait_ms": { "type": "integer", "minimum": 0 }
            }),
        ),
        tool(
            "cancel_delegation",
            "仅当不再需要结果时停止子任务；已经结束的任务返回原结果。",
            &["task_id"],
            json!({ "task_id": property("string", "delegate_to_agent 返回的 task_id") }),
        ),
        tool(
            "check_user_feedback",
            "在关键决策和阶段切换时读取新消息及本团队任务分工，及时响应用户和同僚。",
            &[],
            json!({}),
        ),
        tool(
            "fetch_url",
            "抓取网页并返回正文文本。当 harness 自带的网页工具失败时（例如 Claude Code 的 WebFetch 报 “Unable to verify if domain … is safe to fetch”，即域名安全校验服务不可达），改用本工具抓取同一地址：它直接走本机网络，不依赖该校验。",
            &["url"],
            json!({
                "url": property("string", "http 或 https 地址"),
                "max_chars": property("integer", "可选，返回文本的最大字符数，默认 200000"),
            }),
        ),
        tool(
            "propose_task_split",
            "把整体任务拆成 1–8 个子任务交给用户确认：返回 split_id 并在界面展示卡片，用户确认后由系统并行派发，无需你逐条调用 delegate_to_agent。每个子任务可指定 assistant_id（list_agents 中的 assistant_id，由该子智能体配置决定执行端）或 agent_type（留空=自动选择）与 model（留空=该执行端钉住的默认模型）。调用后立即返回：卡片已在用户界面展示，用户确认后由系统自动派发全部子任务。本轮到此结束即可，不要结束前自行执行这些子任务，也禁止再调用 delegate_to_agent 重复派发；若用户随后发来新消息，先查看上方任务状态，切勿重复提案。无需用户挑选目标的单独立项仍用 delegate_to_agent。",
            &["tasks"],
            json!({
                "tasks": {
                    "type": "array", "minItems": 1, "maxItems": 8,
                    "items": {
                        "type": "object",
                        "required": ["prompt"],
                        "properties": {
                            "title": property("string", "一句话任务名，用于卡片展示"),
                            "prompt": property("string", "完整、自包含的子任务指令（子代理没有你的上下文）"),
                            "assistant_id": property("string", "list_agents 返回的 assistant_id；传入后由该子智能体配置决定执行端"),
                            "agent_type": property("string", "未指定 assistant_id 时的执行端；留空由系统自动选择"),
                            "model": property("string", "可选，目标 Harness 支持的模型 ID；留空使用钉住的默认模型"),
                        }
                    }
                }
            }),
        ),
        tool(
            "send_agent_message",
            "向同一团队的子任务发送消息；对方通过 check_user_feedback 收取。发送确认只代表入箱，不代表对方已经读取。",
            &["task_id", "message"],
            json!({
                "task_id": property("string", "同一团队的目标任务 ID"),
                "message": property("string", "消息正文"),
            }),
        ),
        tool(
            "ask_user_question",
            "向用户提出一个或多个选择题并阻塞等待作答，返回用户的选择。用于需要用户决策、澄清或确认方向时；每题给出若干选项（可单选或多选），用户也可自由作答或跳过。调用后会暂停本轮，直到用户应答。",
            &["questions"],
            json!({
                "questions": {
                    "type": "array", "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["id", "question", "options"],
                        "properties": {
                            "id": property("string", "问题唯一 ID"),
                            "question": property("string", "问题正文"),
                            "header": property("string", "简短标签"),
                            "multi_select": property("boolean", "是否允许多选"),
                            "options": {
                                "type": "array", "minItems": 1,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": property("string", "选项文本"),
                                        "description": property("string", "选项说明"),
                                    }
                                }
                            },
                        }
                    }
                }
            }),
        ),
    ]
}

// ── the stdio MCP server ───────────────────────────────────────────────────

/// Go `MainCompanion` (companion.go:52).
pub fn main_companion(args: &[String]) -> i32 {
    if args.len() != 2 {
        eprintln!("mcp-codeg requires endpoint and token");
        return 2;
    }
    serve(&args[0], &args[1])
}

/// Go `ServeCompanion` (companion.go:59): newline-delimited JSON-RPC over
/// stdin/stdout; only stdout carries protocol messages.
fn serve(endpoint: &str, token: &str) -> i32 {
    // `reqwest::blocking` defaults to a 30-second TOTAL request timeout
    // (reqwest 0.12.x `blocking::client.rs`: `Timeout(Some(Duration::
    // from_secs(30)))`), which kills the parked `ask_user_question` call —
    // it legitimately blocks until the user answers and is released by the
    // layer, so it cannot hang forever. Disable the total timeout
    // explicitly (passing `None` opts out of the 30s default) and bound
    // only the connect phase, so a dead endpoint still fails fast.
    let client = match reqwest::blocking::Client::builder()
        .timeout(None)
        .connect_timeout(Duration::from_secs(15))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("codeg-mcp: 客户端初始化失败：{error}");
            return 1;
        }
    };
    let stdin = std::io::stdin();
    let stdout = std::sync::Arc::new(std::sync::Mutex::new(stdout_handle()));
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(id) = request.get("id").filter(|value| !value.is_null()).cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let params = request.get("params").cloned().unwrap_or(json!({}));
        match method.as_str() {
            "initialize" => {
                write_message(
                    &stdout,
                    id,
                    Ok(json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "codeg-mcp-rust", "version": "0.1.0" },
                    })),
                );
            }
            "ping" => write_message(&stdout, id, Ok(json!({}))),
            "tools/list" => {
                write_message(&stdout, id, Ok(json!({ "tools": tool_definitions() })));
            }
            "tools/call" => {
                // Go answers tool calls concurrently; each call is a separate
                // HTTP round trip, so a slow page must not block the protocol.
                let endpoint = endpoint.to_string();
                let token = token.to_string();
                let client = client.clone();
                let writer = stdout.clone();
                workers.push(std::thread::spawn(move || {
                    let body = serde_json::to_vec(&params).unwrap_or_default();
                    let outcome = client
                        .post(&endpoint)
                        .header("Content-Type", "application/json")
                        .header("Authorization", format!("Bearer {token}"))
                        .body(body)
                        .send();
                    let message = match outcome {
                        Ok(response) => {
                            let status = response.status();
                            // Go hands the raw response body to the model as
                            // one text block; a non-200 marks it as an error.
                            let text = response.text().unwrap_or_default();
                            let mut result = json!({"content": [{"type": "text", "text": text}]});
                            if status != reqwest::StatusCode::OK {
                                result["isError"] = json!(true);
                            }
                            json!({ "jsonrpc": "2.0", "id": id, "result": result })
                        }
                        Err(error) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32603, "message": error.to_string() }
                        }),
                    };
                    if let Ok(mut guard) = writer.lock() {
                        let _ = guard.write_all(go_json_line(&message).as_bytes());
                        let _ = guard.flush();
                    }
                }));
            }
            other => {
                // Go answers an unsupported method with a JSON-RPC error.
                write_message(
                    &stdout,
                    id,
                    Err(format!("unsupported MCP method: {other}")),
                );
            }
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    0
}

/// Go writes the JSON-RPC frames with `json.NewEncoder`, which sorts the map
/// keys and appends a newline — the same shape `goformat` produces.
fn go_json_line(value: &Value) -> String {
    let mut body =
        crate::backend::goformat::marshal_preserve(&crate::backend::goformat::canon(value));
    body.push(b'\n');
    String::from_utf8_lossy(&body).to_string()
}

/// The shared stdout handle the tool-call threads write through.
fn stdout_handle() -> std::io::Stdout {
    std::io::stdout()
}

/// Go's `write` closure (companion.go:64): one JSON-RPC message per line.
fn write_message(
    stdout: &std::sync::Arc<std::sync::Mutex<std::io::Stdout>>,
    id: Value,
    outcome: Result<Value, String>,
) {
    let mut message = json!({ "jsonrpc": "2.0", "id": id });
    match outcome {
        Ok(value) => message["result"] = value,
        Err(error) => message["error"] = json!({ "code": -32603, "message": error }),
    }
    if let Ok(mut guard) = stdout.lock() {
        let _ = guard.write_all(go_json_line(&message).as_bytes());
        let _ = guard.flush();
    }
}

// ── the server side: one tool call ─────────────────────────────────────────

/// Go `s.tool` (delegation.go:215).
pub async fn dispatch(
    root: &Path,
    layer: Option<&std::sync::Arc<crate::backend::acp::connections::Layer>>,
    caller: Option<&crate::backend::acp::tool_identity::Caller>,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    match name {
        "list_agents" => Ok(json!({ "agents": delegation_agents(root) })),
        "fetch_url" => {
            // The page fetch is blocking I/O — keep it off the async workers.
            let root = root.to_path_buf();
            let args = args.clone();
            tokio::task::spawn_blocking(move || fetch_url(&args))
                .await
                .map_err(|error| format!("抓取失败：{error}"))?
        }
        "delegate_to_agent" => {
            let (layer, caller) = collaborators(layer, caller)?;
            layer.delegate_to_agent(caller, args).await
        }
        "get_delegation_status" => {
            let (layer, caller) = collaborators(layer, caller)?;
            let ids = args
                .get("task_ids")
                .and_then(|value| value.as_array())
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| row.as_str().map(|text| text.to_string()))
                        .collect::<Vec<String>>()
                });
            let wait = args.get("wait_ms").and_then(|value| value.as_i64());
            layer.delegation_status(caller, ids, wait).await
        }
        "cancel_delegation" => {
            let (layer, caller) = collaborators(layer, caller)?;
            let task_id = args
                .get("task_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            layer.cancel_delegation(caller, task_id).await
        }
        "resume_delegation" => {
            let (layer, caller) = collaborators(layer, caller)?;
            let task_id = args
                .get("task_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let task = args
                .get("task")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            layer.resume_delegation(caller, task_id, task).await
        }
        "send_agent_message" => {
            let (layer, caller) = collaborators(layer, caller)?;
            let task_id = args
                .get("task_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let message = args
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            layer.send_agent_message(caller, task_id, message)
        }
        "check_user_feedback" => {
            let (layer, caller) = collaborators(layer, caller)?;
            Ok(layer.check_user_feedback(caller))
        }
        "ask_user_question" => {
            let (layer, caller) = collaborators(layer, caller)?;
            layer.ask_user_question(caller, args).await
        }
        "propose_task_split" => {
            let (layer, caller) = collaborators(layer, caller)?;
            layer.propose_task_split(caller, args).await
        }
        other => Err(format!("未知协作工具：{other}")),
    }
}

/// The delegation tools need the live layer and the authenticated caller.
fn collaborators<'a>(
    layer: Option<&'a std::sync::Arc<crate::backend::acp::connections::Layer>>,
    caller: Option<&'a crate::backend::acp::tool_identity::Caller>,
) -> Result<
    (
        &'a std::sync::Arc<crate::backend::acp::connections::Layer>,
        &'a crate::backend::acp::tool_identity::Caller,
    ),
    String,
> {
    match (layer, caller) {
        (Some(layer), Some(caller)) => Ok((layer, caller)),
        _ => Err("协作会话不可用".into()),
    }
}

/// Go `delegationAgents` (delegation_profiles.go:22): the harnesses that can
/// take a delegated task.
pub fn delegation_agents(root: &Path) -> Vec<Value> {
    // 子智能体配置 is the single delegation entry: every enabled assistant
    // row, never the raw ACP instance profiles it happens to run on.
    super::acp::delegation::assistant_rows(root)
        .into_iter()
        .filter(|row| {
            row.get("deleted_at")
                .map(|value| value.is_null())
                .unwrap_or(true)
                && row
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
        })
        .filter_map(|row| {
            let agent_type = row
                .get("agent_type")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if agent_type.is_empty() {
                return None;
            }
            let mut entry = json!({
                "assistant_id": row.get("id").cloned().unwrap_or(Value::Null),
                "assistant_uid": row.get("uid").cloned().unwrap_or(Value::Null),
                "name": row.get("name").cloned().unwrap_or(Value::Null),
                "description": row.get("description").cloned().unwrap_or(Value::Null),
                "agent_type": agent_type,
            });
            let model = row
                .pointer("/agent_options/config_values/model")
                .cloned()
                .unwrap_or(Value::Null);
            if !model.is_null() {
                entry["model"] = model;
            }
            Some(entry)
        })
        .collect()
}

// ── fetch_url (webfetch.go) ───────────────────────────────────────────────

const FETCH_MAX_BYTES: usize = 4 << 20;
const FETCH_MAX_TEXT: usize = 200 << 10;
const FETCH_MAX_REDIRECT: usize = 5;
const FETCH_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) codepluse/2.0 Safari/537.36";

/// Go `fetchURL` (webfetch.go:39).
pub fn fetch_url(args: &Value) -> Result<Value, String> {
    let raw = args
        .get("url")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if raw.is_empty() {
        return Err("url 不能为空".into());
    }
    let parsed = url::Url::parse(&raw).map_err(|_| format!("URL 无效：{raw}"))?;
    if parsed.host_str().unwrap_or("").is_empty() {
        return Err(format!("URL 无效：{raw}"));
    }
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!("只支持 http/https URL：{raw}"));
    }
    let mut limit = FETCH_MAX_TEXT;
    if let Some(value) = args.get("max_chars").and_then(|value| value.as_f64()) {
        if value >= 1.0 {
            limit = if value >= FETCH_MAX_TEXT as f64 {
                FETCH_MAX_TEXT
            } else {
                value as usize
            };
        }
    }
    // The fetch guard blocks private/loopback/link-local targets.
    let host = parsed.host_str().unwrap_or("").to_string();
    crate::backend::provider_catalog::host_is_safe(&host)?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(25))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= FETCH_MAX_REDIRECT {
                return attempt.error("重定向次数过多");
            }
            let next = attempt.url();
            if next.scheme() != "http" && next.scheme() != "https" {
                return attempt.error("重定向目标只支持 http/https");
            }
            let host = next.host_str().unwrap_or("");
            if host.is_empty() || crate::backend::provider_catalog::host_is_safe(host).is_err() {
                return attempt.error("重定向目标地址不允许访问");
            }
            attempt.follow()
        }))
        .build()
        .map_err(|error| format!("抓取失败：{error}"))?;
    let response = client
        .get(parsed.clone())
        .header("User-Agent", FETCH_USER_AGENT)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/json;q=0.9,text/plain;q=0.8,*/*;q=0.7",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .map_err(|error| format!("抓取失败：{error}"))?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut body = Vec::new();
    {
        use std::io::Read;
        response
            .take((FETCH_MAX_BYTES as u64) + 1)
            .read_to_end(&mut body)
            .map_err(|error| format!("读取响应失败：{error}"))?;
    }
    if body.len() > FETCH_MAX_BYTES {
        return Err(format!("页面过大（超过 {FETCH_MAX_BYTES} 字节）"));
    }
    // Go decodes GBK/Big5 pages here; the Rust port reads UTF-8 (the charset
    // decoders are not part of the dependency set yet).
    let payload = String::from_utf8_lossy(&body).to_string();
    let mut text = payload.clone();
    if content_type.to_lowercase().contains("html") || looks_like_html(&payload) {
        text = html_to_text(&payload);
    }
    let mut truncated = false;
    if text.chars().count() > limit {
        text = text.chars().take(limit).collect();
        truncated = true;
    }
    let mut row = json!({
        "url": parsed.to_string(),
        "status": status,
        "content_type": content_type,
        "chars": text.chars().count(),
    });
    if truncated {
        row["truncated"] = json!(true);
    }
    row["text"] = json!(text);
    Ok(row)
}

/// Go `looksLikeHTML` (webfetch.go:123).
fn looks_like_html(body: &str) -> bool {
    let end = body
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .take_while(|&end| end <= 2048)
        .last()
        .unwrap_or(0);
    let head = body[..end].to_ascii_lowercase();
    head.contains("<html")
        || head.contains("<body")
        || head.contains("<div")
        || head.contains("<p>")
}

/// Go `htmlToText` (webfetch.go:136): scripts, styles and comments go; block
/// tags become line breaks; entities are decoded.
fn html_to_text(body: &str) -> String {
    let lower = body.to_ascii_lowercase();
    let mut out = String::with_capacity(body.len() / 2);
    let mut skip_until = String::new();
    let bytes = body.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if !skip_until.is_empty() {
            match lower[index..].find(&skip_until) {
                Some(end) => {
                    index += end + skip_until.len();
                    skip_until.clear();
                }
                None => {
                    // The closing tag never arrives — stop skipping instead of
                    // dropping the rest of the page.
                    skip_until.clear();
                    continue;
                }
            }
            continue;
        }
        if bytes[index] == b'<' {
            let Some(end) = body[index..].find('>') else {
                break;
            };
            let tag = body[index + 1..index + end].to_lowercase();
            if tag.starts_with("script") || tag.starts_with("style") || tag.starts_with("noscript")
            {
                let name = tag.split_whitespace().next().unwrap_or("script");
                skip_until = format!("</{name}>");
            } else if tag.starts_with("!--") {
                if !tag.ends_with("--") {
                    skip_until = "-->".into();
                }
            } else if BLOCK_TAGS.iter().any(|name| tag.starts_with(name)) {
                out.push('\n');
            }
            index += end + 1;
            continue;
        }
        // Copy one UTF-8 character.
        let ch = body[index..].chars().next().unwrap();
        out.push(ch);
        index += ch.len_utf8();
    }
    collapse_spaces(&decode_entities(&out))
}

const BLOCK_TAGS: [&str; 17] = [
    "br", "p", "div", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "section", "article",
    "table", "ul", "ol", "body",
];

/// Go `decodeEntities` (webfetch.go:201).
fn decode_entities(text: &str) -> String {
    let replacements = [
        ("&nbsp;", " "),
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&apos;", "'"),
        ("&#39;", "'"),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
        ("&hellip;", "…"),
        ("&ldquo;", "“"),
        ("&rdquo;", "”"),
        ("&lsquo;", "‘"),
        ("&rsquo;", "’"),
        ("&middot;", "·"),
    ];
    let mut out = text.to_string();
    for (from, to) in replacements {
        out = out.replace(from, to);
    }
    // Numeric entities.
    loop {
        let Some(start) = out.find("&#") else { break };
        let Some(offset) = out[start..].find(';') else {
            break;
        };
        if offset == 0 {
            break;
        }
        let raw = &out[start + 2..start + offset];
        let code = if let Some(hex) = raw.strip_prefix('x').or_else(|| raw.strip_prefix('X')) {
            i64::from_str_radix(hex, 16).ok()
        } else {
            raw.parse::<i64>().ok()
        };
        let Some(code) = code else {
            out = format!("{}{}", &out[..start], &out[start + offset + 1..]);
            continue;
        };
        if code <= 0 {
            out = format!("{}{}", &out[..start], &out[start + offset + 1..]);
            continue;
        }
        let ch = char::from_u32(code as u32).unwrap_or('\u{fffd}');
        out = format!("{}{}{}", &out[..start], ch, &out[start + offset + 1..]);
    }
    out
}

/// Go `collapseSpaces` (webfetch.go:319).
fn collapse_spaces(text: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut blank = 0;
    for line in text.split('\n') {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
            kept.push(String::new());
            continue;
        }
        blank = 0;
        kept.push(line);
    }
    kept.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_match_go() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 10);
        assert_eq!(tools[0]["name"], json!("list_agents"));
        assert_eq!(tools[0]["inputSchema"]["required"], json!([]));
        assert_eq!(tools[6]["name"], json!("fetch_url"));
        assert_eq!(tools[6]["inputSchema"]["required"], json!(["url"]));
    }

    /// The delegation list is the 子智能体配置 store and nothing else: old
    /// ACP instance profiles never surface, disabled or deleted rows stay out.
    #[test]
    fn delegation_agents_lists_enabled_assistants_only() {
        let dir = std::env::temp_dir().join(format!("codeg-mcp-agents-{}", std::process::id()));
        let data = dir.join(".data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            data.join("codeg-workspace.json"),
            json!({
                "assistants": [
                    {"id": 1, "name": "调研", "agent_type": "inst_claude2",
                     "agent_options": {"config_values": {"model": "m1"}},
                     "enabled": true, "deleted_at": null},
                    {"id": 2, "name": "已停用", "agent_type": "claude_code",
                     "enabled": false, "deleted_at": null},
                    {"id": 3, "name": "已删除", "agent_type": "claude_code",
                     "enabled": true, "deleted_at": "2026-01-01T00:00:00Z"},
                    {"id": 4, "name": "无执行端", "agent_type": null,
                     "enabled": true, "deleted_at": null}
                ],
                "agents": [
                    {"agent_type": "inst_claude3", "instance": true,
                     "provider_id": "acct", "enabled": true}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let agents = delegation_agents(&dir);
        assert_eq!(agents.len(), 1, "only the enabled, configured sub-agent");
        assert_eq!(agents[0]["assistant_id"], json!(1));
        assert_eq!(agents[0]["name"], json!("调研"));
        assert_eq!(agents[0]["agent_type"], json!("inst_claude2"));
        assert_eq!(agents[0]["model"], json!("m1"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn html_is_reduced_to_text() {
        let text = html_to_text(
            "<html><head><style>x{}</style></head><body><p>hello</p><script>bad()</script></body></html>",
        );
        assert_eq!(text, "hello");
    }

    #[test]
    fn entities_are_decoded() {
        assert_eq!(decode_entities("a&amp;b&#39;c"), "a&b'c");
    }

    #[test]
    fn multibyte_utf8_at_2048_boundary_and_unicode_case_do_not_panic() {
        let mut body = "a".repeat(2047);
        body.push_str("中文<div>内容</div>");
        assert!(!looks_like_html(&body));
        let html = format!("<html><body>İ{}<script>x()</script><p>好</p></body></html>", "中".repeat(700));
        assert!(looks_like_html(&html));
        let extracted = html_to_text(&html);
        assert!(extracted.contains("好"));
    }
}
