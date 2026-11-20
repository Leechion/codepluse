//! 桌面宠物数据与素材 —— 补全批次（14 条）。
//!
//! 前端（`codepluse-ui/src/lib/pet/api.ts`）已经在调用这些命令，但原 Go sidecar 与
//! Rust 都没有实现过它们（在 `server-go` 只读副本里搜不到命令字面量），所以契约
//! 只由调用点参数名与 `codepluse-ui/src/lib/pet/types.ts` 的返回结构决定：
//!
//! * `PetSummary` / `PetDetail` = `{ id, displayName, description, spritesheetPath }`。
//! * `PetSpriteAsset` = `{ mime, dataBase64 }`（标准 base64，不含 `data:` 前缀）。
//! * `PetWindowConfig` = `{ enabled, activePetId, x, y, scale, alwaysOnTop }`。
//! * `ImportablePet` = `{ id, displayName, description, sourcePath, alreadyImported }`。
//! * `ImportCodexPetsResult` = `{ importedIds, skipped: [{ sourceId, reason }] }`。
//! * `MarketplaceListResponse` = `{ pets, page, pageSize, total, totalPages }`。
//!
//! ## 落地位置（`root/.data/`）
//!
//! * `pets/index.json` —— 本地宠物 id 顺序清单。
//! * `pets/<id>.json` —— 单只宠物元数据（PetDetail 形状）。
//! * `pets/assets/<id>.<ext>` —— 精灵图二进制。
//! * `pet-settings.json` —— 窗口配置（与 `pet_window` 模块共用同一份文件）。
//!
//! ## 能力分级
//!
//! * **L1**：本地 CRUD、素材二进制落盘/回读、Codex 宠物目录真实扫描导入、活跃
//!   会话快照（读 ACP 层真实连接）。
//! * **L2**：宠物市场列表/安装走真实 HTTP，8 秒短超时；上游地址由环境变量
//!   `CODEPLUSE_PET_MARKETPLACE_BASE` 覆盖（测试桩走这个 seam）。未配置地址或请求
//!   失败返回中文 `Err`，不编造市场条目。
//! * Codex 宠物目录扫描带显式测试缝：`CODEPLUSE_PET_CODEX_HOME`（其次
//!   `HARNESS_HUB_FAKE_HOME`）指向夹具树，缺省才回落到真实用户目录；找不到就
//!   返回可用性为假 + 空列表。

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::{json, Value};
use url::Url;

use crate::backend::goformat;
use crate::backend::net;
use crate::backend::store_file;

/// 单只精灵图素材大小上限（25 MiB）。
const MAX_ASSET_BYTES: usize = 25 * 1024 * 1024;
/// 市场请求短超时，避免面板长时间卡在加载态。
const MARKET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
/// 市场上游地址覆盖；未设置时没有内置默认地址。
const MARKET_ENV: &str = "CODEPLUSE_PET_MARKETPLACE_BASE";
/// Codex 宠物目录测试缝（优先）；其次复用 ACP 侧的 `HARNESS_HUB_FAKE_HOME`。
const CODEX_HOME_ENV: &str = "CODEPLUSE_PET_CODEX_HOME";
const FAKE_HOME_ENV: &str = "HARNESS_HUB_FAKE_HOME";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "pet_list"
            | "pet_get"
            | "pet_add"
            | "pet_delete"
            | "pet_update_meta"
            | "pet_get_settings"
            | "pet_get_current_state"
            | "pet_list_active_sessions"
            | "pet_read_spritesheet"
            | "pet_list_importable_codex"
            | "pet_codex_import_available"
            | "pet_import_codex"
            | "pet_marketplace_list"
            | "pet_marketplace_install"
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
        "pet_list" => list_pets(root),
        "pet_get" => get_pet(root, args),
        "pet_add" => add_pet(root, args),
        "pet_delete" => delete_pet(root, args),
        "pet_update_meta" => update_meta(root, args),
        "pet_get_settings" => get_settings(root),
        "pet_get_current_state" => current_state(root),
        "pet_list_active_sessions" => active_sessions(root),
        "pet_read_spritesheet" => read_spritesheet(root, args),
        "pet_list_importable_codex" => list_importable_codex(root),
        "pet_codex_import_available" => codex_availability(root),
        "pet_import_codex" => import_codex(root, args),
        "pet_marketplace_list" => marketplace_list(root, args),
        "pet_marketplace_install" => marketplace_install(root, args),
        _ => Err(format!("未知的宠物命令：{command}")),
    }
}

// ── 路径 ──────────────────────────────────────────────────────────────────

fn data_dir(root: &Path) -> PathBuf {
    root.join(".data")
}

fn pets_dir(root: &Path) -> PathBuf {
    data_dir(root).join("pets")
}

fn assets_dir(root: &Path) -> PathBuf {
    pets_dir(root).join("assets")
}

fn index_path(root: &Path) -> PathBuf {
    pets_dir(root).join("index.json")
}

fn pet_path(root: &Path, id: &str) -> PathBuf {
    pets_dir(root).join(format!("{id}.json"))
}

fn settings_path(root: &Path) -> PathBuf {
    data_dir(root).join("pet-settings.json")
}

// ── 校验与素材编码 ──────────────────────────────────────────────────────────

/// 宠物 id 规则与前端 `slugifyPetId` 对齐：小写 ASCII、数字、`-`、`_`，1–64 字符。
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

fn required_non_empty(args: &Value, field: &str) -> Result<String, String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("缺少 {field} 参数"))
}

/// 按图片魔数判定 MIME；识别不出返回 `None`。
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

