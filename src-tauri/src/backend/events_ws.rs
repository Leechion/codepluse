//! The event WebSocket protocol, ported from `server-go/internal/codeg/websocket.go`.
//!
//! The sidecar used to own this protocol; with the Go process gone the Rust
//! front door has to speak it itself. The frame vocabulary is unchanged:
//!
//! ```text
//! server → client  {"channel":"__ready__","payload":{}}
//! client → server  {"action":"ping"}            → {"type":"pong"}
//! client → server  {"action":"attach", …}       → snapshot | replay | detached
//! client → server  {"action":"detach", …}       → (silence)
//! server → client  {"type":"event","subscription_id":…,"envelope":…}
//! server → client  {"type":"detached","subscription_id":…,"reason":…}
//! ```
//!
//! Every event the backend emits is also broadcast verbatim as
//! `{"channel":…,"payload":…}` — the UI listens on that channel directly,
//! while `acp://event` additionally feeds the per-subscription stream.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::backend::acp::commands::layer_if_ready;
use crate::backend::goformat;

/// Go `websocket.go` constants.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const PING_INTERVAL: Duration = Duration::from_secs(30);
const MAX_SUBSCRIPTIONS: usize = 64;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Go sets a 64KB read limit on the socket.
const MAX_READ_BYTES: usize = 64 << 10;

/// One broadcast event.
#[derive(Clone)]
pub struct WsEvent {
    pub channel: String,
    pub payload: Value,
}

/// Connected clients, addressed by id. Mirrors Go's `s.subscribers` map.
pub struct Hub {
    next_id: AtomicU64,
    subscribers: Mutex<Vec<(u64, mpsc::UnboundedSender<WsEvent>)>>,
}

impl Hub {
    fn new() -> Self {
        Hub {
            next_id: AtomicU64::new(0),
            subscribers: Mutex::new(Vec::new()),
        }
    }

    fn subscribe(&self) -> (u64, mpsc::UnboundedReceiver<WsEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        self.subscribers.lock().unwrap().push((id, tx));
        (id, rx)
    }

    fn unsubscribe(&self, id: u64) {
        self.subscribers
            .lock()
            .unwrap()
            .retain(|(entry, _)| *entry != id);
    }

    /// Broadcast one event to every connected client. Dead sockets are pruned.
    pub fn broadcast(&self, event: &WsEvent) {
        let mut book = self.subscribers.lock().unwrap();
        book.retain(|(_, sink)| sink.send(event.clone()).is_ok());
    }
}

static HUB: OnceLock<Hub> = OnceLock::new();

pub fn hub() -> &'static Hub {
    HUB.get_or_init(Hub::new)
}

/// Convenience entry point for the emitters scattered across the backend.
pub fn broadcast(channel: &str, payload: Value) {
    hub().broadcast(&WsEvent {
        channel: channel.to_string(),
        payload,
    });
}

/// Go's `respond` for WebSocket frames: compact JSON with Go's escaping and a
/// trailing newline (`json.Encoder.WriteJSON`).
fn frame(value: &Value) -> Message {
    let mut body = goformat::marshal_preserve(value);
    body.push(b'\n');
    Message::Text(String::from_utf8_lossy(&body).to_string().into())
}

/// Upgrade handler for `/ws` and `/ws/events`.
pub async fn handle(ws: WebSocketUpgrade, headers: HeaderMap, listen_port: u16) -> Response {
    // Go's `CheckOrigin`: native clients send no Origin, browser clients must
    // name this very server.
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !crate::backend::webapp::origin_allowed_for_ws(origin, host, listen_port) {
            return axum::http::StatusCode::FORBIDDEN.into_response();
        }
    }

    // The subprotocol must be echoed back: a browser rejects an upgrade whose
    // server did not select one of the offered protocols.
    let requested = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let upgrade = match requested {
        Some(protocol) => ws.protocols([protocol]),
        None => ws,
    };

    upgrade.on_upgrade(move |socket| async move {
        if let Err(err) = session(socket).await {
            eprintln!("[ws] session ended: {err}");
        }
    })
}

use axum::response::IntoResponse;

