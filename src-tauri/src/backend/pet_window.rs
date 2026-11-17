//! 宠物窗口与交互 —— 补全批次（13 条）。
//!
//! 前端调用点见 `codeg-ui/src/lib/pet/api.ts`，返回结构见
//! `codeg-ui/src/lib/pet/types.ts`。这一组命令在原 Go sidecar 与 Rust 里都从未
//! 实现过，契约只由调用点参数名与类型定义决定。
//!
//! ## 架构现实（诚实红线）
//!
//! 本项目是**单窗口 Tauri 壳**：`src-tauri/src/main.rs` 只创建一个主 WebView，
//! 前端的 `/pet`、`/pet-panel` 路由都跑在这同一个壳里，后端 HTTP 派发层
//! （本模块所在链路）拿不到 Tauri 窗口句柄，也没有独立子窗口的创建/关闭/缩放
//! 通道。因此：
//!
//! * **真实可做的部分（L1）**：窗口配置、面板几何、当前宠物等状态落
//!   `.data/pet-settings.json`；状态变更通过 `crate::backend::emit_event` 广播给
//!   前端渲染层；精灵图替换写回本地素材；市场素材走真实 HTTP 回读。
//! * **做不到的部分（L3，`limited:`）**：真正弹出/关闭/前置一个独立宠物窗口、
//!   原生右键菜单、主窗口内标签聚焦。这些命令只做入参校验 + 状态记录 + 事件
//!   广播，**不伪装成窗口已被操作**；文档注释逐条标明受限原因。
//!
//! 配置文件形状（`PetWindowConfig`）：
//! `{ enabled, activePetId, x, y, scale, alwaysOnTop }`；另外持久化面板内部状态
//! `panelOpen` / `panelHeight`，不随配置视图外泄。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine;
use serde_json::{json, Value};
use url::Url;

use crate::backend::net;
use crate::backend::store_file;

/// 单次事件序号，保证 oneshot 事件键单调递增（前端按键去重重播）。
static ONESHOT_SEQ: AtomicUsize = AtomicUsize::new(0);
/// 市场素材请求短超时。
const ASSET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
const MAX_ASSET_BYTES: usize = 25 * 1024 * 1024;

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "open_pet_window"
            | "close_pet_window"
            | "resize_pet_panel"
            | "toggle_pet_panel"
            | "close_pet_panel"
            | "focus_conversation"
            | "pet_celebrate"
            | "pet_set_active"
            | "pet_save_window_state"
            | "pet_window_record_position"
            | "pet_show_context_menu"
            | "pet_replace_sprite"
            | "pet_marketplace_asset"
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
        "open_pet_window" => open_window(root),
        "close_pet_window" => close_window(root),
        "resize_pet_panel" => resize_panel(root, args),
        "toggle_pet_panel" => toggle_panel(root),
        "close_pet_panel" => close_panel(root),
        "focus_conversation" => focus_conversation(args),
        "pet_celebrate" => celebrate(args),
        "pet_set_active" => set_active(root, args),
        "pet_save_window_state" => save_window_state(root, args),
        "pet_window_record_position" => record_position(root, args),
        "pet_show_context_menu" => show_context_menu(args),
        "pet_replace_sprite" => replace_sprite(root, args),
        "pet_marketplace_asset" => marketplace_asset(args),
        _ => Err(format!("未知的宠物窗口命令：{command}")),
    }
}

// ── 配置持久化（与 pet.rs 共用同一份 .data/pet-settings.json）────────────────

fn settings_path(root: &Path) -> PathBuf {
    root.join(".data").join("pet-settings.json")
}

fn pets_dir(root: &Path) -> PathBuf {
    root.join(".data").join("pets")
}

fn assets_dir(root: &Path) -> PathBuf {
    pets_dir(root).join("assets")
}

fn pet_path(root: &Path, id: &str) -> PathBuf {
    pets_dir(root).join(format!("{id}.json"))
}

fn default_doc() -> Value {
    json!({
        "enabled": true,
        "activePetId": Value::Null,
        "x": Value::Null,
        "y": Value::Null,
        "scale": 1.0,
        "alwaysOnTop": false,
        "panelOpen": false,
        "panelHeight": Value::Null,
    })
}

fn load_doc(root: &Path) -> Value {
    store_file::read_json(&settings_path(root))
        .ok()
        .flatten()
        .filter(Value::is_object)
        .unwrap_or_else(default_doc)
}

