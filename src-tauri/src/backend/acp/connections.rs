//! The connection layer, ported from Go's `codeg/connections.go` (+ the
//! attachment helpers in `codeg/attachments.go`, `bindConversation` from
//! `delegation_recovery.go`, `extraMCP` from `delegation.go` and the
//! context-string builders from `delegation_recovery.go`/`split.go`).
//!
//! One `Layer` owns the runtime plus every connection-scoped registry
//! (connections, warm pool, callers, parked questions, pending splits).
//! Events reach the WebSocket clients through the front door's `emit_events`
//! via the `EmitSink` closure the dispatcher installs — the same frames Go's
//! `emit` fed to its subscriber channels.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use serde_json::{json, Value};

use crate::backend::acp::agents::{registry, wire_agent, Agent};
use crate::backend::acp::launch::ProductionResolver;
use crate::backend::acp::question::{self, ParkedQuestion};
use crate::backend::acp::runtime::{Emitter, Options, Runtime, RuntimeHooks, RuntimeOptions};
use crate::backend::acp::splits::{self, TaskSplit};
use crate::backend::acp::store_conv;
use crate::backend::acp::tool_identity::{self, Caller};
use crate::backend::acp::wire::{config_wire, mode_wire, permission_options, text};
use crate::backend::goformat;
use crate::backend::store_file;

/// busyStaleGrace (connections.go:21): how long a busy connection with no
/// runtime-active turn is tolerated before the next prompt takes over in place.
const BUSY_STALE_GRACE: Duration = Duration::from_secs(3);

/// warmIdleTTL (connections.go:24): how long an unclaimed pre-built
/// connection is kept.
const WARM_IDLE_TTL: Duration = Duration::from_secs(120);

const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

/// One frame batch for the front door (Go's `emit` → its subscribers).
pub type EmitSink = Arc<dyn Fn(&str, &Value) + Send + Sync>;

pub struct Layer {
    pub root: PathBuf,
    /// The Go sidecar's base URL — the codeg-mcp companion endpoint the
    /// extraMCP hook hands to adapter processes (still Go-served while the
    /// companion domain is unported).
    pub upstream_base: String,
    pub sink: EmitSink,
    /// The runtime — set right after construction (hooks need the layer).
    pub runtime: OnceLock<Arc<Runtime>>,
    /// Self-reference for spawned background tasks (warm pool, sweeper).
    self_weak: OnceLock<Weak<Layer>>,
    pub state: Mutex<LayerState>,
}

pub struct LayerState {
    pub connections: HashMap<String, Arc<Mutex<Connection>>>,
    /// The delegation registry (Go's `s.tasks`).
    pub delegations: HashMap<String, super::delegation::Delegation>,
    pub warm: HashMap<String, Vec<WarmEntry>>,
    pub warming: HashSet<String>,
    pub started: bool,
    pub callers: HashMap<String, Caller>,
    pub questions: HashMap<String, ParkedQuestion>,
    pub splits: HashMap<String, TaskSplit>,
}

pub struct WarmEntry {
    pub id: String,
    pub placed_at: std::time::Instant,
}

/// Go `Connection` (connections.go:31).
pub struct Connection {
    pub id: String,
    pub agent_type: String,
    pub status: String,
    pub agent: Arc<Agent>,
    pub options: Options,
    pub runtime_id: String,
    pub conversation_id: i64,
    pub folder_id: i64,
    pub seq: u64,
    pub events: Vec<Value>,
    pub live: Value,
    pub tools: Vec<Value>,
    pub pending: Value,
    pub pending_split: Value,
    pub pending_question: Value,
    pub busy: bool,
    pub seen_permissions: HashMap<String, String>,
    pub depth: i64,
    pub root_id: String,
    pub feedback: Vec<Value>,
    pub tool_claims: HashMap<String, bool>,
    pub warm: bool,
    pub run_seq: u64,
    pub run_accepted: Option<std::time::Instant>,
    /// The assistant text of the last finished turn, with the run it belongs
    /// to — the delegation domain reads it to report a child task's result.
    pub last_output: String,
    pub output_run_seq: u64,
}

impl Connection {
    fn new(id: String, agent: Arc<Agent>, options: Options) -> Self {
        Connection {
            agent_type: wire_agent(&agent),
            status: "connecting".into(),
            agent,
            options,
            runtime_id: String::new(),
            conversation_id: 0,
            folder_id: 0,
            seq: 0,
            events: Vec::new(),
            live: Value::Null,
            tools: Vec::new(),
            pending: Value::Null,
            pending_split: Value::Null,
            pending_question: Value::Null,
            busy: false,
            seen_permissions: HashMap::new(),
            depth: 0,
            root_id: String::new(),
            feedback: Vec::new(),
            tool_claims: HashMap::new(),
            warm: false,
            run_seq: 0,
            run_accepted: None,
            last_output: String::new(),
            output_run_seq: 0,
            id,
        }
    }
}

/// Runtime hooks bound to the layer (Go wiring: `rt.OnUpdate = s.nativeUpdate`
/// etc.). The layer is stored weakly: the runtime lives inside it.
struct CodegHooks {
    layer: Weak<Layer>,
}

impl RuntimeHooks for CodegHooks {
    fn on_update(&self, _session_id: &str, update: &Value) {
        if let Some(layer) = self.layer.upgrade() {
            // The runtime hands us the session record id (= runtime id view);
            // native_update resolves the owning connection.
            native_update_by_runtime(&layer, _session_id, update);
        }
    }
    fn extra_mcp(&self, agent: &Agent, options: &Options, runtime_id: &str) -> Vec<Value> {
        match self.layer.upgrade() {
            Some(layer) => layer.extra_mcp(agent, options, runtime_id),
            None => Vec::new(),
        }
    }
    fn allow_internal_tool(&self, runtime_id: &str, tool_call: &Value) -> bool {
        match self.layer.upgrade() {
            Some(layer) => {
                let state = layer.state.lock().unwrap();
                tool_identity::allow_internal_tool_layer(&state, runtime_id, tool_call)
            }
            None => false,
        }
    }
}

/// Emitter bridge: runtime events → the front-door sink.
struct SinkEmitter {
    sink: EmitSink,
}

impl Emitter for SinkEmitter {
    fn emit(&self, channel: &str, payload: &Value) {
        (self.sink)(channel, payload);
    }
}

// ----------------------------- construction -----------------------------

/// Which session state file the runtime uses (Go main.go:513-516).
fn state_file(root: &Path) -> Option<PathBuf> {
    if std::env::var("HARNESS_HUB_STATE_FILE")
        .map(|v| v == ":memory:")
        .unwrap_or(false)
    {
        return None;
    }
    let name = if std::env::var("HARNESS_HUB_CODEG")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        "codeg-sessions.json"
    } else {
        "sessions.json"
    };
    Some(root.join(".data").join(name))
}