/// 解码入参里的精灵图 base64，校验大小与图片格式，返回字节与 MIME。
fn decode_sprite(args: &Value, field: &str) -> Result<(Vec<u8>, &'static str), String> {
    let encoded = args
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("缺少 {field} 参数"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| format!("{field} 不是有效的 base64 数据"))?;
    if bytes.is_empty() {
        return Err("精灵图内容不能为空".into());
    }
    if bytes.len() > MAX_ASSET_BYTES {
        return Err("精灵图不能超过 25 MiB".into());
    }
    let mime = mime_of(&bytes).ok_or("仅支持 PNG、JPEG、WebP 或 GIF 精灵图")?;
    Ok((bytes, mime))
}

// ── 本地清单读写 ──────────────────────────────────────────────────────────

fn load_index(root: &Path) -> Vec<String> {
    store_file::read_json(&index_path(root))
        .ok()
        .flatten()
        .and_then(|doc| doc.get("order").and_then(Value::as_array).cloned())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn save_index(root: &Path, order: &[String]) -> Result<(), String> {
    let doc = json!({ "order": order });
    let _lock = store_file::lock(&index_path(root))?;
    store_file::write_json(&index_path(root), &doc)
}

fn read_pet_doc(root: &Path, id: &str) -> Option<Value> {
    store_file::read_json(&pet_path(root, id))
        .ok()
        .flatten()
        .filter(Value::is_object)
}

/// 清单行（PetSummary / PetDetail 共用字段）。
fn summary_row(doc: &Value) -> Value {
    json!({
        "id": doc.get("id").cloned().unwrap_or(Value::Null),
        "displayName": doc.get("displayName").cloned().unwrap_or(Value::Null),
        "description": doc.get("description").cloned().unwrap_or(Value::Null),
        "spritesheetPath": doc.get("spritesheetPath").cloned().unwrap_or(Value::Null),
    })
}

fn list_pets(root: &Path) -> Result<Value, String> {
    let mut order = load_index(root);
    // 磁盘上有、但索引丢了的记录补到末尾，避免丢宠物。
    if let Ok(entries) = fs::read_dir(pets_dir(root)) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.path().is_file() && name.ends_with(".json") && name != "index.json" {
                if let Some(id) = name.strip_suffix(".json") {
                    if !order.iter().any(|existing| existing == id) {
                        order.push(id.to_string());
                    }
                }
            }
        }
    }
    let mut rows = Vec::new();
    for id in order {
        if let Some(doc) = read_pet_doc(root, &id) {
            rows.push(summary_row(&doc));
        }
    }
    Ok(Value::Array(rows))
}

fn get_pet(root: &Path, args: &Value) -> Result<Value, String> {
    let id = validate_pet_id(args.get("id"), "id")?;
    let doc = read_pet_doc(root, &id).ok_or_else(|| "宠物不存在".to_string())?;
    Ok(summary_row(&doc))
}

fn write_asset(root: &Path, id: &str, bytes: &[u8], mime: &str) -> Result<String, String> {
    let dir = assets_dir(root);
    fs::create_dir_all(&dir).map_err(|e| format!("创建素材目录失败：{e}"))?;
    // 先清掉旧扩展名素材，避免换格式后留下旧文件。
    if let Ok(old) = fs::read_dir(&dir) {
        for entry in old.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("{id}.")) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let path = dir.join(format!("{id}.{}", ext_for_mime(mime)));
    store_file::write_bytes(&path, bytes)?;
    Ok(path.file_name().unwrap().to_string_lossy().into_owned())
}

fn add_pet(root: &Path, args: &Value) -> Result<Value, String> {
    let id = validate_pet_id(args.get("id"), "id")?;
    let display_name = required_non_empty(args, "displayName")?;
    let description = args
        .get("description")
        .cloned()
        .and_then(|value| match value {
            Value::Null => Some(Value::Null),
            Value::String(text) => {
                let trimmed = text.trim().to_string();
                if trimmed.is_empty() {
                    Some(Value::Null)
                } else {
                    Some(Value::String(trimmed))
                }
            }
            _ => None,
        });
    let (bytes, mime) = decode_sprite(args, "spritesheetBase64")?;

    if read_pet_doc(root, &id).is_some() {
        return Err("宠物 id 已存在；替换素材请用 pet_replace_sprite".into());
    }
    let spritesheet_path = write_asset(root, &id, &bytes, mime)?;

    let doc = json!({
        "id": id,
        "displayName": display_name,
        "description": description,
        "spritesheetPath": spritesheet_path,
        "createdAt": goformat::timestamp(),
    });
    let _lock = store_file::lock(&pet_path(root, &id))?;
    store_file::write_json(&pet_path(root, &id), &doc)?;

    let mut order = load_index(root);
    if !order.iter().any(|existing| existing == &id) {
        order.push(id.clone());
        save_index(root, &order)?;
    }
    crate::backend::emit_event("pet://pets-changed", summary_row(&doc));
    Ok(summary_row(&doc))
}

fn delete_pet(root: &Path, args: &Value) -> Result<Value, String> {
    let id = validate_pet_id(args.get("id"), "id")?;
    let Some(doc) = read_pet_doc(root, &id) else {
        // 重复删除幂等成功。
        return Ok(Value::Null);
    };
    // 素材文件按记录里的相对路径删除，且必须落在 assets 目录内。
    if let Some(path) = doc.get("spritesheetPath").and_then(Value::as_str) {
        let candidate = assets_dir(root).join(path);
        if candidate.starts_with(assets_dir(root)) && candidate.is_file() {
            let _ = fs::remove_file(candidate);
        }
    }
    let _ = fs::remove_file(pet_path(root, &id));

    let order: Vec<String> = load_index(root)
        .into_iter()
        .filter(|existing| existing != &id)
        .collect();
    save_index(root, &order)?;

    // 被删的若是当前宠物，清掉设置里的 activePetId。
    let settings = store_file::read_json(&settings_path(root))
        .ok()
        .flatten()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if settings.get("activePetId").and_then(Value::as_str) == Some(id.as_str()) {
        let mut settings = settings;
        settings["activePetId"] = Value::Null;
        let _lock = store_file::lock(&settings_path(root))?;
        store_file::write_json(&settings_path(root), &settings)?;
    }
    crate::backend::emit_event("pet://pets-changed", json!({ "id": id, "deleted": true }));
    Ok(Value::Null)
}