fn save_doc(root: &Path, doc: &Value) -> Result<(), String> {
    let _lock = store_file::lock(&settings_path(root))?;
    store_file::write_json(&settings_path(root), doc)
}

/// 对外配置视图：只暴露 `PetWindowConfig` 的六个字段。
fn config_view(doc: &Value) -> Value {
    json!({
        "enabled": doc.get("enabled").cloned().unwrap_or(json!(true)),
        "activePetId": doc.get("activePetId").cloned().unwrap_or(Value::Null),
        "x": doc.get("x").cloned().unwrap_or(Value::Null),
        "y": doc.get("y").cloned().unwrap_or(Value::Null),
        "scale": doc.get("scale").cloned().unwrap_or(json!(1.0)),
        "alwaysOnTop": doc.get("alwaysOnTop").cloned().unwrap_or(json!(false)),
    })
}

fn validate_pet_id(raw: Option<&Value>, field: &str) -> Result<String, String> {
    let id = raw
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("缺少 {field} 参数"))?;
    if id.len() > 64
        || !id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        return Err(format!("{field} 只能包含小写字母、数字、'-' 或 '_'"));
    }
    Ok(id.to_string())
}

// ── 窗口开/关（limited：单窗口架构下不创建独立子窗口）───────────────────────

/// `limited: 本后端派发层持有不到 Tauri 窗口句柄，无法从 HTTP 层弹出独立宠物
/// 子窗口。这里只把 `enabled` 置真并广播 `pet://window`，由单窗口内的前端路由
/// 决定是否渲染宠物；不返回“窗口已打开”的伪造成功态。
fn open_window(root: &Path) -> Result<Value, String> {
    let mut doc = load_doc(root);
    doc["enabled"] = json!(true);
    save_doc(root, &doc)?;
    crate::backend::emit_event("pet://window", json!({ "open": true, "limited": true }));
    Ok(Value::Null)
}

/// `limited: 同上，无法关闭一个本层没有句柄的子窗口；仅落 `enabled=false` 并广播。
fn close_window(root: &Path) -> Result<Value, String> {
    let mut doc = load_doc(root);
    doc["enabled"] = json!(false);
    save_doc(root, &doc)?;
    crate::backend::emit_event("pet://window", json!({ "open": false, "limited": true }));
    Ok(Value::Null)
}

// ── 面板几何 ────────────────────────────────────────────────────────────────

fn number_field(args: &Value, field: &str) -> Result<f64, String> {
    args.get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{field} 必须是数字"))
}

/// 记录面板高度并视为展开态；真实面板是主窗口内的一段路由，后端只能存几何 + 广播。
fn resize_panel(root: &Path, args: &Value) -> Result<Value, String> {
    let height = number_field(args, "height")?;
    if height < 0.0 {
        return Err("height 不能为负数".into());
    }
    let mut doc = load_doc(root);
    doc["panelOpen"] = json!(true);
    doc["panelHeight"] = json!(height);
    save_doc(root, &doc)?;
    crate::backend::emit_event(
        "pet://panel",
        json!({ "open": true, "height": height, "limited": true }),
    );
    Ok(Value::Null)
}

/// 翻转面板展开/收起标记并广播；竞态（点错图标随即失焦）由前端既有守卫处理。
fn toggle_panel(root: &Path) -> Result<Value, String> {
    let mut doc = load_doc(root);
    let next = !doc.get("panelOpen").and_then(Value::as_bool).unwrap_or(false);
    doc["panelOpen"] = json!(next);
    save_doc(root, &doc)?;
    crate::backend::emit_event(
        "pet://panel",
        json!({ "open": next, "height": doc.get("panelHeight").cloned().unwrap_or(Value::Null), "limited": true }),
    );
    Ok(Value::Null)
}

fn close_panel(root: &Path) -> Result<Value, String> {
    let mut doc = load_doc(root);
    doc["panelOpen"] = json!(false);
    save_doc(root, &doc)?;
    crate::backend::emit_event("pet://panel", json!({ "open": false, "limited": true }));
    Ok(Value::Null)
}

// ── 会话聚焦（limited：无法从前置主窗口）────────────────────────────────────