/// One client session: the protocol state machine plus its socket.
async fn session(socket: WebSocket) -> Result<(), String> {
    let (mut sink, mut stream) = socket.split();
    // A writer task owns the sink; every send goes through the channel, so the
    // select! below can write from several branches without borrow fights.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let (subscriber_id, mut events) = hub().subscribe();
    let result = run(&mut stream, &out_tx, &mut events).await;
    hub().unsubscribe(subscriber_id);
    drop(out_tx);
    let _ = writer.await;
    result
}

/// The protocol loop.
async fn run(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    out: &mpsc::UnboundedSender<Message>,
    events: &mut mpsc::UnboundedReceiver<WsEvent>,
) -> Result<(), String> {
    // Go sends the ready frame before anything else.
    let _ = out.send(frame(&json!({"channel": "__ready__", "payload": {}})));

    // subscription_id → connection_id, and the highest seq forwarded per
    // subscription (a gap means events were dropped for that client).
    let mut subscriptions: HashMap<String, String> = HashMap::new();
    let mut last_sent: HashMap<String, u64> = HashMap::new();

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    let mut idle = tokio::time::sleep(IDLE_TIMEOUT);
    tokio::pin!(idle);

    loop {
        tokio::select! {
            _ = &mut idle => {
                // Go's read deadline: a half-open socket must not keep its
                // goroutine and subscriber slot forever.
                return Ok(());
            }
            _ = ping.tick() => {
                if out.send(Message::Ping(Default::default())).is_err() {
                    return Ok(());
                }
            }
            incoming = stream.next() => {
                let message = match incoming {
                    Some(Ok(message)) => message,
                    Some(Err(err)) => return Err(err.to_string()),
                    None => return Ok(()),
                };
                match message {
                    Message::Text(text) => {
                        let command: Value = match serde_json::from_str(&text) {
                            Ok(value) => value,
                            Err(_) => continue,
                        };
                        handle_command(&command, out, &mut subscriptions, &mut last_sent).await;
                    }
                    Message::Binary(bytes) => {
                        if bytes.len() > MAX_READ_BYTES {
                            return Ok(());
                        }
                        let command: Value = match serde_json::from_slice(&bytes) {
                            Ok(value) => value,
                            Err(_) => continue,
                        };
                        handle_command(&command, out, &mut subscriptions, &mut last_sent).await;
                    }
                    Message::Ping(payload) => {
                        let _ = out.send(Message::Pong(payload));
                    }
                    Message::Pong(_) => {
                        // Go extends the read deadline on pong only.
                        idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                    }
                    Message::Close(_) => return Ok(()),
                }
            }
            event = events.recv() => {
                let event = match event {
                    Some(event) => event,
                    None => return Ok(()),
                };
                // Every event is broadcast verbatim to every client.
                let _ = out.send(frame(&json!({
                    "channel": event.channel,
                    "payload": goformat::canon(&event.payload),
                })));
                if event.channel != "acp://event" {
                    continue;
                }
                let envelope = &event.payload;
                let connection_id = envelope
                    .get("connection_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let seq = envelope
                    .get("seq")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                let mut detached: Vec<String> = Vec::new();
                for (subscription_id, cid) in subscriptions.iter() {
                    if *cid != connection_id {
                        continue;
                    }
                    let last = last_sent.get(subscription_id).copied().unwrap_or(0);
                    if last != 0 {
                        if seq <= last {
                            // Already reflected in the snapshot/replay.
                            continue;
                        }
                        if seq > last + 1 {
                            detached.push(subscription_id.clone());
                            continue;
                        }
                    }
                    last_sent.insert(subscription_id.clone(), seq);
                    let _ = out.send(frame(&json!({
                        "type": "event",
                        "subscription_id": subscription_id,
                        "envelope": envelope,
                    })));
                }
                for subscription_id in detached {
                    subscriptions.remove(&subscription_id);
                    last_sent.remove(&subscription_id);
                    let _ = out.send(frame(&json!({
                        "type": "detached",
                        "subscription_id": subscription_id,
                        "reason": "lagged",
                    })));
                }
            }
        }
    }
}

/// One client command: ping / attach / detach.
async fn handle_command(
    command: &Value,
    out: &mpsc::UnboundedSender<Message>,
    subscriptions: &mut HashMap<String, String>,
    last_sent: &mut HashMap<String, u64>,
) {
    let action = command
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match action {
        "ping" => {
            let _ = out.send(frame(&json!({"type": "pong"})));
        }
        "detach" => {
            let subscription_id = text(command.get("subscription_id"));
            subscriptions.remove(&subscription_id);
            last_sent.remove(&subscription_id);
        }
        "attach" => {
            let subscription_id = text(command.get("subscription_id"));
            let connection_id = text(command.get("connection_id"));
            if !subscriptions.contains_key(&subscription_id)
                && subscriptions.len() >= MAX_SUBSCRIPTIONS
            {
                let _ = out.send(frame(&json!({
                    "type": "detached",
                    "subscription_id": subscription_id,
                    "reason": "too_many_subscriptions",
                })));
                return;
            }
            let since = command
                .get("since_seq")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            if since > 0 {
                if let Some((events, high)) = replay_since(&connection_id, since) {
                    subscriptions.insert(subscription_id.clone(), connection_id.clone());
                    last_sent.insert(subscription_id.clone(), high);
                    let _ = out.send(frame(&json!({
                        "type": "replay",
                        "subscription_id": subscription_id,
                        "connection_id": connection_id,
                        "events": events,
                        "high_water_seq": high,
                    })));
                    return;
                }
            }
            let snapshot = snapshot_of(&connection_id).await;
            if snapshot.is_null() {
                let _ = out.send(frame(&json!({
                    "type": "detached",
                    "subscription_id": subscription_id,
                    "reason": "connection_gone",
                })));
                return;
            }
            let event_seq = snapshot
                .get("event_seq")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            subscriptions.insert(subscription_id.clone(), connection_id.clone());
            last_sent.insert(subscription_id.clone(), event_seq);
            let _ = out.send(frame(&json!({
                "type": "snapshot",
                "subscription_id": subscription_id,
                "connection_id": connection_id,
                "snapshot": snapshot,
                "event_seq": event_seq,
            })));
        }
        _ => {}
    }
}

fn text(value: Option<&Value>) -> String {
    value
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Go `snapshot` (connections.go:812).
async fn snapshot_of(connection_id: &str) -> Value {
    match layer_if_ready() {
        Some(layer) => layer.snapshot(connection_id).await,
        None => Value::Null,
    }
}

/// Go `replaySince` (websocket.go:189): the retained events after `since`.
/// `None` means "cannot replay" — the caller sends a full snapshot instead.
fn replay_since(connection_id: &str, since: u64) -> Option<(Vec<Value>, u64)> {
    let layer = layer_if_ready()?;
    let connection = layer.connection(connection_id)?;
    let connection = connection.lock().ok()?;
    let high = connection.seq;
    if since == 0 || since > high {
        return None;
    }
    if since == high {
        return Some((Vec::new(), high));
    }
    let oldest = connection
        .events
        .first()
        .and_then(|event| event.get("seq"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    if oldest == 0 || since + 1 < oldest {
        return None;
    }
    let events = connection
        .events
        .iter()
        .filter(|event| {
            event
                .get("seq")
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
                > since
        })
        .cloned()
        .collect();
    Some((events, high))
}

/// The write timeout Go applies per frame; surfaced for the writer task.
pub const WRITE_TIMEOUT_DURATION: Duration = WRITE_TIMEOUT;

/// Status used when the WS upgrade is refused.
pub fn forbidden() -> Response {
    (StatusCode::FORBIDDEN, "forbidden").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_broadcasts_to_every_subscriber() {
        let hub = Hub::new();
        let (id_a, mut a) = hub.subscribe();
        let (id_b, mut b) = hub.subscribe();
        hub.broadcast(&WsEvent {
            channel: "conversation://changed".into(),
            payload: json!({"id": 1}),
        });
        let event = a.try_recv().expect("event for a");
        assert_eq!(event.channel, "conversation://changed");
        assert!(b.try_recv().is_ok());
        hub.unsubscribe(id_a);
        hub.unsubscribe(id_b);
        assert!(hub.subscribers.lock().unwrap().is_empty());
    }

    #[test]
    fn frames_are_go_shaped() {
        let text = match frame(&json!({"type": "pong"})) {
            Message::Text(text) => text.to_string(),
            _ => panic!("text frame"),
        };
        assert_eq!(text, "{\"type\":\"pong\"}\n");
    }
}
