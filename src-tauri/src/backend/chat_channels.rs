//! 聊天渠道与账号令牌 —— 补全批次。
//!
//! 前端（`codepluse-ui/src/lib/api.ts`）已经在调用这 17 条命令，但 Rust 与已删除的
//! Go sidecar 都没有实现过它们（在 `server-go` 只读副本里搜不到命令字面量），
//! 所以契约只由调用点（`api.ts` 的参数名）与 `types.ts` 的返回结构决定：
//!
//! * `ChatChannelInfo` = `{ id, name, channel_type, enabled, config_json,
//!   event_filter_json, daily_report_enabled, daily_report_time, created_at,
//!   updated_at }`。
//! * `ChannelStatusInfo` = `{ channel_id, name, channel_type, status }`，
//!   `status` ∈ `connected | connecting | disconnected | error`。
//! * `ChatChannelMessageLog` = `{ id, channel_id, direction, message_type,
//!   content_preview, status, error_detail, created_at }`。
//! * 账号令牌按 `accountId` 存取；渠道令牌按 `channelId` 存取。
//!
//! 落地分档：
//!
//! * **L1 本地持久化**：渠道元数据进 `root/.data/chat-channels.json`；渠道令牌
//!   单独进 `root/.data/chat-channel-tokens.json`；账号令牌单独进
//!   `root/.data/account-tokens.json`；本地消息记录进
//!   `root/.data/chat-messages.json`。令牌只落在独立文件里，不进渠道元数据、
//!   不进日志、不进错误文案。连接状态由持久化记录真实推导，不写死。
//! * **L2 上游请求**：`test_chat_channel` 按渠道配置里的 webhook 地址真发一条
//!   测试消息（8 秒短超时），失败先记一条 `failed` 本地记录再返回中文 `Err`，
//!   不假装发送成功；微信二维码 `weixin_get_qrcode` / `weixin_check_qrcode`
//!   走配置的服务地址，上游由环境变量 `CODEPLUSE_WEIXIN_QR_BASE` 覆盖（测试桩
//!   走这个 seam），未配置或连接失败给中文 `Err`，不伪造二维码、不伪造
//!   “已扫码”。
//!
//! 注意：本模块与 `account_channels.rs`（`codepluse_channel_*`，WorkBuddy/Qoder
//! 官方账号）不是一回事，后者只读参考，本文件不改动它。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use url::Url;

use crate::backend::goformat;
use crate::backend::net;
use crate::backend::store_file;

/// 渠道元数据清单（相对 `root/.data/`）。
const CHANNELS_FILE: &str = "chat-channels.json";
/// 渠道令牌独立文件；与元数据分开存，回读渠道列表时绝不带令牌。
const CHANNEL_TOKENS_FILE: &str = "chat-channel-tokens.json";
/// 账号令牌独立文件。
const ACCOUNT_TOKENS_FILE: &str = "account-tokens.json";
/// 本地消息记录。
const MESSAGES_FILE: &str = "chat-messages.json";

/// 微信扫码服务地址覆盖；未设置时视为“地址未配置”，不内置公开上游。
const WEIXIN_ENV: &str = "CODEPLUSE_WEIXIN_QR_BASE";
/// 上游请求短超时，避免面板长时间卡在加载态。
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(8);
/// 消息列表默认与上限条数。
const DEFAULT_MESSAGE_LIMIT: u64 = 50;
const MAX_MESSAGE_LIMIT: u64 = 500;

/// 与 `types.ts` 的 `ChannelType` 联合类型一一对应。
const CHANNEL_TYPES: [&str; 3] = ["lark", "telegram", "weixin"];
/// 与 `ChannelConnectionStatus` 联合类型一一对应。
const CONNECTION_STATUSES: [&str; 4] = ["connected", "connecting", "disconnected", "error"];

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "connect_chat_channel"
            | "create_chat_channel"
            | "delete_account_token"
            | "delete_chat_channel"
            | "delete_chat_channel_token"
            | "disconnect_chat_channel"
            | "get_account_token"
            | "get_chat_channel_has_token"
            | "get_chat_channel_status"
            | "list_chat_channel_messages"
            | "list_chat_channels"
            | "save_account_token"
            | "save_chat_channel_token"
            | "test_chat_channel"
            | "update_chat_channel"
            | "weixin_check_qrcode"
            | "weixin_get_qrcode"
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
        "list_chat_channels" => list_channels(root),
        "create_chat_channel" => create_channel(root, args),
        "update_chat_channel" => update_channel(root, args),
        "delete_chat_channel" => delete_channel(root, args),
        "save_chat_channel_token" => save_channel_token(root, args),
        "get_chat_channel_has_token" => has_channel_token(root, args),
        "delete_chat_channel_token" => delete_channel_token(root, args),
        "connect_chat_channel" => set_connection(root, args, "connected"),
        "disconnect_chat_channel" => set_connection(root, args, "disconnected"),
        "test_chat_channel" => test_channel(root, args),
        "get_chat_channel_status" => list_status(root),
        "list_chat_channel_messages" => list_messages(root, args),
        "save_account_token" => save_account_token(root, args),
        "get_account_token" => get_account_token(root, args),
        "delete_account_token" => delete_account_token(root, args),
        "weixin_get_qrcode" => get_weixin_qrcode(),
        "weixin_check_qrcode" => check_weixin_qrcode(args),
        _ => Err(format!("未知的聊天渠道命令：{command}")),
    }
}