/// `limited: 本 HTTP 后端无法 raise/前置主窗口，也拿不到渲染层标签句柄；校验
/// 入参后把目标会话通过 `pet://focus-conversation` 广播给前端桥接层执行聚焦。
fn focus_conversation(args: &Value) -> Result<Value, String> {
    let folder_id = args
        .get("folderId")
        .and_then(Value::as_i64)
        .ok_or_else(|| "缺少 folderId 参数".to_string())?;
    let conversation_id = args
        .get("conversationId")
        .and_then(Value::as_i64)
        .ok_or_else(|| "缺少 conversationId 参数".to_string())?;
    let agent = args
        .get("agent")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 agent 参数".to_string())?;
    if folder_id < 0 || conversation_id < 0 {
        return Err("folderId 与 conversationId 不能为负数".into());
    }
    crate::backend::emit_event(
        "pet://focus-conversation",
        json!({
            "folderId": folder_id,
            "conversationId": conversation_id,
            "agent": agent,
            "limited": true,
        }),
    );
    Ok(Value::Null)
}

// ── 庆祝动画 ────────────────────────────────────────────────────────────────

const ONESHOT_KINDS: [&str; 3] = ["jumping", "waving", "failed"];

/// 广播一次 oneshot 动画事件；`key` 单调递增，前端按键去重重播。
fn celebrate(args: &Value) -> Result<Value, String> {
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 kind 参数".to_string())?;
    if !ONESHOT_KINDS.contains(&kind) {
        return Err(format!("kind 必须是 {} 之一", ONESHOT_KINDS.join(" / ")));
    }
    let key = ONESHOT_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    crate::backend::emit_event("pet://oneshot", json!({ "kind": kind, "key": key }));
    Ok(Value::Null)
}

// ── 当前宠物与窗口状态 ──────────────────────────────────────────────────────

fn set_active(root: &Path, args: &Value) -> Result<Value, String> {
    let mut doc = load_doc(root);
    match args.get("petId") {
        Some(Value::Null) | None => doc["activePetId"] = Value::Null,
        Some(Value::String(_)) => {
            let id = validate_pet_id(args.get("petId"), "petId")?;
            doc["activePetId"] = json!(id);
        }
        Some(_) => return Err("petId 必须是字符串或 null".into()),
    }
    save_doc(root, &doc)?;
    let config = config_view(&doc);
    crate::backend::emit_event("pet://active-changed", config.clone());
    Ok(config)
}

fn save_window_state(root: &Path, args: &Value) -> Result<Value, String> {
    let patch = args
        .as_object()
        .ok_or_else(|| "窗口状态必须是对象".to_string())?;
    let mut doc = load_doc(root);

    if let Some(x) = patch.get("x") {
        match x {
            Value::Null => doc["x"] = Value::Null,
            Value::Number(_) => doc["x"] = x.clone(),
            _ => return Err("x 必须是数字或 null".into()),
        }
    }
    if let Some(y) = patch.get("y") {
        match y {
            Value::Null => doc["y"] = Value::Null,
            Value::Number(_) => doc["y"] = y.clone(),
            _ => return Err("y 必须是数字或 null".into()),
        }
    }
    if let Some(scale) = patch.get("scale") {
        let scale = scale
            .as_f64()
            .filter(|value| *value > 0.0 && *value <= 10.0)
            .ok_or_else(|| "scale 必须是 0 到 10 之间的正数".to_string())?;
        doc["scale"] = json!(scale);
    }
    if let Some(always_on_top) = patch.get("alwaysOnTop") {
        doc["alwaysOnTop"] = always_on_top
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| "alwaysOnTop 必须是布尔值".to_string())?;
    }
    if let Some(enabled) = patch.get("enabled") {
        doc["enabled"] = enabled
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| "enabled 必须是布尔值".to_string())?;
    }

    save_doc(root, &doc)?;
    let config = config_view(&doc);
    crate::backend::emit_event("pet://settings", config.clone());
    Ok(config)
}

fn record_position(root: &Path, args: &Value) -> Result<Value, String> {
    let x = number_field(args, "x")?;
    let y = number_field(args, "y")?;
    let mut doc = load_doc(root);
    doc["x"] = json!(x);
    doc["y"] = json!(y);
    save_doc(root, &doc)?;
    Ok(Value::Null)
}

// ── 右键菜单（limited：无原生菜单构建通道）───────────────────────────────────

/// `limited: 本派发层没有接入 Tauri 原生菜单 API，也无法弹出 OS 级上下文菜单；
/// 这里只校验标签与坐标入参。菜单动作（缩放/打开管理/关闭）应由前端在自身
/// 渲染层承接，后端不伪造 `pet://menu-action` 事件。
fn show_context_menu(args: &Value) -> Result<Value, String> {
    let labels = args
        .get("labels")
        .ok_or_else(|| "缺少 labels 参数".to_string())?
        .as_object()
        .ok_or_else(|| "labels 必须是对象".to_string())?;
    for field in ["scale", "openManager", "close"] {
        labels
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("labels.{field} 不能为空"))?;
    }
    let _x = args
        .get("x")
        .and_then(Value::as_f64)
        .ok_or_else(|| "缺少 x 坐标".to_string())?;
    let _y = args
        .get("y")
        .and_then(Value::as_f64)
        .ok_or_else(|| "缺少 y 坐标".to_string())?;
    Ok(Value::Null)
}