fn timeout_ms() -> i64 {
    std::env::var("HARNESS_HUB_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(600_000)
}

/// Build the layer. The dispatcher calls this once (front door startup).
pub fn init_layer(root: PathBuf, base_url: String, sink: EmitSink) -> Arc<Layer> {
    // The delegation registry is restored from disk so a restart keeps the
    // tasks (Go `loadDelegationsLocked`).
    let layer = Arc::new(Layer {
        root: root.clone(),
        upstream_base: base_url,
        sink,
        runtime: OnceLock::new(),
        self_weak: OnceLock::new(),
        state: Mutex::new(LayerState {
            connections: HashMap::new(),
            delegations: HashMap::new(),
            warm: HashMap::new(),
            warming: HashSet::new(),
            started: std::env::var("HARNESS_HUB_WARM")
                .map(|v| v == "1")
                .unwrap_or(false),
            callers: HashMap::new(),
            questions: HashMap::new(),
            splits: HashMap::new(),
        }),
    });
    let _ = layer.self_weak.set(Arc::downgrade(&layer));
    {
        let mut state = layer.state.lock().unwrap();
        super::delegation::load(&root, &mut state.delegations);
    }
    let runtime = Runtime::new(RuntimeOptions {
        state_file: state_file(&root),
        timeout_ms: timeout_ms(),
        resolver: Arc::new(ProductionResolver { root }),
        emitter: Arc::new(SinkEmitter {
            sink: layer.sink.clone(),
        }),
        hooks: Arc::new(CodegHooks {
            layer: Arc::downgrade(&layer),
        }),
    });
    let _ = layer.runtime.set(runtime);
    splits::load(&layer.root, &mut layer.state.lock().unwrap().splits);
    // Go Server.Start(): permissionsLoop always, warm pool only when enabled.
    spawn_permissions_loop(layer.clone());
    if layer.state.lock().unwrap().started {
        tokio::spawn(warm_all(layer.clone()));
        tokio::spawn(warm_sweeper(layer.clone()));
    }
    layer
}

impl Layer {
    pub fn runtime(&self) -> Arc<Runtime> {
        self.runtime.get().expect("runtime installed").clone()
    }

    fn emit(&self, channel: &str, payload: &Value) {
        (self.sink)(channel, &goformat::canon(payload));
    }

    /// Public frame emitter for the command layer (install progress etc.).
    pub fn emit_event(&self, channel: &str, payload: &Value) {
        self.emit(channel, payload);
    }

    /// The connection entry for command handlers.
    /// The MCP companion authenticates with the caller's token.
    pub fn caller_for_token(&self, token: &str) -> Option<Caller> {
        if token.is_empty() {
            return None;
        }
        let state = self.state.lock().unwrap();
        state
            .callers
            .values()
            .find(|caller| caller.token == token)
            .cloned()
    }

    pub fn connection(&self, cid: &str) -> Option<Arc<Mutex<Connection>>> {
        self.state.lock().unwrap().connections.get(cid).cloned()
    }

    /// The runtime session id behind a connection (Go `c.RuntimeID`).
    pub fn runtime_id_of(&self, cid: &str) -> Result<String, String> {
        match self.connection(cid) {
            Some(conn) => Ok(conn.lock().unwrap().runtime_id.clone()),
            None => Err("连接不存在".into()),
        }
    }

    /// Go `acp_get_session_snapshot_by_conversation` / `find_connection…`
    /// (server.go:553): the non-warm connection bound to a conversation.
    pub fn find_connection_for_conversation(&self, conversation_id: i64) -> Option<String> {
        let state = self.state.lock().unwrap();
        let mut cid: Option<String> = None;
        for conn in state.connections.values() {
            let c = conn.lock().unwrap();
            // A warm pre-built connection has no conversation yet and can be
            // claimed (or released) at any moment.
            if c.conversation_id == conversation_id && !c.warm {
                cid = Some(c.id.clone());
            }
        }
        cid
    }

    /// Go `answerQuestion` (question.go:59): idempotent settle.
    pub fn answer_question(&self, args: &Value) {
        let qid = text(args.get("questionId"));
        let answer = match args.get("answer").and_then(Value::as_object) {
            Some(map) => Value::Object(map.clone()),
            None => question::declined_answer(),
        };
        let mut state = self.state.lock().unwrap();
        let Some(park) = state.questions.remove(&qid) else {
            return;
        };
        if let Some(conn) = state.connections.get(&park.conn_id) {
            let mut c = conn.lock().unwrap();
            if text(c.pending_question.get("question_id")) == qid {
                c.pending_question = Value::Null;
            }
        }
        drop(state);
        self.emit_event(
            &park.conn_id,
            &json!({"type": "question_resolved", "question_id": qid}),
        );
        let layer = self.self_arc();
        let conn_id = park.conn_id.clone();
        tokio::spawn(async move {
            layer.publish_session(&conn_id).await;
        });
        let _ = park.answer_tx.send(answer);
    }

    /// Go `respondTaskSplit` (split.go:446). Approve fans the plan out through
    /// the (unported) delegation engine — the splits registry can only be
    /// filled by the companion MCP, so on this side approve is unreachable in
    /// practice; reject/validation paths are faithful.
    pub async fn respond_task_split(&self, args: &Value) -> Result<Value, String> {
        let split_id = text(args.get("splitId"));
        let mut action = text(args.get("action"));
        if action.is_empty() {
            action = "approve".into();
        }
        let tasks_confirmed = args.get("tasks").and_then(Value::as_array).cloned();
        let (parent, outcome) = {
            let mut state = self.state.lock().unwrap();
            let Some(sp) = state.splits.get(&split_id).cloned() else {
                return Err("拆分提案不存在或已处理".into());
            };
            let Some(parent) = state.connections.get(&sp.parent).cloned() else {
                splits::clear_locked(&mut state, &split_id);
                let root = self.root.clone();
                let rows: Vec<Value> = state.splits.values().map(splits::split_wire).collect();
                let _ = tokio::task::spawn_blocking(move || splits::save(&root, &rows));
                return Err("父会话已断开".into());
            };
            if action == "approve" {
                if let Some(raw) = &tasks_confirmed {
                    splits::normalize_confirmed_tasks(raw)?;
                }
            }
            let _ = parent;
            (sp.parent, split_id.clone())
        };
        // Retire the card first, under one lock, so a second respond cannot
        // double-launch.
        let dropped = {
            let mut state = self.state.lock().unwrap();
            splits::clear_locked(&mut state, &split_id)
        };
        if dropped.is_some() {
            let root = self.root.clone();
            let rows: Vec<Value> = self
                .state
                .lock()
                .unwrap()
                .splits
                .values()
                .map(splits::split_wire)
                .collect();
            tokio::task::spawn_blocking(move || splits::save(&root, &rows));
        }
        if action == "reject" {
            self.event(
                &parent,
                json!({"type": "task_split_resolved", "split_id": split_id, "action": "reject"}),
            );
            return Ok(json!({"split_id": split_id, "action": "reject"}));
        }
        let _ = outcome;
        // Approve → delegation fan-out (D 组, unported). The card is already
        // retired, so the user's decision is not lost to a retry.
        Err("协作派发尚未移植：拆分提案已撤销，请重新提案后使用 delegate_to_agent 分工".into())
    }

    /// Go `s.event` (connections.go:372): connection-scoped bookkeeping plus
    /// the `acp://event` firehose frame.
    pub(crate) fn event(&self, connection_id: &str, e: Value) {
        let mut e = e;
        let state = self.state.lock().unwrap();
        let conn = match state.connections.get(connection_id) {
            Some(conn) => conn.clone(),
            None => return,
        };
        drop(state);
        let mut conn_state = conn.lock().unwrap();
        if conn_state.live.is_null() {
            conn_state.live = json!({
                "id": goformat::hex_id(),
                "role": "assistant",
                "content": [],
                "started_at": goformat::timestamp(),
            });
        }
        match text(e.get("type")).as_str() {
            "content_delta" => {
                let parent = text(e.get("parent_tool_use_id"));
                // ACP message IDs are optional: only the last non-empty text
                // chunk's id is retained.
                if !text(e.get("text")).is_empty() && parent.is_empty() {
                    let live = conn_state.live.as_object_mut().unwrap();
                    if text(live.get("created_at")).is_empty() {
                        live.insert("created_at".into(), json!(goformat::timestamp()));
                    }
                    let native = text(e.get("source_message_id"));
                    if !native.is_empty() {
                        live.insert("source_turn_id".into(), json!(native));
                    } else {
                        live.remove("source_turn_id");
                    }
                }
                if let Some(obj) = e.as_object_mut() {
                    obj.remove("source_message_id");
                }
                append_live_block(&mut conn_state.live, "text", &parent, &text(e.get("text")));
            }
            "thinking" => {
                let parent = text(e.get("parent_tool_use_id"));
                append_live_block(
                    &mut conn_state.live,
                    "thinking",
                    &parent,
                    &text(e.get("text")),
                );
            }
            "tool_call" | "tool_call_update" => {
                let id = text(e.get("tool_call_id"));
                let mut found = false;
                for tool in conn_state.tools.iter_mut() {
                    if text(tool.get("tool_call_id")) == id {
                        if let (Some(tool_obj), Some(e_obj)) = (tool.as_object_mut(), e.as_object())
                        {
                            for (k, v) in e_obj {
                                if !v.is_null() {
                                    tool_obj.insert(k.clone(), v.clone());
                                }
                            }
                        }
                        found = true;
                        break;
                    }
                }
                if !found {
                    conn_state.tools.push(e.clone());
                    if conn_state.live.is_null() {
                        conn_state.live = json!({
                            "id": goformat::hex_id(),
                            "role": "assistant",
                            "content": [],
                            "started_at": goformat::timestamp(),
                        });
                    }
                    if let Some(live) = conn_state.live.as_object_mut() {
                        let content = live
                            .entry("content")
                            .or_insert_with(|| json!([]))
                            .as_array_mut()
                            .expect("content array");
                        content.push(json!({"kind": "tool_call_ref", "tool_call_id": id}));
                    }
                }
            }
            _ => {}
        }
        conn_state.seq += 1;
        let seq = conn_state.seq;
        if let Some(obj) = e.as_object_mut() {
            obj.insert("seq".into(), json!(seq));
            obj.insert("connection_id".into(), json!(conn_state.id));
        }
        conn_state.events.push(e.clone());
        if conn_state.events.len() > 256 {
            let overflow = conn_state.events.len() - 256;
            conn_state.events.drain(0..overflow);
        }
        drop(conn_state);
        self.emit("acp://event", &e);
    }

    /// Go `runtimeView` (connections.go:775).
    pub async fn runtime_view(&self, runtime_id: &str) -> Value {
        for view in self.runtime().list_json("").await {
            if text(view.get("id")) == runtime_id {
                return view;
            }
        }
        json!({})
    }

    /// Go `publishSession` (connections.go:785).
    pub async fn publish_session(&self, cid: &str) {
        let (rid, agent, agent_type) = {
            let state = self.state.lock().unwrap();
            match state.connections.get(cid) {
                Some(conn) => {
                    let c = conn.lock().unwrap();
                    (c.runtime_id.clone(), c.agent.clone(), c.agent_type.clone())
                }
                None => return,
            }
        };
        let view = self.runtime_view(&rid).await;
        self.event(cid, json!({"type": "session_started", "session_id": view.get("nativeSessionId").cloned().unwrap_or(Value::Null)}));
        self.event(
            cid,
            json!({
                "type": "fork_supported",
                "supported": self.runtime().supports_point_fork(&rid, Some(&agent)),
            }),
        );
        self.event(
            cid,
            json!({"type": "prompt_capabilities", "prompt_capabilities": self.runtime().prompt_capabilities(&rid)}),
        );
        let modes = view.get("modes").cloned().unwrap_or(Value::Null);
        if !modes.is_null() {
            self.event(
                cid,
                json!({"type": "session_modes", "modes": mode_wire(&modes)}),
            );
        }
        self.event(
            cid,
            json!({
                "type": "session_config_options",
                "config_options": config_wire(&view),
                "agent_type": agent_type,
            }),
        );
    }

    /// Go `snapshot` (connections.go:812).
    pub async fn snapshot(&self, cid: &str) -> Value {
        let (
            conversation_id,
            folder_id,
            status,
            seq,
            live,
            tools,
            pending,
            pending_split,
            pending_question,
            feedback,
            rid,
            agent,
        ) = {
            let state = self.state.lock().unwrap();
            match state.connections.get(cid) {
                Some(conn) => {
                    let c = conn.lock().unwrap();
                    (
                        c.conversation_id,
                        c.folder_id,
                        c.status.clone(),
                        c.seq,
                        c.live.clone(),
                        c.tools.clone(),
                        c.pending.clone(),
                        c.pending_split.clone(),
                        c.pending_question.clone(),
                        c.feedback.clone(),
                        c.runtime_id.clone(),
                        c.agent.clone(),
                    )
                }
                None => return Value::Null,
            }
        };
        let view = self.runtime_view(&rid).await;
        let mut out = json!({
            "connection_id": cid,
            "conversation_id": conversation_id,
            "folder_id": folder_id,
            "status": status,
            "event_seq": seq,
            "live_message": live,
            "active_tool_calls": crate::backend::acp::wire::tool_state_wire(&tools),
            "pending_permission": crate::backend::acp::wire::nil_if_empty(&pending),
            "pending_split": crate::backend::acp::wire::nil_if_empty(&pending_split),
            "pending_question": crate::backend::acp::wire::nil_if_empty(&pending_question),
            "active_delegations": [],
            "feedback": feedback,
            "feedback_tool_available": true,
            "fork_supported": false,
            "available_commands": [],
            "selectors_ready": true,
            "config_options": [],
            "prompt_capabilities": {"image": false, "audio": false, "embedded_context": false},
        });
        // active delegations come from the (unported) delegation registry —
        // an empty list matches this layer's empty `tasks` map in Go terms.
        out["fork_supported"] = json!(self.runtime().supports_point_fork(&rid, Some(&agent)));
        out["prompt_capabilities"] = self.runtime().prompt_capabilities(&rid);
        out["native_steering_available"] = json!(self.runtime().supports_native_steering(&rid));
        out["external_id"] = view.get("nativeSessionId").cloned().unwrap_or(Value::Null);
        let modes = view.get("modes").cloned().unwrap_or(Value::Null);
        out["modes"] = mode_wire(&modes);
        out["config_options"] = json!(config_wire(&view));
        if let Some(modes_obj) = view.get("modes") {
            out["current_mode"] = modes_obj
                .get("currentModeId")
                .cloned()
                .unwrap_or(Value::Null);
        }
        out
    }

    /// Go `connectionList` (connections.go:763).
    pub fn connection_list(&self) -> Vec<Value> {
        let state = self.state.lock().unwrap();
        let mut out = Vec::new();
        for conn in state.connections.values() {
            let c = conn.lock().unwrap();
            if c.warm {
                continue;
            }
            out.push(json!({"id": c.id, "agent_type": c.agent_type, "status": c.status}));
        }
        out
    }

    /// Go `extraMCP` (delegation.go:42): the codeg-mcp companion server entry
    /// handed to the adapter, keyed by a fresh caller token.
    fn extra_mcp(&self, agent: &Agent, o: &Options, runtime_id: &str) -> Vec<Value> {
        // A custom agent without MCP support carries no companion.
        if crate::backend::acp::agents::custom_def_for_agent(&self.root, agent)
            .map(|d| !d.supports_mcp)
            .unwrap_or(false)
        {
            return Vec::new();
        }
        let mut state = self.state.lock().unwrap();
        let conn = state
            .connections
            .values()
            .find(|conn| conn.lock().unwrap().options.session_key == o.session_key)
            .cloned();
        let Some(conn) = conn else { return Vec::new() };
        {
            let mut c = conn.lock().unwrap();
            c.runtime_id = runtime_id.to_string();
            let cid = c.id.clone();
            let root_id = if c.root_id.is_empty() {
                c.id.clone()
            } else {
                c.root_id.clone()
            };
            let depth = c.depth;
            if !state.callers.contains_key(&cid) {
                let token = format!("{}{}", goformat::hex_id(), goformat::hex_id());
                state.callers.insert(
                    cid.clone(),
                    Caller {
                        token,
                        connection_id: cid.clone(),
                        root_id,
                        depth,
                    },
                );
            }
        }
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let token = state
            .callers
            .get(&conn.lock().unwrap().id)
            .map(|c| c.token.clone())
            .unwrap_or_default();
        vec![json!({
            "name": "codeg-mcp",
            "command": exe,
            "args": ["mcp-codeg", format!("{}/api/codeg_mcp", self.upstream_base), token],
            "env": [],
        })]
    }

    /// Go `bindConversation` (delegation_recovery.go:30): rebind the
    /// conversation linkage (root/depth from the store tree).
    fn bind_conversation(&self, conn: &Arc<Mutex<Connection>>, conv: i64) {
        if conv == 0 {
            return;
        }
        let (root, depth, folder) = {
            let doc = store_file::read_json(&self.root.join(".data").join("codeg-workspace.json"))
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
            tree_root(&doc, conv, folder_of(&doc, conv))
        };
        let mut state = self.state.lock().unwrap();
        let Some(conn) = state.connections.get(&conn.lock().unwrap().id).cloned() else {
            return;
        };
        let mut c = conn.lock().unwrap();
        c.conversation_id = conv;
        c.folder_id = folder;
        if c.depth == 0 || c.root_id.is_empty() {
            c.root_id = format!("conversation:{root}");
            c.depth = depth;
        }
        let cid = c.id.clone();
        let root_id = c.root_id.clone();
        drop(c);
        if let Some(caller) = state.callers.get_mut(&cid) {
            caller.root_id = root_id;
            caller.depth = depth;
        }
    }
}

fn folder_of(doc: &Value, conv: i64) -> i64 {
    for row in doc
        .get("conversations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if crate::backend::acp::wire::number(row.get("id")) == conv {
            return crate::backend::acp::wire::number(row.get("folder_id"));
        }
    }
    0
}