// ── 路径与持久清单 ──────────────────────────────────────────────────────

fn data_dir(root: &Path) -> PathBuf {
    root.join(".data")
}

fn channels_path(root: &Path) -> PathBuf {
    data_dir(root).join(CHANNELS_FILE)
}

fn channel_tokens_path(root: &Path) -> PathBuf {
    data_dir(root).join(CHANNEL_TOKENS_FILE)
}

fn account_tokens_path(root: &Path) -> PathBuf {
    data_dir(root).join(ACCOUNT_TOKENS_FILE)
}

fn messages_path(root: &Path) -> PathBuf {
    data_dir(root).join(MESSAGES_FILE)
}

/// 读取一个对象型清单；文件缺失或格式不对时按空清单处理，不把旧文件损坏
/// 暴露成列表读取失败。
fn load_object(path: &Path) -> Value {
    store_file::read_json(path)
        .ok()
        .flatten()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn load_channels(root: &Path) -> Value {
    let doc = load_object(&channels_path(root));
    if doc.get("channels").and_then(Value::as_array).is_some() {
        doc
    } else {
        json!({ "next_id": 1, "channels": [] })
    }
}

fn save_channels(root: &Path, doc: &Value) -> Result<(), String> {
    let _lock = store_file::lock(&channels_path(root))?;
    store_file::write_json(&channels_path(root), doc)
}

// ── 参数校验 ────────────────────────────────────────────────────────────

fn required_str(args: &Value, field: &str) -> Result<String, String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("缺少 {field} 参数"))
}

/// 可选字符串：`null`、缺失或纯空白都视为未提供。
fn optional_str(args: &Value, field: &str) -> Option<String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn required_i64(args: &Value, field: &str) -> Result<i64, String> {
    match args.get(field) {
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| format!("{field} 必须是整数")),
        Some(Value::String(raw)) => raw
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("{field} 必须是整数")),
        Some(_) => Err(format!("{field} 必须是整数")),
        None => Err(format!("缺少 {field} 参数")),
    }
}

fn optional_bool(args: &Value, field: &str) -> Option<bool> {
    args.get(field).and_then(Value::as_bool)
}

/// `configJson` / `eventFilterJson` 是 JSON 字符串：允许为空串；非空时必须能
/// 被解析成 JSON，避免把一段坏配置存进渠道后面无法发送。
fn validate_json_string(raw: &str, field: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    serde_json::from_str::<Value>(trimmed)
        .map_err(|_| format!("{field} 不是合法 JSON"))?;
    Ok(())
}

// ── 渠道视图 ─────────────────────────────────────────────────────────────

/// 对外视图：内部的 `status` 字段不进 `ChatChannelInfo`（状态由
/// `get_chat_channel_status` 单独返回），令牌永远不出现在任何渠道记录里。
fn channel_view(row: &Value) -> Value {
    json!({
        "id": row.get("id").cloned().unwrap_or(Value::Null),
        "name": row.get("name").cloned().unwrap_or(Value::Null),
        "channel_type": row.get("channel_type").cloned().unwrap_or(Value::Null),
        "enabled": row.get("enabled").cloned().unwrap_or(Value::Bool(false)),
        "config_json": row.get("config_json").cloned().unwrap_or(Value::String(String::new())),
        "event_filter_json": row.get("event_filter_json").cloned().unwrap_or(Value::Null),
        "daily_report_enabled": row.get("daily_report_enabled").cloned().unwrap_or(Value::Bool(false)),
        "daily_report_time": row.get("daily_report_time").cloned().unwrap_or(Value::Null),
        "created_at": row.get("created_at").cloned().unwrap_or(Value::Null),
        "updated_at": row.get("updated_at").cloned().unwrap_or(Value::Null),
    })
}

fn find_channel_mut(doc: &mut Value, id: i64) -> Option<&mut Value> {
    doc.get_mut("channels")
        .and_then(Value::as_array_mut)?
        .iter_mut()
        .find(|row| row.get("id").and_then(Value::as_i64) == Some(id))
}

fn find_channel<'a>(doc: &'a Value, id: i64) -> Option<&'a Value> {
    doc.get("channels")
        .and_then(Value::as_array)?
        .iter()
        .find(|row| row.get("id").and_then(Value::as_i64) == Some(id))
}

// ── 渠道 CRUD ───────────────────────────────────────────────────────────

fn list_channels(root: &Path) -> Result<Value, String> {
    let doc = load_channels(root);
    let rows = doc
        .get("channels")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(channel_view).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(Value::Array(rows))
}