// ── 精灵图替换（L1 本地素材写回）────────────────────────────────────────────

fn mime_of(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

fn ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

fn replace_sprite(root: &Path, args: &Value) -> Result<Value, String> {
    let id = validate_pet_id(args.get("id"), "id")?;
    let encoded = args
        .get("spritesheetBase64")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 spritesheetBase64 参数".to_string())?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "spritesheetBase64 不是有效的 base64 数据")?;
    if bytes.is_empty() {
        return Err("精灵图内容不能为空".into());
    }
    if bytes.len() > MAX_ASSET_BYTES {
        return Err("精灵图不能超过 25 MiB".into());
    }
    let mime = mime_of(&bytes).ok_or("仅支持 PNG、JPEG、WebP 或 GIF 精灵图")?;

    let doc = store_file::read_json(&pet_path(root, &id))
        .ok()
        .flatten()
        .filter(Value::is_object)
        .ok_or_else(|| "宠物不存在".to_string())?;

    let dir = assets_dir(root);
    fs::create_dir_all(&dir).map_err(|e| format!("创建素材目录失败：{e}"))?;
    if let Ok(old) = fs::read_dir(&dir) {
        for entry in old.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("{id}.")) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let asset_name = format!("{id}.{}", ext_for_mime(mime));
    let path = dir.join(&asset_name);
    store_file::write_bytes(&path, &bytes)?;

    let mut doc = doc;
    doc["spritesheetPath"] = json!(asset_name);
    let _lock = store_file::lock(&pet_path(root, &id))?;
    store_file::write_json(&pet_path(root, &id), &doc)?;
    Ok(Value::Null)
}

// ── 市场素材代理（L2 HTTP）──────────────────────────────────────────────────

fn safe_asset_url(raw: Option<&Value>) -> Result<Url, String> {
    let text = raw
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 url 参数".to_string())?;
    let parsed = Url::parse(text).map_err(|_| "url 地址无效".to_string())?;
    let scheme = parsed.scheme();
    if scheme != "https" && scheme != "http" {
        return Err("url 仅支持 http/https 地址".into());
    }
    if parsed.host_str().unwrap_or("").is_empty()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("url 包含非法凭证信息".into());
    }
    Ok(parsed)
}