/// Go `treeRootLocked` (delegation_recovery.go:12): walk parent links to the
/// conversation tree root, counting depth.
fn tree_root(doc: &Value, conv: i64, folder: i64) -> (i64, i64, i64) {
    let mut conv = conv;
    let mut depth = 0i64;
    let mut seen: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    let mut folder = folder;
    loop {
        if seen.contains(&conv) {
            return (conv, depth, folder);
        }
        seen.insert(conv);
        let mut row = None;
        for candidate in doc
            .get("conversations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if crate::backend::acp::wire::number(candidate.get("id")) == conv {
                row = Some(candidate.clone());
                break;
            }
        }
        let Some(r) = row else {
            return (conv, depth, folder);
        };
        let parent = crate::backend::acp::wire::number(r.get("parent_id"));
        if parent == 0 {
            return (conv, depth, folder);
        }
        conv = parent;
        depth += 1;
    }
}

fn append_live_block(live: &mut Value, kind: &str, parent: &str, text_chunk: &str) {
    let Some(live_obj) = live.as_object_mut() else {
        return;
    };
    let content = live_obj
        .entry("content")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("content array");
    let extend = match content.last() {
        Some(last)
            if text(last.get("kind")) == kind && text(last.get("parent_tool_use_id")) == parent =>
        {
            true
        }
        _ => {
            content.push(json!({"kind": kind, "text": "", "parent_tool_use_id": parent}));
            true
        }
    };
    if extend {
        if let Some(last) = content.last_mut() {
            let existing = text(last.get("text"));
            last["text"] = json!(format!("{existing}{text_chunk}"));
        }
    }
}

// ----------------------------- workdir resolution -----------------------------

/// Go `resolveWorkdir` (state.go:101): constrain workdirs under allowedRoot
/// (lexical + symlink).
pub fn resolve_workdir(root: &Path, input: &str) -> Result<PathBuf, String> {
    let mut input = input.to_string();
    if input.is_empty() {
        input = std::env::var("HARNESS_HUB_WORKDIR").unwrap_or_default();
        if input.is_empty() {
            input = root.to_string_lossy().to_string();
        }
    }
    let candidate =
        normalize(&std::path::absolute(&input).map_err(|e| format!("无法解析工作目录：{e}"))?);
    let allowed_root = std::env::var("HARNESS_HUB_ALLOWED_ROOT")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf());
    let real_root = std::fs::canonicalize(&allowed_root)
        .map(|p| normalize(&p))
        .unwrap_or(allowed_root.clone());
    // Resolve the deepest existing ancestor.
    let mut ancestor = candidate.clone();
    loop {
        if ancestor.exists() {
            break;
        }
        match ancestor.parent() {
            Some(parent) if parent != ancestor => ancestor = parent.to_path_buf(),
            _ => break,
        }
    }
    let mut resolved = std::fs::canonicalize(&ancestor)
        .map(|p| normalize(&p))
        .unwrap_or_else(|_| ancestor.clone());
    if resolved != ancestor {
        let rel = relative(&ancestor, &candidate)?;
        resolved = resolved.join(rel);
    }
    if std::env::var("HARNESS_HUB_ALLOW_ANY_PATH")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return Ok(resolved);
    }
    let rel = relative(&real_root, &resolved)?;
    if rel == ".." || rel.starts_with("..\\") || rel.starts_with("../") || is_abs_rel(&rel) {
        return Err(format!(
            "工作目录必须位于 {} 之内",
            real_root.to_string_lossy()
        ));
    }
    Ok(resolved)
}

