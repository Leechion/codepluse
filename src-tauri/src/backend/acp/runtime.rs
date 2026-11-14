//! Group-C runtime core, ported behaviour-for-behaviour from Go's
//! `internal/acp/runtime.go`: one driver per (agent, permission, cwd[,
//! provider version]) over NDJSON JSON-RPC stdio, native session
//! reuse/restoration, permission cards with a pausable clock, model switching
//! and submit_result capture.
//!
//! Mapping notes:
//! - Go's per-record `activeMu` becomes `Mutex<RecordData>`; `turnMu` becomes
//!   a `tokio::sync::Mutex` used with `try_lock` where Go used `TryLock`.
//! - Permission cards are shared as `Arc<Mutex<PermissionCard>>` between the
//!   record's list and the approval entry, like Go's shared pointer.
//! - The reader task keeps Go's discipline: notifications apply in wire order
//!   under the record lock only, while agent→client requests run detached so
//!   a pending human permission decision cannot stall the RPC loop.
//! - Limits are copied verbatim: 1 MiB fs reads/writes, 200 000-byte output,
//!   40 000-byte tool rows, 40 tools, 20 turns, 100 persisted records,
//!   8 000/400-char stderr tails, 500 ms turn ticker, 10 s cancel grace.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;

use serde_json::{json, Map, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, watch, Mutex as AsyncMutex};

use crate::backend::acp::agents::Agent;
use crate::backend::acp::launch::{Launch, LaunchResolver};
use crate::backend::goformat;

// ----------------------------- ids and helpers -----------------------------

/// Go `newID` (8 random bytes hex).
fn short_id() -> String {
    let a: u64 = rand::random();
    format!("{a:016x}")
}

fn unix_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

/// Go `truncate` (runtime.go:710): byte-slice semantics, invalid UTF-8 tails
/// become U+FFFD exactly like Go's JSON encoder renders them.
fn truncate_bytes(s: &str, n: usize) -> String {
    if s.len() > n {
        String::from_utf8_lossy(&s.as_bytes()[..n]).to_string()
    } else {
        s.to_string()
    }
}

fn to_map_slice(v: Option<&Value>) -> Vec<Value> {
    v.and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|item| item.is_object())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

// ----------------------------- options / result -----------------------------

/// Per-turn knobs the orchestrator passes down (Go `Options`, runtime.go:38).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Options {
    #[serde(rename = "sessionKey")]
    pub session_key: String,
    #[serde(default)]
    pub permission: String,
    #[serde(default)]
    pub workdir: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    #[serde(rename = "chatId", default, skip_serializing_if = "String::is_empty")]
    pub chat_id: String,
    #[serde(rename = "missionId", default, skip_serializing_if = "String::is_empty")]
    pub mission_id: String,
    #[serde(rename = "taskId", default, skip_serializing_if = "String::is_empty")]
    pub task_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stage: String,
    #[serde(default)]
    pub structured: bool,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub sensitivity: i64,
    #[serde(rename = "timeoutMs", default, skip_serializing_if = "is_zero_i64")]
    pub timeout_ms: i64,
    #[serde(rename = "outputSchema", default, skip_serializing_if = "Value::is_null")]
    pub output_schema: Value,
}

impl Options {
    /// Go `optionsKey` (runtime.go:1024): `<sessionKey|direct>|<workdir>`.
    pub fn options_key(&self) -> String {
        let key = if self.session_key.is_empty() { "direct" } else { &self.session_key };
        format!("{}|{}", key, self.workdir)
    }
}

/// Go `Result` (runtime.go:56) — the turn result the orchestrator reads.
#[derive(Clone, Debug, Default)]
pub struct TurnResult {
    pub ok: bool,
    pub output: String,
    pub structured: Option<Value>,
    pub error: String,
    pub meta: Value,
}

impl TurnResult {
    fn failed(error: impl Into<String>) -> Self {
        TurnResult {
            ok: false,
            error: error.into(),
            meta: json!({}),
            ..TurnResult::default()
        }
    }
}

const RESULT_KEYS: [&str; 11] = [
    "output", "complete", "blocked", "messages", "reads", "tasks", "reply", "action", "report",
    "updates", "newTasks",
];

/// Go `extractSubmitted` (runtime.go:68): the last tool row whose rawInput
/// carries one of the submit_result keys.
fn extract_submitted(tools: &[Value]) -> Option<Value> {
    for tool in tools.iter().rev() {
        let Some(raw_input) = tool.get("rawInput") else { continue };
        match raw_input {
            Value::Object(map) => {
                if map.keys().any(|k| RESULT_KEYS.contains(&k.as_str())) {
                    return Some(Value::Object(map.clone()));
                }
            }
            Value::String(raw) => {
                if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) {
                    if map.keys().any(|k| RESULT_KEYS.contains(&k.as_str())) {
                        return Some(Value::Object(map));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Go `normalizeUsage` (runtime.go:721): adapter-specific usage keys → the
/// persisted TurnUsage shape; totals ≤ 0 yield None.
fn normalize_usage(u: Option<&Value>) -> Option<Value> {
    let u = u?;
    let num = |keys: &[&str]| -> f64 {
        for k in keys {
            if let Some(f) = u.get(*k).and_then(Value::as_f64) {
                return f;
            }
        }
        0.0
    };
    let out = json!({
        "input_tokens": num(&["inputTokens", "input_tokens"]),
        "output_tokens": num(&["outputTokens", "output_tokens"]),
        "cache_creation_input_tokens": num(&["cachedWriteTokens", "cacheWriteInputTokens", "cache_creation_input_tokens"]),
        "cache_read_input_tokens": num(&["cachedReadTokens", "cachedInputTokens", "cache_read_input_tokens"]),
    });
    let total: f64 = out.as_object().unwrap().values().filter_map(Value::as_f64).sum();
    if total <= 0.0 {
        None
    } else {
        Some(out)
    }
}

// ----------------------------- hooks -----------------------------

/// Fire-and-forget event sink (Go `hub.Emit` on the runtime side).
pub trait Emitter: Send + Sync {
    fn emit(&self, channel: &str, payload: &Value);
}

pub struct NoopEmitter;
impl Emitter for NoopEmitter {
    fn emit(&self, _channel: &str, _payload: &Value) {}
}

/// Injection hooks the codeg layer installs (Go's `OnUpdate`, `ExtraMCP`,
/// `AllowInternalTool` fields).
pub trait RuntimeHooks: Send + Sync {
    fn on_update(&self, session_id: &str, update: &Value) {}
    fn extra_mcp(&self, agent: &Agent, options: &Options, session_id: &str) -> Vec<Value> {
        Vec::new()
    }
    fn allow_internal_tool(&self, session_id: &str, tool_call: &Value) -> bool {
        false
    }
}

pub struct NoopHooks;
impl RuntimeHooks for NoopHooks {}

// ----------------------------- session records -----------------------------

/// Go `PermissionCard` (runtime.go:119), wire order preserved.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PermissionCard {
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "nativeSessionId")]
    pub native_id: String,
    pub title: String,
    #[serde(rename = "toolCall", default, skip_serializing_if = "Value::is_null")]
    pub tool_call: Value,
    #[serde(default)]
    pub options: Vec<Value>,
    #[serde(default)]
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default)]
    pub at: i64,
}

/// Go `Turn` (runtime.go:133); `last_activity` is runtime-only (unexported).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Turn {
    pub id: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub structured: bool,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub tools: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan: Vec<Value>,
    #[serde(rename = "startedAt", default)]
    pub started_at: i64,
    #[serde(rename = "finishedAt", default, skip_serializing_if = "is_zero_i64")]
    pub finished_at: i64,
    #[serde(default)]
    pub status: String,
    #[serde(rename = "stopReason", default, skip_serializing_if = "String::is_empty")]
    pub stop_reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(rename = "receivedChars", default, skip_serializing_if = "is_zero_i64")]
    pub received_chars: i64,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub usage: Value,
    #[serde(skip)]
    pub last_activity_millis: i64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Catalog {
    #[serde(rename = "currentModelId", default)]
    pub current_model_id: String,
    #[serde(rename = "availableModels", default, skip_serializing_if = "Option::is_none")]
    pub available_models: Option<Vec<Value>>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ModesInfo {
    #[serde(rename = "currentModeId", default)]
    pub current_mode_id: String,
    #[serde(rename = "availableModes", default, skip_serializing_if = "Option::is_none")]
    pub available_modes: Option<Vec<Value>>,
}

/// The mutable payload of one session record — guarded like Go's `activeMu`.
#[derive(Default)]
pub struct RecordData {
    pub chat_id: String,
    pub mission_id: String,
    pub task_id: String,
    pub role: String,
    pub permission: String,
    pub native_mode: String,
    pub state: String,
    pub native_session_id: String,
    pub turns: Vec<Turn>,
    pub permissions: Vec<Arc<Mutex<PermissionCard>>>,
    pub models: Option<Catalog>,
    pub modes: Option<ModesInfo>,
    pub config_options: Vec<Value>,
    pub notices: Vec<Value>,
    pub default_model: String,
    pub error: String,
    pub created_at: i64,
    pub handed_off: bool,
    pub handed_off_to: String,
    pub active: Option<Arc<AsyncMutex<Turn>>>,
}

/// Go `SessionRecord` (runtime.go:164).
pub struct SessionRecord {
    pub id: String,
    pub key: String,
    pub agent_id: String,
    pub cwd: String,
    /// Serialises whole turns per session (Go `turnMu`).
    pub turn_lock: AsyncMutex<()>,
    pub data: Mutex<RecordData>,
}

/// Go `ModeLevel` (runtime.go:1177).
pub fn mode_level(mode: &str) -> &'static str {
    match mode {
        "read-only" | "readonly" | "plan" => "readonly",
        "workspace-write" | "default" | "acceptEdits" | "auto" | "agent" => "workspace",
        "danger-full-access" | "agent-full-access" | "bypassPermissions" => "full",
        _ => "",
    }
}

impl SessionRecord {
    /// Go `canWrite` (runtime.go:1190): caller holds the record lock in Go.
    fn can_write(data: &RecordData) -> bool {
        let level = mode_level(&data.native_mode);
        if !level.is_empty() {
            return level != "readonly";
        }
        data.permission == "workspace"
    }
}

/// Go `DelegatedMode` (runtime.go:1197).
pub fn delegated_mode(agent: &Agent, level: &str) -> String {
    if level != "full" {
        if let Some(mode) = agent.acp_modes.get(level) {
            if !mode.is_empty() {
                return mode.clone();
            }
        }
    }
    let mode = match agent.real_tool.as_str() {
        "codex" => match level {
            "readonly" => "read-only",
            "workspace" => "workspace-write",
            "full" => "agent-full-access",
            _ => "",
        },
        "claude" | "workbuddy" => match level {
            "readonly" => "plan",
            "workspace" => "default",
            "full" => "bypassPermissions",
            _ => "",
        },
        _ => "",
    };
    mode.to_string()
}

fn wanted_mode(agent: &Agent, permission: &str) -> String {
    if let Some(mode) = agent.acp_modes.get(permission) {
        return mode.clone();
    }
    let mode = match agent.real_tool.as_str() {
        "codex" => match permission {
            "readonly" => "read-only",
            "workspace" => "workspace-write",
            "shell" => "read-only",
            _ => "",
        },
        "claude" => match permission {
            "readonly" => "plan",
            "workspace" => "default",
            "shell" => "plan",
            _ => "",
        },
        _ => "",
    };
    mode.to_string()
}

/// Go `applyConfiguration` (runtime.go:1212).
fn apply_configuration(data: &mut RecordData, res: &Value) {
    if res.is_null() {
        return;
    }
    if let Some(modes) = res.get("modes").filter(|m| m.is_object()) {
        if let Ok(m) = serde_json::from_value::<ModesInfo>(modes.clone()) {
            data.modes = Some(m);
        }
    }
    if let Some(models) = res.get("models").filter(|m| m.is_object()) {
        if let Ok(c) = serde_json::from_value::<Catalog>(models.clone()) {
            data.models = Some(c);
        }
    }
    if let Some(opts) = res.get("configOptions").and_then(Value::as_array) {
        let mut clean: Vec<Value> = Vec::new();
        for raw in opts {
            let Some(o) = raw.as_object() else { continue };
            let category = o.get("category").and_then(Value::as_str).unwrap_or("");
            let id = o.get("id").and_then(Value::as_str).unwrap_or("");
            if matches!(category, "model" | "mode" | "thought_level")
                || matches!(id, "model" | "mode" | "reasoning_effort")
            {
                clean.push(raw.clone());
            }
        }
        data.config_options = clean;
        for o in &data.config_options {
            let category = o.get("category").and_then(Value::as_str).unwrap_or("");
            let id = o.get("id").and_then(Value::as_str).unwrap_or("");
            if category == "model" || id == "model" {
                if let Some(cur) = o.get("currentValue").and_then(Value::as_str) {
                    let models = data.models.get_or_insert_with(Catalog::default);
                    models.current_model_id = cur.to_string();
                }
            }
        }
    }
}