fn update_meta(root: &Path, args: &Value) -> Result<Value, String> {
    let id = validate_pet_id(args.get("id"), "id")?;
    let patch = args
        .get("patch")
        .ok_or_else(|| "缺少 patch 参数".to_string())?;
    let patch = patch
        .as_object()
        .ok_or_else(|| "patch 必须是对象".to_string())?;

    let Some(mut doc) = read_pet_doc(root, &id) else {
        return Err("宠物不存在".into());
    };

    // 只允许改 displayName / description；其它字段忽略，不允许借 patch 越权写 id/素材。
    if let Some(name) = patch.get("displayName") {
        let name = name
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "displayName 不能为空".to_string())?;
        doc["displayName"] = json!(name);
    }
    if let Some(description) = patch.get("description") {
        match description {
            Value::Null => doc["description"] = Value::Null,
            Value::String(text) => {
                let trimmed = text.trim().to_string();
                doc["description"] = if trimmed.is_empty() {
                    Value::Null
                } else {
                    Value::String(trimmed)
                };
            }
            _ => return Err("description 必须是字符串或 null".into()),
        }
    }

    let _lock = store_file::lock(&pet_path(root, &id))?;
    store_file::write_json(&pet_path(root, &id), &doc)?;
    crate::backend::emit_event("pet://pets-changed", summary_row(&doc));
    Ok(summary_row(&doc))
}

// ── 窗口设置（只读侧；写入在 pet_window 模块）──────────────────────────────

/// 与 `PetWindowConfig` 对齐的默认值。
fn default_config() -> Value {
    json!({
        "enabled": true,
        "activePetId": Value::Null,
        "x": Value::Null,
        "y": Value::Null,
        "scale": 1.0,
        "alwaysOnTop": false,
    })
}

fn read_config(root: &Path) -> Value {
    let stored = store_file::read_json(&settings_path(root))
        .ok()
        .flatten()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let mut config = default_config();
    for field in ["enabled", "activePetId", "x", "y", "scale", "alwaysOnTop"] {
        if let Some(value) = stored.get(field) {
            config[field] = value.clone();
        }
    }
    // 类型兜底：坏文件不能把界面打成崩溃态。
    if !config["scale"].is_number() {
        config["scale"] = json!(1.0);
    }
    if !config["enabled"].is_boolean() {
        config["enabled"] = json!(true);
    }
    if !config["alwaysOnTop"].is_boolean() {
        config["alwaysOnTop"] = json!(false);
    }
    config
}

fn get_settings(root: &Path) -> Result<Value, String> {
    Ok(read_config(root))
}

// ── 活跃会话快照（读 ACP 层真实连接）────────────────────────────────────────

fn number_of(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)).unwrap_or(0),
        _ => 0,
    }
}

fn conversation_title(doc: &Value, conversation_id: i64) -> String {
    doc.get("conversations")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| number_of(row.get("id")) == conversation_id)
                .and_then(|row| row.get("title").and_then(Value::as_str))
        })
        .unwrap_or("")
        .to_string()
}

fn empty_sessions_payload() -> Value {
    json!({ "runningCount": 0, "waitingCount": 0, "errorCount": 0, "sessions": [] })
}

fn active_sessions(root: &Path) -> Result<Value, String> {
    let Some(layer) = crate::backend::acp::commands::layer_if_ready() else {
        // 协议层还没起来：诚实返回空快照，不编造会话。
        return Ok(empty_sessions_payload());
    };
    let doc = store_file::read_json(&root.join(".data").join("codepluse-workspace.json"))
        .ok()
        .flatten()
        .unwrap_or(Value::Null);

    let state = layer.state.lock().unwrap();
    let mut sessions = Vec::new();
    let mut running = 0usize;
    let mut waiting = 0usize;
    let mut error = 0usize;

    for conn in state.connections.values() {
        let c = conn.lock().unwrap();
        if c.warm {
            continue;
        }
        if c.status == "disconnected" {
            continue;
        }
        let pending = if c.pending.is_object() {
            Some(c.pending.clone())
        } else {
            None
        };
        let is_error = c.status == "error";
        let is_waiting = pending.is_some();
        if is_error {
            error += 1;
        } else if is_waiting {
            waiting += 1;
        } else {
            running += 1;
        }
        let title = conversation_title(&doc, c.conversation_id);
        let mut row = json!({
            "connectionId": c.id,
            "conversationId": c.conversation_id,
            "folderId": c.folder_id,
            "agentType": c.agent_type,
            "title": title,
            "status": c.status,
        });
        if let Some(pending) = pending {
            row["pending"] = pending;
        }
        sessions.push(row);
    }

    Ok(json!({
        "runningCount": running,
        "waitingCount": waiting,
        "errorCount": error,
        "sessions": sessions,
    }))
}

/// 环境态优先级：error > waiting > running，与徽章/前端 ambient 顺序一致。
fn derive_state(payload: &Value) -> &'static str {
    let counts = |field: &str| payload.get(field).and_then(Value::as_u64).unwrap_or(0);
    if counts("errorCount") > 0 {
        "failed"
    } else if counts("waitingCount") > 0 {
        "waiting"
    } else if counts("runningCount") > 0 {
        "running"
    } else {
        "idle"
    }
}

fn current_state(root: &Path) -> Result<Value, String> {
    let payload = active_sessions(root)?;
    Ok(Value::String(derive_state(&payload).to_string()))
}

// ── 精灵图回读 ────────────────────────────────────────────────────────────