fn normalize(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

fn relative(base: &Path, target: &Path) -> Result<String, String> {
    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();
    if base_components == target_components {
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
        return Err(format!("路径不在同一根目录：{base:?} 与 {target:?}"));
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in 0..(base_components.len() - common) {
        parts.push("..".into());
    }
    for c in &target_components[common..] {
        parts.push(c.as_os_str().to_string_lossy().to_string());
    }
    Ok(parts.join("\\"))
}

fn is_abs_rel(rel: &str) -> bool {
    let bytes = rel.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

// ----------------------------- attachments -----------------------------

/// Go `resourceLink` (attachments.go:19).
pub fn resource_link(block: &Value) -> Result<(Value, Value), String> {
    let uri = text(block.get("uri")).trim().to_string();
    let name = text(block.get("name")).trim().to_string();
    if uri.is_empty() || name.is_empty() || uri.contains('\r') || uri.contains('\n') {
        return Err("资源引用无效".into());
    }
    let mut wire = json!({"type": "resource_link", "uri": uri, "name": name});
    let mime = text(block.get("mime_type"));
    if !mime.is_empty() {
        wire["mimeType"] = json!(mime);
    }
    let description = text(block.get("description"));
    if !description.is_empty() {
        wire["description"] = json!(description);
    }
    let label = name.replace('[', "\\[").replace(']', "\\]");
    let target = uri.replace('(', "%28").replace(')', "%29");
    let mut stored = json!({"type": "resource_link", "uri": uri, "name": name});
    if !mime.is_empty() {
        stored["mime_type"] = json!(mime);
    }
    if !description.is_empty() {
        stored["description"] = json!(description);
    }
    let display = json!({
        "type": "text",
        "text": format!("[{label}]({target})"),
        "source_prompt_block": stored,
    });
    Ok((wire, display))
}

/// Go `embeddedResource` (attachments.go:46).
pub fn embedded_resource(root: &Path, block: &Value) -> Result<(Value, Value), String> {
    let uri = text(block.get("uri")).trim().to_string();
    if uri.is_empty() || uri.contains('\r') || uri.contains('\n') {
        return Err("嵌入资源地址无效".into());
    }
    let mime = text(block.get("mime_type"));
    let mut resource = json!({"uri": uri});
    if !mime.is_empty() {
        resource["mimeType"] = json!(mime);
    }
    let mut display = Value::Null;
    let text_payload = block.get("text").and_then(Value::as_str);
    if let Some(payload) = text_payload {
        if payload.len() > MAX_ATTACHMENT_BYTES {
            return Err("嵌入资源过大".into());
        }
        resource["text"] = json!(payload);
    } else if let Some(blob) = block.get("blob").and_then(Value::as_str) {
        let data = image_data(root, &json!({"data": blob, "uri": uri}))?;
        resource["blob"] = json!(data);
        if mime.starts_with("image/") {
            display = json!({"type": "image", "data": data, "mime_type": mime, "uri": uri});
        }
    } else {
        return Err("嵌入资源缺少内容".into());
    }
    if display.is_null() {
        let name = Path::new(uri.trim_end_matches('/'))
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let (_, display_block) = resource_link(&json!({"uri": uri, "name": name}))?;
        display = display_block;
    }
    let mut stored = json!({"type": "resource", "uri": uri});
    if !mime.is_empty() {
        stored["mime_type"] = json!(mime);
    }
    if let Some(t) = resource.get("text") {
        stored["text"] = t.clone();
    }
    if let Some(blob) = resource.get("blob") {
        if display.get("type").and_then(Value::as_str) != Some("image") {
            stored["blob"] = blob.clone();
        }
    }
    display["source_prompt_block"] = stored;
    Ok((json!({"type": "resource", "resource": resource}), display))
}

/// Go `imageData` (attachments.go:96): inline base64 or a file:// URL inside
/// the upload directory.
pub fn image_data(root: &Path, block: &Value) -> Result<String, String> {
    let data = text(block.get("data"));
    if !data.is_empty() {
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&data)
            .map_err(|_| "图片数据无效或过大".to_string())?;
        if decoded.is_empty() || decoded.len() > MAX_ATTACHMENT_BYTES {
            return Err("图片数据无效或过大".into());
        }
        return Ok(data);
    }
    let uri = text(block.get("uri"));
    let u = url::Url::parse(&uri).map_err(|_| "图片上传地址无效".to_string())?;
    if u.scheme() != "file"
        || (!u.host_str().unwrap_or("").is_empty() && u.host_str() != Some("localhost"))
    {
        return Err("图片上传地址无效".to_string());
    }
    let mut path = u.path().to_string();
    // Windows file URLs spell drives as /C:/...
    if path.len() >= 3 && path.starts_with('/') && path.as_bytes()[2] == b':' {
        path = path[1..].to_string();
    }
    let uploads =
        std::path::absolute(root.join(".data").join("uploads")).map_err(|e| e.to_string())?;
    let path = std::path::absolute(Path::new(&path)).map_err(|e| e.to_string())?;
    let uploads = std::fs::canonicalize(&uploads)
        .map(|p| normalize(&p))
        .map_err(|e| e.to_string())?;
    let path = std::fs::canonicalize(&path)
        .map(|p| normalize(&p))
        .map_err(|e| e.to_string())?;
    let rel = relative(&uploads, &path)?;
    if rel == ".." || rel.starts_with("..\\") || rel.starts_with("../") || is_abs_rel(&rel) {
        return Err("图片不在上传目录中".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err("图片为空或过大".into());
    }
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

// ----------------------------- connect -----------------------------

impl Layer {
    /// Go `connect` (connections.go:65).
    pub async fn connect(&self, args: &Value) -> Result<String, String> {
        let agent_type = text(args.get("agentType"));
        let agent = crate::backend::acp::agents::pick(&self.root, &agent_type)
            .ok_or_else(|| format!("执行端 {agent_type} 未连接"))?;
        let cwd = resolve_workdir(&self.root, &text(args.get("workingDir")))?;
        if agent_type == "pi" || agent.wire == "pi" || agent.real_tool == "pi" {
            crate::backend::acp::pi_trust::check_launch(&self.root, &cwd)?;
        }
        let mut permission = "readonly".to_string();
        // DSH advertises model configuration but no ACP permission modes; its
        // native policy owns tools, so the session is not enforced read-only.
        if agent.real_tool == "dsh" {
            permission = "workspace".into();
        }
        let mut mode = text(args.get("preferredModeId"));
        if let Some(config) = args.get("preferredConfigValues") {
            let config_mode = text(config.get("mode"));
            if !config_mode.is_empty() {
                mode = config_mode;
            }
        }
        let level = crate::backend::acp::runtime::mode_level(&mode);
        if level == "workspace" || level == "full" {
            permission = "workspace".into();
        }
        let mut key = format!("codeg:{}", goformat::hex_id());
        let mut model = String::new();
        if let Some(config) = args.get("preferredConfigValues") {
            model = text(config.get("model"));
        }
        if model.is_empty() {
            model = agent.default_model.clone();
        }
        if agent.real_tool == "dsh" {
            mode.clear();
            // Pinned DSH profiles select provider+model in their per-process patch.
            if !agent.provider_id.is_empty() {
                model.clear();
            }
        }
        let mut options = Options {
            session_key: key.clone(),
            permission: permission.clone(),
            workdir: cwd.to_string_lossy().to_string(),
            model: model.clone(),
            mode: mode.clone(),
            structured: false,
            role: "manager".into(),
            ..Options::default()
        };
        let cid = goformat::hex_id();
        let mut conn = Connection::new(cid.clone(), agent.clone(), options.clone());
        conn.depth = crate::backend::acp::wire::number(args.get("_depth"));
        conn.root_id = text(args.get("_root"));
        if conn.depth > 0 {
            options.role = "worker".into();
            conn.options.role = "worker".into();
        }
        let previous = text(args.get("sessionId"));
        if !previous.is_empty() {
            // Reuse only a session already owned by this runtime; native
            // history import has its own adapter-specific path.
            let views = self.runtime().list_json("").await;
            let mut found = false;
            for view in &views {
                if text(view.get("nativeSessionId")) == previous
                    && text(view.get("agentId")) == agent.id
                {
                    // Keep the record's identity after an in-session mode change.
                    let view_permission = text(view.get("permission"));
                    permission = view_permission.clone();
                    conn.options.permission = view_permission.clone();
                    options.permission = view_permission.clone();
                    let saved_mode = text(view.get("nativeMode"));
                    if !saved_mode.is_empty() {
                        conn.options.mode = saved_mode.clone();
                        options.mode = saved_mode;
                    }
                    // One native stream has one owner: reconnecting another
                    // window must not create two wrappers.
                    let mut state = self.state.lock().unwrap();
                    let mut reused = None;
                    for existing in state.connections.values() {
                        let c = existing.lock().unwrap();
                        if c.runtime_id == text(view.get("id"))
                            && c.options.workdir == options.workdir
                            && c.options.permission == options.permission
                        {
                            reused = Some(c.id.clone());
                        }
                    }
                    if let Some(cid) = reused {
                        if let Some(existing) = state.connections.get(&cid) {
                            let mut c = existing.lock().unwrap();
                            if c.status == "disconnected" {
                                c.status = "connected".into();
                            }
                        }
                        drop(state);
                        return Ok(cid);
                    }
                    drop(state);
                    key = text(view.get("key"));
                    if let Some(idx) = key.rfind(&format!("|{}", agent.id)) {
                        if idx > 0 {
                            key.truncate(idx);
                        }
                    }
                    let suffix = format!("|{}", options.workdir);
                    if key.ends_with(&suffix) {
                        key.truncate(key.len() - suffix.len());
                    }
                    conn.options.session_key = key.clone();
                    options.session_key = key.clone();
                    found = true;
                    break;
                }
            }
            if !found {
                return Err("未导入此原生会话，不能无上下文恢复".into());
            }
            let mut conversation_id = 0i64;
            {
                let doc =
                    store_file::read_json(&self.root.join(".data").join("codeg-workspace.json"))
                        .ok()
                        .flatten()
                        .unwrap_or(Value::Null);
                for row in doc
                    .get("conversations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if text(row.get("external_id")) == previous
                        && text(row.get("agent_type")) == conn.agent_type
                    {
                        conversation_id = crate::backend::acp::wire::number(row.get("id"));
                        break;
                    }
                }
            }
            let conn_arc = Arc::new(Mutex::new(conn));
            self.bind_conversation(&conn_arc, conversation_id);
            // continue with the bound connection below
            return self
                .connect_finish(conn_arc, &agent, &options, args, &permission)
                .await;
        }
        let pool_key = self.warm_pool_key(
            &conn.agent_type,
            &options.workdir,
            &permission,
            &mode,
            &model,
        );
        let warm = args.get("_warm").and_then(Value::as_bool).unwrap_or(false);
        if !warm && previous.is_empty() && conn.depth == 0 && conn.root_id.is_empty() {
            if let Some(claimed) = self.take_warm(&pool_key).await {
                self.queue_warm(args, &pool_key);
                return Ok(claimed);
            }
        }
        conn.warm = warm;
        let conn_arc = Arc::new(Mutex::new(conn));
        self.state
            .lock()
            .unwrap()
            .connections
            .insert(cid.clone(), conn_arc.clone());
        self.connect_finish(conn_arc, &agent, &options, args, &permission)
            .await
    }

    /// The second half of Go `connect` (lines 179-197): the runtime session
    /// bring-up, warm parking, and the initial event batch.
    async fn connect_finish(
        &self,
        conn_arc: Arc<Mutex<Connection>>,
        agent: &Agent,
        options: &Options,
        args: &Value,
        _permission: &str,
    ) -> Result<String, String> {
        let native = match self.runtime().connect(agent, options.clone()).await {
            Ok(native) => native,
            Err(err) => {
                let cid = conn_arc.lock().unwrap().id.clone();
                let mut state = self.state.lock().unwrap();
                state.connections.remove(&cid);
                state.callers.remove(&cid);
                return Err(err);
            }
        };
        let warm = args.get("_warm").and_then(Value::as_bool).unwrap_or(false);
        let pool_key = {
            let c = conn_arc.lock().unwrap();
            self.warm_pool_key(
                &c.agent_type,
                &options.workdir,
                &options.permission,
                &options.mode,
                &options.model,
            )
        };
        {
            let mut c = conn_arc.lock().unwrap();
            c.runtime_id = native;
            c.status = "connected".into();
        }
        if warm {
            let cid = conn_arc.lock().unwrap().id.clone();
            self.put_warm(&pool_key, &cid);
            return Ok(cid);
        }
        let cid = conn_arc.lock().unwrap().id.clone();
        self.event(
            &cid,
            json!({"type": "status_changed", "status": "connected"}),
        );
        self.publish_session(&cid).await;
        self.queue_warm(args, &pool_key);
        Ok(cid)
    }

    // ----------------------------- warm pool -----------------------------

    /// Go `warmPoolKey` (connections.go:204).
    fn warm_pool_key(
        &self,
        agent_type: &str,
        cwd: &str,
        permission: &str,
        mode: &str,
        model: &str,
    ) -> String {
        format!("{agent_type}|{cwd}|{permission}|{mode}|{model}")
    }

    /// Go `takeWarm` (connections.go:210).
    async fn take_warm(&self, key: &str) -> Option<String> {
        loop {
            let e = {
                let mut state = self.state.lock().unwrap();
                let entries = state.warm.get_mut(key)?;
                if entries.is_empty() {
                    return None;
                }
                entries.remove(0)
            };
            if e.placed_at.elapsed() > WARM_IDLE_TTL {
                // Nobody claimed it in time; free it instead of pinning a
                // harness process with a session no conversation will use.
                self.release_warm(&e.id);
                continue;
            }
            let valid = {
                let state = self.state.lock().unwrap();
                match state.connections.get(&e.id) {
                    Some(conn) => {
                        let mut c = conn.lock().unwrap();
                        let warm = c.warm;
                        let valid =
                            warm && c.status == "connected" && !c.busy && c.conversation_id == 0;
                        if valid {
                            c.warm = false;
                        }
                        valid
                    }
                    None => false,
                }
            };
            if !valid {
                // Retire only entries still marked warm (idle ones that went
                // stale/busy); a connection whose warm flag is gone was claimed
                // by a real conversation.
                let still_warm = {
                    let state = self.state.lock().unwrap();
                    state
                        .connections
                        .get(&e.id)
                        .map(|c| c.lock().unwrap().warm)
                        .unwrap_or(false)
                };
                if still_warm {
                    self.release_warm(&e.id);
                }
                continue;
            }
            self.event(
                &e.id,
                json!({"type": "status_changed", "status": "connected"}),
            );
            self.publish_session(&e.id).await;
            return Some(e.id);
        }
    }

    /// Go `putWarm` (connections.go:252).
    fn put_warm(&self, key: &str, cid: &str) {
        let mut state = self.state.lock().unwrap();
        if state.warm.get(key).map(|l| !l.is_empty()).unwrap_or(false) {
            drop(state);
            self.release_warm(cid);
            return;
        }
        state
            .warm
            .entry(key.to_string())
            .or_default()
            .push(WarmEntry {
                id: cid.to_string(),
                placed_at: std::time::Instant::now(),
            });
    }

    /// Go `retireAgentConnections` (instances.go:350): release every warm
    /// entry belonging to one agent wire. Live conversations keep their driver.
    pub fn retire_agent(&self, wire: &str) {
        let cids: Vec<String> = {
            let state = self.state.lock().unwrap();
            state
                .connections
                .iter()
                .filter(|(_, conn)| {
                    let conn = conn.lock().unwrap();
                    conn.warm && wire_agent(&conn.agent) == wire
                })
                .map(|(cid, _)| cid.clone())
                .collect()
        };
        for cid in cids {
            self.release_warm(&cid);
        }
    }

    /// Go `get_codeg_mcp_service_status` 的计数面：(companions, sessions, active)。
    pub fn service_counts(&self) -> (usize, usize, usize) {
        let state = self.state.lock().unwrap();
        let companions = state.callers.len();
        let sessions = state.connections.len();
        (companions, sessions, 0)
    }

    /// Go `Runtime.KillDriversAt`：工作树目录下的驱动进程不再需要。
    pub fn kill_drivers_at(&self, dir: &str) {
        let runtime = self.runtime();
        let dir = dir.to_string();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { runtime.kill_drivers_at(&dir).await });
        }
    }

    /// 断连的同步包装（discard 路径在阻塞上下文里跑，不能 await）。
    pub fn cancel_and_disconnect_later(&self, cid: &str) {
        let layer = self.self_weak.get().and_then(|weak| weak.upgrade());
        let cid = cid.to_string();
        if let (Some(layer), Ok(handle)) = (layer, tokio::runtime::Handle::try_current()) {
            handle.spawn(async move {
                let _ = layer.disconnect(&cid).await;
            });
        }
    }

    /// 前端门的上游地址（Go 的 `s.Base`）：mcp socket_path 拼在它后面。
    pub fn upstream_base(&self) -> &str {
        &self.upstream_base
    }

    /// Go `releaseWarm` (connections.go:266): retire an idle pre-built
    /// connection for good; the shared driver process stays alive.
    fn release_warm(&self, cid: &str) {
        {
            let mut state = self.state.lock().unwrap();
            for list in state.warm.values_mut() {
                list.retain(|e| e.id != cid);
            }
            if let Some(conn) = state.connections.get(cid) {
                conn.lock().unwrap().warm = false;
            }
            let runtime_id = state
                .connections
                .get(cid)
                .map(|c| c.lock().unwrap().runtime_id.clone())
                .unwrap_or_default();
            state.connections.remove(cid);
            drop(state);
            if !runtime_id.is_empty() {
                self.runtime().forget_session(&runtime_id);
            }
            return;
        }
    }

    /// Go `warmSweeper` (connections.go:294).
    async fn sweep_warm(&self) {
        let expired: Vec<String> = {
            let mut state = self.state.lock().unwrap();
            let now = std::time::Instant::now();
            let mut expired = Vec::new();
            for list in state.warm.values_mut() {
                let keep: Vec<WarmEntry> = list
                    .drain(..)
                    .filter(|e| {
                        if now.duration_since(e.placed_at) > WARM_IDLE_TTL {
                            expired.push(e.id.clone());
                            false
                        } else {
                            true
                        }
                    })
                    .collect();
                *list = keep;
            }
            expired
        };
        for cid in expired {
            self.release_warm(&cid);
        }
    }

    /// Go `queueWarm` (connections.go:322): pre-build one idle connection with
    /// the exact parameters of a successful connect; never blocks.
    fn queue_warm(&self, args: &Value, pool_key: &str) {
        let started = self.state.lock().unwrap().started;
        if !started
            || std::env::var("HARNESS_HUB_WARM")
                .map(|v| v == "0")
                .unwrap_or(false)
        {
            return;
        }
        let mut next = args.clone();
        if let Some(obj) = next.as_object_mut() {
            obj.remove("sessionId");
            obj.remove("_depth");
            obj.remove("_root");
            obj.insert("_warm".into(), json!(true));
        }
        let dedup = pool_key.to_string();
        {
            let mut state = self.state.lock().unwrap();
            if state.warming.contains(&dedup) {
                return;
            }
            state.warming.insert(dedup.clone());
        }
        let layer = self.self_arc();
        tokio::spawn(async move {
            let outcome = layer.connect(&next).await;
            if let Err(err) = outcome {
                eprintln!("[warm] 预建连接失败：{err}");
            }
            let mut state = layer.state.lock().unwrap();
            state.warming.remove(&dedup);
        });
    }

    /// Go `warmAll` (connections.go:358): one idle session per installed
    /// harness at startup, serial on purpose.
    async fn run_warm_all(&self) {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        for agent in registry(&self.root) {
            if !agent.enabled || !agent.real {
                continue;
            }
            if std::env::var("HARNESS_HUB_WARM")
                .map(|v| v == "0")
                .unwrap_or(false)
            {
                return;
            }
            if let Err(err) = self
                .connect(&json!({"agentType": wire_agent(&agent), "workingDir": "", "_warm": true}))
                .await
            {
                eprintln!("[warm] 启动预热失败：{} {err}", wire_agent(&agent));
            }
        }
    }

    pub(crate) fn self_arc(&self) -> Arc<Layer> {
        self.self_weak
            .get()
            .expect("layer self reference")
            .upgrade()
            .expect("layer alive")
    }
}

async fn warm_all(layer: Arc<Layer>) {
    layer.run_warm_all().await;
}

async fn warm_sweeper(layer: Arc<Layer>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(30));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        layer.sweep_warm().await;
    }
}