// ----------------------------- wire (de)serialization -----------------------------

/// Go's wire shape of a turn: `tools` is always an array (runtime.go:151).
fn turn_to_wire(turn: &Turn) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), json!(turn.id));
    map.insert("stage".into(), json!(turn.stage));
    map.insert("structured".into(), json!(turn.structured));
    map.insert("output".into(), json!(turn.output));
    map.insert("tools".into(), json!(turn.tools));
    if !turn.plan.is_empty() {
        map.insert("plan".into(), json!(turn.plan));
    }
    map.insert("startedAt".into(), json!(turn.started_at));
    if turn.finished_at != 0 {
        map.insert("finishedAt".into(), json!(turn.finished_at));
    }
    map.insert("status".into(), json!(turn.status));
    if !turn.stop_reason.is_empty() {
        map.insert("stopReason".into(), json!(turn.stop_reason));
    }
    if !turn.error.is_empty() {
        map.insert("error".into(), json!(turn.error));
    }
    if turn.received_chars != 0 {
        map.insert("receivedChars".into(), json!(turn.received_chars));
    }
    if !turn.usage.is_null() {
        map.insert("usage".into(), turn.usage.clone());
    }
    Value::Object(map)
}

fn catalog_to_wire(c: &Catalog) -> Value {
    let mut map = Map::new();
    map.insert("currentModelId".into(), json!(c.current_model_id));
    if let Some(models) = &c.available_models {
        map.insert("availableModels".into(), json!(models));
    }
    Value::Object(map)
}

fn modes_to_wire(m: &ModesInfo) -> Value {
    let mut map = Map::new();
    map.insert("currentModeId".into(), json!(m.current_mode_id));
    if let Some(modes) = &m.available_modes {
        map.insert("availableModes".into(), json!(modes));
    }
    Value::Object(map)
}

/// Go `SessionRecord` JSON field order with omitempty semantics.
fn record_to_wire(rec: &SessionRecord) -> Value {
    let data = rec.data.lock().unwrap();
    let mut map = Map::new();
    map.insert("id".into(), json!(rec.id));
    map.insert("key".into(), json!(rec.key));
    map.insert("agentId".into(), json!(rec.agent_id));
    if !data.chat_id.is_empty() {
        map.insert("chatId".into(), json!(data.chat_id));
    }
    if !data.mission_id.is_empty() {
        map.insert("missionId".into(), json!(data.mission_id));
    }
    if !data.task_id.is_empty() {
        map.insert("taskId".into(), json!(data.task_id));
    }
    if !data.role.is_empty() {
        map.insert("role".into(), json!(data.role));
    }
    map.insert("permission".into(), json!(data.permission));
    if !data.native_mode.is_empty() {
        map.insert("nativeMode".into(), json!(data.native_mode));
    }
    map.insert("cwd".into(), json!(rec.cwd));
    map.insert("state".into(), json!(data.state));
    if !data.native_session_id.is_empty() {
        map.insert("nativeSessionId".into(), json!(data.native_session_id));
    }
    map.insert("turns".into(), Value::Array(data.turns.iter().map(turn_to_wire).collect()));
    map.insert(
        "permissions".into(),
        Value::Array(
            data.permissions
                .iter()
                .map(|card| {
                    serde_json::to_value(&*card.lock().unwrap()).unwrap_or(Value::Null)
                })
                .collect(),
        ),
    );
    if let Some(models) = &data.models {
        map.insert("models".into(), catalog_to_wire(models));
    }
    if let Some(modes) = &data.modes {
        map.insert("modes".into(), modes_to_wire(modes));
    }
    if !data.config_options.is_empty() {
        map.insert("configOptions".into(), json!(data.config_options));
    }
    if !data.notices.is_empty() {
        map.insert("notices".into(), json!(data.notices));
    }
    if !data.default_model.is_empty() {
        map.insert("defaultModel".into(), json!(data.default_model));
    }
    if !data.error.is_empty() {
        map.insert("error".into(), json!(data.error));
    }
    map.insert("createdAt".into(), json!(data.created_at));
    if data.handed_off {
        map.insert("handedOff".into(), json!(true));
        if !data.handed_off_to.is_empty() {
            map.insert("handedOffTo".into(), json!(data.handed_off_to));
        }
    }
    Value::Object(map)
}

fn wire_to_record(raw: &Value) -> Arc<SessionRecord> {
    let get_str = |key: &str| {
        raw.get(key).and_then(Value::as_str).unwrap_or("").to_string()
    };
    let permissions = raw
        .get("permissions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|card| {
                    Arc::new(Mutex::new(
                        serde_json::from_value::<PermissionCard>(card.clone())
                            .unwrap_or_default(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Arc::new(SessionRecord {
        id: get_str("id"),
        key: get_str("key"),
        agent_id: get_str("agentId"),
        cwd: get_str("cwd"),
        turn_lock: AsyncMutex::new(()),
        data: Mutex::new(RecordData {
            chat_id: get_str("chatId"),
            mission_id: get_str("missionId"),
            task_id: get_str("taskId"),
            role: get_str("role"),
            permission: get_str("permission"),
            native_mode: get_str("nativeMode"),
            state: get_str("state"),
            native_session_id: get_str("nativeSessionId"),
            turns: raw
                .get("turns")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|t| serde_json::from_value::<Turn>(t.clone()).ok())
                        .collect()
                })
                .unwrap_or_default(),
            permissions,
            models: raw
                .get("models")
                .filter(|m| m.is_object())
                .and_then(|m| serde_json::from_value(m.clone()).ok()),
            modes: raw
                .get("modes")
                .filter(|m| m.is_object())
                .and_then(|m| serde_json::from_value(m.clone()).ok()),
            config_options: raw
                .get("configOptions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            notices: raw
                .get("notices")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            default_model: get_str("defaultModel"),
            error: get_str("error"),
            created_at: raw.get("createdAt").and_then(Value::as_i64).unwrap_or(0),
            handed_off: raw.get("handedOff").and_then(Value::as_bool).unwrap_or(false),
            handed_off_to: get_str("handedOffTo"),
            active: None,
        }),
    })
}

// ----------------------------- driver -----------------------------

#[derive(serde::Deserialize)]
struct RpcErrorWire {
    #[serde(default)]
    message: String,
}

#[derive(serde::Deserialize)]
struct RpcMsg {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    method: String,
    #[serde(default)]
    params: Value,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<RpcErrorWire>,
}

struct Approval {
    card: Arc<Mutex<PermissionCard>>,
    record: Arc<SessionRecord>,
    reply: tokio::sync::mpsc::UnboundedSender<Value>,
}

pub struct Driver {
    stdin: AsyncMutex<tokio::process::ChildStdin>,
    pending: Mutex<HashMap<i64, oneshot::Sender<RpcMsg>>>,
    next_id: AtomicI64,
    sessions: Mutex<Vec<Arc<SessionRecord>>>,
    pub info: Mutex<Value>,
    alive: Mutex<bool>,
    closed_rx: watch::Receiver<bool>,
    closed_tx: watch::Sender<bool>,
    stopped_tx: watch::Sender<bool>,
    stopped_rx: watch::Receiver<bool>,
    /// Kill requests: the watcher owns the child process and honours these
    /// (Go calls `cmd.Process.Kill()` directly; taking the child for `wait()`
    /// would leave `kill()` a permanent no-op).
    kill_tx: watch::Sender<bool>,
    stderr_tail: Mutex<String>,
    pub perm: String,
    pub cwd: String,
    pub agent_id: String,
}

impl Driver {
    async fn send(&self, msg: &Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&line).await.map_err(|e| e.to_string())
    }

    /// Go `call` (runtime.go:227). `timeout_ms <= 0` waits on the caller's
    /// clock (session/prompt) or the driver's exit.
    async fn call(&self, method: &str, params: &Value, timeout_ms: i64) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel::<RpcMsg>();
        self.pending.lock().unwrap().insert(id, tx);
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(err) = self.send(&request).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(err);
        }
        let wait = async {
            match rx.await {
                Ok(msg) => match msg.error {
                    Some(err) => Err(err.message),
                    None => Ok(msg.result),
                },
                Err(_) => Err("ACP 进程已退出".to_string()),
            }
        };
        if timeout_ms <= 0 {
            let mut closed = self.closed_rx.clone();
            tokio::select! {
                outcome = wait => outcome,
                _ = closed.changed() => Err("ACP 进程已退出".to_string()),
            }
        } else {
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms as u64), wait)
                .await
            {
                Ok(outcome) => outcome,
                Err(_) => {
                    self.pending.lock().unwrap().remove(&id);
                    Err(format!("{method} 调用超时"))
                }
            }
        }
    }

    async fn notify(&self, method: &str, params: &Value) -> Result<(), String> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    async fn reply(&self, id: i64, result: Value, err: Option<String>) {
        let mut msg = json!({"jsonrpc": "2.0", "id": id});
        if let Some(err) = err {
            msg["error"] = json!({"code": -32603, "message": err});
        } else {
            msg["result"] = result;
        }
        let _ = self.send(&msg).await;
    }

    fn append_stderr(&self, chunk: &str) {
        let mut tail = self.stderr_tail.lock().unwrap();
        tail.push_str(chunk);
        let chars: Vec<char> = tail.chars().collect();
        if chars.len() > 8000 {
            let keep: String = chars[chars.len() - 8000..].iter().collect();
            *tail = keep;
        }
    }

    fn stderr_tail(&self) -> String {
        let tail = self.stderr_tail.lock().unwrap().clone();
        let chars: Vec<char> = tail.chars().collect();
        let start = chars.len().saturating_sub(400);
        chars[start..].iter().collect()
    }

    fn kill(&self) {
        let _ = self.kill_tx.send(true);
    }

    fn is_alive(&self) -> bool {
        *self.alive.lock().unwrap()
    }

    fn record_for_native(&self, native_id: &str) -> Option<Arc<SessionRecord>> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|rec| rec.data.lock().unwrap().native_session_id == native_id)
            .cloned()
    }

    fn add_session(&self, rec: &Arc<SessionRecord>) {
        let mut sessions = self.sessions.lock().unwrap();
        if !sessions.iter().any(|s| s.id == rec.id) {
            sessions.push(rec.clone());
        }
    }

    fn remove_session(&self, record_id: &str) {
        self.sessions.lock().unwrap().retain(|s| s.id != record_id);
    }
}

// ----------------------------- runtime -----------------------------

pub struct RuntimeOptions {
    pub state_file: Option<PathBuf>,
    pub timeout_ms: i64,
    pub resolver: Arc<dyn LaunchResolver>,
    pub emitter: Arc<dyn Emitter>,
    pub hooks: Arc<dyn RuntimeHooks>,
}

struct NoopResolver;
impl LaunchResolver for NoopResolver {
    fn launch(&self, _agent: &Agent, _permission: &str, _cwd: &str) -> Result<Launch, String> {
        Err("未配置适配器启动方式".into())
    }
    fn version(&self, _agent: &Agent) -> String {
        String::new()
    }
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        RuntimeOptions {
            state_file: None,
            timeout_ms: 600_000,
            resolver: Arc::new(NoopResolver),
            emitter: Arc::new(NoopEmitter),
            hooks: Arc::new(NoopHooks),
        }
    }
}

#[derive(Default)]
struct Inner {
    drivers: HashMap<String, Arc<Driver>>,
    records: Vec<Arc<SessionRecord>>,
    bindings: HashMap<String, Arc<Driver>>,
    approvals: HashMap<String, Approval>,
}

pub struct Runtime {
    options: RuntimeOptions,
    inner: Mutex<Inner>,
    driver_start_lock: AsyncMutex<()>,
    self_weak: OnceLock<Weak<Runtime>>,
}

impl Runtime {
    /// Go `NewRuntime` (runtime.go:311): constructs and loads the state file.
    pub fn new(options: RuntimeOptions) -> Arc<Runtime> {
        let rt = Arc::new(Runtime {
            options,
            inner: Mutex::new(Inner::default()),
            driver_start_lock: AsyncMutex::new(()),
            self_weak: OnceLock::new(),
        });
        let _ = rt.self_weak.set(Arc::downgrade(&rt));
        rt.load();
        rt
    }

    fn self_arc(&self) -> Arc<Runtime> {
        self.self_weak
            .get()
            .expect("runtime self reference")
            .upgrade()
            .expect("runtime alive")
    }