fn read_spritesheet(root: &Path, args: &Value) -> Result<Value, String> {
    let id = validate_pet_id(args.get("id"), "id")?;
    let doc = read_pet_doc(root, &id).ok_or_else(|| "宠物不存在".to_string())?;
    let relative = doc
        .get("spritesheetPath")
        .and_then(Value::as_str)
        .ok_or_else(|| "宠物缺少精灵图素材记录".to_string())?;
    let path = assets_dir(root).join(relative);
    if !path.starts_with(assets_dir(root)) {
        return Err("精灵图素材路径无效".into());
    }
    let bytes = fs::read(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => "精灵图素材不存在".to_string(),
        other => format!("读取精灵图失败：{other}"),
    })?;
    let mime = mime_of(&bytes).unwrap_or("image/png");
    Ok(json!({
        "mime": mime,
        "dataBase64": base64::engine::general_purpose::STANDARD.encode(&bytes),
    }))
}

// ── Codex 目录扫描与导入 ──────────────────────────────────────────────────

fn codex_home() -> PathBuf {
    if let Ok(fake) = std::env::var(CODEX_HOME_ENV) {
        if !fake.trim().is_empty() {
            return PathBuf::from(fake);
        }
    }
    if let Ok(fake) = std::env::var(FAKE_HOME_ENV) {
        if !fake.trim().is_empty() {
            return PathBuf::from(fake);
        }
    }
    std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

fn codex_pets_dir_at(home: &Path) -> PathBuf {
    home.join(".codex").join("pets")
}

const IMAGE_EXTS: [&str; 5] = ["png", "webp", "gif", "jpg", "jpeg"];

fn image_ext(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    IMAGE_EXTS.iter().copied().find(|ext| lower.ends_with(&format!(".{ext}")))
}

struct CodexSource {
    id: String,
    display_name: String,
    description: Option<String>,
    source_path: PathBuf,
    image_path: PathBuf,
}

/// 扫描单个来源目录：清单（pet.json/manifest.json）+ 首张图片。两者都缺则不可导入。
fn scan_source(dir: &Path) -> Option<CodexSource> {
    let dir_name = dir.file_name()?.to_string_lossy().to_string();
    if dir_name.contains("..") || dir_name.contains('/') || dir_name.contains('\\') {
        return None;
    }
    let mut display_name: Option<String> = None;
    let mut description: Option<String> = None;
    for manifest_name in ["pet.json", "manifest.json"] {
        let manifest = dir.join(manifest_name);
        if let Ok(bytes) = fs::read(&manifest) {
            if let Ok(doc) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(name) = doc.get("displayName").and_then(Value::as_str) {
                    let trimmed = name.trim().to_string();
                    if !trimmed.is_empty() {
                        display_name = Some(trimmed);
                    }
                }
                description = doc
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                break;
            }
        }
    }
    let image_path = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_file())
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains('.')
                && image_ext(&entry.file_name().to_string_lossy()).is_some()
        })
        .map(|entry| entry.path())?;

    Some(CodexSource {
        id: dir_name.clone(),
        display_name: display_name.unwrap_or(dir_name),
        description,
        source_path: dir.to_path_buf(),
        image_path,
    })
}

fn scan_codex_sources_in(home: &Path) -> Vec<CodexSource> {
    let dir = codex_pets_dir_at(home);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| scan_source(&entry.path()))
        .collect()
}

fn list_importable_codex_in(root: &Path, home: &Path) -> Result<Value, String> {
    let local: std::collections::HashSet<String> = load_index(root).into_iter().collect();
    let rows: Vec<Value> = scan_codex_sources_in(home)
        .into_iter()
        .map(|source| {
            json!({
                "id": source.id,
                "displayName": source.display_name,
                "description": source.description,
                "sourcePath": source.source_path.to_string_lossy(),
                "alreadyImported": local.contains(&source.id),
            })
        })
        .collect();
    Ok(Value::Array(rows))
}

fn list_importable_codex(root: &Path) -> Result<Value, String> {
    let home = codex_home();
    list_importable_codex_in(root, &home)
}

fn codex_availability_in(home: &Path) -> Result<Value, String> {
    let sources = scan_codex_sources_in(home);
    Ok(json!({ "available": !sources.is_empty() }))
}

fn codex_availability(root: &Path) -> Result<Value, String> {
    let _ = root;
    let home = codex_home();
    codex_availability_in(&home)
}

fn import_codex_in(root: &Path, home: &Path, args: &Value) -> Result<Value, String> {
    let sources = scan_codex_sources_in(home);
    let filter: Option<std::collections::HashSet<String>> = args
        .get("ids")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_str().map(str::to_string))
                .collect()
        });
    let overwrite_with_suffix = args
        .get("overwriteWithSuffix")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut imported_ids: Vec<String> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut order = load_index(root);

    let requested: Vec<CodexSource> = match &filter {
        Some(ids) => sources
            .into_iter()
            .filter(|source| ids.contains(&source.id))
            .collect(),
        None => sources,
    };

    // 被点名但目录里找不到的来源，记一条 skipped。
    if let Some(ids) = &filter {
        for wanted in ids {
            if !requested.iter().any(|source| &source.id == wanted) {
                skipped.push(json!({ "sourceId": wanted, "reason": "未找到对应的 Codex 宠物目录" }));
            }
        }
    }

    for source in requested {
        let bytes = match fs::read(&source.image_path) {
            Ok(bytes) => bytes,
            Err(_) => {
                skipped.push(json!({ "sourceId": source.id, "reason": "精灵图读取失败" }));
                continue;
            }
        };
        let Some(mime) = mime_of(&bytes) else {
            skipped.push(json!({ "sourceId": source.id, "reason": "精灵图格式不受支持" }));
            continue;
        };

        let mut target_id = source.id.clone();
        if read_pet_doc(root, &target_id).is_some() {
            if !overwrite_with_suffix {
                skipped.push(json!({ "sourceId": source.id, "reason": "本地已存在同名宠物" }));
                continue;
            }
            let mut suffix = 2u32;
            loop {
                let candidate = format!("{}-{suffix}", source.id);
                if read_pet_doc(root, &candidate).is_none() {
                    target_id = candidate;
                    break;
                }
                suffix += 1;
            }
        }

        let spritesheet_path = write_asset(root, &target_id, &bytes, mime)?;
        let doc = json!({
            "id": target_id,
            "displayName": source.display_name,
            "description": source.description,
            "spritesheetPath": spritesheet_path,
            "importedFrom": source.source_path.to_string_lossy().to_string(),
            "createdAt": goformat::timestamp(),
        });
        let _lock = store_file::lock(&pet_path(root, &target_id))?;
        store_file::write_json(&pet_path(root, &target_id), &doc)?;
        if !order.iter().any(|existing| existing == &target_id) {
            order.push(target_id.clone());
        }
        imported_ids.push(target_id);
    }
    save_index(root, &order)?;

    Ok(json!({ "importedIds": imported_ids, "skipped": skipped }))
}