fn marketplace_asset(args: &Value) -> Result<Value, String> {
    let url = safe_asset_url(args.get("url"))?;
    let response = net::no_proxy_client()
        .get(url.as_str())
        .timeout(ASSET_TIMEOUT)
        .send()
        .map_err(|err| format!("下载宠物素材失败：{err}"))?;
    if !response.status().is_success() {
        return Err(format!("下载宠物素材失败：HTTP {}", response.status()));
    }
    let bytes = response.bytes().map_err(|err| format!("读取宠物素材失败：{err}"))?;
    if bytes.is_empty() {
        return Err("宠物素材内容为空".into());
    }
    if bytes.len() > MAX_ASSET_BYTES {
        return Err("宠物素材不能超过 25 MiB".into());
    }
    let mime = mime_of(&bytes).unwrap_or("application/octet-stream");
    Ok(json!({
        "mime": mime,
        "dataBase64": base64::engine::general_purpose::STANDARD.encode(&bytes),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-pet-window-{tag}-{}-{}",
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

    fn png_bytes() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D,
            0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x0D, 0x0B, 0xF2,
            0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
            0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00,
            0x00, 0x00, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ]
    }

    fn sprite_b64() -> String {
        base64::engine::general_purpose::STANDARD.encode(png_bytes())
    }

    #[test]
    fn owns_covers_the_thirteen_documented_commands_only() {
        for command in [
            "open_pet_window",
            "close_pet_window",
            "resize_pet_panel",
            "toggle_pet_panel",
            "close_pet_panel",
            "focus_conversation",
            "pet_celebrate",
            "pet_set_active",
            "pet_save_window_state",
            "pet_window_record_position",
            "pet_show_context_menu",
            "pet_replace_sprite",
            "pet_marketplace_asset",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("pet_list"));
        assert!(!owns("pet_add"));
        assert!(!owns("pet_marketplace_list"));
        assert!(!owns("close_Pet_Panel"));
    }

    #[test]
    fn save_window_state_merges_patch_and_returns_config_shape() {
        let root = temp_root("state");
        let config = call(
            &root,
            "pet_save_window_state",
            json!({ "scale": 1.5, "alwaysOnTop": true, "x": 12, "y": -4 }),
        )
        .unwrap();
        assert_eq!(config["scale"], 1.5);
        assert_eq!(config["alwaysOnTop"], true);
        assert_eq!(config["x"], 12);
        assert_eq!(config["y"], -4);
        assert_eq!(config["enabled"], true);
        assert!(config["activePetId"].is_null());

        // 坏参数：scale 为 0 / 字符串、x 类型错误、对象入参缺失。
        assert!(call(&root, "pet_save_window_state", json!({ "scale": 0 }))
            .unwrap_err()
            .contains("scale"));
        assert!(call(&root, "pet_save_window_state", json!({ "scale": "big" }))
            .unwrap_err()
            .contains("scale"));
        assert!(call(&root, "pet_save_window_state", json!({ "x": "abc" }))
            .unwrap_err()
            .contains("x"));
        assert!(handle(&root, "pet_save_window_state", &json!("not-an-object"))
            .unwrap()
            .unwrap_err()
            .contains("对象"));
    }

    #[test]
    fn set_active_clears_and_sets_pet_id() {
        let root = temp_root("active");
        let config = call(&root, "pet_set_active", json!({ "petId": "fox" })).unwrap();
        assert_eq!(config["activePetId"], "fox");

        let cleared = call(&root, "pet_set_active", json!({ "petId": null })).unwrap();
        assert!(cleared["activePetId"].is_null());

        // 非法 id / 类型错误。
        assert!(call(&root, "pet_set_active", json!({ "petId": "../fox" }))
            .unwrap_err()
            .contains("小写字母"));
        assert!(call(&root, "pet_set_active", json!({ "petId": 42 }))
            .unwrap_err()
            .contains("字符串"));
    }

    #[test]
    fn record_position_persists_x_and_y() {
        let root = temp_root("position");
        call(&root, "pet_window_record_position", json!({ "x": 320, "y": 180 })).unwrap();
        let doc = load_doc(&root);
        assert_eq!(doc["x"].as_f64(), Some(320.0));
        assert_eq!(doc["y"].as_f64(), Some(180.0));

        assert!(call(&root, "pet_window_record_position", json!({ "x": 1, "y": "a" }))
            .unwrap_err()
            .contains("y"));
        assert!(call(&root, "pet_window_record_position", json!({ "x": 1 }))
            .unwrap_err()
            .contains("y"));
    }

    #[test]
    fn panel_resize_toggle_close_roundtrip() {
        let root = temp_root("panel");
        call(&root, "resize_pet_panel", json!({ "height": 240 })).unwrap();
        let doc = load_doc(&root);
        assert_eq!(doc["panelOpen"], true);
        assert_eq!(doc["panelHeight"], 240.0);

        call(&root, "toggle_pet_panel", json!({})).unwrap();
        assert_eq!(load_doc(&root)["panelOpen"], false);
        call(&root, "toggle_pet_panel", json!({})).unwrap();
        assert_eq!(load_doc(&root)["panelOpen"], true);

        call(&root, "close_pet_panel", json!({})).unwrap();
        assert_eq!(load_doc(&root)["panelOpen"], false);

        assert!(call(&root, "resize_pet_panel", json!({ "height": -1 }))
            .unwrap_err()
            .contains("负数"));
        assert!(call(&root, "resize_pet_panel", json!({})).unwrap_err().contains("height"));
    }

    #[test]
    fn open_and_close_record_enabled_flag_without_faking_a_window() {
        let root = temp_root("open-close");
        call(&root, "open_pet_window", json!({})).unwrap();
        assert_eq!(load_doc(&root)["enabled"], true);
        call(&root, "close_pet_window", json!({})).unwrap();
        assert_eq!(load_doc(&root)["enabled"], false);
    }

    #[test]
    fn focus_conversation_validates_target() {
        assert!(call(
            &temp_root("focus"),
            "focus_conversation",
            json!({ "folderId": 1, "conversationId": 2, "agent": "claude_code" })
        )
        .unwrap()
        .is_null());
        assert!(call(&temp_root("focus-bad"), "focus_conversation", json!({}))
            .unwrap_err()
            .contains("folderId"));
        assert!(call(
            &temp_root("focus-bad2"),
            "focus_conversation",
            json!({ "folderId": 1, "conversationId": -1, "agent": "codex" })
        )
        .unwrap_err()
        .contains("负数"));
    }

    #[test]
    fn celebrate_accepts_known_oneshot_kinds_only() {
        for kind in ["jumping", "waving", "failed"] {
            assert!(call(&temp_root("celebrate"), "pet_celebrate", json!({ "kind": kind }))
                .unwrap()
                .is_null());
        }
        assert!(call(&temp_root("celebrate-bad"), "pet_celebrate", json!({ "kind": "dancing" }))
            .unwrap_err()
            .contains("kind"));
        assert!(call(&temp_root("celebrate-bad2"), "pet_celebrate", json!({})).unwrap_err().contains("kind"));
    }

    #[test]
    fn context_menu_validates_labels_and_coordinates() {
        let labels = json!({ "scale": "缩放", "openManager": "管理", "close": "关闭" });
        assert!(call(
            &temp_root("menu"),
            "pet_show_context_menu",
            json!({ "labels": labels, "x": 100, "y": 200 })
        )
        .unwrap()
        .is_null());
        assert!(call(
            &temp_root("menu-bad"),
            "pet_show_context_menu",
            json!({ "labels": { "scale": "", "openManager": "m", "close": "c" }, "x": 1, "y": 2 })
        )
        .unwrap_err()
        .contains("labels.scale"));
        assert!(call(&temp_root("menu-bad2"), "pet_show_context_menu", json!({ "x": 1, "y": 2 }))
            .unwrap_err()
            .contains("labels"));
    }

    #[test]
    fn replace_sprite_swaps_asset_bytes_for_an_existing_pet() {
        let root = temp_root("replace");
        // 夹具：先落一只本地宠物（元数据 + 旧素材）。
        fs::create_dir_all(assets_dir(&root)).unwrap();
        let pet_doc = json!({
            "id": "fox",
            "displayName": "F",
            "description": null,
            "spritesheetPath": format!("assets/fox.png"),
        });
        store_file::write_json(&pet_path(&root, "fox"), &pet_doc).unwrap();
        store_file::write_bytes(&assets_dir(&root).join("fox.png"), b"old").unwrap();

        call(
            &root,
            "pet_replace_sprite",
            json!({ "id": "fox", "spritesheetBase64": sprite_b64() }),
        )
        .unwrap();
        let stored = store_file::read_json(&pet_path(&root, "fox"))
            .unwrap()
            .unwrap();
        assert!(stored["spritesheetPath"].as_str().unwrap().starts_with("fox."));
        let on_disk = fs::read_dir(assets_dir(&root)).unwrap().flatten().count();
        assert_eq!(on_disk, 1);

        // 不存在的宠物 / 坏 base64 / 缺素材。
        assert!(call(
            &root,
            "pet_replace_sprite",
            json!({ "id": "ghost", "spritesheetBase64": sprite_b64() })
        )
        .unwrap_err()
        .contains("宠物不存在"));
        assert!(call(
            &root,
            "pet_replace_sprite",
            json!({ "id": "fox", "spritesheetBase64": "@@@" })
        )
        .unwrap_err()
        .contains("base64"));
        assert!(call(&root, "pet_replace_sprite", json!({ "id": "fox" }))
            .unwrap_err()
            .contains("spritesheetBase64"));
    }

    #[test]
    fn marketplace_asset_proxies_real_bytes_from_local_stub() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let bytes = png_bytes();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let body = bytes.clone();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        let url = format!("http://{address}/sheet.png");
        let asset = call(&temp_root("asset"), "pet_marketplace_asset", json!({ "url": url })).unwrap();
        assert_eq!(asset["mime"], "image/png");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(asset["dataBase64"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, png_bytes());

        // 缺 url / 非法 scheme / 带凭证。
        assert!(call(&temp_root("asset-bad"), "pet_marketplace_asset", json!({}))
            .unwrap_err()
            .contains("url"));
        assert!(call(
            &temp_root("asset-bad2"),
            "pet_marketplace_asset",
            json!({ "url": "file:///etc/passwd" })
        )
        .unwrap_err()
        .contains("http/https"));
        assert!(call(
            &temp_root("asset-bad3"),
            "pet_marketplace_asset",
            json!({ "url": "http://user:pass@127.0.0.1/x" })
        )
        .unwrap_err()
        .contains("凭证"));
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