// ----------------------------- permission loop -----------------------------

fn spawn_permissions_loop(layer: Arc<Layer>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(200));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let connections: Vec<Arc<Mutex<Connection>>> = {
                let state = layer.state.lock().unwrap();
                state.connections.values().cloned().collect()
            };
            for conn in connections {
                let (runtime_id, cid) = {
                    let c = conn.lock().unwrap();
                    (c.runtime_id.clone(), c.id.clone())
                };
                if runtime_id.is_empty() {
                    continue;
                }
                let view = layer.runtime_view(&runtime_id).await;
                let cards = view
                    .get("permissions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for card in cards {
                    let pid = text(card.get("id"));
                    let card_state = text(card.get("state"));
                    let previous = {
                        let mut c = conn.lock().unwrap();
                        let previous = c.seen_permissions.get(&pid).cloned().unwrap_or_default();
                        c.seen_permissions.insert(pid.clone(), card_state.clone());
                        previous
                    };
                    if previous == card_state {
                        continue;
                    }
                    if card_state == "pending" {
                        let payload = json!({
                            "request_id": pid,
                            "tool_call": card.get("toolCall").cloned().unwrap_or(Value::Null),
                            "options": permission_options(card.get("options")),
                            "created_at": goformat::timestamp(),
                            "queued": 0,
                        });
                        {
                            let mut c = conn.lock().unwrap();
                            c.pending = payload.clone();
                        }
                        layer.event(
                            &cid,
                            json!({
                                "type": "permission_request",
                                "request_id": pid,
                                "tool_call": card.get("toolCall").cloned().unwrap_or(Value::Null),
                                "options": permission_options(card.get("options")),
                                "queued": 0,
                            }),
                        );
                    } else if previous == "pending" {
                        {
                            let mut c = conn.lock().unwrap();
                            c.pending = Value::Null;
                        }
                        layer.event(
                            &cid,
                            json!({
                                "type": "permission_resolved",
                                "request_id": pid,
                                "option_id": card.get("selected").cloned().unwrap_or(Value::Null),
                            }),
                        );
                    }
                }
            }
        }
    });
}