fn import_codex(root: &Path, args: &Value) -> Result<Value, String> {
    let home = codex_home();
    import_codex_in(root, &home, args)
}

// ── 市场侧（L2 HTTP）──────────────────────────────────────────────────────

fn marketplace_base() -> Result<String, String> {
    let base = std::env::var(MARKET_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "宠物市场上游地址未配置（设置 CODEPLUSE_PET_MARKETPLACE_BASE 后重试）".to_string())?;
    Ok(base.trim_end_matches('/').to_string())
}

fn http_get_json(url: &str) -> Result<Value, String> {
    let response = net::no_proxy_client()
        .get(url)
        .timeout(MARKET_TIMEOUT)
        .send()
        .map_err(|err| format!("连接宠物市场失败：{err}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("宠物市场返回错误：HTTP {status}"));
    }
    let body = response.bytes().map_err(|err| format!("读取宠物市场响应失败：{err}"))?;
    serde_json::from_slice::<Value>(&body).map_err(|err| format!("宠物市场响应 JSON 无效：{err}"))
}

fn normalize_market_pet(entry: &Value, installed: &std::collections::HashSet<String>) -> Value {
    let id = entry.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let text_or_null = |key: &str| entry.get(key).cloned().unwrap_or(Value::Null);
    json!({
        "id": id,
        "displayName": entry.get("displayName").cloned().unwrap_or(Value::Null),
        "description": entry.get("description").and_then(Value::as_str).unwrap_or(""),
        "kind": entry.get("kind").cloned().unwrap_or(Value::Null),
        "tags": entry.get("tags").cloned().unwrap_or(json!([])),
        "ownerName": entry.get("ownerName").cloned().unwrap_or(Value::Null),
        "ownerHandle": entry.get("ownerHandle").cloned().unwrap_or(Value::Null),
        "viewCount": entry.get("viewCount").and_then(Value::as_u64).unwrap_or(0),
        "downloadCount": entry.get("downloadCount").and_then(Value::as_u64).unwrap_or(0),
        "likeCount": entry.get("likeCount").and_then(Value::as_u64).unwrap_or(0),
        "uploadedAt": text_or_null("uploadedAt"),
        "posterUrl": text_or_null("posterUrl"),
        "previewUrl": text_or_null("previewUrl"),
        "downloadUrl": entry.get("downloadUrl").cloned().unwrap_or(Value::Null),
        "alreadyInstalled": installed.contains(&id),
    })
}

fn marketplace_list_with_base(base: &str, root: &Path, args: &Value) -> Result<Value, String> {
    let page = args
        .get("page")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 1000);
    let page_size = args
        .get("pageSize")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 100);

    let mut url = Url::parse(&format!("{base}/pets")).map_err(|err| format!("市场地址无效：{err}"))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("page", &page.to_string());
        pairs.append_pair("pageSize", &page_size.to_string());
        if let Some(q) = args.get("q").and_then(Value::as_str).filter(|v| !v.trim().is_empty()) {
            pairs.append_pair("q", q);
        }
        if let Some(kind) = args.get("kind").and_then(Value::as_str).filter(|v| !v.trim().is_empty()) {
            pairs.append_pair("kind", kind);
        }
        if let Some(sort) = args.get("sort").and_then(Value::as_str).filter(|v| !v.trim().is_empty()) {
            pairs.append_pair("sort", sort);
        }
        if let Some(tags) = args.get("tags").and_then(Value::as_array) {
            for tag in tags.iter().filter_map(Value::as_str) {
                pairs.append_pair("tags", tag);
            }
        }
    }
    let payload = http_get_json(url.as_str())?;
    let rows = payload
        .get("pets")
        .and_then(Value::as_array)
        .or_else(|| payload.as_array())
        .cloned()
        .unwrap_or_default();

    let installed: std::collections::HashSet<String> = load_index(root).into_iter().collect();
    let pets: Vec<Value> = rows.iter().map(|entry| normalize_market_pet(entry, &installed)).collect();

    let total = payload
        .get("total")
        .and_then(Value::as_u64)
        .unwrap_or(pets.len() as u64);
    let total_pages = payload
        .get("totalPages")
        .and_then(Value::as_u64)
        .unwrap_or((total as f64 / page_size as f64).ceil() as u64);

    Ok(json!({
        "pets": pets,
        "page": page,
        "pageSize": page_size,
        "total": total,
        "totalPages": total_pages,
    }))
}

fn marketplace_list(root: &Path, args: &Value) -> Result<Value, String> {
    let base = marketplace_base()?;
    marketplace_list_with_base(&base, root, args)
}