    #[cfg(test)]
    fn test_inject_record(&self, rec: Arc<SessionRecord>) {
        self.inner.lock().unwrap().records.push(rec);
    }

    fn load(&self) {
        let Some(path) = self.options.state_file.as_ref() else { return };
        let Ok(data) = std::fs::read(path) else { return };
        let Ok(saved) = serde_json::from_slice::<Vec<Value>>(&data) else { return };
        let mut inner = self.inner.lock().unwrap();
        for raw in saved {
            let rec = wire_to_record(&raw);
            {
                let mut data = rec.data.lock().unwrap();
                data.state = "disconnected".into();
                data.permissions.clear();
            }
            inner.records.push(rec);
        }
    }

    /// Go `save` (runtime.go:337): keep the latest 100 records, tmp+rename.
    fn save(&self) {
        let Some(path) = self.options.state_file.as_ref() else { return };
        let records = self.records("");
        let start = records.len().saturating_sub(100);
        let snapshots: Vec<Value> = records[start..]
            .iter()
            .map(|rec| record_to_wire(rec))
            .collect();
        let body = serde_json::to_vec(&snapshots).unwrap_or_default();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &body).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    fn emit_changed(&self, rec: &SessionRecord) {
        let payload = {
            let data = rec.data.lock().unwrap();
            json!({
                "id": rec.id,
                "chatId": data.chat_id,
                "agentId": rec.agent_id,
                "state": data.state,
            })
        };
        self.options
            .emitter
            .emit("session", &goformat::canon(&payload));
    }

    fn set_state(&self, rec: &Arc<SessionRecord>, state: &str, message: &str) {
        {
            let mut data = rec.data.lock().unwrap();
            data.state = state.into();
            data.error = message.into();
        }
        self.emit_changed(rec);
    }

    /// Go `Records` (runtime.go:383), optionally filtered by chat.
    pub fn records(&self, chat_id: &str) -> Vec<Arc<SessionRecord>> {
        let inner = self.inner.lock().unwrap();
        inner
            .records
            .iter()
            .filter(|rec| chat_id.is_empty() || rec.data.lock().unwrap().chat_id == chat_id)
            .cloned()
            .collect()
    }

    fn record(&self, id: &str) -> Option<Arc<SessionRecord>> {
        let inner = self.inner.lock().unwrap();
        inner.records.iter().find(|rec| rec.id == id).cloned()
    }

    fn binding(&self, id: &str) -> Option<Arc<Driver>> {
        let inner = self.inner.lock().unwrap();
        inner.bindings.get(id).cloned()
    }

    // ----------------------------- driver lifecycle -----------------------------

    /// Go `driverKey` (runtime.go:406) — the version suffix is always present
    /// because wiring installs LaunchVersion unconditionally.
    fn driver_key(&self, agent: &Agent, permission: &str, cwd: &str) -> String {
        format!("{}|{}|{}|{}", agent.id, permission, cwd, self.options.resolver.version(agent))
    }

    async fn driver(&self, agent: &Agent, permission: &str, cwd: &str) -> Result<Arc<Driver>, String> {
        let _guard = self.driver_start_lock.lock().await;
        let key = self.driver_key(agent, permission, cwd);
        {
            let inner = self.inner.lock().unwrap();
            if let Some(d) = inner.drivers.get(&key) {
                if d.is_alive() {
                    return Ok(d.clone());
                }
            }
        }
        let d = self.start_driver(agent, permission, cwd).await?;
        self.inner.lock().unwrap().drivers.insert(key, d.clone());
        Ok(d)
    }

    async fn start_driver(&self, agent: &Agent, permission: &str, cwd: &str) -> Result<Arc<Driver>, String> {
        let launch = self.options.resolver.launch(agent, permission, cwd)?;
        if launch.command.is_empty() {
            return Err("尚未安装此 Harness 的 ACP 适配器".into());
        }
        let mut cmd = Command::new(&launch.command);
        cmd.args(&launch.args).current_dir(cwd);
        if let Some(env) = &launch.env {
            cmd.env_clear();
            for item in env {
                if let Some((k, v)) = item.split_once('=') {
                    cmd.env(k, v);
                }
            }
        }
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("适配器进程启动失败：{e}"))?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let (closed_tx, closed_rx) = watch::channel(false);
        let (stopped_tx, stopped_rx) = watch::channel(false);
        let (kill_tx, kill_rx) = watch::channel(false);
        let driver = Arc::new(Driver {
            stdin: AsyncMutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(0),
            sessions: Mutex::new(Vec::new()),
            info: Mutex::new(Value::Null),
            alive: Mutex::new(true),
            closed_rx: closed_rx.clone(),
            closed_tx,
            stopped_tx,
            stopped_rx: stopped_rx.clone(),
            kill_tx,
            stderr_tail: Mutex::new(String::new()),
            perm: permission.to_string(),
            cwd: cwd.to_string(),
            agent_id: agent.id.clone(),
        });