// ----------------------------- native updates -----------------------------

/// Go `nativeUpdate` (connections.go:452): runtime session updates →
/// connection events. `_session_id` is the runtime session id here.
fn native_update_by_runtime(layer: &Arc<Layer>, runtime_id: &str, u: &Value) {
    let cid = {
        let state = layer.state.lock().unwrap();
        let mut cid = String::new();
        for conn in state.connections.values() {
            let c = conn.lock().unwrap();
            if c.runtime_id == runtime_id {
                cid = c.id.clone();
                break;
            }
        }
        cid
    };
    if cid.is_empty() {
        return;
    }
    let meta = u.get("_meta").cloned().unwrap_or(Value::Null);
    let claude = meta.get("claudeCode").cloned().unwrap_or(Value::Null);
    let parent = text(claude.get("parentToolUseId"));
    match text(u.get("sessionUpdate")).as_str() {
        "agent_message_chunk" => {
            let content = u.get("content").cloned().unwrap_or(Value::Null);
            layer.event(
                &cid,
                json!({
                    "type": "content_delta",
                    "text": text(content.get("text")),
                    "source_message_id": u.get("messageId").cloned().unwrap_or(Value::Null),
                    "parent_tool_use_id": parent,
                }),
            );
        }
        "agent_thought_chunk" => {
            let content = u.get("content").cloned().unwrap_or(Value::Null);
            layer.event(
                &cid,
                json!({
                    "type": "thinking",
                    "text": text(content.get("text")),
                    "parent_tool_use_id": parent,
                }),
            );
        }
        "tool_call" | "tool_call_update" => {
            let mut event = json!({
                "type": text(u.get("sessionUpdate")),
                "tool_call_id": u.get("toolCallId").cloned().unwrap_or(Value::Null),
                "title": u.get("title").cloned().unwrap_or(Value::Null),
                "kind": u.get("kind").cloned().unwrap_or(Value::Null),
                "status": u.get("status").cloned().unwrap_or(Value::Null),
                "locations": u.get("locations").cloned().unwrap_or(Value::Null),
                "raw_input": Value::Null,
                "raw_output": Value::Null,
                "content": Value::Null,
                "meta": u.get("_meta").cloned().unwrap_or(Value::Null),
            });
            for key in ["rawInput", "rawOutput", "content"] {
                if let Some(value) = u.get(key) {
                    let wire = match key {
                        "rawInput" => "raw_input",
                        "rawOutput" => "raw_output",
                        _ => "content",
                    };
                    event[wire] = match value.as_str() {
                        Some(s) => json!(s),
                        None => json!(serde_json::to_string(value).unwrap_or_default()),
                    };
                }
            }
            layer.event(&cid, event);
        }
        "plan" => {
            layer.event(&cid, json!({"type": "plan_update", "entries": u.get("entries").cloned().unwrap_or(Value::Null)}));
        }
        "notice" => {
            layer.event(
                &cid,
                json!({
                    "type": "session_notice",
                    "notice": {
                        "severity": u.get("severity").cloned().unwrap_or(Value::Null),
                        "title": u.get("title").cloned().unwrap_or(Value::Null),
                        "description": u.get("description").cloned().unwrap_or(Value::Null),
                    },
                }),
            );
        }
        "config_option_update" | "current_mode_update" => {
            // publish_session reads the runtime view (async); the sync hook
            // path defers it. These updates are isolated events, so the
            // deferred re-publish cannot interleave with turn data.
            let layer = layer.clone();
            let cid = cid.clone();
            tokio::spawn(async move {
                layer.publish_session(&cid).await;
            });
        }
        _ => {}
    }
}

// ----------------------------- prompt -----------------------------