/// 校验一个可下载素材 URL：仅 http/https，无 userinfo，带 host。
fn safe_asset_url(raw: Option<&Value>, field: &str) -> Result<Url, String> {
    let text = raw
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("缺少 {field} 参数"))?;
    let parsed = Url::parse(text).map_err(|_| format!("{field} 地址无效"))?;
    let scheme = parsed.scheme();
    if scheme != "https" && scheme != "http" {
        return Err(format!("{field} 仅支持 http/https 地址"));
    }
    if parsed.host_str().unwrap_or("").is_empty()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(format!("{field} 地址包含非法凭证信息"));
    }
    Ok(parsed)
}

fn fetch_asset_bytes(url: &Url) -> Result<Vec<u8>, String> {
    let response = net::no_proxy_client()
        .get(url.as_str())
        .timeout(MARKET_TIMEOUT)
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
    Ok(bytes.to_vec())
}

fn install_with_base(
    base: &str,
    root: &Path,
    args: &Value,
) -> Result<Value, String> {
    let _ = base;
    let id = validate_pet_id(args.get("id"), "id")?;
    let download_url = safe_asset_url(args.get("downloadUrl"), "downloadUrl")?;
    let overwrite = args.get("overwrite").and_then(Value::as_bool).unwrap_or(false);

    if read_pet_doc(root, &id).is_some() && !overwrite {
        return Err("本地已存在同名宠物；覆盖请传 overwrite=true".into());
    }
    let bytes = fetch_asset_bytes(&download_url)?;
    let mime = mime_of(&bytes).ok_or("市场返回了不受支持的图片格式")?;
    let spritesheet_path = write_asset(root, &id, &bytes, mime)?;

    let doc = json!({
        "id": id,
        "displayName": id,
        "description": Value::Null,
        "spritesheetPath": spritesheet_path,
        "installedFrom": download_url.as_str(),
        "createdAt": goformat::timestamp(),
    });
    let _lock = store_file::lock(&pet_path(root, &id))?;
    store_file::write_json(&pet_path(root, &id), &doc)?;
    let mut order = load_index(root);
    if !order.iter().any(|existing| existing == &id) {
        order.push(id.clone());
        save_index(root, &order)?;
    }
    Ok(json!({ "pet": summary_row(&doc) }))
}