        // stderr tail for error messages (Go runtime.go:459)
        tokio::spawn({
            let driver = driver.clone();
            async move {
                use tokio::io::AsyncReadExt;
                let mut stderr = stderr;
                let mut buf = vec![0u8; 8192];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => driver.append_stderr(&String::from_utf8_lossy(&buf[..n])),
                    }
                }
            }
        });

        // exit watcher (Go runtime.go:479): owns the child process, honours
        // kill requests, and disconnects bound records on exit.
        let rt_weak = Arc::downgrade(&self.self_arc());
        tokio::spawn({
            let driver = driver.clone();
            let rt_weak = rt_weak.clone();
            let mut kill_rx = kill_rx.clone();
            async move {
                let mut child = child;
                loop {
                    tokio::select! {
                        st = child.wait() => {
                            break;
                        }
                        _ = kill_rx.changed() => {
                            if *kill_rx.borrow() {
                                let _ = child.start_kill();
                            }
                        }
                    }
                }
                let _ = driver.closed_tx.send(true);
                *driver.alive.lock().unwrap() = false;
                let Some(rt) = rt_weak.upgrade() else {
                    let _ = driver.stopped_tx.send(true);
                    return;
                };
                let bound: Vec<Arc<SessionRecord>> = {
                    let guard = driver.sessions.lock().unwrap();
                    guard.clone()
                };
                for rec in bound {
                    rt.cancel_permissions(&rec.id);
                    {
                        let mut data = rec.data.lock().unwrap();
                        data.state = "disconnected".into();
                        data.active = None;
                    }
                    rt.inner.lock().unwrap().bindings.remove(&rec.id);
                    rt.emit_changed(&rec);
                }
                rt.save();
                let _ = driver.stopped_tx.send(true);
            }
        });

        // serve loop (Go runtime.go:547): notifications apply in wire order
        // under the record lock; requests stay detached so a human permission
        // decision cannot stall the RPC loop.
        tokio::spawn({
            let driver = driver.clone();
            let rt_weak = rt_weak.clone();
            async move {
                let mut lines = BufReaderLines::new(stdout);
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if line.trim().is_empty() {
                                continue;
                            }
                            let Ok(msg) = serde_json::from_str::<RpcMsg>(&line) else {
                                continue;
                            };
                            match (msg.id, msg.method.is_empty()) {
                                (Some(id), false) => {
                                    let driver = driver.clone();
                                    let rt = rt_weak.clone();
                                    tokio::spawn(async move {
                                        if let Some(rt) = rt.upgrade() {
                                            handle_request(&rt, &driver, id, &msg.method, &msg.params)
                                                .await;
                                        }
                                    });
                                }
                                (Some(id), true) => {
                                    let sender = driver.pending.lock().unwrap().remove(&id);
                                    if let Some(sender) = sender {
                                        let _ = sender.send(RpcMsg {
                                            id: msg.id,
                                            method: String::new(),
                                            params: msg.params,
                                            result: msg.result,
                                            error: msg.error,
                                        });
                                    }
                                }
                                (None, _) => {
                                    if let Some(rt) = rt_weak.upgrade() {
                                        rt.handle_notification(&driver, &msg.params).await;
                                    }
                                }
                            }
                        }
                        Ok(None) | Err(_) => {
                            break;
                        }
                    }
                }
            }
        });

        // initialize handshake (Go runtime.go:508): cold Claude drivers can
        // take well over 30s; a dead process still errors immediately. The
        // yield gives the stderr/serve/watcher tasks their first polls before
        // the handshake — on a current-thread executor they would otherwise
        // only run after the call's first await point, which races with a
        // fast-spawning adapter (heisenbug seen in the fixture tests).
        tokio::task::yield_now().await;
        let params = json!({
            "protocolVersion": 1,
            "clientInfo": {"name": "harness-hub-go", "version": "2.0.0"},
            "clientCapabilities": {"fs": {
                "readTextFile": true,
                "writeTextFile": permission == "workspace",
            }, "terminal": false, "session": {"notices": {}}},
        });
        let result = match driver.call("initialize", &params, 90_000).await {
            Ok(result) => {
                result
            }
            Err(err) => {
                driver.kill();
                // give the stderr tail a moment to drain (Go waits ≤1s)
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                let tail = driver.stderr_tail().trim().to_string();
                return Err(if tail.is_empty() {
                    format!("ACP 初始化失败：{err}")
                } else {
                    format!("ACP 初始化失败：{err}: {tail}")
                });
            }
        };
        *driver.info.lock().unwrap() = result.clone();
        let version = result.get("protocolVersion").and_then(Value::as_f64).unwrap_or(0.0);
        if version != 1.0 {
            driver.kill();
            return Err("ACP 协议版本不兼容".into());
        }
        Ok(driver)
    }

    // ----------------------------- fs handlers -----------------------------

    fn fs_read(&self, driver: &Driver, p: &Value) -> Result<Value, String> {
        let path = p.get("path").and_then(Value::as_str).unwrap_or("");
        let file = within_dir(Path::new(&driver.cwd), &Path::new(&driver.cwd).join(path))?;
        let meta = std::fs::metadata(&file).map_err(|e| e.to_string())?;
        if meta.len() > 1024 * 1024 {
            return Err("文件超过 1 MiB，请使用分段读取".into());
        }
        let data = std::fs::read(&file).map_err(|e| e.to_string())?;
        let body = String::from_utf8_lossy(&data);
        let lines = split_lines(&body);
        let mut line: i64 = 1;
        if let Some(v) = p.get("line").and_then(Value::as_f64) {
            if v as i64 > 1 {
                line = v as i64;
            }
        }
        let mut limit: i64 = 0;
        if let Some(v) = p.get("limit").and_then(Value::as_f64) {
            if v > 0.0 {
                limit = v as i64;
            }
        }
        let start = (line - 1).max(0) as usize;
        let mut end = lines.len();
        if limit > 0 && start + (limit as usize) < end {
            end = start + limit as usize;
        }
        let start = start.min(end);
        Ok(json!({"content": lines[start..end].join("\n")}))
    }

    fn fs_write(&self, driver: &Driver, p: &Value) -> Result<Value, String> {
        let mut allowed = driver.perm == "workspace";
        let native_id = p.get("sessionId").and_then(Value::as_str).unwrap_or("");
        let record = driver.record_for_native(native_id);
        if let Some(rec) = &record {
            let data = rec.data.lock().unwrap();
            allowed = SessionRecord::can_write(&data);
        }
        if !allowed {
            return Err("此会话未授予文件写入权限".into());
        }
        let path = p.get("path").and_then(Value::as_str).unwrap_or("");
        let content = p.get("content").and_then(Value::as_str).unwrap_or("");
        if content.len() > 1024 * 1024 {
            return Err("写入内容超过 1 MiB".into());
        }
        let target = Path::new(&driver.cwd).join(path);
        let check = if target.exists() {
            target.clone()
        } else {
            target.parent().map(Path::to_path_buf).unwrap_or_else(|| target.clone())
        };
        within_dir(Path::new(&driver.cwd), &check)?;
        std::fs::write(&target, content.as_bytes()).map_err(|e| e.to_string())?;
        Ok(json!({}))
    }

    // ----------------------------- permission cards -----------------------------

    /// Go `requestPermission` (runtime.go:751).
    async fn request_permission(&self, driver: &Driver, p: &Value) -> Result<Value, String> {
        let native_id = p.get("sessionId").and_then(Value::as_str).unwrap_or("");
        let Some(rec) = driver.record_for_native(native_id) else {
            return Ok(json!({"outcome": {"outcome": "cancelled"}}));
        };
        let has_active = rec.data.lock().unwrap().active.is_some();
        if !has_active {
            return Ok(json!({"outcome": {"outcome": "cancelled"}}));
        }
        let mut title = "Harness 请求权限".to_string();
        let tool_call = p.get("toolCall").cloned().unwrap_or(Value::Null);
        if let Some(t) = tool_call.get("title").and_then(Value::as_str) {
            if !t.is_empty() {
                title = t.to_string();
            }
        }
        let options = to_map_slice(p.get("options"));
        let card = Arc::new(Mutex::new(PermissionCard {
            id: format!("permission_{}", short_id()),
            session_id: rec.id.clone(),
            native_id: native_id.to_string(),
            title,
            tool_call: if tool_call.is_null() { Value::Null } else { tool_call.clone() },
            options: options.clone(),
            state: "pending".into(),
            at: unix_millis(),
            ..PermissionCard::default()
        }));
        // The workbench's own collaboration tools are pre-approved once.
        if self
            .options
            .hooks
            .allow_internal_tool(&rec.id, &card.lock().unwrap().tool_call.clone())
        {
            for option in &options {
                if option.get("kind").and_then(Value::as_str) == Some("allow_once") {
                    let choice = option
                        .get("optionId")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    {
                        let mut card_state = card.lock().unwrap();
                        card_state.state = "answered".into();
                        card_state.selected = Some(choice.clone());
                        card_state.reason = "工作台内置协作工具，授权一次".into();
                    }
                    rec.data.lock().unwrap().permissions.push(card.clone());
                    self.emit_changed(&rec);
                    return Ok(json!({"outcome": {"outcome": "selected", "optionId": choice}}));
                }
            }
        }
        let cancelled = json!({"outcome": {"outcome": "cancelled"}});
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let denied = {
            let mut data = rec.data.lock().unwrap();
            data.permissions.push(card.clone());
            if !SessionRecord::can_write(&data) {
                let mut card_state = card.lock().unwrap();
                card_state.state = "denied".into();
                card_state.reason =
                    "当前为只读会话，权限提升已拒绝。需要写入时请选择目录内读写权限启动新任务。".into();
                true
            } else {
                data.state = "requires_action".into();
                false
            }
        };
        self.emit_changed(&rec);
        if denied {
            return Ok(cancelled);
        }
        self.save();
        let card_id = card.lock().unwrap().id.clone();
        self.inner.lock().unwrap().approvals.insert(
            card_id.clone(),
            Approval { card: card.clone(), record: rec.clone(), reply: reply_tx },
        );
        let mut closed = driver.closed_rx.clone();
        let outcome = tokio::select! {
            outcome = reply_rx.recv() => {
                outcome.unwrap_or_else(|| json!({"outcome": {"outcome": "cancelled"}}))
            }
            _ = async {
                loop {
                    match closed.changed().await {
                        Ok(_) if *closed.borrow() => break,
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
            } => json!({"outcome": {"outcome": "cancelled"}}),
        };
        self.inner.lock().unwrap().approvals.remove(&card_id);
        Ok(outcome)
    }

    /// Go `RespondPermission` (runtime.go:823): answers a pending card; a None
    /// option cancels. Sync because nothing here awaits.
    pub fn respond_permission(&self, id: &str, option_id: Option<&str>) -> Result<(), String> {
        let (card, rec, reply) = {
            let inner = self.inner.lock().unwrap();
            match inner.approvals.get(id) {
                Some(app) => (app.card.clone(), app.record.clone(), app.reply.clone()),
                None => return Err("该权限请求已失效".into()),
            }
        };
        {
            let card_state = card.lock().unwrap();
            if card_state.state != "pending" {
                return Err("该权限请求已失效".into());
            }
            if let Some(choice) = option_id {
                let valid = card_state
                    .options
                    .iter()
                    .any(|o| o.get("optionId").and_then(Value::as_str) == Some(choice));
                if !valid {
                    return Err("无效的权限选项".into());
                }
            }
        }
        self.inner.lock().unwrap().approvals.remove(id);
        {
            let mut card_state = card.lock().unwrap();
            match option_id {
                None => {
                    card_state.state = "cancelled".into();
                    let _ = reply.send(json!({"outcome": {"outcome": "cancelled"}}));
                }
                Some(choice) => {
                    card_state.state = "answered".into();
                    card_state.selected = Some(choice.to_string());
                    let _ = reply.send(json!({
                        "outcome": {"outcome": "selected", "optionId": choice}
                    }));
                }
            }
        }
        {
            let mut data = rec.data.lock().unwrap();
            if data.state != "cancelling" {
                data.state = "running".into();
            }
        }
        self.emit_changed(&rec);
        self.save();
        Ok(())
    }

    fn cancel_permissions(&self, session_id: &str) {
        let ids: Vec<String> = {
            let inner = self.inner.lock().unwrap();
            inner
                .approvals
                .iter()
                .filter(|(_, app)| app.record.id == session_id)
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            let _ = self.respond_permission(&id, None);
        }
    }

    fn pending_count(&self, rec_id: &str) -> usize {
        let inner = self.inner.lock().unwrap();
        inner
            .approvals
            .values()
            .filter(|app| app.record.id == rec_id)
            .count()
    }

    // ----------------------------- updates -----------------------------

    async fn handle_notification(&self, driver: &Driver, params: &Value) {
        let session_id = params.get("sessionId").and_then(Value::as_str).unwrap_or("");
        let update = params.get("update").cloned().unwrap_or(Value::Null);
        if update.is_null() {
            return;
        }
        let Some(rec) = driver.record_for_native(session_id) else { return };
        self.apply_update(&rec, &update).await;
    }

    /// Go `applyUpdate` (runtime.go:631). The record guard is released before
    /// awaiting the turn lock — a std guard must never live across an await
    /// (and on a single-thread executor it would deadlock the clock task).
    async fn apply_update(&self, rec: &Arc<SessionRecord>, u: &Value) {
        let kind = u.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
        let active: Option<Arc<AsyncMutex<Turn>>> = rec.data.lock().unwrap().active.clone();
        if let Some(active) = &active {
            active.lock().await.last_activity_millis = unix_millis();
        }
        match kind {
            "agent_message_chunk" => {
                if let Some(active) = &active {
                    let text = u
                        .get("content")
                        .and_then(|c| c.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !text.is_empty() {
                        let mut turn = active.lock().await;
                        let next = format!("{}{}", turn.output, text);
                        turn.output = truncate_bytes(&next, 200_000);
                        turn.received_chars += text.len() as i64;
                    }
                }
            }
            "tool_call" | "tool_call_update" => {
                if let Some(active) = &active {
                    let id = u.get("toolCallId").and_then(Value::as_str).unwrap_or("");
                    let mut turn = active.lock().await;
                    let mut found = false;
                    for tool in turn.tools.iter_mut() {
                        if tool.get("toolCallId").and_then(Value::as_str) == Some(id) {
                            let mut merged: Map<String, Value> =
                                tool.as_object().cloned().unwrap_or_default();
                            if let Some(u_obj) = u.as_object() {
                                for (k, v) in u_obj {
                                    merged.insert(k.clone(), v.clone());
                                }
                            }
                            let merged = Value::Object(merged);
                            if serde_json::to_vec(&merged).map(|b| b.len()).unwrap_or(0) > 40_000 {
                                if let Some(obj) = merged.as_object() {
                                    let mut m = obj.clone();
                                    m.insert(
                                        "content".into(),
                                        json!([{"type": "content", "content": {
                                            "type": "text",
                                            "text": "此工具输出较大，请查看 Harness 原生记录。"
                                        }}]),
                                    );
                                    *tool = Value::Object(m);
                                }
                            } else {
                                *tool = merged;
                            }
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        let row_bytes = serde_json::to_vec(u).map(|b| b.len()).unwrap_or(0);
                        if row_bytes <= 40_000 {
                            turn.tools.push(u.clone());
                        }
                        if turn.tools.len() > 40 {
                            let overflow = turn.tools.len() - 40;
                            turn.tools.drain(0..overflow);
                        }
                    }
                }
            }
            "plan" => {
                if let Some(active) = &active {
                    if u.get("entries").and_then(Value::as_array).is_some() {
                        let entries = to_map_slice(u.get("entries"));
                        let mut turn = active.lock().await;
                        turn.plan = entries;
                    }
                }
            }
            "notice" => {
                let mut data = rec.data.lock().unwrap();
                data.notices.push(u.clone());
                if data.notices.len() > 5 {
                    let overflow = data.notices.len() - 5;
                    data.notices.drain(0..overflow);
                }
            }
            "config_option_update" => {
                let mut data = rec.data.lock().unwrap();
                apply_configuration(&mut data, &json!({"configOptions": u.get("configOptions")}));
                let mut new_mode: Option<String> = None;
                for option in &data.config_options {
                    if option.get("id").and_then(Value::as_str) == Some("mode") {
                        new_mode = option
                            .get("currentValue")
                            .and_then(Value::as_str)
                            .map(String::from);
                    }
                }
                if let Some(mode) = new_mode {
                    data.native_mode = mode;
                }
            }
            "current_mode_update" => {
                let mut data = rec.data.lock().unwrap();
                data.native_mode = u
                    .get("currentModeId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mode = data.native_mode.clone();
                if let Some(modes) = &mut data.modes {
                    modes.current_mode_id = mode;
                }
            }
            _ => {}
        }
        self.emit_changed(rec);
        self.options.hooks.on_update(&rec.id, u);
    }

    // ----------------------------- sessions and turns -----------------------------

    /// Go `mcpServers` (runtime.go:1257).
    fn mcp_servers(&self, agent: &Agent, o: &Options) -> Vec<Value> {
        if !o.structured || (agent.real_tool != "codex" && agent.real_tool != "claude") {
            return Vec::new();
        }
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut args = vec![json!("mcp-result")];
        if !o.output_schema.is_null() {
            if let Ok(body) = serde_json::to_string(&o.output_schema) {
                args.push(json!(body));
            }
        }
        vec![json!({
            "name": "harness-result",
            "command": exe,
            "args": args,
            "env": [],
        })]
    }

    /// Go `ensure` (runtime.go:1032).
    async fn ensure(&self, agent: &Agent, o: &Options) -> Result<(Arc<SessionRecord>, Arc<Driver>), String> {
        let mut permission = o.permission.clone();
        if permission.is_empty() {
            permission = "readonly".into();
        }
        if o.workdir.is_empty() {
            return Err("缺少工作目录".into());
        }
        let key = format!("{}|{}|{}", o.options_key(), agent.id, permission);
        let rec = {
            let mut inner = self.inner.lock().unwrap();
            let existing = inner
                .records
                .iter()
                .find(|cand| cand.key == key)
                .cloned();
            existing.unwrap_or_else(|| {
                let rec = Arc::new(SessionRecord {
                    id: format!("session_{}", short_id()),
                    key: key.clone(),
                    agent_id: agent.id.clone(),
                    cwd: o.workdir.clone(),
                    turn_lock: AsyncMutex::new(()),
                    data: Mutex::new(RecordData {
                        chat_id: o.chat_id.clone(),
                        mission_id: o.mission_id.clone(),
                        task_id: o.task_id.clone(),
                        role: o.role.clone(),
                        permission: permission.clone(),
                        state: "connecting".into(),
                        created_at: unix_millis(),
                        ..RecordData::default()
                    }),
                });
                inner.records.push(rec.clone());
                rec
            })
        };
        self.emit_changed(&rec);

        if let Some(bound) = self.binding(&rec.id) {
            if bound.is_alive() {
                return Ok((rec, bound));
            }
        }
        let driver = match self.driver(agent, &permission, &o.workdir).await {
            Ok(d) => {
                d
            }
            Err(err) => {
                self.set_state(&rec, "error", &err);
                return Err(err);
            }
        };
        let _turn_guard = rec.turn_lock.lock().await;
        {
            let data = rec.data.lock().unwrap();
            let state = if data.native_session_id.is_empty() { "connecting" } else { "restoring" };
            drop(data);
            self.set_state(&rec, state, "");
        }
        let mut mcp_servers = self.mcp_servers(agent, o);
        mcp_servers.extend(self.options.hooks.extra_mcp(agent, o, &rec.id));
        // Same cold-start allowance as the initialize handshake above: the
        // first session/new on a fresh driver can exceed 30s.
        let result = {
            let native = rec.data.lock().unwrap().native_session_id.clone();
            if !native.is_empty() {
                let info = driver.info.lock().unwrap().clone();
                let caps = info.get("agentCapabilities").cloned().unwrap_or(Value::Null);
                let load = caps.get("loadSession").and_then(Value::as_bool).unwrap_or(false);
                let session_caps = caps.get("sessionCapabilities").cloned().unwrap_or(Value::Null);
                let resume = session_caps.get("resume").is_some();
                if !load && !resume {
                    return Err("该 Harness 不支持恢复旧会话，请新建对话".into());
                }
                let method = if load { "session/load" } else { "session/resume" };
                driver
                    .call(
                        method,
                        &json!({"sessionId": native, "cwd": o.workdir, "mcpServers": mcp_servers}),
                        90_000,
                    )
                    .await
            } else {
                driver
                    .call("session/new", &json!({"cwd": o.workdir, "mcpServers": mcp_servers}), 90_000)
                    .await
            }
        };
        let new_res = match result {
            Ok(res) => res,
            Err(err) => {
                self.set_state(&rec, "error", &err);
                return Err(err);
            }
        };
        {
            let mut data = rec.data.lock().unwrap();
            if let Some(sid) = new_res.get("sessionId").and_then(Value::as_str) {
                if !sid.is_empty() {
                    data.native_session_id = sid.to_string();
                }
            }
            apply_configuration(&mut data, &new_res);
            if data.default_model.is_empty() {
                if let Some(models) = &data.models {
                    data.default_model = models.current_model_id.clone();
                }
            }
        }
        driver.add_session(&rec);
        let wanted = {
            let data = rec.data.lock().unwrap();
            let mut wanted = data.native_mode.clone();
            if wanted.is_empty() {
                wanted = o.mode.clone();
            }
            if wanted.is_empty() {
                wanted = wanted_mode(agent, &permission);
            }
            wanted
        };
        if !wanted.is_empty() {
            let modes_known = {
                let data = rec.data.lock().unwrap();
                data.modes
                    .as_ref()
                    .map(|m| m.available_modes.as_ref().map(|a| !a.is_empty()).unwrap_or(false))
                    .unwrap_or(false)
            };
            if modes_known {
                let found = rec.data.lock().unwrap().modes.as_ref().unwrap().available_modes.as_ref().unwrap().iter().any(|m| {
                    m.get("id").and_then(Value::as_str) == Some(wanted.as_str())
                });
                if !found {
                    let err = format!("该 Harness 未提供所需权限模式：{permission}");
                    self.set_state(&rec, "error", &err);
                    return Err(err);
                }
            }
            // Hoist the native id: a `rec.data.lock()` temporary inside the
            // json! argument lives until the end of the whole statement —
            // across the `.await` — which deadlocks the exit watcher's
            // bookkeeping on a single-thread executor.
            let native_for_mode = rec.data.lock().unwrap().native_session_id.clone();
            if let Err(err) = driver
                .call(
                    "session/set_mode",
                    &json!({"sessionId": native_for_mode, "modeId": wanted}),
                    15_000,
                )
                .await
            {
                self.set_state(&rec, "error", &err);
                return Err(err);
            }
            let mut data = rec.data.lock().unwrap();
            data.native_mode = wanted.clone();
            if let Some(modes) = &mut data.modes {
                modes.current_mode_id = wanted.clone();
            }
            for option in &mut data.config_options {
                let category = option.get("category").and_then(Value::as_str).unwrap_or("");
                let id = option.get("id").and_then(Value::as_str).unwrap_or("");
                if category == "mode" || id == "mode" {
                    if let Some(obj) = option.as_object_mut() {
                        obj.insert("currentValue".into(), json!(wanted));
                    }
                }
            }
        }
        driver.add_session(&rec);
        self.inner.lock().unwrap().bindings.insert(rec.id.clone(), driver.clone());
        self.set_state(&rec, "idle", "");
        self.save();
        drop(_turn_guard);
        Ok((rec, driver))
    }

    /// Go `Connect` (runtime.go:1272).
    pub async fn connect(&self, agent: &Agent, o: Options) -> Result<String, String> {
        let (rec, _) = self.ensure(agent, &o).await?;
        Ok(rec.id.clone())
    }

    /// Go `HasActiveTurn` (runtime.go:1286).
    pub fn has_active_turn(&self, session_id: &str) -> bool {
        self.record(session_id)
            .map(|rec| rec.data.lock().unwrap().active.is_some())
            .unwrap_or(false)
    }

    /// Go `ForgetSession` (runtime.go:1301).
    pub fn forget_session(&self, session_id: &str) {
        self.forget_fork(session_id);
    }

    /// Go `SupportsFork` (runtime.go:1305).
    pub fn supports_fork(&self, session_id: &str) -> bool {
        let Some(d) = self.binding(session_id) else { return false };
        if !d.is_alive() {
            return false;
        }
        let info = d.info.lock().unwrap().clone();
        let caps = info.get("agentCapabilities").cloned().unwrap_or(Value::Null);
        let sessions_caps = caps.get("sessionCapabilities").cloned().unwrap_or(Value::Null);
        sessions_caps
            .get("fork")
            .map(|f| f.is_object())
            .unwrap_or(false)
    }

    /// Go `PromptCapabilities` (runtime.go:1323).
    pub fn prompt_capabilities(&self, session_id: &str) -> Value {
        let mut out = json!({"image": false, "audio": false, "embedded_context": false});
        let Some(d) = self.binding(session_id) else { return out };
        if !d.is_alive() {
            return out;
        }
        let info = d.info.lock().unwrap().clone();
        let caps = info.get("agentCapabilities").cloned().unwrap_or(Value::Null);
        if let Some(prompt) = caps.get("promptCapabilities") {
            if let Some(image) = prompt.get("image").and_then(Value::as_bool) {
                out["image"] = json!(image);
            }
            if let Some(embedded) = prompt.get("embeddedContext").and_then(Value::as_bool) {
                out["embedded_context"] = json!(embedded);
            }
        }
        out
    }

    /// Go `SupportsNativeSteering` (runtime.go:1345): the extension declaration
    /// plus locally verified adapter implementations only.
    pub fn supports_native_steering(&self, session_id: &str) -> bool {
        let Some(d) = self.binding(session_id) else { return false };
        if !d.is_alive() {
            return false;
        }
        let info = d.info.lock().unwrap().clone();
        let supported = info
            .get("_meta")
            .and_then(|m| m.get("steering"))
            .and_then(|s| s.get("supported"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let name = info
            .get("agentInfo")
            .and_then(|i| i.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let version = info
            .get("agentInfo")
            .and_then(|i| i.get("version"))
            .and_then(Value::as_str)
            .unwrap_or("");
        supported
            && ((name == "@agentclientprotocol/codex-acp" && version == "2.5.0")
                || (name == "@agentclientprotocol/claude-agent-acp" && version == "0.101.0"))
    }

    /// Go `Steer` (runtime.go:1368): never starts a detached turn implicitly.
    pub async fn steer(&self, session_id: &str, blocks: &[Value]) -> Result<(), String> {
        if !self.supports_native_steering(session_id) {
            return Err("native steering unavailable".into());
        }
        let (d, rec) = {
            let inner = self.inner.lock().unwrap();
            let d = inner.bindings.get(session_id).cloned();
            let rec = inner.records.iter().find(|r| r.id == session_id).cloned();
            (d, rec)
        };
        let native = rec
            .as_ref()
            .map(|r| r.data.lock().unwrap().native_session_id.clone())
            .unwrap_or_default();
        match (&d, &rec) {
            (Some(d), Some(_)) if !native.is_empty() => {
                let response = d
                    .call(
                        "_session/steering",
                        &json!({
                            "sessionId": native,
                            "prompt": blocks,
                            "_meta": {"steering": {"idleBehavior": "promptRequired"}},
                        }),
                        20_000,
                    )
                    .await?;
                let outcome = response.get("outcome").and_then(Value::as_str).unwrap_or("");
                match outcome {
                    "promptRequired" => Err("no active turn".into()),
                    "injected" => Ok(()),
                    other => Err(format!("unexpected steering outcome: {other}")),
                }
            }
            _ => Err("no active turn".into()),
        }
    }

    /// Go `SupportsPointFork` (runtime.go:1400).
    pub fn supports_point_fork(&self, session_id: &str, agent: Option<&Agent>) -> bool {
        let Some(agent) = agent else { return false };
        if !self.supports_fork(session_id) {
            return false;
        }
        let Some(d) = self.binding(session_id) else { return false };
        if !d.is_alive() {
            return false;
        }
        let info = d.info.lock().unwrap().clone();
        let name = info
            .get("agentInfo")
            .and_then(|i| i.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let version = info
            .get("agentInfo")
            .and_then(|i| i.get("version"))
            .and_then(Value::as_str)
            .unwrap_or("");
        (agent.real_tool == "codex"
            && name == "@agentclientprotocol/codex-acp"
            && version == "2.5.0")
            || (agent.real_tool == "claude"
                && name == "@agentclientprotocol/claude-agent-acp"
                && version == "0.101.0")
    }

    /// Go `Fork` (runtime.go:1423): a new ACP session at a native message id.
    /// The caller owns the conversation-store transaction.
    pub async fn fork(
        &self,
        agent: &Agent,
        source_id: &str,
        source: Options,
        new_session_key: &str,
        message_id: &str,
        extra_mcp: Option<Vec<Value>>,
    ) -> Result<String, String> {
        if new_session_key.is_empty() || message_id.is_empty() {
            return Err("分支需要执行端、新会话标识和原生消息标识".into());
        }
        if agent.real_tool != "codex" && agent.real_tool != "claude" {
            return Err("该执行端未确认支持按消息位置分支".into());
        }
        let rec = self.record(source_id);
        let d = self.binding(source_id);
        if rec.is_none() || d.is_none() {
            return Err("当前执行端未声明会话分支能力".into());
        }
        let rec = rec.unwrap();
        let d = d.unwrap();
        if rec.agent_id != agent.id || !self.supports_point_fork(source_id, Some(agent)) {
            return Err("当前执行端未声明会话分支能力".into());
        }
        let Ok(_turn_guard) = rec.turn_lock.try_lock() else {
            return Err("turn_in_progress".into());
        };
        {
            let data = rec.data.lock().unwrap();
            if source.workdir != rec.cwd
                || source.permission != data.permission
                || format!("{}|{}|{}", source.options_key(), agent.id, data.permission) != rec.key
            {
                return Err("分支工作目录或权限与原会话不一致".into());
            }
        }
        let dest_options = Options {
            session_key: new_session_key.to_string(),
            ..source.clone()
        };
        let dest_key = format!(
            "{}|{}|{}",
            dest_options.options_key(),
            agent.id,
            rec.data.lock().unwrap().permission
        );
        {
            let inner = self.inner.lock().unwrap();
            if inner.records.iter().any(|r| r.key == dest_key) {
                return Err("分支会话标识已存在".into());
            }
        }
        let mut servers = self.mcp_servers(agent, &source);
        match extra_mcp {
            Some(extra) => servers.extend(extra),
            None => servers.extend(self.options.hooks.extra_mcp(agent, &source, &rec.id)),
        }
        let native = rec.data.lock().unwrap().native_session_id.clone();
        let result = d
            .call(
                "session/fork",
                &json!({
                    "sessionId": native,
                    "cwd": rec.cwd,
                    "mcpServers": servers,
                    "_meta": {"jetbrains": {"air": {"fork": {
                        "version": 1, "messageId": message_id,
                    }}}},
                }),
                30_000,
            )
            .await?;
        let native_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if native_id.is_empty() || native_id == native {
            return Err("分支未返回新的原生会话标识".into());
        }
        // Clone the persisted settings without the source record's locks.
        let fork = {
            let mut wire = record_to_wire(&rec);
            let obj = wire.as_object_mut().unwrap();
            obj.insert("id".into(), json!(format!("session_{}", short_id())));
            obj.insert("key".into(), json!(dest_key));
            obj.insert("nativeSessionId".into(), json!(native_id));
            obj.insert("state".into(), json!("idle"));
            obj.insert("error".into(), json!(""));
            obj.insert("createdAt".into(), json!(unix_millis()));
            obj.insert("turns".into(), json!([]));
            obj.insert("permissions".into(), json!([]));
            obj.insert("notices".into(), json!([]));
            obj.insert("handedOff".into(), json!(false));
            obj.insert("handedOffTo".into(), json!(""));
            let fork = wire_to_record(&wire);
            {
                let mut fork_data = fork.data.lock().unwrap();
                fork_data.native_session_id = native_id.clone();
                fork_data.state = "idle".into();
                fork_data.error.clear();
                fork_data.turns.clear();
                fork_data.permissions.clear();
                fork_data.notices.clear();
                fork_data.handed_off = false;
                fork_data.handed_off_to.clear();
                apply_configuration(&mut fork_data, &result);
            }
            fork
        };
        {
            let mut inner = self.inner.lock().unwrap();
            if inner
                .records
                .iter()
                .any(|r| r.key == dest_key || r.data.lock().unwrap().native_session_id == native_id)
            {
                return Err("分支会话标识发生冲突".into());
            }
            inner.records.push(fork.clone());
            inner.bindings.insert(fork.id.clone(), d.clone());
        }
        d.add_session(&fork);
        self.emit_changed(&fork);
        self.save();
        Ok(fork.id.clone())
    }

    /// Go `ForgetFork` (runtime.go:1517): drops an unpublished local record.
    pub fn forget_fork(&self, session_id: &str) {
        let (rec, driver) = {
            let mut inner = self.inner.lock().unwrap();
            let rec = inner.records.iter().find(|r| r.id == session_id).cloned();
            let driver = inner.bindings.get(session_id).cloned();
            if let Some(rec) = &rec {
                inner.records.retain(|r| r.id != session_id);
            }
            inner.bindings.remove(session_id);
            (rec, driver)
        };
        if let (Some(rec), Some(driver)) = (rec, driver) {
            driver.remove_session(&rec.id);
        }
        self.save();
    }

    /// Go `Configure` (runtime.go:1538).
    pub async fn configure(&self, session_id: &str, config_id: &str, value: &Value) -> Result<(), String> {
        let rec = self.record(session_id);
        let d = self.binding(session_id);
        let (Some(rec), Some(d)) = (rec, d) else {
            return Err("原生会话未连接".into());
        };
        let Ok(_turn_guard) = rec.turn_lock.try_lock() else {
            return Err("请等待当前调用结束后切换模型".into());
        };
        let native = rec.data.lock().unwrap().native_session_id.clone();
        let result = d
            .call(
                "session/set_config_option",
                &json!({"sessionId": native, "configId": config_id, "value": value}),
                20_000,
            )
            .await?;
        {
            let mut data = rec.data.lock().unwrap();
            apply_configuration(&mut data, &result);
            if config_id == "mode" {
                if let Some(mode) = value.as_str() {
                    data.native_mode = mode.to_string();
                    if let Some(modes) = &mut data.modes {
                        modes.current_mode_id = mode.to_string();
                    }
                }
            }
        }
        self.emit_changed(&rec);
        self.save();
        Ok(())
    }

    /// Go `SetMode` (runtime.go:1576): mirrors Configure's turn guard.
    pub async fn set_mode(&self, session_id: &str, mode_id: &str) -> Result<(), String> {
        let rec = self.record(session_id);
        let d = self.binding(session_id);
        let (Some(rec), Some(d)) = (rec, d) else {
            return Err("原生会话未连接".into());
        };
        let Ok(_turn_guard) = rec.turn_lock.try_lock() else {
            return Err("请等待当前调用结束后切换模式".into());
        };
        let native = rec.data.lock().unwrap().native_session_id.clone();
        d.call(
            "session/set_mode",
            &json!({"sessionId": native, "modeId": mode_id}),
            20_000,
        )
        .await?;
        {
            let mut data = rec.data.lock().unwrap();
            data.native_mode = mode_id.to_string();
            if let Some(modes) = &mut data.modes {
                modes.current_mode_id = mode_id.to_string();
            }
        }
        self.emit_changed(&rec);
        self.save();
        Ok(())
    }

    /// Go `GoalControl` (runtime.go:1604): codex-acp `_session/goal`.
    pub async fn goal_control(&self, session_id: &str, action: &str) -> Result<(), String> {
        let rec = self.record(session_id);
        let d = self.binding(session_id);
        let (Some(rec), Some(d)) = (rec, d) else {
            return Err("原生会话未连接".into());
        };
        let native = rec.data.lock().unwrap().native_session_id.clone();
        d.call(
            "_session/goal",
            &json!({"sessionId": native, "action": action}),
            20_000,
        )
        .await?;
        Ok(())
    }

    /// Go `StopAsyncTask` (runtime.go:1621): the adapter's verdict, verbatim.
    pub async fn stop_async_task(&self, session_id: &str, task_id: &str) -> Result<Value, String> {
        let rec = self.record(session_id);
        let d = self.binding(session_id);
        let (Some(rec), Some(d)) = (rec, d) else {
            return Err("原生会话未连接".into());
        };
        let native = rec.data.lock().unwrap().native_session_id.clone();
        let result = d
            .call(
                "_session/async_task/stop",
                &json!({"sessionId": native, "taskId": task_id}),
                20_000,
            )
            .await?;
        Ok(result)
    }

    /// Go `Prompt` (runtime.go:1640).
    pub async fn prompt(&self, agent: &Agent, text: &str, o: Options) -> TurnResult {
        self.prompt_blocks(agent, &[json!({"type": "text", "text": text})], o)
            .await
    }

    /// Go `PromptBlocks` (runtime.go:1644).
    pub async fn prompt_blocks(&self, agent: &Agent, blocks: &[Value], o: Options) -> TurnResult {
        let (rec, _) = match self.ensure(agent, &o).await {
            Ok(pair) => pair,
            Err(err) => {
                let mut failed = TurnResult::failed(err);
                failed.meta = json!({});
                return failed;
            }
        };
        let _turn_guard = rec.turn_lock.lock().await;
        self.turn(&rec, agent, blocks, &o).await
    }

    /// Go `turn` (runtime.go:1654) — the pausable clock.
    async fn turn(&self, rec: &Arc<SessionRecord>, agent: &Agent, blocks: &[Value], o: &Options) -> TurnResult {
        let d = match self.binding(&rec.id) {
            Some(d) if d.is_alive() => d,
            _ => {
                let mut failed = TurnResult::failed("会话驱动已断开");
                failed.meta = json!({});
                return failed;
            }
        };
        let model = {
            let data = rec.data.lock().unwrap();
            let mut model = o.model.clone();
            if model.is_empty() {
                model = data.default_model.clone();
            }
            model
        };
        if !model.is_empty() {
            let needs_switch = {
                let data = rec.data.lock().unwrap();
                data.models
                    .as_ref()
                    .map(|m| m.current_model_id != model)
                    .unwrap_or(true)
            };
            if needs_switch {
                let native = rec.data.lock().unwrap().native_session_id.clone();
                let cfg = match d
                    .call(
                        "session/set_config_option",
                        &json!({"sessionId": native, "configId": "model", "value": model}),
                        20_000,
                    )
                    .await
                {
                    Ok(cfg) => cfg,
                    Err(err) => {
                        let mut failed = TurnResult::failed(err);
                        failed.meta = json!({});
                        return failed;
                    }
                };
                let mut data = rec.data.lock().unwrap();
                apply_configuration(&mut data, &cfg);
            }
        }
        let turn = Arc::new(AsyncMutex::new(Turn {
            id: format!("turn_{}", short_id()),
            stage: o.stage.clone(),
            structured: o.structured,
            started_at: unix_millis(),
            status: "running".into(),
            last_activity_millis: unix_millis(),
            ..Turn::default()
        }));
        {
            let mut data = rec.data.lock().unwrap();
            data.active = Some(turn.clone());
            data.state = "running".into();
            data.error.clear();
        }
        self.emit_changed(rec);
        let timeout_ms = if o.timeout_ms != 0 { o.timeout_ms } else { self.options.timeout_ms };
        // Only inactivity consumes the timeout; streaming and tool updates
        // renew it.
        let native = rec.data.lock().unwrap().native_session_id.clone();
        let mut deadline = unix_millis() + timeout_ms;
        let (done_tx, mut done_rx) = oneshot::channel::<(Value, Option<String>)>();
        {
            let d = d.clone();
            let native = native.clone();
            let blocks = blocks.to_vec();
            tokio::spawn(async move {
                let result = d
                    .call("session/prompt", &json!({"sessionId": native, "prompt": blocks}), 0)
                    .await;
                match result {
                    Ok(res) if res.is_null() => {
                        let _ = done_tx.send((Value::Null, Some("空响应".into())));
                    }
                    Ok(res) => {
                        let _ = done_tx.send((res, None));
                    }
                    Err(err) => {
                        let _ = done_tx.send((Value::Null, Some(err)));
                    }
                }
            });
        }
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut prompt_err: Option<String> = None;
        let mut stop_reason = String::new();
        let mut usage: Option<Value> = None;
        let mut hard_killed = false;
        let mut cancelled_at: Option<i64> = None;
        let mut closed = d.closed_rx.clone();
        'clock: loop {
            tokio::select! {
                reply = &mut done_rx => {
                    let (raw, err) = match reply {
                        Ok(pair) => pair,
                        Err(_) => (Value::Null, Some("空响应".to_string())),
                    };
                    if let Some(err) = err {
                        prompt_err = Some(err);
                    } else {
                        let stop = raw.get("stopReason").and_then(Value::as_str).unwrap_or("");
                        stop_reason = stop.to_string();
                        usage = normalize_usage(raw.get("usage"));
                        if stop_reason == "refusal" {
                            let mut message = "执行端拒绝了本次请求".to_string();
                            let meta = raw.get("_meta").cloned().unwrap_or(Value::Null);
                            if let Some(inner) = meta.get("codebuddy.ai/errorMessage").and_then(Value::as_str) {
                                if let Ok(detail) = serde_json::from_str::<Value>(inner) {
                                    let msg_text = detail.get("message").and_then(Value::as_str).unwrap_or("");
                                    if !msg_text.is_empty() {
                                        message = msg_text.to_string();
                                    }
                                    let details = detail
                                        .get("data")
                                        .and_then(|d| d.get("details"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("");
                                    if !details.is_empty() {
                                        message = details.to_string();
                                    }
                                }
                            }
                            prompt_err = Some(message);
                        }
                    }
                    break 'clock;
                }
                _ = ticker.tick() => {
                    // Pure wall-clock arithmetic like Go: `millis_to_instant`
                    // clamping would push the deadline forward every tick and
                    // idle turns would never time out.
                    let now = unix_millis();
                    if !hard_killed {
                        let last_activity = {
                            let turn = turn.lock().await;
                            turn.last_activity_millis
                        };
                        let candidate = last_activity + timeout_ms;
                        if candidate > deadline {
                            deadline = candidate;
                        }
                        if self.pending_count(&rec.id) > 0 {
                            deadline = now + timeout_ms;
                        }
                    }
                    if !hard_killed && now > deadline {
                        let _ = d
                            .notify("session/cancel", &json!({"sessionId": native}))
                            .await;
                        hard_killed = true;
                        cancelled_at = Some(now);
                    }
                    if hard_killed {
                        if let Some(at) = cancelled_at {
                            if now - at > 10_000 {
                                d.kill();
                                prompt_err = Some("ACP 会话超时".into());
                                break 'clock;
                            }
                        }
                    }
                }
                _ = closed.changed() => {
                    prompt_err = Some("ACP 进程已退出".into());
                    break 'clock;
                }
            }
        }
        if hard_killed && prompt_err.is_none() {
            prompt_err = Some("ACP 会话超时".into());
        }
        self.cancel_permissions(&rec.id);
        let mut turn_state = turn.lock().await;
        turn_state.finished_at = unix_millis();
        turn_state.usage = usage.clone().unwrap_or(Value::Null);
        let turn_snapshot = turn_state.clone();
        drop(turn_state);
        {
            let mut data = rec.data.lock().unwrap();
            data.turns.push(turn_snapshot.clone());
            if data.turns.len() > 20 {
                let overflow = data.turns.len() - 20;
                data.turns.drain(0..overflow);
            }
            data.active = None;
            match &prompt_err {
                Some(err) => {
                    let mut failed = turn_snapshot.clone();
                    failed.status = "failed".into();
                    failed.error = err.clone();
                    if let Some(last) = data.turns.last_mut() {
                        *last = failed.clone();
                    }
                    data.state = if d.is_alive() { "idle".into() } else { "disconnected".into() };
                }
                None => {
                    let mut done = turn_snapshot.clone();
                    done.status = if stop_reason == "cancelled" { "cancelled" } else { "completed" }.into();
                    done.stop_reason = stop_reason.clone();
                    if let Some(last) = data.turns.last_mut() {
                        *last = done;
                    }
                    data.state = "idle".into();
                }
            }
        }
        self.emit_changed(rec);
        self.save();
        if let Some(err) = &prompt_err {
            let mut failed = TurnResult::failed(err.clone());
            failed.meta = json!({"runtimeSessionId": rec.id});
            return failed;
        }
        let structured = extract_submitted(&turn_snapshot.tools);
        let mut meta = json!({
            "sessionId": turn_snapshot_native(&rec),
            "runtimeSessionId": rec.id,
            "model": model,
            "permission": rec.data.lock().unwrap().permission,
            "tool": agent.real_tool,
            "durationMs": turn_snapshot.finished_at.saturating_sub(turn_snapshot.started_at),
            "stopReason": stop_reason,
        });
        if let Some(u) = &usage {
            meta["usage"] = u.clone();
        }
        if hard_killed {
            return TurnResult {
                ok: false,
                output: turn_snapshot.output.clone(),
                structured: None,
                error: "ACP 会话超时".into(),
                meta,
            };
        }
        if stop_reason == "cancelled" {
            return TurnResult {
                ok: false,
                output: turn_snapshot.output.clone(),
                structured: None,
                error: "会话已取消".into(),
                meta,
            };
        }
        let ok = !turn_snapshot.output.is_empty() || structured.is_some();
        TurnResult {
            ok,
            output: turn_snapshot.output.clone(),
            structured,
            error: if ok { String::new() } else { "会话已停止或未返回有效结果".into() },
            meta,
        }
    }

    /// Go `Cancel` (runtime.go:1823).
    pub async fn cancel(&self, id: &str) -> Result<(), String> {
        let rec = self.record(id);
        let d = self.binding(id);
        let (Some(rec), Some(d)) = (rec, d) else {
            return Err("此会话没有正在执行的调用".into());
        };
        let has_active = rec.data.lock().unwrap().active.is_some();
        if !has_active {
            return Err("此会话没有正在执行的调用".into());
        }
        {
            rec.data.lock().unwrap().state = "cancelling".into();
        }
        self.cancel_permissions(id);
        self.emit_changed(&rec);
        let native = rec.data.lock().unwrap().native_session_id.clone();
        d.notify("session/cancel", &json!({"sessionId": native})).await
    }

    // ----------------------------- listings -----------------------------

    /// Go `ListJSON` (runtime.go:1853): records with normalized arrays and the
    /// active turn, optionally filtered by chat.
    pub async fn list_json(&self, chat_id: &str) -> Vec<Value> {
        let mut out = Vec::new();
        for rec in self.records(chat_id) {
            let mut wire = record_to_wire(&rec);
            let active = rec.data.lock().unwrap().active.clone();
            if let Some(active) = active {
                let turn = active.lock().await;
                wire.as_object_mut()
                    .unwrap()
                    .insert("activeTurn".into(), turn_to_wire(&turn));
            } else {
                wire.as_object_mut()
                    .unwrap()
                    .insert("activeTurn".into(), Value::Null);
            }
            out.push(wire);
        }
        out
    }

    /// Go `ExportTranscript` (runtime.go:1879).
    pub fn export_transcript(&self, id: &str) -> String {
        let Some(rec) = self.record(id) else { return String::new() };
        let mut out = String::new();
        for t in &rec.data.lock().unwrap().turns {
            let stage = if t.stage.is_empty() { "执行" } else { &t.stage };
            out.push_str(&format!("### 回合 {}（{stage}）\n", t.id));
            if !t.output.is_empty() {
                out.push_str(&t.output);
                out.push('\n');
            }
            for tool in &t.tools {
                let title = tool.get("title").and_then(Value::as_str).unwrap_or("");
                let kind = tool.get("kind").and_then(Value::as_str).unwrap_or("");
                out.push_str(&format!("- 工具 {title}（{kind}）\n"));
                if let Some(raw) = tool.get("rawInput") {
                    let body = serde_json::to_string(raw).unwrap_or_default();
                    let body: String = body.chars().take(2000).collect();
                    out.push_str(&format!("  参数: {body}\n"));
                }
            }
            if !t.plan.is_empty() {
                let body = serde_json::to_string(&t.plan).unwrap_or_default();
                let body: String = body.chars().take(2000).collect();
                out.push_str(&format!("  计划: {body}\n"));
            }
        }
        out
    }

    /// Go `SaveRecords` (runtime.go:1923).
    pub fn save_records(&self) {
        self.save();
    }

    /// Go `Shutdown` (runtime.go:1926): stop every driver, ≤3s each.
    pub async fn shutdown(&self) {
        let drivers: Vec<Arc<Driver>> = {
            let inner = self.inner.lock().unwrap();
            inner.drivers.values().cloned().collect()
        };
        for d in drivers {
            for rec in d.sessions.lock().unwrap().clone() {
                self.cancel_permissions(&rec.id);
            }
            d.kill();
            let mut stopped = d.stopped_rx.clone();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), stopped.changed()).await;
        }
    }

    /// Go `KillDriversAt` (runtime.go:1948): terminate pooled drivers rooted at
    /// one path (before removing a worktree on Windows).
    pub async fn kill_drivers_at(&self, dir: &str) {
        let doomed: Vec<Arc<Driver>> = {
            let mut inner = self.inner.lock().unwrap();
            let mut doomed = Vec::new();
            let keys: Vec<String> = inner
                .drivers
                .iter()
                .filter(|(_, d)| d.cwd == dir)
                .map(|(k, _)| k.clone())
                .collect();
            for key in keys {
                if let Some(d) = inner.drivers.remove(&key) {
                    doomed.push(d);
                }
            }
            doomed
        };
        for d in doomed {
            for rec in d.sessions.lock().unwrap().clone() {
                self.cancel_permissions(&rec.id);
            }
            d.kill();
            let mut stopped = d.stopped_rx.clone();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), stopped.changed()).await;
        }
    }
}

// ----------------------------- request handling -----------------------------

async fn handle_request(rt: &Runtime, driver: &Driver, id: i64, method: &str, params: &Value) {
    match method {
        "fs/read_text_file" => match rt.fs_read(driver, params) {
            Ok(result) => driver.reply(id, result, None).await,
            Err(err) => driver.reply(id, Value::Null, Some(err)).await,
        },
        "fs/write_text_file" => match rt.fs_write(driver, params) {
            Ok(result) => driver.reply(id, result, None).await,
            Err(err) => driver.reply(id, Value::Null, Some(err)).await,
        },
        "session/request_permission" => match rt.request_permission(driver, params).await {
            Ok(result) => driver.reply(id, result, None).await,
            Err(err) => driver.reply(id, Value::Null, Some(err)).await,
        },
        _ => {
            driver
                .reply(id, Value::Null, Some(format!("method not supported: {method}")))
                .await;
        }
    }
}

// ----------------------------- fs path helpers -----------------------------

/// Go `splitLines` (runtime.go:896): split on \n, trim a trailing \r.
fn split_lines(s: &str) -> Vec<String> {
    s.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect()
}

/// Normalise a Windows canonicalised path (`\\?\C:\...`) back to a plain one so
/// path comparison matches Go's `filepath.EvalSymlinks` output.
fn normalize_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// Go `withinDir` (runtime.go:920): resolve symlinks and require the candidate
/// to stay inside the session's working directory.
fn within_dir(root: &Path, candidate: &Path) -> Result<PathBuf, String> {
    let abs = normalize_path(&std::path::absolute(candidate).map_err(|e| e.to_string())?);
    let real_root = std::fs::canonicalize(root)
        .map(|p| normalize_path(&p))
        .unwrap_or_else(|_| root.to_path_buf());
    let real = match std::fs::canonicalize(&abs) {
        Ok(p) => normalize_path(&p),
        Err(_) => {
            // target may not exist yet (write); resolve the deepest existing
            // ancestor
            let dir = abs.parent().map(Path::to_path_buf).unwrap_or_else(|| abs.clone());
            match std::fs::canonicalize(&dir) {
                Ok(resolved) => {
                    let base = abs.file_name().map(PathBuf::from).unwrap_or_default();
                    normalize_path(&resolved).join(base)
                }
                Err(_) => abs.clone(),
            }
        }
    };
    let rel = relative(&real_root, &real)?;
    if rel == ".." || rel.starts_with("..\\") || rel.starts_with("../") || is_absolute_rel(&rel) {
        return Err("文件路径超出此会话工作目录".into());
    }
    Ok(real)
}

/// `filepath.Rel` equivalent for the two spellings this port sees.
fn relative(base: &Path, target: &Path) -> Result<String, String> {
    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();
    if base_components.as_slice() == target_components.as_slice() {
        return Ok(".".into());
    }
    let mut common = 0usize;
    for (b, t) in base_components.iter().zip(target_components.iter()) {
        if b == t {
            common += 1;
        } else {
            break;
        }
    }
    if common == 0 {
        // Different roots (e.g. C:\ vs D:\) — Go returns an error.
        return Err(format!(
            "路径不在同一根目录：{base:?} 与 {target:?}"
        ));
    }
    let ups = base_components.len() - common;
    let mut parts: Vec<String> = Vec::new();
    for _ in 0..ups {
        parts.push("..".into());
    }
    for c in &target_components[common..] {
        parts.push(c.as_os_str().to_string_lossy().to_string());
    }
    Ok(parts.join("\\"))
}

fn is_absolute_rel(rel: &str) -> bool {
    // Go: filepath.IsAbs(rel) — a Windows drive prefix.
    let bytes = rel.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

fn turn_snapshot_native(rec: &SessionRecord) -> String {
    rec.data.lock().unwrap().native_session_id.clone()
}

/// BufReader over a `ChildStdout` yielding \n-split lines (16 MiB cap like
/// Go's scanner buffer).
struct BufReaderLines {
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
}

impl BufReaderLines {
    fn new(stdout: tokio::process::ChildStdout) -> Self {
        BufReaderLines { reader: tokio::io::BufReader::with_capacity(1024 * 1024, stdout) }
    }

    async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        use tokio::io::AsyncBufReadExt;
        let mut line = Vec::new();
        let n = self.reader.read_until(b'\n', &mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.len() > 16 * 1024 * 1024 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "ACP 行超长"));
        }
        Ok(Some(String::from_utf8_lossy(&line).to_string()))
    }
}

// ----------------------------- fixture tests (ported from runtime_test.go) -----------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::acp::launch::which;
    use serde_json::json;
    use std::time::Duration;

    /// Go `fixtureRuntime`: the node protocol fixture, skipping when node is
    /// absent from PATH (like Go's `t.Skip`).
    fn fixture_runtime(tag: &str, extra_args: &[&str]) -> Option<(Arc<Runtime>, Agent, Options)> {
        let node = match which("node") {
            Ok(node) => node,
            Err(_) => {
                eprintln!("skip {tag}: Node is needed for the ACP protocol fixture");
                return None;
            }
        };
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut args = vec![root.join("fixtures").join("acp-agent.cjs").to_string_lossy().to_string()];
        args.extend(extra_args.iter().map(|a| a.to_string()));
        let resolver = crate::backend::acp::launch::StaticLaunchResolver {
            launch: Launch { command: node, args, env: None },
        };
        let rt = Runtime::new(RuntimeOptions {
            state_file: None,
            timeout_ms: 10_000,
            resolver: Arc::new(resolver),
            emitter: Arc::new(NoopEmitter),
            hooks: Arc::new(NoopHooks),
        });
        let agent = Agent { id: "agent".into(), real_tool: "codex".into(), ..Agent::default() };
        let options = Options {
            workdir: root.to_string_lossy().to_string(),
            permission: "readonly".into(),
            session_key: "test".into(),
            ..Options::default()
        };
        Some((rt, agent, options))
    }

    async fn wait_for(deadline_ms: u64, mut probe: impl AsyncFnMut() -> bool) -> bool {
        let start = Instant::now();
        loop {
            if probe().await {
                return true;
            }
            if start.elapsed() > Duration::from_millis(deadline_ms) {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn test_native_steering_injects_only_into_running_turn() {
        let Some((rt, agent, options)) = fixture_runtime("steer", &["--fork", "--steer"]) else { return };
        let sid = rt.connect(&agent, options.clone()).await.expect("connect");
        assert!(rt.supports_native_steering(&sid), "steering capability not detected");
        let blocks = vec![json!({"type": "text", "text": "follow up"})];
        match rt.steer(&sid, &blocks).await {
            Err(err) => assert!(err.contains("no active turn"), "idle steer: {err}"),
            Ok(()) => panic!("idle steer must fail"),
        }
        let done = tokio::spawn({
            let rt = rt.clone();
            let agent = agent.clone();
            let options = options.clone();
            async move { rt.prompt(&agent, "wait", options).await }
        });
        let steered = wait_for(3000, || async {
            rt.steer(&sid, &blocks).await.is_ok()
        })
        .await;
        if !steered {
            panic!("steering failed");
        }
        let result = tokio::time::timeout(Duration::from_secs(3), done)
            .await
            .expect("steered turn did not complete")
            .expect("join");
        assert!(result.ok && result.output == "steered:follow up", "wrong result: {result:?}");
        match rt.steer(&sid, &blocks).await {
            Err(err) => assert!(err.contains("no active turn"), "idle steer started a detached turn: {err}"),
            Ok(()) => panic!("idle steer started a detached turn"),
        }
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn test_prompt_preserves_wire_order_across_turns() {
        let Some((rt, agent, options)) = fixture_runtime("order", &[]) else { return };
        for i in 0..40 {
            let text = format!("turn-{i:02}:abcdefghijklmnopqrstuv");
            let res = rt.prompt(&agent, &text, options.clone()).await;
            assert!(res.ok && res.output == text, "turn {i}: got {:?}", res);
        }
        let res = rt.prompt(&agent, "submit", options.clone()).await;
        assert!(res.ok, "lost final tool submission: {res:?}");
        assert_eq!(
            res.structured.as_ref().and_then(|s| s.get("output")).and_then(Value::as_str),
            Some("tool says done")
        );
        let records = rt.records("");
        // Bind the guard explicitly: `let turns = &records[0].data.lock()…`
        // extends the temporary MutexGuard to the end of the block — across
        // the `shutdown().await` — which deadlocks the exit watcher.
        let (tool_count, tool_status) = {
            let data = records[0].data.lock().unwrap();
            let last = data.turns.last().unwrap();
            (last.tools.len(), last.tools[0]["status"].clone())
        };
        assert_eq!(tool_count, 1, "tool updates out of order");
        assert_eq!(tool_status, json!("completed"));
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn test_fork_capability_requires_an_explicit_live_agent_declaration() {
        let Some((rt, agent, options)) = fixture_runtime("forkcap", &[]) else { return };
        let id = rt.connect(&agent, options.clone()).await.expect("connect");
        assert!(!rt.supports_fork(&id), "unadvertised fork was reported as supported");
        assert!(!rt.supports_fork("missing"), "missing session reported supported");
        {
            let d = rt.binding(&id).unwrap();
            let mut info = d.info.lock().unwrap().clone();
            info["agentCapabilities"]["sessionCapabilities"] = json!({"fork": {}});
            *d.info.lock().unwrap() = info;
        }
        assert!(rt.supports_fork(&id), "advertised fork capability was not detected");
        assert!(!rt.supports_point_fork(&id, Some(&agent)), "unverified adapter must not expose point forks");
        {
            let d = rt.binding(&id).unwrap();
            let mut info = d.info.lock().unwrap().clone();
            info["agentInfo"] = json!({"name": "@agentclientprotocol/codex-acp", "version": "2.5.0"});
            *d.info.lock().unwrap() = info;
        }
        assert!(rt.supports_point_fork(&id, Some(&agent)), "verified adapter did not expose point forks");
        {
            let d = rt.binding(&id).unwrap();
            let mut info = d.info.lock().unwrap().clone();
            info["agentCapabilities"]["sessionCapabilities"] = json!({"fork": null});
            *d.info.lock().unwrap() = info;
        }
        assert!(!rt.supports_fork(&id), "null fork capability was reported as supported");
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn test_fork_uses_native_message_point_and_keeps_sessions_independent() {
        let Some((rt, agent, source)) = fixture_runtime("fork", &["--fork"]) else { return };
        let got = rt.prompt(&agent, "remember original", source.clone()).await;
        assert!(got.ok, "{}", got.error);
        let got = rt.prompt(&agent, "remember later", source.clone()).await;
        assert!(got.ok, "{}", got.error);
        let source_id = rt.records("")[0].id.clone();
        let fork_id = rt
            .fork(&agent, &source_id, source.clone(), "new-branch", "fixture-message-1", None)
            .await
            .expect("fork");
        assert_ne!(fork_id, source_id);
        let fork_options = Options { session_key: "new-branch".into(), ..source.clone() };
        let got = rt.prompt(&agent, "recall", fork_options).await;
        assert!(got.ok && got.output == "original", "fork has wrong history: {:?}", got);
        let got = rt.prompt(&agent, "recall", source.clone()).await;
        assert!(got.ok && got.output == "later", "source history changed: {:?}", got);
        let err = rt
            .fork(&agent, &source_id, source, "bad-branch", "missing-native-id", None)
            .await
            .expect_err("unknown fork point should fail");
        assert!(!err.is_empty());
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn test_streaming_renews_inactivity_timeout() {
        let Some((rt, agent, mut options)) = fixture_runtime("stream", &[]) else { return };
        options.timeout_ms = 1000;
        let res = rt.prompt(&agent, "stream-slow", options).await;
        assert!(res.ok && res.output == ".....", "active stream was cancelled: {res:?}");
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn test_idle_prompt_still_times_out() {
        let Some((rt, agent, mut options)) = fixture_runtime("idle", &[]) else { return };
        options.timeout_ms = 700;
        let res = rt.prompt(&agent, "wait", options).await;
        assert!(!res.ok && res.error == "ACP 会话超时", "idle prompt did not time out: {res:?}");
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn test_full_access_mode_survives_driver_restart() {
        let Some((rt, agent, mut options)) = fixture_runtime("restart", &[]) else { return };
        options.permission = "workspace".into();
        options.mode = "agent-full-access".into();
        let id = rt.connect(&agent, options.clone()).await.expect("connect");
        {
            let rec = &rt.records("")[0];
            let data = rec.data.lock().unwrap();
            assert_eq!(
                data.modes.as_ref().map(|m| m.current_mode_id.as_str()),
                Some("agent-full-access"),
                "full access was downgraded"
            );
        }
        rt.configure(&id, "mode", &json!("read-only")).await.expect("configure");
        rt.shutdown().await;
        rt.connect(&agent, options.clone()).await.expect("reconnect");
        let rec = &rt.records("")[0];
        let data = rec.data.lock().unwrap();
        assert_eq!(data.native_mode, "read-only", "latest user mode was not restored");
        assert_eq!(
            data.modes.as_ref().map(|m| m.current_mode_id.as_str()),
            Some("read-only")
        );
        drop(data);
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn test_resume_only_harness_without_modes() {
        let Some((rt, mut agent, mut options)) = fixture_runtime("resume", &["--resume-only"]) else { return };
        agent.real_tool = "dsh".into();
        options.permission = "workspace".into();
        let result = rt.prompt(&agent, "first", options.clone()).await;
        assert!(result.ok, "{result:?}");
        rt.shutdown().await;
        let result = rt.prompt(&agent, "resumed", options).await;
        assert!(result.ok && result.output == "resumed", "{result:?}");
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn test_refusal_reports_native_authentication_error() {
        let Some((rt, agent, options)) = fixture_runtime("refusal", &[]) else { return };
        let result = rt.prompt(&agent, "refusal", options).await;
        assert!(!result.ok, "{result:?}");
        assert_eq!(result.error, "Please sign in to WorkBuddy", "{result:?}");
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn test_session_view_normalizes_arrays_and_shows_active_turn() {
        let rt = Runtime::new(RuntimeOptions::default());
        assert_eq!(
            serde_json::to_string(&json!(rt.list_json("").await)).unwrap(),
            "[]",
            "empty session list"
        );
        let first = Arc::new(SessionRecord {
            id: "first".into(),
            key: String::new(),
            agent_id: String::new(),
            cwd: String::new(),
            turn_lock: AsyncMutex::new(()),
            data: Mutex::new(RecordData::default()),
        });
        {
            let mut data = first.data.lock().unwrap();
            data.state = "running".into();
            data.active = Some(Arc::new(AsyncMutex::new(Turn {
                output: "stream".into(),
                received_chars: 6,
                ..Turn::default()
            })));
        }
        rt.test_inject_record(first.clone());
        let view = rt.list_json("").await[0].clone();
        for key in ["turns", "permissions"] {
            let values = view.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
            assert!(values.is_empty(), "{key} must be []: {view}");
        }
        let active = view.get("activeTurn").cloned().unwrap_or(Value::Null);
        assert_eq!(active.get("output").and_then(Value::as_str), Some("stream"));
        assert_eq!(active.get("receivedChars").and_then(Value::as_i64), Some(6));
        assert!(active.get("tools").and_then(Value::as_array).is_some(), "tools must be []");
        first.data.lock().unwrap().active = None;
        first.data.lock().unwrap().turns.push(Turn {
            status: "completed".into(),
            ..Turn::default()
        });
        let view = rt.list_json("").await[0].clone();
        assert!(view.get("activeTurn").map(Value::is_null).unwrap_or(false), "finished turn still active");
        let turns = view.get("turns").and_then(Value::as_array).cloned().unwrap_or_default();
        assert!(turns[0].get("tools").and_then(Value::as_array).is_some(), "finished tools must be []");
    }

    #[tokio::test]
    async fn test_active_turn_can_be_observed_and_cancelled() {
        let Some((rt, agent, options)) = fixture_runtime("cancel", &[]) else { return };
        let done = tokio::spawn({
            let rt = rt.clone();
            let agent = agent.clone();
            let options = options.clone();
            async move { rt.prompt(&agent, "wait", options).await }
        });
        let observed = wait_for(3000, || async {
            match rt.list_json("").await.first().cloned() {
                Some(view) => !view.get("activeTurn").map(Value::is_null).unwrap_or(true),
                None => false,
            }
        })
        .await;
        assert!(observed, "active session did not appear");
        let id = rt.records("")[0].id.clone();
        rt.cancel(&id).await.expect("cancel");
        tokio::time::timeout(Duration::from_secs(3), done)
            .await
            .expect("cancel stalled")
            .expect("join");
        let view = rt.list_json("").await[0].clone();
        assert!(view.get("activeTurn").map(Value::is_null).unwrap_or(false), "cancelled turn still active");
        rt.shutdown().await;
    }

    #[tokio::test]
    async fn test_persistence_restores_latest_hundred_sessions() {
        let dir = std::env::temp_dir().join(format!("acp-persist-{}", short_id()));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        let file = dir.join("nested").join("sessions.json");
        let rt = Runtime::new(RuntimeOptions { state_file: Some(file.clone()), ..RuntimeOptions::default() });
        for i in 0..105 {
            let rec = Arc::new(SessionRecord {
                id: format!("session-{i:03}"),
                key: String::new(),
                agent_id: String::new(),
                cwd: String::new(),
                turn_lock: AsyncMutex::new(()),
                data: Mutex::new(RecordData {
                    native_session_id: format!("native-{i:03}"),
                    ..RecordData::default()
                }),
            });
            rt.test_inject_record(rec);
        }
        rt.save_records();
        let restored = Runtime::new(RuntimeOptions { state_file: Some(file), ..RuntimeOptions::default() });
        let records = restored.records("");
        assert_eq!(records.len(), 100, "latest sessions were not retained");
        assert_eq!(records[0].id, "session-005");
        assert_eq!(records[99].data.lock().unwrap().native_session_id, "native-104");
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn test_permission_request_does_not_block_stream_reader() {
        let Some((rt, agent, mut options)) = fixture_runtime("perm", &[]) else { return };
        options.permission = "workspace".into();
        let done = tokio::spawn({
            let rt = rt.clone();
            let agent = agent.clone();
            let options = options.clone();
            async move { rt.prompt(&agent, "permission", options).await }
        });
        let appeared = wait_for(3000, || async {
            match rt.list_json("").await.first().cloned() {
                Some(view) => {
                    let cards = view.get("permissions").and_then(Value::as_array).cloned().unwrap_or_default();
                    !cards.is_empty()
                }
                None => false,
            }
        })
        .await;
        assert!(appeared, "permission request stalled");
        let view = rt.list_json("").await[0].clone();
        let card = view["permissions"].as_array().unwrap()[0].clone();
        let choice = "yes";
        rt.respond_permission(card["id"].as_str().unwrap(), Some(choice))
            .expect("respond");
        let res = tokio::time::timeout(Duration::from_secs(3), done)
            .await
            .expect("permission response stalled")
            .expect("join");
        assert!(res.ok && res.output == choice, "permission reply lost: {res:?}");
        rt.shutdown().await;
    }

    struct AllowListHook;
    impl RuntimeHooks for AllowListHook {
        fn allow_internal_tool(&self, _session_id: &str, tool_call: &Value) -> bool {
            tool_call.get("title").and_then(Value::as_str) == Some("mcp__codeg-mcp__list_agents")
        }
    }

    #[tokio::test]
    async fn test_only_owned_internal_tools_may_be_allowed_in_read_only_sessions() {
        let Some((_, agent, options)) = fixture_runtime("readonly", &[]) else { return };
        // Rebuild with the internal-tool hook: fixture_runtime uses NoopHooks.
        let node = which("node").expect("node present (fixture_runtime checked)");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let resolver = crate::backend::acp::launch::StaticLaunchResolver {
            launch: Launch {
                command: node,
                args: vec![root.join("fixtures").join("acp-agent.cjs").to_string_lossy().to_string()],
                env: None,
            },
        };
        let rt = Runtime::new(RuntimeOptions {
            state_file: None,
            timeout_ms: 10_000,
            resolver: Arc::new(resolver),
            emitter: Arc::new(NoopEmitter),
            hooks: Arc::new(AllowListHook),
        });
        let allowed = rt.prompt(&agent, "permission-internal", options.clone()).await;
        assert!(allowed.ok && allowed.output == "yes", "internal tool blocked: {allowed:?}");
        let denied = rt.prompt(&agent, "permission", options).await;
        assert!(denied.ok && denied.output == "cancelled", "native edit bypassed read-only: {denied:?}");
        rt.shutdown().await;
    }
}