impl Layer {
    /// Go `prompt` (connections.go:497).
    /// Runs one turn. The assistant's output text is returned as well: the
    /// delegation domain records it as the child task's result.
    pub async fn prompt(&self, args: &Value) -> Result<String, String> {
        let cid = text(args.get("connectionId"));
        let conn = self
            .state
            .lock()
            .unwrap()
            .connections
            .get(&cid)
            .cloned()
            .ok_or("连接不存在")?;
        // Busy handling (connections.go:505-522): the runtime is the source of
        // truth — an active turn means real work; a stale busy flag past the
        // grace window with no active turn is a leak to heal in place.
        {
            let (busy, rid, since) = {
                let c = conn.lock().unwrap();
                (c.busy, c.runtime_id.clone(), c.run_accepted)
            };
            if busy {
                let stale = !self.runtime().has_active_turn(&rid)
                    && since
                        .map(|at| at.elapsed() >= BUSY_STALE_GRACE)
                        .unwrap_or(false);
                let c = conn.lock().unwrap();
                if c.busy && (!stale || c.run_accepted != since) {
                    return Err("turn_in_progress".into());
                }
            }
        }
        let my_seq = {
            let mut c = conn.lock().unwrap();
            c.busy = true;
            c.status = "prompting".into();
            c.run_seq += 1;
            let my_seq = c.run_seq;
            c.run_accepted = Some(std::time::Instant::now());
            c.live = Value::Null;
            c.tools.clear();
            c.feedback.clear();
            c.tool_claims.clear();
            my_seq
        };
        // blocks parsing (connections.go:539-589)
        let mut prompt_text = String::new();
        let mut wire_content: Vec<Value> = Vec::new();
        let mut attachment_count = 0usize;
        let mut user_blocks: Vec<Value> = Vec::new();
        for raw in args
            .get("blocks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let block = raw.as_object().cloned().unwrap_or_default();
            let block = Value::Object(block);
            match text(block.get("type")).as_str() {
                "text" => {
                    let t = text(block.get("text"));
                    prompt_text.push_str(&t);
                    wire_content.push(json!({"type": "text", "text": t}));
                    user_blocks.push(json!({"type": "text", "text": t}));
                }
                "image" => {
                    let caps = {
                        let rid = conn.lock().unwrap().runtime_id.clone();
                        self.runtime().prompt_capabilities(&rid)
                    };
                    if !caps.get("image").and_then(Value::as_bool).unwrap_or(false) {
                        self.release_run(&conn, my_seq);
                        return Err("当前执行端不支持图片输入".into());
                    }
                    let mime = text(block.get("mime_type"));
                    if !mime.starts_with("image/") {
                        self.release_run(&conn, my_seq);
                        return Err("图片类型无效".into());
                    }
                    let data = match image_data(&self.root, &block) {
                        Ok(data) => data,
                        Err(err) => {
                            self.release_run(&conn, my_seq);
                            return Err(err);
                        }
                    };
                    wire_content.push(json!({"type": "image", "data": data, "mimeType": mime}));
                    attachment_count += 1;
                    user_blocks.push(json!({"type": "image", "data": data, "mime_type": mime, "uri": block.get("uri").cloned().unwrap_or(Value::Null)}));
                }
                "resource_link" => match resource_link(&block) {
                    Ok((wire, display)) => {
                        wire_content.push(wire);
                        user_blocks.push(display);
                        attachment_count += 1;
                    }
                    Err(err) => {
                        self.release_run(&conn, my_seq);
                        return Err(err);
                    }
                },
                "resource" => {
                    let caps = {
                        let rid = conn.lock().unwrap().runtime_id.clone();
                        self.runtime().prompt_capabilities(&rid)
                    };
                    if !caps
                        .get("embedded_context")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        self.release_run(&conn, my_seq);
                        return Err("当前执行端不支持嵌入资源".into());
                    }
                    match embedded_resource(&self.root, &block) {
                        Ok((wire, display)) => {
                            wire_content.push(wire);
                            user_blocks.push(display);
                            attachment_count += 1;
                        }
                        Err(err) => {
                            self.release_run(&conn, my_seq);
                            return Err(err);
                        }
                    }
                }
                _ => {
                    self.release_run(&conn, my_seq);
                    return Err("不支持的消息内容类型".into());
                }
            }
        }
        if prompt_text.trim().is_empty() && attachment_count == 0 {
            self.release_run(&conn, my_seq);
            return Err("消息不能为空".into());
        }
        // Store half (connections.go:594-631) on the blocking pool.
        let (workdir, agent_type, runtime_id) = {
            let c = conn.lock().unwrap();
            (
                c.options.workdir.clone(),
                c.agent_type.clone(),
                c.runtime_id.clone(),
            )
        };
        let native = text(self.runtime_view(&runtime_id).await.get("nativeSessionId"));
        let root = self.root.clone();
        let conversation_arg = crate::backend::acp::wire::number(args.get("conversationId"));
        let folder_arg = crate::backend::acp::wire::number(args.get("folderId"));
        let prompt_for_store = prompt_text.clone();
        let user_blocks_for_store = user_blocks.clone();
        let store = tokio::task::spawn_blocking(move || {
            store_conv::prompt_side_write(
                &root,
                conversation_arg,
                folder_arg,
                &workdir,
                &agent_type,
                &prompt_for_store,
                &user_blocks_for_store,
                &native,
            )
        })
        .await
        .map_err(|e| format!("存储命令执行失败：{e}"))?;
        let store = match store {
            Ok(outcome) => outcome,
            Err(err) => {
                self.release_run(&conn, my_seq);
                return Err(err);
            }
        };
        for (channel, payload) in &store.events {
            self.emit(channel, payload);
        }
        {
            let mut c = conn.lock().unwrap();
            c.conversation_id = store.conversation_id;
            c.folder_id = store.folder_id;
        }
        let conn_for_bind = conn.clone();
        self.bind_conversation(&conn_for_bind, store.conversation_id);
        self.event(&cid, json!({"type": "conversation_linked", "conversation_id": store.conversation_id, "folder_id": store.folder_id}));
        self.event(&cid, json!({"type": "conversation_status_changed", "conversation_id": store.conversation_id, "status": "in_progress"}));
        self.event(
            &cid,
            json!({"type": "status_changed", "status": "prompting"}),
        );
        // Spawn the turn (Go: go s.runWithContent(c, prompt, wireContent)).
        let layer_self = self.self_arc();
        let agent = conn.lock().unwrap().agent.clone();
        tokio::spawn(async move {
            layer_self
                .run_with_content(&conn, &agent, &prompt_text, Some(wire_content))
                .await;
        });
        // The turn runs in the background (Go's `go s.runWithContent`); the
        // delegation domain waits for `last_output` instead.
        Ok(String::new())
    }

    /// Go `releaseRun` (connections.go:638): clear the busy state for the run
    /// that started at seq, only if no newer prompt claimed the connection.
    fn release_run(&self, conn: &Arc<Mutex<Connection>>, seq: u64) {
        let mut c = conn.lock().unwrap();
        if c.run_seq == seq {
            c.busy = false;
            c.status = "connected".into();
            c.run_accepted = None;
        }
    }

    /// Go `runWithContent` (connections.go:650).
    async fn run_with_content(
        &self,
        conn: &Arc<Mutex<Connection>>,
        agent: &Agent,
        prompt_text: &str,
        content: Option<Vec<Value>>,
    ) {
        let (my_seq, options, cid) = {
            let c = conn.lock().unwrap();
            (c.run_seq, c.options.clone(), c.id.clone())
        };
        // Native dialogue prefix (connections.go:657): the model chooses tools
        // and sub-agents itself.
        let prefix = "本工作台支持 codeg-mcp 协作工具。需要其他执行端时使用 delegate_to_agent 自主分工，并用 get_delegation_status 收集真实结果后汇报；用户点名执行端时遵从其选择。任务可拆成多个各自独立的子任务时，优先用 propose_task_split 一次性提案（界面卡片让用户确认或改选执行端），确认后系统自动派发，你无需也不得重复派发；无需用户挑选的单独立项仍用 delegate_to_agent。子任务必须自包含并继承当前权限边界。抓取网页时使用 codeg-mcp 的 fetch_url 工具：harness 自带的网页工具（如 Claude Code 的 WebFetch）会先向域名安全校验服务查询，该服务不可达时会直接失败并提示 “Unable to verify if domain … is safe to fetch”，此时用 fetch_url 抓取同一地址。\n\n".to_string()
            + &self.delegation_context(&cid)
            + &self.split_context(&cid);
        let mut wire_blocks = content.unwrap_or_default();
        if wire_blocks.is_empty() {
            wire_blocks = vec![json!({"type": "text", "text": format!("{prefix}{prompt_text}")})];
        } else if text(wire_blocks[0].get("type")) == "text" {
            let existing = text(wire_blocks[0].get("text"));
            wire_blocks[0]["text"] = json!(format!("{prefix}{existing}"));
        } else {
            let mut blocks = vec![json!({"type": "text", "text": prefix})];
            blocks.extend(wire_blocks);
            wire_blocks = blocks;
        }
        let result = self
            .runtime()
            .prompt_blocks(agent, &wire_blocks, options)
            .await;
        // Completion (connections.go:667-733): a self-healed run must not
        // clobber the live turn's state.
        let stale = conn.lock().unwrap().run_seq != my_seq;
        if stale {
            return;
        }
        let (live, tools) = {
            let c = conn.lock().unwrap();
            (c.live.clone(), c.tools.clone())
        };
        let blocks = crate::backend::acp::wire::persisted_blocks(&live, &tools, &result.output);
        let source_turn_id = text(live.get("source_turn_id"));
        let created_at = text(live.get("created_at"));
        let conversation_id = conn.lock().unwrap().conversation_id;
        let model = text(result.meta.get("model"));
        let usage = result.meta.get("usage").cloned().filter(|u| !u.is_null());
        let duration = result.meta.get("durationMs").and_then(Value::as_i64);
        let native_session = text(result.meta.get("sessionId"));
        let ok = result.ok;
        let output = result.output.clone();
        let output_for_slot = output.clone();
        let root = self.root.clone();
        let events = tokio::task::spawn_blocking(move || {
            store_conv::completion_side_write(
                &root,
                conversation_id,
                ok,
                &output,
                &blocks,
                &created_at,
                &source_turn_id,
                usage.as_ref(),
                &model,
                duration,
                &native_session,
            )
        })
        .await
        .unwrap_or_else(|e| Err(format!("存储命令执行失败：{e}")));
        if let Ok(frames) = events {
            for (channel, payload) in frames {
                self.emit(&channel, &payload);
            }
        }
        {
            let mut c = conn.lock().unwrap();
            c.busy = false;
            c.status = "connected".into();
            c.live = Value::Null;
            c.pending = Value::Null;
            c.last_output = output_for_slot.clone();
            c.output_run_seq = my_seq;
        }
        // A cancelled/ended turn can strand a blocking ask_user_question.
        self.release_questions_for_connection(&cid).await;
        if !result.ok {
            self.event(&cid, json!({"type": "error", "message": result.error}));
        }
        let mut reason = text(result.meta.get("stopReason"));
        if reason.is_empty() {
            reason = "end_turn".into();
        }
        self.event(
            &cid,
            json!({
                "type": "turn_complete",
                "session_id": result.meta.get("sessionId").cloned().unwrap_or(Value::Null),
                "stop_reason": reason,
                "agent_type": agent_type_of(conn),
            }),
        );
    }

    /// Go `delegationContext` (delegation_recovery.go:141): the layer has no
    /// delegation registry yet (D 组), so this is empty until then — the Go
    /// side returns "" with no tasks too.
    fn delegation_context(&self, _cid: &str) -> String {
        String::new()
    }

    /// Go `splitContext` (split.go:422): splits waiting on the connection.
    fn split_context(&self, cid: &str) -> String {
        let state = self.state.lock().unwrap();
        let mut body = String::new();
        for sp in state.splits.values() {
            if sp.parent != cid {
                continue;
            }
            body.push_str(&format!("拆分 {}：\n", sp.id));
            for task in &sp.tasks {
                body.push_str(&format!(
                    "- {}；执行端 {}；模型 {}\n",
                    text(task.get("title")),
                    text(task.get("agent_type")),
                    text(task.get("model"))
                ));
            }
        }
        drop(state);
        if body.is_empty() {
            return String::new();
        }
        format!("等待用户在界面上确认的拆分提案（确认后系统自动派发；不要自行重新提案或重复派发）：\n{body}\n")
    }

    /// Go `cancel` (connections.go:735).
    pub async fn cancel(&self, cid: &str) -> Result<(), String> {
        let runtime_id = {
            let state = self.state.lock().unwrap();
            match state.connections.get(cid) {
                Some(conn) => conn.lock().unwrap().runtime_id.clone(),
                None => return Err("连接不存在".into()),
            }
        };
        self.runtime().cancel(&runtime_id).await
    }

    /// Go `disconnect` (connections.go:744).
    pub async fn disconnect(&self, cid: &str) -> Result<(), String> {
        let (runtime_id, busy) = {
            let state = self.state.lock().unwrap();
            match state.connections.get(cid) {
                Some(conn) => {
                    let c = conn.lock().unwrap();
                    (c.runtime_id.clone(), c.busy)
                }
                None => return Ok(()),
            }
        };
        // The cards can no longer be answered once the session is gone.
        self.drop_connection_splits(cid);
        self.release_questions_for_connection(cid).await;
        if busy {
            let _ = self.runtime().cancel(&runtime_id).await;
        }
        {
            let state = self.state.lock().unwrap();
            if let Some(conn) = state.connections.get(cid) {
                conn.lock().unwrap().status = "disconnected".into();
            }
        }
        self.event(
            cid,
            json!({"type": "status_changed", "status": "disconnected"}),
        );
        Ok(())
    }

    /// Go `dropConnectionSplits` (split.go:547).
    fn drop_connection_splits(&self, cid: &str) {
        let dropped: Vec<TaskSplit> = {
            let mut state = self.state.lock().unwrap();
            let mut dropped = Vec::new();
            let ids: Vec<String> = state
                .splits
                .iter()
                .filter(|(_, sp)| sp.parent == cid)
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                if let Some(sp) = splits::clear_locked(&mut state, &id) {
                    dropped.push(sp);
                }
            }
            if !dropped.is_empty() {
                let root = self.root.clone();
                let rows: Vec<Value> = state.splits.values().map(splits::split_wire).collect();
                tokio::task::spawn_blocking(move || splits::save(&root, &rows));
            }
            dropped
        };
        for sp in dropped {
            self.event(
                cid,
                json!({
                    "type": "task_split_resolved",
                    "split_id": sp.id,
                    "action": "expired",
                }),
            );
        }
    }

    /// Go `releaseQuestionsForConnection` (question.go:90) through the layer.
    async fn release_questions_for_connection(&self, cid: &str) {
        // Collect first: the state guard must never live across an await.
        let parks: Vec<(String, question::ParkedQuestion)> = {
            let mut state = self.state.lock().unwrap();
            let ids: Vec<String> = state
                .questions
                .iter()
                .filter(|(_, park)| park.conn_id == cid)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| state.questions.remove(&id).map(|park| (id, park)))
                .collect()
        };
        for (id, park) in parks {
            {
                let state = self.state.lock().unwrap();
                if let Some(conn) = state.connections.get(&park.conn_id) {
                    let mut c = conn.lock().unwrap();
                    if text(c.pending_question.get("question_id")) == id {
                        c.pending_question = Value::Null;
                    }
                }
            }
            self.event(
                &park.conn_id,
                json!({"type": "question_resolved", "question_id": id}),
            );
            self.publish_session(&park.conn_id).await;
            let _ = park.answer_tx.send(question::declined_answer());
        }
    }
}