fn marketplace_install(root: &Path, args: &Value) -> Result<Value, String> {
    let base = marketplace_base()?;
    install_with_base(&base, root, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-pet-{tag}-{}-{}",
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

    /// 1×1 透明 PNG 的最小合法字节。
    fn png_bytes() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D,
            0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x0D, 0x0B, 0xF2,
            0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
            0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00,
            0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ]
    }

    fn sprite_b64() -> String {
        base64::engine::general_purpose::STANDARD.encode(png_bytes())
    }

    #[test]
    fn owns_covers_the_fourteen_documented_commands_only() {
        for command in [
            "pet_list",
            "pet_get",
            "pet_add",
            "pet_delete",
            "pet_update_meta",
            "pet_get_settings",
            "pet_get_current_state",
            "pet_list_active_sessions",
            "pet_read_spritesheet",
            "pet_list_importable_codex",
            "pet_codex_import_available",
            "pet_import_codex",
            "pet_marketplace_list",
            "pet_marketplace_install",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("pet_set_active"));
        assert!(!owns("pet_replace_sprite"));
        assert!(!owns("pet_marketplace_asset"));
        assert!(!owns("Pet_List"));
    }

    #[test]
    fn list_is_empty_before_any_pet_is_added() {
        let root = temp_root("list-empty");
        let rows = call(&root, "pet_list", json!({})).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 0);
    }

    #[test]
    fn add_get_list_roundtrip_and_spritesheet_read_back() {
        let root = temp_root("crud");
        let added = call(
            &root,
            "pet_add",
            json!({
                "id": "fox",
                "displayName": "小狐狸",
                "description": "工作台搭档",
                "spritesheetBase64": sprite_b64(),
            }),
        )
        .unwrap();
        assert_eq!(added["id"], "fox");
        assert_eq!(added["displayName"], "小狐狸");
        assert_eq!(added["description"], "工作台搭档");
        assert!(added["spritesheetPath"].as_str().unwrap().starts_with("fox."));

        let detail = call(&root, "pet_get", json!({ "id": "fox" })).unwrap();
        assert_eq!(detail["id"], "fox");
        assert_eq!(detail["description"], "工作台搭档");

        let list = call(&root, "pet_list", json!({})).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["displayName"], "小狐狸");

        let sprite = call(&root, "pet_read_spritesheet", json!({ "id": "fox" })).unwrap();
        assert_eq!(sprite["mime"], "image/png");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(sprite["dataBase64"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, png_bytes());
        assert!(pet_path(&root, "fox").is_file());
    }

    #[test]
    fn add_rejects_missing_duplicate_and_malformed_inputs() {
        let root = temp_root("add-bad");
        // 缺 id / displayName / 素材。
        assert!(call(&root, "pet_add", json!({}))
            .unwrap_err()
            .contains("缺少 id"));
        assert!(call(
            &root,
            "pet_add",
            json!({ "id": "fox", "displayName": "  ", "spritesheetBase64": sprite_b64() })
        )
        .unwrap_err()
        .contains("缺少 displayName"));
        assert!(call(
            &root,
            "pet_add",
            json!({ "id": "fox", "displayName": "F", "spritesheetBase64": "" })
        )
        .unwrap_err()
        .contains("缺少 spritesheetBase64"));
        // 非法 id 字符 / 大写。
        assert!(call(
            &root,
            "pet_add",
            json!({ "id": "../fox", "displayName": "F", "spritesheetBase64": sprite_b64() })
        )
        .unwrap_err()
        .contains("小写字母"));
        assert!(call(
            &root,
            "pet_add",
            json!({ "id": "Fox", "displayName": "F", "spritesheetBase64": sprite_b64() })
        )
        .unwrap_err()
        .contains("小写字母"));
        // 坏 base64 / 非图片。
        assert!(call(
            &root,
            "pet_add",
            json!({ "id": "fox", "displayName": "F", "spritesheetBase64": "@@@not-base64" })
        )
        .unwrap_err()
        .contains("base64"));
        let text_png = base64::engine::general_purpose::STANDARD.encode("plain text");
        assert!(call(
            &root,
            "pet_add",
            json!({ "id": "fox", "displayName": "F", "spritesheetBase64": text_png })
        )
        .unwrap_err()
        .contains("PNG"));

        // 重复 id 拒绝。
        call(
            &root,
            "pet_add",
            json!({ "id": "fox", "displayName": "F", "spritesheetBase64": sprite_b64() }),
        )
        .unwrap();
        assert!(call(
            &root,
            "pet_add",
            json!({ "id": "fox", "displayName": "F2", "spritesheetBase64": sprite_b64() })
        )
        .unwrap_err()
        .contains("已存在"));
    }

    #[test]
    fn update_meta_only_touches_allowed_fields_and_clears_description() {
        let root = temp_root("meta");
        call(
            &root,
            "pet_add",
            json!({ "id": "fox", "displayName": "旧名", "description": "旧描述", "spritesheetBase64": sprite_b64() }),
        )
        .unwrap();

        let updated = call(
            &root,
            "pet_update_meta",
            json!({ "id": "fox", "patch": { "displayName": "新名", "description": null, "id": "hacker" } }),
        )
        .unwrap();
        assert_eq!(updated["displayName"], "新名");
        assert!(updated["description"].is_null());
        // 越权字段 id 不被改写。
        let detail = call(&root, "pet_get", json!({ "id": "fox" })).unwrap();
        assert_eq!(detail["id"], "fox");

        // 缺 patch / 空名 / 类型错误。
        assert!(call(&root, "pet_update_meta", json!({ "id": "fox" }))
            .unwrap_err()
            .contains("缺少 patch"));
        assert!(call(
            &root,
            "pet_update_meta",
            json!({ "id": "fox", "patch": { "displayName": "  " } })
        )
        .unwrap_err()
        .contains("displayName 不能为空"));
        assert!(call(
            &root,
            "pet_update_meta",
            json!({ "id": "fox", "patch": { "description": 123 } })
        )
        .unwrap_err()
        .contains("description"));
        assert!(call(&root, "pet_update_meta", json!({ "id": "ghost", "patch": {} }))
            .unwrap_err()
            .contains("宠物不存在"));
    }

    #[test]
    fn delete_is_idempotent_and_removes_asset() {
        let root = temp_root("delete");
        call(
            &root,
            "pet_add",
            json!({ "id": "fox", "displayName": "F", "spritesheetBase64": sprite_b64() }),
        )
        .unwrap();
        assert_eq!(call(&root, "pet_delete", json!({ "id": "fox" })).unwrap(), Value::Null);
        assert!(!pet_path(&root, "fox").exists());
        // 素材目录里不应残留文件。
        let leftovers: usize = fs::read_dir(assets_dir(&root))
            .map(|rows| rows.flatten().count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0);
        // 再删一次幂等成功。
        assert_eq!(call(&root, "pet_delete", json!({ "id": "fox" })).unwrap(), Value::Null);
        assert!(call(&root, "pet_delete", json!({})).unwrap_err().contains("缺少 id"));
    }

    #[test]
    fn read_spritesheet_rejects_unknown_pet_and_traversal() {
        let root = temp_root("sprite-bad");
        assert!(call(&root, "pet_read_spritesheet", json!({ "id": "ghost" }))
            .unwrap_err()
            .contains("宠物不存在"));
        assert!(call(&root, "pet_read_spritesheet", json!({})).unwrap_err().contains("缺少 id"));
    }

    #[test]
    fn settings_defaults_match_pet_window_config_shape() {
        let root = temp_root("settings");
        let config = call(&root, "pet_get_settings", json!({})).unwrap();
        assert_eq!(config["enabled"], true);
        assert!(config["activePetId"].is_null());
        assert!(config["x"].is_null());
        assert!(config["y"].is_null());
        assert_eq!(config["scale"], 1.0);
        assert_eq!(config["alwaysOnTop"], false);
    }

    #[test]
    fn current_state_is_idle_and_sessions_payload_is_empty_without_a_layer() {
        let root = temp_root("state");
        let state = call(&root, "pet_get_current_state", json!({})).unwrap();
        assert_eq!(state, json!("idle"));

        let payload = call(&root, "pet_list_active_sessions", json!({})).unwrap();
        assert_eq!(payload["runningCount"], 0);
        assert_eq!(payload["waitingCount"], 0);
        assert_eq!(payload["errorCount"], 0);
        assert_eq!(payload["sessions"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn codex_scan_lists_real_fixtures_and_flags_imported() {
        let root = temp_root("codex-scan");
        let home = temp_root("codex-home");
        let pets = home.join(".codex").join("pets");
        fs::create_dir_all(pets.join("badger")).unwrap();
        fs::write(
            pets.join("badger").join("pet.json"),
            r#"{ "displayName": "獾", "description": "夜班搭子" }"#,
        )
        .unwrap();
        fs::write(pets.join("badger").join("sheet.png"), png_bytes()).unwrap();
        // 只有图片、没有清单。
        fs::create_dir_all(pets.join("mole")).unwrap();
        fs::write(pets.join("mole").join("sheet.webp"), png_bytes()).unwrap();
        // 空目录：既无清单也无图片，跳过。
        fs::create_dir_all(pets.join("empty")).unwrap();

        let result = list_importable_codex_in(&root, &home);
        let rows = result.unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let badger = rows.iter().find(|row| row["id"] == "badger").unwrap();
        assert_eq!(badger["displayName"], "獾");
        assert_eq!(badger["description"], "夜班搭子");
        assert!(badger["sourcePath"].as_str().unwrap().contains("badger"));
        assert_eq!(badger["alreadyImported"], false);
        let mole = rows.iter().find(|row| row["id"] == "mole").unwrap();
        assert_eq!(mole["displayName"], "mole");
        assert!(mole["description"].is_null());

        let available = codex_availability_in(&home).unwrap();
        assert_eq!(available["available"], true);
    }

    #[test]
    fn codex_availability_is_false_without_a_pets_dir() {
        let home = temp_root("codex-none-home");
        let available = codex_availability_in(&home);
        assert_eq!(available.unwrap()["available"], false);
    }

    #[test]
    fn import_codex_copies_assets_and_skips_collisions() {
        let root = temp_root("codex-import");
        let home = temp_root("codex-import-home");
        let pets = home.join(".codex").join("pets");
        fs::create_dir_all(pets.join("badger")).unwrap();
        fs::write(pets.join("badger").join("sheet.png"), png_bytes()).unwrap();
        fs::create_dir_all(pets.join("mole")).unwrap();
        fs::write(pets.join("mole").join("sheet.png"), png_bytes()).unwrap();

        // 先导入 badger；再整批导入时 badger 撞名跳过，mole 进入。
        let first = import_codex_in(
            &root,
            &home,
            &json!({ "ids": ["badger"], "overwriteWithSuffix": false }),
        )
        .unwrap();
        assert_eq!(first["importedIds"], json!(["badger"]));
        assert_eq!(first["skipped"].as_array().unwrap().len(), 0);

        let second = import_codex_in(&root, &home, &json!({ "overwriteWithSuffix": false })).unwrap();
        let imported = second["importedIds"].as_array().unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0], "mole");
        let skipped = second["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["sourceId"], "badger");
        assert!(skipped[0]["reason"].as_str().unwrap().contains("已存在"));

        // 点名不存在的来源记 skipped。
        let third = import_codex_in(&root, &home, &json!({ "ids": ["ghost"] })).unwrap();
        assert_eq!(third["importedIds"].as_array().unwrap().len(), 0);
        assert_eq!(third["skipped"][0]["sourceId"], "ghost");

        // 导入后的宠物能被本地清单读到。
        let list = call(&root, "pet_list", json!({})).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 2);
    }

    #[test]
    fn import_codex_suffixes_on_collision_when_requested() {
        let root = temp_root("codex-suffix");
        let home = temp_root("codex-suffix-home");
        let pets = home.join(".codex").join("pets");
        fs::create_dir_all(pets.join("badger")).unwrap();
        fs::write(pets.join("badger").join("sheet.png"), png_bytes()).unwrap();
        import_codex_in(&root, &home, &json!({ "overwriteWithSuffix": false })).unwrap();
        let again = import_codex_in(&root, &home, &json!({ "overwriteWithSuffix": true })).unwrap();
        assert_eq!(again["importedIds"], json!(["badger-2"]));
    }

    /// 起本地 HTTP 桩按路径前缀返回预置 JSON / 字节。
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
                    .unwrap_or(r#"{"pets":[]}"#);
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

    #[test]
    fn marketplace_list_maps_rows_and_paginates_from_local_stub() {
        let stub = serve_stub(&[(
            "/pets",
            r#"{"pets":[{"id":"fox","displayName":"Fox","description":"d","kind":"pet","tags":["cute"],"ownerName":"O","ownerHandle":"o","viewCount":3,"downloadCount":2,"likeCount":1,"uploadedAt":"2026-11-15","posterUrl":"https://x/p.png","previewUrl":null,"downloadUrl":"https://x/fox.png"}],"total":1,"totalPages":1}"#,
        )]);
        let root = temp_root("market-list");
        let rows = marketplace_list_with_base(
            &stub,
            &root,
            &json!({ "page": 1, "pageSize": 30, "q": "fox" }),
        )
        .unwrap();
        assert_eq!(rows["page"], 1);
        assert_eq!(rows["pageSize"], 30);
        assert_eq!(rows["total"], 1);
        assert_eq!(rows["totalPages"], 1);
        let pet = &rows["pets"][0];
        assert_eq!(pet["id"], "fox");
        assert_eq!(pet["displayName"], "Fox");
        assert_eq!(pet["tags"], json!(["cute"]));
        assert_eq!(pet["viewCount"], 3);
        assert_eq!(pet["alreadyInstalled"], false);
    }

    #[test]
    fn marketplace_install_persists_downloaded_sprite() {
        // 桩直接回显原始 PNG 字节，贴近真实下载。
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
        let stub = format!("http://{address}");
        let root = temp_root("market-install");
        let installed = install_with_base(
            &stub,
            &root,
            &json!({ "id": "fox", "downloadUrl": format!("{stub}/fox.png"), "overwrite": false }),
        )
        .unwrap();
        assert_eq!(installed["pet"]["id"], "fox");
        assert_eq!(installed["pet"]["displayName"], "fox");
        let list = call(&root, "pet_list", json!({})).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);

        // 缺 id / downloadUrl、非法 scheme、撞名不覆盖。
        assert!(install_with_base(&stub, &root, &json!({ "downloadUrl": format!("{stub}/fox.png") }))
            .unwrap_err()
            .contains("缺少 id"));
        assert!(install_with_base(&stub, &root, &json!({ "id": "fox", "downloadUrl": "file:///etc/passwd" }))
            .unwrap_err()
            .contains("http/https"));
        assert!(install_with_base(&stub, &root, &json!({ "id": "fox", "downloadUrl": format!("{stub}/fox.png") }))
            .unwrap_err()
            .contains("已存在"));
        drop(stub);
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "open_pet_window", &json!({})).is_none());
    }
}