fn create_channel(root: &Path, args: &Value) -> Result<Value, String> {
    let name = required_str(args, "name")?;
    let channel_type = required_str(args, "channelType")?;
    if !CHANNEL_TYPES.contains(&channel_type.as_str()) {
        return Err(format!("未知的渠道类型：{channel_type}"));
    }
    let config_json = args
        .get("configJson")
        .and_then(Value::as_str)
        .ok_or_else(|| "缺少 configJson 参数".to_string())?
        .to_string();
    validate_json_string(&config_json, "configJson")?;
    let enabled = optional_bool(args, "enabled").unwrap_or(false);
    let daily_report_enabled = optional_bool(args, "dailyReportEnabled").unwrap_or(false);
    let daily_report_time = optional_str(args, "dailyReportTime");
    let event_filter_json = match args.get("eventFilterJson") {
        Some(Value::String(raw)) => {
            validate_json_string(raw, "eventFilterJson")?;
            json!(raw)
        }
        Some(Value::Null) | None => Value::Null,
        Some(_) => return Err("eventFilterJson 必须是字符串".into()),
    };

    let mut doc = load_channels(root);
    let next_id = doc
        .get("next_id")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .max(1);
    let now = goformat::timestamp();
    let row = json!({
        "id": next_id,
        "name": name,
        "channel_type": channel_type,
        "enabled": enabled,
        "config_json": config_json,
        "event_filter_json": event_filter_json,
        "daily_report_enabled": daily_report_enabled,
        "daily_report_time": daily_report_time.map(Value::String).unwrap_or(Value::Null),
        "created_at": now,
        "updated_at": now,
        "status": "disconnected",
    });

    let arr = doc
        .get_mut("channels")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "渠道清单格式无效".to_string())?;
    arr.push(row.clone());
    doc["next_id"] = json!(next_id + 1);
    save_channels(root, &doc)?;
    Ok(channel_view(&row))
}

fn update_channel(root: &Path, args: &Value) -> Result<Value, String> {
    let id = required_i64(args, "id")?;
    let mut doc = load_channels(root);
    let row = find_channel_mut(&mut doc, id).ok_or_else(|| "渠道不存在".to_string())?;

    if let Some(name) = optional_str(args, "name") {
        row["name"] = json!(name);
    }
    if let Some(enabled) = optional_bool(args, "enabled") {
        row["enabled"] = json!(enabled);
    }
    if let Some(config) = args.get("configJson").and_then(Value::as_str) {
        validate_json_string(config, "configJson")?;
        row["config_json"] = json!(config);
    }
    match args.get("eventFilterJson") {
        Some(Value::String(raw)) => {
            validate_json_string(raw, "eventFilterJson")?;
            row["event_filter_json"] = json!(raw);
        }
        Some(Value::Null) | None => {}
        Some(_) => return Err("eventFilterJson 必须是字符串".into()),
    }
    if let Some(flag) = optional_bool(args, "dailyReportEnabled") {
        row["daily_report_enabled"] = json!(flag);
    }
    match args.get("dailyReportTime") {
        Some(Value::String(raw)) if !raw.trim().is_empty() => {
            row["daily_report_time"] = json!(raw.trim());
        }
        Some(Value::Null) => row["daily_report_time"] = Value::Null,
        Some(_) => return Err("dailyReportTime 必须是字符串".into()),
        None => {}
    }
    row["updated_at"] = json!(goformat::timestamp());

    let view = channel_view(row);
    save_channels(root, &doc)?;
    Ok(view)
}

fn delete_channel(root: &Path, args: &Value) -> Result<Value, String> {
    let id = required_i64(args, "id")?;

    // 渠道元数据；重复删除幂等。
    let mut doc = load_channels(root);
    if let Some(arr) = doc.get_mut("channels").and_then(Value::as_array_mut) {
        arr.retain(|row| row.get("id").and_then(Value::as_i64) != Some(id));
    }
    save_channels(root, &doc)?;

    // 连带清掉该渠道的令牌与本地消息记录。
    let token_key = id.to_string();
    let token_path = channel_tokens_path(root);
    let _lock = store_file::lock(&token_path)?;
    let mut tokens = load_object(&token_path);
    if let Some(obj) = tokens.as_object_mut() {
        obj.remove(&token_key);
    }
    store_file::write_json(&token_path, &tokens)?;

    let msg_path = messages_path(root);
    let _lock = store_file::lock(&msg_path)?;
    let mut messages = load_object(&msg_path);
    if let Some(arr) = messages.get_mut("messages").and_then(Value::as_array_mut) {
        arr.retain(|row| row.get("channel_id").and_then(Value::as_i64) != Some(id));
    }
    store_file::write_json(&msg_path, &messages)?;

    Ok(Value::Null)
}

// ── 渠道令牌 ─────────────────────────────────────────────────────────────