fn agent_type_of(conn: &Arc<Mutex<Connection>>) -> String {
    conn.lock().unwrap().agent_type.clone()
}

// ── submit_session_feedback（feedback.go）──────────────────────────────────

impl Layer {
    /// Go `submitFeedback`：运行中的连接支持原生引导时立即 steer 并落一条
    /// user turn；否则把反馈挂到连接的 pending 队列（下一轮 consumeFeedback 取走）。
    pub async fn submit_feedback(&self, args: &Value) -> Result<Value, String> {
        let cid = text(args.get("connectionId"));
        let body = text(args.get("text")).trim().to_string();
        if body.is_empty() || body.len() > 12000 {
            return Err("反馈需要 1–12000 字节".into());
        }
        let mut note = json!({
            "id": goformat::hex_id(),
            "text": body,
            "status": "pending",
            "created_at": goformat::timestamp(),
        });
        let (busy, runtime_id, conversation_id) = {
            let state = self.state.lock().unwrap();
            match state.connections.get(&cid) {
                Some(conn) => {
                    let conn = conn.lock().unwrap();
                    (conn.busy, conn.runtime_id.clone(), conn.conversation_id)
                }
                None => (false, String::new(), 0),
            }
        };
        if runtime_id.is_empty() || !busy {
            // 前端靠这个标记保留草稿，改走正常队列。
            return Err("no active turn".into());
        }
        let blocks: Vec<Value> = args
            .get("blocks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if self.runtime().supports_native_steering(&runtime_id) {
            let capabilities = self.runtime().prompt_capabilities(&runtime_id);
            let mut wire: Vec<Value> = Vec::new();
            let mut stored: Vec<Value> = Vec::new();
            for block in &blocks {
                match text(block.get("type")).as_str() {
                    "text" => {
                        let value = text(block.get("text"));
                        wire.push(json!({"type": "text", "text": value}));
                        stored.push(json!({"type": "text", "text": value}));
                    }
                    "image" => {
                        let mime = text(block.get("mime_type"));
                        if capabilities.get("image") != Some(&Value::Bool(true))
                            || !mime.starts_with("image/")
                        {
                            return Err("当前执行端不支持此图片".into());
                        }
                        let data = image_data(&self.root, block)?;
                        wire.push(json!({"type": "image", "mimeType": mime, "data": data}));
                        stored.push(json!({
                            "type": "image",
                            "mime_type": mime,
                            "data": data,
                            "uri": block.get("uri").cloned().unwrap_or(Value::Null),
                        }));
                    }
                    "resource_link" => {
                        let (link, display) = resource_link(block)?;
                        wire.push(link);
                        stored.push(display);
                    }
                    "resource" => {
                        if capabilities.get("embedded_context") != Some(&Value::Bool(true)) {
                            return Err("当前执行端不支持嵌入资源".into());
                        }
                        let (resource, display) = embedded_resource(&self.root, block)?;
                        wire.push(resource);
                        stored.push(display);
                    }
                    _ => return Err("当前引导不支持此附件类型".into()),
                }
            }
            if wire.is_empty() {
                wire.push(json!({"type": "text", "text": body}));
                stored.push(json!({"type": "text", "text": body}));
            }
            self.runtime().steer(&runtime_id, &wire).await?;
            let now = goformat::timestamp();
            let turn = json!({
                "id": goformat::hex_id(),
                "role": "user",
                "blocks": stored,
                "timestamp": now,
            });
            crate::backend::turn_store::append_raw(&self.root, conversation_id, &turn)?;
            note["status"] = json!("delivered");
            note["delivered_at"] = json!(goformat::timestamp());
            {
                let state = self.state.lock().unwrap();
                if let Some(conn) = state.connections.get(&cid) {
                    conn.lock().unwrap().feedback.push(note.clone());
                }
            }
            self.event(
                &cid,
                json!({"type": "feedback_submitted", "item": note.clone()}),
            );
            return Ok(note);
        }
        if !blocks.is_empty() {
            return Err("no active turn".into());
        }
        {
            let state = self.state.lock().unwrap();
            match state.connections.get(&cid) {
                Some(conn) => {
                    let mut conn = conn.lock().unwrap();
                    if !conn.busy {
                        return Err("no active turn".into());
                    }
                    conn.feedback.push(note.clone());
                }
                None => return Err("no active turn".into()),
            }
        }
        self.event(
            &cid,
            json!({"type": "feedback_submitted", "item": note.clone()}),
        );
        Ok(note)
    }
}