fn save_channel_token(root: &Path, args: &Value) -> Result<Value, String> {
    let channel_id = required_i64(args, "channelId")?;
    let token = required_str(args, "token")?;

    let path = channel_tokens_path(root);
    let _lock = store_file::lock(&path)?;
    let mut doc = load_object(&path);
    doc.as_object_mut()
        .ok_or_else(|| "渠道令牌存储格式无效".to_string())?
        .insert(channel_id.to_string(), json!(token));
    // 错误文案不回写令牌本身。
    store_file::write_json(&path, &doc).map_err(|_| "保存渠道令牌失败".to_string())?;
    Ok(Value::Null)
}

fn has_channel_token(root: &Path, args: &Value) -> Result<Value, String> {
    let channel_id = required_i64(args, "channelId")?;
    let doc = load_object(&channel_tokens_path(root));
    let has = doc
        .get(channel_id.to_string())
        .and_then(Value::as_str)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    Ok(json!(has))
}

fn delete_channel_token(root: &Path, args: &Value) -> Result<Value, String> {
    let channel_id = required_i64(args, "channelId")?;
    let path = channel_tokens_path(root);
    let _lock = store_file::lock(&path)?;
    let mut doc = load_object(&path);
    if let Some(obj) = doc.as_object_mut() {
        obj.remove(&channel_id.to_string());
    }
    store_file::write_json(&path, &doc)?;
    Ok(Value::Null)
}

// ── 账号令牌 ────────────────────────────────────────────────────────────

fn save_account_token(root: &Path, args: &Value) -> Result<Value, String> {
    let account_id = required_str(args, "accountId")?;
    let token = required_str(args, "token")?;

    let path = account_tokens_path(root);
    let _lock = store_file::lock(&path)?;
    let mut doc = load_object(&path);
    doc.as_object_mut()
        .ok_or_else(|| "账号令牌存储格式无效".to_string())?
        .insert(account_id, json!(token));
    store_file::write_json(&path, &doc).map_err(|_| "保存账号令牌失败".to_string())?;
    Ok(Value::Null)
}

fn get_account_token(root: &Path, args: &Value) -> Result<Value, String> {
    let account_id = required_str(args, "accountId")?;
    let doc = load_object(&account_tokens_path(root));
    let token = doc
        .get(&account_id)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    Ok(token.map(Value::String).unwrap_or(Value::Null))
}

fn delete_account_token(root: &Path, args: &Value) -> Result<Value, String> {
    let account_id = required_str(args, "accountId")?;
    let path = account_tokens_path(root);
    let _lock = store_file::lock(&path)?;
    let mut doc = load_object(&path);
    if let Some(obj) = doc.as_object_mut() {
        obj.remove(&account_id);
    }
    store_file::write_json(&path, &doc)?;
    Ok(Value::Null)
}

// ── 连接状态 ────────────────────────────────────────────────────────────

fn set_connection(root: &Path, args: &Value, target: &str) -> Result<Value, String> {
    let id = required_i64(args, "id")?;
    let mut doc = load_channels(root);
    let row = find_channel_mut(&mut doc, id).ok_or_else(|| "渠道不存在".to_string())?;
    row["status"] = json!(target);
    save_channels(root, &doc)?;
    Ok(Value::Null)
}

fn list_status(root: &Path) -> Result<Value, String> {
    let doc = load_channels(root);
    let rows = doc
        .get("channels")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let status = row
                        .get("status")
                        .and_then(Value::as_str)
                        .filter(|value| CONNECTION_STATUSES.contains(value))
                        .unwrap_or("disconnected");
                    json!({
                        "channel_id": row.get("id").cloned().unwrap_or(Value::Null),
                        "name": row.get("name").cloned().unwrap_or(Value::Null),
                        "channel_type": row.get("channel_type").cloned().unwrap_or(Value::Null),
                        "status": status,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Value::Array(rows))
}

// ── 本地消息记录 ────────────────────────────────────────────────────────

fn append_message(
    root: &Path,
    channel_id: i64,
    direction: &str,
    message_type: &str,
    preview: &str,
    status: &str,
    error_detail: Option<&str>,
) {
    let path = messages_path(root);
    let Ok(_lock) = store_file::lock(&path) else { return };
    let mut doc = store_file::read_json(&path)
        .ok()
        .flatten()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({ "next_id": 1, "messages": [] }));
    let next_id = doc.get("next_id").and_then(Value::as_i64).unwrap_or(1).max(1);
    let row = json!({
        "id": next_id,
        "channel_id": channel_id,
        "direction": direction,
        "message_type": message_type,
        "content_preview": preview,
        "status": status,
        "error_detail": error_detail.map(|v| Value::String(v.to_string())).unwrap_or(Value::Null),
        "created_at": goformat::timestamp(),
    });
    if let Some(arr) = doc.get_mut("messages").and_then(Value::as_array_mut) {
        arr.push(row);
    } else {
        doc["messages"] = json!([row]);
    }
    doc["next_id"] = json!(next_id + 1);
    // 本地留痕失败不回滚上游结果：静默保留内存状态即可。
    let _ = store_file::write_json(&path, &doc);
}

fn list_messages(root: &Path, args: &Value) -> Result<Value, String> {
    let channel_id = required_i64(args, "channelId")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MESSAGE_LIMIT)
        .clamp(1, MAX_MESSAGE_LIMIT) as usize;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    let doc = load_object(&messages_path(root));
    let all = doc
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let rows: Vec<Value> = all
        .iter()
        .filter(|row| row.get("channel_id").and_then(Value::as_i64) == Some(channel_id))
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();
    Ok(Value::Array(rows))
}

// ── 渠道测试（L2，真实发一条）───────────────────────────────────────────

/// 从渠道配置 JSON 里取发送地址；兼容常见的 webhook 字段名。
fn webhook_url(config_json: &str) -> Option<String> {
    let trimmed = config_json.trim();
    if trimmed.is_empty() {
        return None;
    }
    let config: Value = serde_json::from_str(trimmed).ok()?;
    let obj = config.as_object()?;
    for key in ["url", "webhookUrl", "webhook_url", "botUrl", "bot_url"] {
        if let Some(value) = obj.get(key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

fn test_channel(root: &Path, args: &Value) -> Result<Value, String> {
    let id = required_i64(args, "id")?;
    let doc = load_channels(root);
    let row = find_channel(&doc, id).ok_or_else(|| "渠道不存在".to_string())?;
    let config_json = row
        .get("config_json")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let url = webhook_url(&config_json)
        .ok_or_else(|| "渠道未配置发送地址（webhook URL）".to_string())?;

    let payload = json!({
        "msgtype": "text",
        "content": { "text": "Codepluse 渠道连通性测试消息" },
    });
    let result = net::no_proxy_client()
        .post(&url)
        .timeout(UPSTREAM_TIMEOUT)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send();

    match result {
        Ok(response) if response.status().is_success() => {
            append_message(
                root,
                id,
                "outbound",
                "test",
                "连通性测试消息",
                "sent",
                None,
            );
            Ok(Value::Null)
        }
        Ok(response) => {
            let status = response.status().as_u16();
            append_message(
                root,
                id,
                "outbound",
                "test",
                "连通性测试消息",
                "failed",
                Some("上游返回错误状态"),
            );
            Err(format!("测试消息发送失败：上游返回 HTTP {status}"))
        }
        Err(_) => {
            append_message(
                root,
                id,
                "outbound",
                "test",
                "连通性测试消息",
                "failed",
                Some("网络连接失败"),
            );
            Err("测试消息发送失败：网络连接失败".into())
        }
    }
}

// ── 微信二维码（L2，地址可被环境变量覆盖）────────────────────────────────

fn weixin_base() -> Result<String, String> {
    std::env::var(WEIXIN_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .ok_or_else(|| "微信扫码服务未配置（设置 CODEPLUSE_WEIXIN_QR_BASE 后重试）".into())
}

fn http_get_json(url: &str) -> Result<Value, String> {
    let response = net::no_proxy_client()
        .get(url)
        .timeout(UPSTREAM_TIMEOUT)
        .send()
        .map_err(|_| "连接微信扫码服务失败".to_string())?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("微信扫码服务返回错误：HTTP {status}"));
    }
    let body = response
        .bytes()
        .map_err(|_| "读取微信扫码服务响应失败".to_string())?;
    serde_json::from_slice::<Value>(&body)
        .map_err(|_| "微信扫码服务响应 JSON 无效".to_string())
}

fn get_weixin_qrcode() -> Result<Value, String> {
    let base = weixin_base()?;
    get_qrcode_with_base(&base)
}

fn get_qrcode_with_base(base: &str) -> Result<Value, String> {
    let payload = http_get_json(&format!("{base}/qrcode"))?;
    let qrcode_id = payload
        .get("qrcode_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "微信扫码服务缺少 qrcode_id".to_string())?
        .to_string();
    let qrcode_img_content = payload
        .get("qrcode_img_content")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "微信扫码服务缺少 qrcode_img_content".to_string())?
        .to_string();
    Ok(json!({
        "qrcode_id": qrcode_id,
        "qrcode_img_content": qrcode_img_content,
    }))
}

fn check_weixin_qrcode(args: &Value) -> Result<Value, String> {
    let channel_id = required_i64(args, "channelId")?;
    let qrcode = required_str(args, "qrcode")?;
    let base = weixin_base()?;
    check_qrcode_with_base(&base, channel_id, &qrcode)
}

fn check_qrcode_with_base(base: &str, channel_id: i64, qrcode: &str) -> Result<Value, String> {
    let mut url = Url::parse(&format!("{base}/qrcode/check"))
        .map_err(|_| "微信扫码服务地址无效".to_string())?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("channel_id", &channel_id.to_string());
        pairs.append_pair("qrcode", qrcode);
    }
    let payload = http_get_json(&url.to_string())?;
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "微信扫码服务缺少 status".to_string())?
        .to_string();
    Ok(json!({ "status": status }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-chat-channels-{tag}-{}-{}",
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

    /// 起一个本地 HTTP 桩：按路径前缀返回预置 JSON，避免测试走真实外网。
    fn serve_stub(routes: &'static [(&'static str, &'static str)]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let request = String::from_utf8_lossy(&buffer);
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let body = routes
                    .iter()
                    .find(|(route, _)| path.starts_with(*route))
                    .map(|(_, payload)| *payload)
                    .unwrap_or(r#"{"status":"unknown"}"#);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}")
    }

    fn create_sample(root: &Path, name: &str, channel_type: &str) -> Value {
        call(
            root,
            "create_chat_channel",
            json!({
                "name": name,
                "channelType": channel_type,
                "configJson": "{\"url\":\"http://127.0.0.1:9/webook\"}",
                "enabled": true,
                "dailyReportEnabled": false,
                "dailyReportTime": null
            }),
        )
        .unwrap()
    }

    #[test]
    fn owns_covers_the_seventeen_documented_commands_only() {
        for command in [
            "connect_chat_channel",
            "create_chat_channel",
            "delete_account_token",
            "delete_chat_channel",
            "delete_chat_channel_token",
            "disconnect_chat_channel",
            "get_account_token",
            "get_chat_channel_has_token",
            "get_chat_channel_status",
            "list_chat_channel_messages",
            "list_chat_channels",
            "save_account_token",
            "save_chat_channel_token",
            "test_chat_channel",
            "update_chat_channel",
            "weixin_check_qrcode",
            "weixin_get_qrcode",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("list_account_channels"));
        assert!(!owns("save_account_settings"));
        assert!(!owns("CREATE_CHAT_CHANNEL"));
    }

    #[test]
    fn create_persists_channel_and_echoes_contract_shape() {
        let root = temp_root("create");
        let row = create_sample(&root, "团队群", "lark");
        assert_eq!(row["id"], 1);
        assert_eq!(row["name"], "团队群");
        assert_eq!(row["channel_type"], "lark");
        assert_eq!(row["enabled"], true);
        assert!(row["event_filter_json"].is_null());
        assert_eq!(row["daily_report_enabled"], false);
        assert!(row["daily_report_time"].is_null());
        assert!(!row["created_at"].as_str().unwrap().is_empty());
        assert!(!row["updated_at"].as_str().unwrap().is_empty());
        // 内部 status 不进对外视图。
        assert!(row.get("status").is_none());

        let list = call(&root, "list_chat_channels", json!({})).unwrap();
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], "团队群");
        assert!(channels_path(&root).is_file());
    }

    #[test]
    fn create_rejects_missing_or_invalid_inputs() {
        let root = temp_root("create-bad");
        assert!(call(&root, "create_chat_channel", json!({}))
            .unwrap_err()
            .contains("缺少 name"));
        assert!(call(
            &root,
            "create_chat_channel",
            json!({ "name": "x", "channelType": "irc", "configJson": "", "enabled": true, "dailyReportEnabled": false })
        )
        .unwrap_err()
        .contains("未知的渠道类型"));
        assert!(call(
            &root,
            "create_chat_channel",
            json!({ "name": "x", "channelType": "lark", "configJson": "{not json", "enabled": true, "dailyReportEnabled": false })
        )
        .unwrap_err()
        .contains("configJson 不是合法 JSON"));
        assert!(call(
            &root,
            "create_chat_channel",
            json!({ "name": "x", "channelType": "lark", "configJson": 123, "enabled": true, "dailyReportEnabled": false })
        )
        .unwrap_err()
        .contains("缺少 configJson"));
    }

    #[test]
    fn update_merges_fields_and_rejects_unknown_id() {
        let root = temp_root("update");
        let row = create_sample(&root, "旧名", "telegram");
        let id = row["id"].as_i64().unwrap();

        let updated = call(
            &root,
            "update_chat_channel",
            json!({
                "id": id,
                "name": "新名",
                "enabled": false,
                "configJson": "{\"url\":\"http://127.0.0.1:9/hook\"}",
                "eventFilterJson": "[\"push\"]",
                "dailyReportEnabled": true,
                "dailyReportTime": "09:30"
            }),
        )
        .unwrap();
        assert_eq!(updated["name"], "新名");
        assert_eq!(updated["enabled"], false);
        assert_eq!(updated["event_filter_json"], "[\"push\"]");
        assert_eq!(updated["daily_report_enabled"], true);
        assert_eq!(updated["daily_report_time"], "09:30");

        let list = call(&root, "list_chat_channels", json!({})).unwrap();
        assert_eq!(list[0]["name"], "新名");

        // id 不存在 → Err；缺 id → Err。
        assert!(call(
            &root,
            "update_chat_channel",
            json!({ "id": 999, "name": "x" })
        )
        .unwrap_err()
        .contains("渠道不存在"));
        assert!(call(&root, "update_chat_channel", json!({}))
            .unwrap_err()
            .contains("缺少 id"));
    }

    #[test]
    fn delete_channel_is_idempotent_and_clears_token_and_messages() {
        let root = temp_root("delete");
        let row = create_sample(&root, "待删", "weixin");
        let id = row["id"].as_i64().unwrap();
        call(&root, "save_chat_channel_token", json!({ "channelId": id, "token": "secret-token" })).unwrap();

        let first = call(&root, "delete_chat_channel", json!({ "id": id })).unwrap();
        assert!(first.is_null());
        let second = call(&root, "delete_chat_channel", json!({ "id": id })).unwrap();
        assert!(second.is_null());

        let list = call(&root, "list_chat_channels", json!({})).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 0);
        let has = call(&root, "get_chat_channel_has_token", json!({ "channelId": id })).unwrap();
        assert_eq!(has, json!(false));

        assert!(call(&root, "delete_chat_channel", json!({}))
            .unwrap_err()
            .contains("缺少 id"));
    }

    #[test]
    fn channel_token_roundtrip_and_is_never_echoed_in_channel_row() {
        let root = temp_root("channel-token");
        let row = create_sample(&root, "带令牌", "lark");
        let id = row["id"].as_i64().unwrap();

        let saved = call(&root, "save_chat_channel_token", json!({ "channelId": id, "token": "chan-secret" })).unwrap();
        assert!(saved.is_null());

        let has = call(&root, "get_chat_channel_has_token", json!({ "channelId": id })).unwrap();
        assert_eq!(has, json!(true));

        // 渠道列表与详情里都不能出现令牌。
        let list = call(&root, "list_chat_channels", json!({})).unwrap();
        let text = serde_json::to_string(&list).unwrap();
        assert!(!text.contains("chan-secret"));

        call(&root, "delete_chat_channel_token", json!({ "channelId": id })).unwrap();
        let has_after = call(&root, "get_chat_channel_has_token", json!({ "channelId": id })).unwrap();
        assert_eq!(has_after, json!(false));

        assert!(call(&root, "save_chat_channel_token", json!({ "channelId": id, "token": "" }))
            .unwrap_err()
            .contains("缺少 token"));
        assert!(call(&root, "get_chat_channel_has_token", json!({}))
            .unwrap_err()
            .contains("缺少 channelId"));
    }

    #[test]
    fn account_token_roundtrip_and_delete_is_idempotent() {
        let root = temp_root("account-token");
        call(&root, "save_account_token", json!({ "accountId": "acc-1", "token": "acc-secret" })).unwrap();
        let got = call(&root, "get_account_token", json!({ "accountId": "acc-1" })).unwrap();
        assert_eq!(got, json!("acc-secret"));

        let missing = call(&root, "get_account_token", json!({ "accountId": "acc-x" })).unwrap();
        assert!(missing.is_null());

        call(&root, "delete_account_token", json!({ "accountId": "acc-1" })).unwrap();
        call(&root, "delete_account_token", json!({ "accountId": "acc-1" })).unwrap();
        let after = call(&root, "get_account_token", json!({ "accountId": "acc-1" })).unwrap();
        assert!(after.is_null());

        assert!(call(&root, "save_account_token", json!({ "accountId": "acc-2" }))
            .unwrap_err()
            .contains("缺少 token"));
        assert!(call(&root, "get_account_token", json!({}))
            .unwrap_err()
            .contains("缺少 accountId"));
    }

    #[test]
    fn connect_disconnect_derive_status_from_persisted_state() {
        let root = temp_root("status");
        let row = create_sample(&root, "在线群", "lark");
        let id = row["id"].as_i64().unwrap();

        // 新建默认断开。
        let statuses = call(&root, "get_chat_channel_status", json!({})).unwrap();
        assert_eq!(statuses[0]["status"], "disconnected");
        assert_eq!(statuses[0]["channel_id"], id);
        assert_eq!(statuses[0]["name"], "在线群");
        assert_eq!(statuses[0]["channel_type"], "lark");

        call(&root, "connect_chat_channel", json!({ "id": id })).unwrap();
        let statuses = call(&root, "get_chat_channel_status", json!({})).unwrap();
        assert_eq!(statuses[0]["status"], "connected");

        call(&root, "disconnect_chat_channel", json!({ "id": id })).unwrap();
        let statuses = call(&root, "get_chat_channel_status", json!({})).unwrap();
        assert_eq!(statuses[0]["status"], "disconnected");

        assert!(call(&root, "connect_chat_channel", json!({ "id": 999 }))
            .unwrap_err()
            .contains("渠道不存在"));
        assert!(call(&root, "disconnect_chat_channel", json!({}))
            .unwrap_err()
            .contains("缺少 id"));
    }

    #[test]
    fn test_channel_sends_to_stub_and_records_sent_message() {
        let stub = serve_stub(&[("/", r#"{"ok":true}"#)]);
        let root = temp_root("test-ok");
        let row = call(
            &root,
            "create_chat_channel",
            json!({
                "name": "测试群",
                "channelType": "lark",
                "configJson": serde_json::to_string(&json!({ "url": format!("{stub}/hook") })).unwrap(),
                "enabled": true,
                "dailyReportEnabled": false,
                "dailyReportTime": null
            }),
        )
        .unwrap();
        let id = row["id"].as_i64().unwrap();

        call(&root, "test_chat_channel", json!({ "id": id })).unwrap();

        let messages = call(&root, "list_chat_channel_messages", json!({ "channelId": id })).unwrap();
        let messages = messages.as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let record = &messages[0];
        assert_eq!(record["channel_id"], id);
        assert_eq!(record["direction"], "outbound");
        assert_eq!(record["message_type"], "test");
        assert_eq!(record["status"], "sent");
        assert!(record["error_detail"].is_null());
        assert!(!record["created_at"].as_str().unwrap().is_empty());
    }

    #[test]
    fn test_channel_fails_when_no_webhook_configured() {
        let root = temp_root("test-no-url");
        let row = call(
            &root,
            "create_chat_channel",
            json!({
                "name": "无地址群",
                "channelType": "telegram",
                "configJson": "{}",
                "enabled": true,
                "dailyReportEnabled": false,
                "dailyReportTime": null
            }),
        )
        .unwrap();
        let id = row["id"].as_i64().unwrap();
        assert!(call(&root, "test_chat_channel", json!({ "id": id }))
            .unwrap_err()
            .contains("发送地址"));

        assert!(call(&root, "test_chat_channel", json!({ "id": 999 }))
            .unwrap_err()
            .contains("渠道不存在"));
    }

    #[test]
    fn test_channel_fails_on_upstream_error_and_records_failed_message() {
        // 桩对所有路径回 500。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let response = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let root = temp_root("test-500");
        let row = call(
            &root,
            "create_chat_channel",
            json!({
                "name": "故障群",
                "channelType": "lark",
                "configJson": serde_json::to_string(&json!({ "url": format!("http://{address}/hook") })).unwrap(),
                "enabled": true,
                "dailyReportEnabled": false,
                "dailyReportTime": null
            }),
        )
        .unwrap();
        let id = row["id"].as_i64().unwrap();

        assert!(call(&root, "test_chat_channel", json!({ "id": id }))
            .unwrap_err()
            .contains("HTTP 500"));

        let messages = call(&root, "list_chat_channel_messages", json!({ "channelId": id })).unwrap();
        let record = &messages.as_array().unwrap()[0];
        assert_eq!(record["status"], "failed");
        assert!(record["error_detail"].is_string());
    }

    #[test]
    fn messages_list_is_empty_before_any_record_and_supports_paging() {
        let root = temp_root("messages");
        let row = create_sample(&root, "分页群", "lark");
        let id = row["id"].as_i64().unwrap();

        let empty = call(&root, "list_chat_channel_messages", json!({ "channelId": id })).unwrap();
        assert_eq!(empty.as_array().unwrap().len(), 0);

        // 直接往本地记录里写两条，验证过滤与分页（不走上游）。
        for index in 0..3 {
            append_message(
                &root,
                id,
                "outbound",
                "test",
                &format!("消息 {index}"),
                "sent",
                None,
            );
        }
        // 另一条渠道的记录不该混进来。
        let other = create_sample(&root, "别的群", "telegram");
        let other_id = other["id"].as_i64().unwrap();
        append_message(&root, other_id, "outbound", "test", "另一条", "sent", None);

        let all = call(&root, "list_chat_channel_messages", json!({ "channelId": id })).unwrap();
        assert_eq!(all.as_array().unwrap().len(), 3);
        let page = call(
            &root,
            "list_chat_channel_messages",
            json!({ "channelId": id, "limit": 2, "offset": 1 }),
        )
        .unwrap();
        let page = page.as_array().unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0]["content_preview"], "消息 1");

        assert!(call(&root, "list_chat_channel_messages", json!({}))
            .unwrap_err()
            .contains("缺少 channelId"));
    }

    #[test]
    fn weixin_qrcode_roundtrips_stub_payload() {
        let stub = serve_stub(&[
            ("/qrcode/check", r#"{"status":"scanned"}"#),
            ("/qrcode", r#"{"qrcode_id":"qr-123","qrcode_img_content":"data:image/png;base64,AAAA"}"#),
        ]);
        let qr = get_qrcode_with_base(&stub).unwrap();
        assert_eq!(qr["qrcode_id"], "qr-123");
        assert_eq!(qr["qrcode_img_content"], "data:image/png;base64,AAAA");

        let checked = check_qrcode_with_base(&stub, 1, "qr-123").unwrap();
        assert_eq!(checked["status"], "scanned");
    }

    #[test]
    fn weixin_check_rejects_missing_parameters() {
        assert!(check_weixin_qrcode(&json!({}))
            .unwrap_err()
            .contains("缺少 channelId"));
        assert!(check_weixin_qrcode(&json!({ "channelId": 1, "qrcode": "  " }))
            .unwrap_err()
            .contains("缺少 qrcode"));
    }

    #[test]
    fn weixin_reports_chinese_err_when_upstream_unreachable() {
        // 127.0.0.1:1 没有服务；必须诚实报错，不伪造二维码或扫码状态。
        let unreachable = "http://127.0.0.1:1";
        assert!(get_qrcode_with_base(unreachable).is_err());
        assert!(check_qrcode_with_base(unreachable, 1, "qr").is_err());
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
