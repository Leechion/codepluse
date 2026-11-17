//! Codex 目录与设备授权 —— 补全批次（S4，9 条）。
//!
//! 前端 `codeg-ui/src/lib/api.ts` 已经在调用这 9 条命令，但原 Go sidecar
//! （提交 76712f5）与 Rust 都没有实现过它们（全树搜不到字面量）；契约只
//! 取自调用点参数名与 `codeg-ui/src/lib/types.ts` 的返回结构，没有可移植
//! 的 Go 原型。
//!
//! 命令清单：
//! * `codex_bundled_catalog` —— 内置/快照模型列表（`CodexModelInfo[]`）。
//! * `codex_catalog_plan_preview` —— 草稿保存前的目录接管预览，不联网不写盘。
//! * `codex_endpoint_catalog_cached` —— 按请求查本地缓存，绝不联网。
//! * `codex_endpoint_catalog_published` —— 查已发布快照。
//! * `codex_endpoint_catalog_status` —— 已发布目录与刷新状态总览。
//! * `codex_fetch_endpoint_catalog` —— 短超时 HTTP 拉取提供方模型列表并写缓存。
//! * `codex_refresh_endpoint_catalog` —— 按已发布来源立即刷新并重发布。
//! * `codex_request_device_code` —— 生成待授权设备码并落 `.data/`。
//! * `codex_poll_device_code` —— 设备码状态机轮询。
//!
//! 两面：
//! 1. **端点目录缓存**：`fetch`/`refresh` 走短超时 HTTP（`net::client`），
//!    成功后把视图写进 `.data/`；`cached`/`published`/`status` 只读本地落盘
//!    结果，绝不联网。
//! 2. **设备码授权流**：`request` 生成待授权记录落 `.data/`；`poll` 按状态机
//!    回 `pending`/`success`/`error`。
//!
//! 限制（L3，能力在本产品中不存在）：
//! * 仓库 `fixtures/`、`codeg-assets/` 下没有随附的 Codex 模型快照，bundled
//!   目录在没有本地快照时返回空数组，不编造官方模型。
//! * 本产品没有接入真实 Codex/OAuth 上游：设备码 `success` 里的令牌只从已落盘
//!   记录透传，模块本身绝不生成 id/access/refresh token。
//! * 请求里的 `configToml`（`query_params`/`http_headers`）暂未注入目录请求。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::Url;

use crate::backend::goformat;
use crate::backend::net;
use crate::backend::store_file;

/// 目录请求的 HTTP 超时（秒）。
const FETCH_TIMEOUT_SECS: u64 = 10;
/// 设备码有效期（毫秒）：10 分钟。
const DEVICE_TTL_MS: i64 = 600_000;
/// 轮询建议间隔（秒），随设备码一起返回给前端。
const POLL_INTERVAL: i64 = 5;
/// 缓存条目上限：超出后丢弃最旧的一条。
const CACHE_LIMIT: usize = 50;

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "codex_bundled_catalog"
            | "codex_catalog_plan_preview"
            | "codex_endpoint_catalog_cached"
            | "codex_endpoint_catalog_published"
            | "codex_endpoint_catalog_status"
            | "codex_fetch_endpoint_catalog"
            | "codex_poll_device_code"
            | "codex_refresh_endpoint_catalog"
            | "codex_request_device_code"
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
        "codex_bundled_catalog" => bundled(root, args),
        "codex_catalog_plan_preview" => plan_preview(args),
        "codex_endpoint_catalog_cached" => lookup_cached(root, args),
        "codex_endpoint_catalog_published" => lookup_published(root, args),
        "codex_endpoint_catalog_status" => status(root),
        "codex_fetch_endpoint_catalog" => fetch(root, args),
        "codex_poll_device_code" => poll_device(root, args),
        "codex_refresh_endpoint_catalog" => refresh(root),
        "codex_request_device_code" => request_device(root),
        _ => Err(format!("未知的 Codex 命令：{command}")),
    }
}

// ── 路径 ────────────────────────────────────────────────────────────────

fn data_dir(root: &Path) -> PathBuf {
    root.join(".data")
}

fn cache_path(root: &Path) -> PathBuf {
    data_dir(root).join("codex-catalog-cache.json")
}

fn published_path(root: &Path) -> PathBuf {
    data_dir(root).join("codex-catalog-published.json")
}

fn refresh_path(root: &Path) -> PathBuf {
    data_dir(root).join("codex-catalog-refresh.json")
}

fn device_path(root: &Path) -> PathBuf {
    data_dir(root).join("codex-device-code.json")
}

// ── 时间 ────────────────────────────────────────────────────────────────

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

// ── 目录请求解析 ────────────────────────────────────────────────────────

/// 前端 `CodexCatalogRequest` 的本模块视图；密钥只用于摘要，不落缓存原文。
struct CatalogRequest {
    base_url: String,
    catalog_url: Option<String>,
    key_kind: String,
    provider_id: Option<i64>,
    api_key: Option<String>,
    #[allow(dead_code)]
    config_toml: Option<String>,
    format: Option<String>,
}

fn parse_request(args: &Value) -> Result<CatalogRequest, String> {
    let params = args
        .get("params")
        .ok_or_else(|| "缺少 params 参数".to_string())?;
    if !params.is_object() {
        return Err("params 必须是对象".to_string());
    }
    let base_url = params
        .get("baseUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 baseUrl 参数".to_string())?
        .to_string();
    let source = params
        .get("keySource")
        .ok_or_else(|| "缺少 keySource 参数".to_string())?;
    if !source.is_object() {
        return Err("keySource 必须是对象".to_string());
    }
    let key_kind = source
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| matches!(*kind, "inline" | "provider" | "codexAuth"))
        .ok_or_else(|| "keySource.kind 无效".to_string())?
        .to_string();
    let provider_id = source.get("providerId").and_then(Value::as_i64);
    let api_key = source
        .get("apiKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let catalog_url = params
        .get("catalogUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let config_toml = params.get("configToml").and_then(Value::as_str).map(str::to_string);
    let format = match params.get("format").and_then(Value::as_str) {
        Some(format) if format == "native" || format == "openai_list" => Some(format.to_string()),
        Some(_) => return Err("format 必须是 native 或 openai_list".to_string()),
        None => None,
    };
    Ok(CatalogRequest {
        base_url,
        catalog_url,
        key_kind,
        provider_id,
        api_key,
        config_toml,
        format,
    })
}

/// 缓存/发布快照的请求指纹：同一组参数命中同一条记录，不含任何密钥原文。
fn fingerprint(req: &CatalogRequest) -> String {
    format!(
        "{}|{}|{}|{}",
        req.base_url,
        req.catalog_url.as_deref().unwrap_or(""),
        req.key_kind,
        req.provider_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
    )
}

fn request_summary(req: &CatalogRequest) -> Value {
    json!({
        "baseUrl": req.base_url,
        "catalogUrl": req.catalog_url.clone().map(Value::from).unwrap_or(Value::Null),
        "format": req.format.clone().map(Value::from).unwrap_or(Value::Null),
        "keyKind": req.key_kind,
        "providerId": req.provider_id.map(Value::from).unwrap_or(Value::Null),
    })
}

/// 密钥摘要：只留 sha256 前 16 位十六进制，缓存里绝不存原始 key。
fn key_digest(req: &CatalogRequest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(req.key_kind.as_bytes());
    if let Some(provider_id) = req.provider_id {
        hasher.update(provider_id.to_string().as_bytes());
    }
    if let Some(api_key) = req.api_key.as_deref() {
        hasher.update(api_key.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn resolve_list_url(req: &CatalogRequest) -> Result<String, String> {
    if let Some(catalog_url) = req.catalog_url.as_deref().filter(|value| !value.is_empty()) {
        return Ok(catalog_url.to_string());
    }
    let base = req.base_url.trim_end_matches('/');
    if base.is_empty() {
        return Err("baseUrl 无效".to_string());
    }
    Ok(format!("{base}/models"))
}

// ── 目录视图构造 ─────────────────────────────────────────────────────────

fn build_view(
    req: &CatalogRequest,
    list_url: &str,
    bytes: &[u8],
    elapsed_ms: u64,
    started_at: &str,
) -> Result<Value, String> {
    let body: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("目录响应 JSON 无效：{error}"))?;

    let inferred = if body.get("models").and_then(Value::as_array).is_some() {
        Some("native")
    } else if body.get("data").and_then(Value::as_array).is_some() {
        Some("openai_list")
    } else {
        None
    };

    let mut warnings: Vec<String> = Vec::new();
    let format = match req.format.clone() {
        Some(explicit) => {
            if inferred != Some(explicit.as_str()) {
                warnings.push(format!("响应形状与请求的 {explicit} 格式不一致"));
            }
            explicit
        }
        None => inferred
            .ok_or_else(|| "目录响应既不是 native models 也不是 openai data 列表".to_string())?
            .to_string(),
    };

    let mut models: Vec<Value> = Vec::new();
    let mut ids: Vec<Value> = Vec::new();
    if format == "openai_list" {
        match body.get("data").and_then(Value::as_array) {
            Some(rows) => {
                for row in rows {
                    let id = row
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty());
                    let Some(id) = id else { continue };
                    ids.push(json!({
                        "id": id,
                        "contextWindow": Value::Null,
                        "template": Value::Null,
                    }));
                }
            }
            None => warnings.push("openai_list 响应缺少 data 数组".to_string()),
        }
    } else {
        match body.get("models").and_then(Value::as_array) {
            Some(rows) => {
                for row in rows {
                    let slug = row
                        .get("slug")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty());
                    let Some(slug) = slug else { continue };
                    let mut entry = serde_json::Map::new();
                    entry.insert("slug".into(), Value::String(slug.to_string()));
                    for key in [
                        "display_name",
                        "description",
                        "context_window",
                        "max_context_window",
                        "visibility",
                    ] {
                        if let Some(value) = row.get(key) {
                            entry.insert((*key).into(), value.clone());
                        }
                    }
                    models.push(Value::Object(entry));
                }
            }
            None => warnings.push("native 响应缺少 models 数组".to_string()),
        }
    }

    let total = models.len().max(ids.len());
    let host = match Url::parse(list_url) {
        Ok(url) => url.host_str().unwrap_or("").to_string(),
        Err(_) => String::new(),
    };
    let counts = json!({"total": total, "listed": total, "usable": total});
    let (models_value, ids_value) = if format == "openai_list" {
        (Value::Null, Value::Array(ids))
    } else {
        (Value::Array(models), Value::Null)
    };

    Ok(json!({
        "url": list_url,
        "host": host,
        "format": format,
        "models": models_value,
        "ids": ids_value,
        "missing": Value::Array(Vec::new()),
        "counts": counts,
        "bytes": bytes.len(),
        "elapsedMs": elapsed_ms,
        "clientVersion": "",
        "clientVersionConfirmed": false,
        "requestSeq": 1,
        "requestStartedAt": started_at,
        "fetchedAt": now_rfc3339(),
        "diff": json!({"added": Value::Array(Vec::new()), "removed": Value::Array(Vec::new())}),
        "keyDigest": key_digest(req),
        "warnings": Value::Array(warnings.into_iter().map(Value::String).collect()),
    }))
}

// ── fetch：拉取并写缓存 ──────────────────────────────────────────────────

fn fetch(root: &Path, args: &Value) -> Result<Value, String> {
    let req = parse_request(args)?;
    let list_url = resolve_list_url(&req)?;
    let started = Instant::now();
    let started_iso = now_rfc3339();

    let mut builder = net::client()
        .get(&list_url)
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS));
    if let Some(api_key) = req.api_key.as_deref().filter(|value| !value.is_empty()) {
        builder = builder.header("Authorization", format!("Bearer {api_key}"));
    }

    let response = builder
        .send()
        .map_err(|error| format!("读取提供方模型目录失败：{error}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("读取提供方模型目录失败：HTTP {status}"));
    }
    let bytes = response
        .bytes()
        .map_err(|error| format!("读取目录响应失败：{error}"))?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let view = build_view(&req, &list_url, &bytes, elapsed_ms, &started_iso)?;

    cache_entry(root, &req, &view)?;
    Ok(view)
}

fn cache_entry(root: &Path, req: &CatalogRequest, view: &Value) -> Result<(), String> {
    let path = cache_path(root);
    let mut entries: Vec<Value> = store_file::read_json(&path)?
        .and_then(|doc| doc.get("entries").cloned())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let key = fingerprint(req);
    entries.retain(|entry| entry.get("key").and_then(Value::as_str) != Some(key.as_str()));
    entries.push(json!({"key": key, "request": request_summary(req), "view": view}));
    if entries.len() > CACHE_LIMIT {
        entries.drain(0..entries.len() - CACHE_LIMIT);
    }
    store_file::write_json(&path, &json!({"entries": entries}))
}

// ── cached / published：只读本地快照 ──────────────────────────────────────

fn empty_lookup() -> Value {
    json!({"catalog": Value::Null, "mismatch": "none"})
}

fn mismatch_for(req: &CatalogRequest, view: &Value) -> Value {
    match view.get("format").and_then(Value::as_str) {
        // 请求显式指定了格式且与缓存不同：才算格式不匹配；未指定时按通配处理。
        Some(format) if req.format.as_deref().is_some() && req.format.as_deref() != Some(format) => {
            Value::from("format")
        }
        _ => Value::Null,
    }
}

fn lookup_cached(root: &Path, args: &Value) -> Result<Value, String> {
    let req = parse_request(args)?;
    let Some(doc) = store_file::read_json(&cache_path(root))? else {
        return Ok(empty_lookup());
    };
    let Some(entries) = doc.get("entries").and_then(Value::as_array) else {
        return Ok(empty_lookup());
    };
    let key = fingerprint(&req);
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.get("key").and_then(Value::as_str) == Some(key.as_str()))
    else {
        return Ok(json!({"catalog": Value::Null, "mismatch": "request"}));
    };
    let view = entry.get("view").cloned().unwrap_or(Value::Null);
    Ok(json!({"catalog": view, "mismatch": mismatch_for(&req, &view)}))
}

fn lookup_published(root: &Path, args: &Value) -> Result<Value, String> {
    let req = parse_request(args)?;
    let Some(doc) = store_file::read_json(&published_path(root))? else {
        return Ok(empty_lookup());
    };
    let Some(published_key) = doc.get("requestKey").and_then(Value::as_str) else {
        return Ok(empty_lookup());
    };
    if published_key != fingerprint(&req) {
        return Ok(json!({"catalog": Value::Null, "mismatch": "request"}));
    }
    let view = doc.get("view").cloned().unwrap_or(Value::Null);
    Ok(json!({"catalog": view, "mismatch": mismatch_for(&req, &view)}))
}

// ── status / refresh ────────────────────────────────────────────────────

fn refresh_defaults() -> Value {
    json!({
        "inProgress": false,
        "lastAttemptAt": Value::Null,
        "lastError": Value::Null,
        "lastErrorAt": Value::Null,
        "consecutiveFailures": 0,
        "nextRefreshAt": Value::Null,
        "paused": Value::Null,
        "scheduled": false,
    })
}

fn read_refresh(root: &Path) -> Value {
    let mut out = refresh_defaults();
    if let Some(doc) = store_file::read_json(&refresh_path(root))
        .ok()
        .flatten()
    {
        if let Some(object) = doc.as_object() {
            for (key, value) in object {
                out[key] = value.clone();
            }
        }
    }
    out
}

fn write_refresh(root: &Path, refresh: &Value) -> Result<(), String> {
    store_file::write_json(&refresh_path(root), refresh)
}

fn status(root: &Path) -> Result<Value, String> {
    let published = store_file::read_json(&published_path(root))?;
    let meta = published.as_ref().and_then(|doc| doc.get("meta")).cloned();
    let source = published.as_ref().and_then(|doc| doc.get("source")).cloned();
    let refresh = read_refresh(root);
    let fetch_error = refresh.get("lastError").cloned().unwrap_or(Value::Null);

    Ok(json!({
        "meta": meta,
        "source": source,
        "publishesCatalog": meta.is_some(),
        "refresh": refresh,
        "refreshHours": Value::Null,
        "pendingLocalChanges": false,
        "catalogUrlHint": false,
        "foreignProvider": Value::Null,
        "missingSelected": Value::Array(Vec::new()),
        "newIds": Value::Array(Vec::new()),
        "runtime": {
            "version": "",
            "confirmed": false,
            "adapterVersion": Value::Null,
        },
        "fetchError": fetch_error,
    }))
}

fn build_meta(req: &CatalogRequest, view: &Value) -> Value {
    json!({
        "version": 1,
        "intentHash": fingerprint(req),
        "binding": "codex",
        "validatedBy": Value::Array(Vec::new()),
        "counts": view.get("counts").cloned().unwrap_or(json!({"total": 0, "listed": 0, "usable": 0})),
        "url": view.get("url").cloned().unwrap_or(Value::Null),
        "format": view.get("format").cloned().unwrap_or(Value::Null),
        "clientVersion": view.get("clientVersion").cloned().unwrap_or(Value::Null),
        "clientVersionConfirmed": false,
        "stale": false,
        "diff": view.get("diff").cloned().unwrap_or(json!({"added": [], "removed": []})),
    })
}

fn refresh(root: &Path) -> Result<Value, String> {
    let published = store_file::read_json(&published_path(root))?;
    let source = published.as_ref().and_then(|doc| doc.get("source")).cloned();

    let Some(source) = source else {
        let error = json!({"code": "no_source", "message": "尚未配置提供方目录来源，无法刷新"});
        let failures = read_refresh(root)
            .get("consecutiveFailures")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        write_refresh(
            root,
            &json!({
                "inProgress": false,
                "lastAttemptAt": now_rfc3339(),
                "lastError": error,
                "lastErrorAt": now_rfc3339(),
                "consecutiveFailures": failures,
                "nextRefreshAt": Value::Null,
                "paused": Value::Null,
                "scheduled": false,
            }),
        )?;
        return status(root);
    };

    let Some(url) = source
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        let error = json!({"code": "no_url", "message": "已发布来源缺少目录 URL"});
        let failures = read_refresh(root)
            .get("consecutiveFailures")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        write_refresh(
            root,
            &json!({
                "inProgress": false,
                "lastAttemptAt": now_rfc3339(),
                "lastError": error,
                "lastErrorAt": now_rfc3339(),
                "consecutiveFailures": failures,
                "nextRefreshAt": Value::Null,
                "paused": Value::Null,
                "scheduled": false,
            }),
        )?;
        return status(root);
    };

    let format = source
        .get("format")
        .and_then(Value::as_str)
        .filter(|value| *value == "native" || *value == "openai_list")
        .map(str::to_string);
    let req = CatalogRequest {
        base_url: url.to_string(),
        catalog_url: None,
        key_kind: "codexAuth".to_string(),
        provider_id: None,
        api_key: None,
        config_toml: None,
        format,
    };

    let started = Instant::now();
    let started_iso = now_rfc3339();
    let attempt = net::client()
        .get(url)
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .send();

    match attempt {
        Ok(response) if response.status().as_u16() == 200 => {
            let bytes = response
                .bytes()
                .map_err(|error| format!("读取目录响应失败：{error}"))?;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let view = build_view(&req, url, &bytes, elapsed_ms, &started_iso)?;
            let meta = build_meta(&req, &view);
            store_file::write_json(
                &published_path(root),
                &json!({
                    "requestKey": fingerprint(&req),
                    "view": view,
                    "meta": meta,
                    "source": source,
                    "checkedAt": now_rfc3339(),
                }),
            )?;
            write_refresh(
                root,
                &json!({
                    "inProgress": false,
                    "lastAttemptAt": now_rfc3339(),
                    "lastError": Value::Null,
                    "lastErrorAt": Value::Null,
                    "consecutiveFailures": 0,
                    "nextRefreshAt": Value::Null,
                    "paused": Value::Null,
                    "scheduled": false,
                }),
            )?;
        }
        Ok(response) => {
            let error = json!({
                "code": "fetch_failed",
                "message": format!("读取提供方模型目录失败：HTTP {}", response.status().as_u16()),
            });
            let failures = read_refresh(root)
                .get("consecutiveFailures")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            write_refresh(
                root,
                &json!({
                    "inProgress": false,
                    "lastAttemptAt": now_rfc3339(),
                    "lastError": error,
                    "lastErrorAt": now_rfc3339(),
                    "consecutiveFailures": failures,
                    "nextRefreshAt": Value::Null,
                    "paused": Value::Null,
                    "scheduled": false,
                }),
            )?;
        }
        Err(error) => {
            let error = json!({"code": "fetch_failed", "message": format!("读取提供方模型目录失败：{error}")});
            let failures = read_refresh(root)
                .get("consecutiveFailures")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            write_refresh(
                root,
                &json!({
                    "inProgress": false,
                    "lastAttemptAt": now_rfc3339(),
                    "lastError": error,
                    "lastErrorAt": now_rfc3339(),
                    "consecutiveFailures": failures,
                    "nextRefreshAt": Value::Null,
                    "paused": Value::Null,
                    "scheduled": false,
                }),
            )?;
        }
    }

    status(root)
}

// ── bundled：内置/快照模型 ───────────────────────────────────────────────

fn read_model_array(path: &Path) -> Option<Vec<Value>> {
    let doc = store_file::read_json(path).ok()??;
    let rows = doc.as_array()?;
    let mut out = Vec::new();
    for row in rows {
        let slug = row
            .get("slug")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let mut entry = serde_json::Map::new();
        entry.insert("slug".into(), Value::String(slug.to_string()));
        for key in [
            "display_name",
            "description",
            "context_window",
            "max_context_window",
            "visibility",
        ] {
            if let Some(value) = row.get(key) {
                entry.insert((*key).into(), value.clone());
            }
        }
        out.push(Value::Object(entry));
    }
    Some(out)
}

fn bundled(root: &Path, args: &Value) -> Result<Value, String> {
    // `forceRefresh` 接受但暂不生效：本产品没有可重跑的 codex CLI 上游（L3）。
    let _ = args.get("forceRefresh");
    let asset = root.join("codeg-assets").join("codex-models.json");
    let cache = data_dir(root).join("codex-bundled-models.json");
    let rows = read_model_array(&asset).or_else(|| read_model_array(&cache)).unwrap_or_default();
    Ok(Value::Array(rows))
}

// ── plan preview：只读草稿，不联网不写盘 ─────────────────────────────────

fn parse_toml_string(toml: &str, key: &str) -> Option<String> {
    for line in toml.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix(key) else { continue };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let value = rest.trim();
        let Some(quoted) = value.strip_prefix('"').and_then(|remainder| remainder.split('"').next())
        else {
            continue;
        };
        if !quoted.is_empty() {
            return Some(quoted.to_string());
        }
    }
    None
}

fn plan_preview(args: &Value) -> Result<Value, String> {
    let params = args
        .get("params")
        .ok_or_else(|| "缺少 params 参数".to_string())?;
    if !params.is_object() {
        return Err("params 必须是对象".to_string());
    }
    let toml_raw = params
        .get("configToml")
        .and_then(Value::as_str)
        .ok_or_else(|| "缺少 configToml 参数".to_string())?;
    if toml_raw.trim().is_empty() {
        return Err("缺少 configToml 参数".to_string());
    }
    let manages_catalog = toml_raw
        .lines()
        .any(|line| line.trim_start().starts_with("model_catalog_json"));
    let default_model = parse_toml_string(toml_raw, "model");

    Ok(json!({
        "finalToml": toml_raw,
        "managesCatalog": manages_catalog,
        "defaultModel": default_model.map(Value::String).unwrap_or(Value::Null),
        "counts": Value::Null,
        "warnings": Value::Array(Vec::new()),
    }))
}

// ── 设备码授权流 ─────────────────────────────────────────────────────────

fn gen_user_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut code = String::new();
    for index in 0..8 {
        if index == 4 {
            code.push('-');
        }
        let pick = (rand::random::<u32>() % ALPHABET.len() as u32) as usize;
        code.push(ALPHABET[pick] as char);
    }
    code
}

fn request_device(root: &Path) -> Result<Value, String> {
    let device_auth_id = goformat::hex_id();
    let user_code = gen_user_code();
    let verification_url = std::env::var("CODEPLUSE_CODEX_VERIFICATION_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "https://chatgpt.com/device".to_string());
    let now = now_ms();

    let record = json!({
        "deviceAuthId": device_auth_id,
        "userCode": user_code,
        "verificationUrl": verification_url,
        "interval": POLL_INTERVAL,
        "status": "pending",
        "createdAtMs": now,
        "expiresAtMs": now + DEVICE_TTL_MS,
    });
    store_file::write_json(&device_path(root), &record)?;

    Ok(json!({
        "userCode": user_code,
        "verificationUrl": verification_url,
        "deviceAuthId": device_auth_id,
        "interval": POLL_INTERVAL,
    }))
}

fn device_error(message: &str) -> Value {
    json!({"status": "error", "message": message})
}

fn poll_device(root: &Path, args: &Value) -> Result<Value, String> {
    let device_auth_id = args
        .get("deviceAuthId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 deviceAuthId 参数".to_string())?;
    let user_code = args
        .get("userCode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 userCode 参数".to_string())?;

    let Some(record) = store_file::read_json(&device_path(root))? else {
        return Ok(device_error("设备授权码不存在或已过期"));
    };
    let rec_id = record.get("deviceAuthId").and_then(Value::as_str).unwrap_or("");
    if rec_id != device_auth_id {
        return Ok(device_error("设备授权码不存在或已过期"));
    }
    if record.get("userCode").and_then(Value::as_str).unwrap_or("") != user_code {
        return Ok(device_error("授权码与设备号不匹配"));
    }
    let expires_at = record.get("expiresAtMs").and_then(Value::as_i64).unwrap_or(0);
    if expires_at > 0 && now_ms() > expires_at {
        return Ok(device_error("设备授权码已过期"));
    }

    match record.get("status").and_then(Value::as_str).unwrap_or("pending") {
        "pending" => Ok(json!({"status": "pending"})),
        "success" => Ok(json!({
            "status": "success",
            "idToken": record.get("idToken").cloned().unwrap_or(Value::Null),
            "accessToken": record.get("accessToken").cloned().unwrap_or(Value::Null),
            "refreshToken": record.get("refreshToken").cloned().unwrap_or(Value::Null),
            "accountId": record.get("accountId").cloned().unwrap_or(Value::Null),
        })),
        "denied" => Ok(device_error("用户拒绝了设备授权")),
        other => Ok(device_error(&format!("未知授权状态：{other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-codex-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    /// 本地 HTTP 桩：忽略路径，回固定 JSON。测试不触外网。
    fn serve(payload: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}")
    }

    fn native_params(base_url: &str) -> Value {
        json!({
            "params": {
                "baseUrl": base_url,
                "keySource": {"kind": "codexAuth"}
            }
        })
    }

    #[test]
    fn owns_covers_the_nine_documented_commands_only() {
        for command in [
            "codex_bundled_catalog",
            "codex_catalog_plan_preview",
            "codex_endpoint_catalog_cached",
            "codex_endpoint_catalog_published",
            "codex_endpoint_catalog_status",
            "codex_fetch_endpoint_catalog",
            "codex_poll_device_code",
            "codex_refresh_endpoint_catalog",
            "codex_request_device_code",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("codex_bundled_catalogs"));
        assert!(!owns("fetch_codex_endpoint_catalog"));
        assert!(!owns("Codex_Poll_Device_Code"));
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }

    #[test]
    fn bundled_catalog_is_empty_without_a_shipped_snapshot() {
        let root = temp_root("bundled-empty");
        let rows = call(&root, "codex_bundled_catalog", json!({"forceRefresh": null})).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 0);
    }

    #[test]
    fn bundled_catalog_reads_a_local_snapshot_when_present() {
        let root = temp_root("bundled-snapshot");
        let asset_dir = root.join("codeg-assets");
        fs::create_dir_all(&asset_dir).unwrap();
        fs::write(
            asset_dir.join("codex-models.json"),
            json!([
                {"slug": "gpt-5", "display_name": "GPT-5", "context_window": 160000},
                {"slug": "gpt-5-mini", "visibility": "list"}
            ])
            .to_string(),
        )
        .unwrap();
        let rows = call(&root, "codex_bundled_catalog", json!({"forceRefresh": true})).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["slug"], "gpt-5");
        assert_eq!(rows[0]["display_name"], "GPT-5");
        assert_eq!(rows[1]["visibility"], "list");
    }

    #[test]
    fn plan_preview_echoes_toml_and_detects_managed_keys() {
        let root = temp_root("plan");
        let result = call(
            &root,
            "codex_catalog_plan_preview",
            json!({"params": {"configToml": "model = \"gpt-5\"\nmodel_catalog_json = \"{}\"\n"}}),
        )
        .unwrap();
        assert_eq!(result["finalToml"], "model = \"gpt-5\"\nmodel_catalog_json = \"{}\"\n");
        assert_eq!(result["managesCatalog"], true);
        assert_eq!(result["defaultModel"], "gpt-5");
        assert!(result["counts"].is_null());
        assert_eq!(result["warnings"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn plan_preview_without_catalog_key_reports_no_management() {
        let root = temp_root("plan-plain");
        let result = call(
            &root,
            "codex_catalog_plan_preview",
            json!({"params": {"configToml": "model = \"o3\"\n"}}),
        )
        .unwrap();
        assert_eq!(result["managesCatalog"], false);
        assert_eq!(result["defaultModel"], "o3");
    }

    #[test]
    fn plan_preview_rejects_missing_config_toml() {
        let root = temp_root("plan-bad");
        assert!(call(&root, "codex_catalog_plan_preview", json!({}))
            .unwrap_err()
            .contains("缺少 params"));
        assert!(call(
            &root,
            "codex_catalog_plan_preview",
            json!({"params": {"configToml": "  "}})
        )
        .unwrap_err()
        .contains("缺少 configToml"));
    }

    #[test]
    fn fetch_parses_native_list_and_writes_the_cache() {
        let payload = r#"{"models":[
            {"slug":"gpt-5","display_name":"GPT-5","context_window":160000},
            {"slug":"gpt-5-mini","visibility":"list"}
        ]}"#;
        let base = serve(payload);
        let root = temp_root("fetch-native");
        let view = call(&root, "codex_fetch_endpoint_catalog", native_params(&base)).unwrap();

        assert!(view["url"].as_str().unwrap().ends_with("/models"));
        assert_eq!(view["host"], "127.0.0.1");
        assert_eq!(view["format"], "native");
        let models = view["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert!(view["ids"].is_null());
        assert_eq!(view["counts"]["total"], 2);
        assert_eq!(view["counts"]["listed"], 2);
        assert_eq!(view["counts"]["usable"], 2);
        assert!(view["bytes"].as_u64().unwrap() > 0);
        assert!(view["elapsedMs"].as_u64().is_some());
        assert!(view["fetchedAt"].as_str().unwrap().contains('T'));
        assert!(view["keyDigest"].as_str().unwrap().len() == 16);
        assert_eq!(view["warnings"].as_array().unwrap().len(), 0);

        // 缓存落盘后，cached 命令按同一请求命中同一条视图。
        let cached = call(&root, "codex_endpoint_catalog_cached", native_params(&base)).unwrap();
        assert!(cached["catalog"].is_object());
        assert_eq!(cached["mismatch"], Value::Null);
        assert_eq!(cached["catalog"]["models"][0]["slug"], "gpt-5");
    }

    #[test]
    fn fetch_parses_openai_list_shape_into_ids() {
        let payload = r#"{"data":[{"id":"gpt-4o"},{"id":"o3"}]}"#;
        let base = serve(payload);
        let root = temp_root("fetch-openai");
        let mut params = native_params(&base);
        params["params"]["format"] = json!("openai_list");
        let view = call(&root, "codex_fetch_endpoint_catalog", params).unwrap();
        assert_eq!(view["format"], "openai_list");
        assert!(view["models"].is_null());
        let ids = view["ids"].as_array().unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0]["id"], "gpt-4o");
        assert!(ids[0]["contextWindow"].is_null());
        assert!(ids[0]["template"].is_null());
        assert_eq!(view["counts"]["total"], 2);
    }

    #[test]
    fn fetch_rejects_malformed_requests() {
        let root = temp_root("fetch-bad");
        assert!(call(&root, "codex_fetch_endpoint_catalog", json!({}))
            .unwrap_err()
            .contains("缺少 params"));
        assert!(call(
            &root,
            "codex_fetch_endpoint_catalog",
            json!({"params": {"baseUrl": "", "keySource": {"kind": "codexAuth"}}})
        )
        .unwrap_err()
        .contains("缺少 baseUrl"));
        assert!(call(
            &root,
            "codex_fetch_endpoint_catalog",
            json!({"params": {"baseUrl": "http://x", "keySource": {"kind": "magic"}}})
        )
        .unwrap_err()
        .contains("keySource.kind 无效"));
    }

    #[test]
    fn cached_lookup_reports_request_mismatch_without_fetching() {
        let payload = r#"{"models":[{"slug":"gpt-5"}]}"#;
        let base = serve(payload);
        let root = temp_root("cached-mismatch");
        call(&root, "codex_fetch_endpoint_catalog", native_params(&base)).unwrap();

        // 另一个 baseUrl：缓存里有记录但对不上请求。
        let other = json!({"params": {"baseUrl": "http://127.0.0.1:9999/models", "keySource": {"kind": "codexAuth"}}});
        let lookup = call(&root, "codex_endpoint_catalog_cached", other).unwrap();
        assert!(lookup["catalog"].is_null());
        assert_eq!(lookup["mismatch"], "request");

        // 还没有任何缓存时是 none，不是错误。
        let empty_root = temp_root("cached-none");
        let empty = call(&empty_root, "codex_endpoint_catalog_cached", native_params("http://127.0.0.1:1")).unwrap();
        assert!(empty["catalog"].is_null());
        assert_eq!(empty["mismatch"], "none");
    }

    #[test]
    fn published_lookup_is_empty_before_any_refresh() {
        let root = temp_root("published-empty");
        let lookup = call(&root, "codex_endpoint_catalog_published", native_params("http://127.0.0.1:1")).unwrap();
        assert!(lookup["catalog"].is_null());
        assert_eq!(lookup["mismatch"], "none");
    }

    #[test]
    fn status_reports_defaults_without_published_catalog() {
        let root = temp_root("status-default");
        let status = call(&root, "codex_endpoint_catalog_status", json!({})).unwrap();
        assert!(status["meta"].is_null());
        assert!(status["source"].is_null());
        assert_eq!(status["publishesCatalog"], false);
        assert_eq!(status["refresh"]["consecutiveFailures"], 0);
        assert_eq!(status["refresh"]["inProgress"], false);
        assert!(status["refresh"]["lastError"].is_null());
        assert!(status["refreshHours"].is_null());
        assert_eq!(status["pendingLocalChanges"], false);
        assert_eq!(status["missingSelected"].as_array().unwrap().len(), 0);
        assert_eq!(status["runtime"]["confirmed"], false);
        assert!(status["fetchError"].is_null());
    }

    #[test]
    fn refresh_without_source_records_a_structured_fetch_error() {
        let root = temp_root("refresh-no-source");
        let status = call(&root, "codex_refresh_endpoint_catalog", json!({})).unwrap();
        assert_eq!(status["publishesCatalog"], false);
        assert_eq!(status["fetchError"]["code"], "no_source");
        assert_eq!(status["refresh"]["consecutiveFailures"], 1);
        assert!(status["refresh"]["lastErrorAt"].as_str().is_some());
    }

    #[test]
    fn refresh_republishes_what_the_seeded_source_serves() {
        let payload = r#"{"models":[{"slug":"gpt-5","display_name":"GPT-5"}]}"#;
        let base = serve(payload);
        let root = temp_root("refresh-ok");
        // 模拟上一次发布留下的来源记录。
        store_file::write_json(
            &published_path(&root),
            &json!({
                "requestKey": "seed",
                "view": Value::Null,
                "meta": Value::Null,
                "source": {"url": format!("{base}/models"), "format": "native"},
                "checkedAt": null,
            }),
        )
        .unwrap();

        let status = call(&root, "codex_refresh_endpoint_catalog", json!({})).unwrap();
        assert_eq!(status["publishesCatalog"], true);
        assert!(status["meta"].is_object());
        assert!(status["fetchError"].is_null());
        assert_eq!(status["refresh"]["consecutiveFailures"], 0);

        let published = store_file::read_json(&published_path(&root))
            .unwrap()
            .unwrap();
        assert_eq!(published["view"]["models"][0]["slug"], "gpt-5");

        // 重发布后，published 命令按同一来源请求能命中。
        let lookup = call(
            &root,
            "codex_endpoint_catalog_published",
            json!({"params": {"baseUrl": format!("{base}/models"), "keySource": {"kind": "codexAuth"}}}),
        )
        .unwrap();
        assert!(lookup["catalog"].is_object());
        assert_eq!(lookup["mismatch"], Value::Null);
    }

    #[test]
    fn request_device_code_persists_a_pending_record() {
        let root = temp_root("device-request");
        let result = call(&root, "codex_request_device_code", json!({})).unwrap();
        assert!(result["userCode"].as_str().unwrap().contains('-'));
        assert_eq!(result["userCode"].as_str().unwrap().len(), 9);
        assert!(result["verificationUrl"].as_str().unwrap().starts_with("http"));
        assert!(!result["deviceAuthId"].as_str().unwrap().is_empty());
        assert_eq!(result["interval"], POLL_INTERVAL);

        let record = store_file::read_json(&device_path(&root)).unwrap().unwrap();
        assert_eq!(record["status"], "pending");
        assert!(record["expiresAtMs"].as_i64().unwrap() > record["createdAtMs"].as_i64().unwrap());
    }

    #[test]
    fn poll_device_code_follows_the_pending_state_machine() {
        let root = temp_root("device-poll");
        let requested = call(&root, "codex_request_device_code", json!({})).unwrap();
        let id = requested["deviceAuthId"].as_str().unwrap().to_string();
        let code = requested["userCode"].as_str().unwrap().to_string();

        // 待授权：pending，不带任何令牌字段。
        let pending = call(
            &root,
            "codex_poll_device_code",
            json!({"deviceAuthId": id, "userCode": code}),
        )
        .unwrap();
        assert_eq!(pending["status"], "pending");
        assert!(pending.get("accessToken").is_none());

        // 未知设备号 / 错误用户码：结构化 error，不是命令 Err。
        let unknown = call(
            &root,
            "codex_poll_device_code",
            json!({"deviceAuthId": "no-such-id", "userCode": code}),
        )
        .unwrap();
        assert_eq!(unknown["status"], "error");
        assert!(unknown["message"].as_str().unwrap().contains("不存在"));

        let mismatch = call(
            &root,
            "codex_poll_device_code",
            json!({"deviceAuthId": id, "userCode": "XXXX-YYYY"}),
        )
        .unwrap();
        assert_eq!(mismatch["status"], "error");
        assert!(mismatch["message"].as_str().unwrap().contains("不匹配"));
    }

    #[test]
    fn poll_device_code_reports_expired_and_denied() {
        let root = temp_root("device-expired");
        store_file::write_json(
            &device_path(&root),
            &json!({
                "deviceAuthId": "expired-id",
                "userCode": "ABCD-1234",
                "status": "pending",
                "createdAtMs": 0,
                "expiresAtMs": 1,
            }),
        )
        .unwrap();
        let expired = call(
            &root,
            "codex_poll_device_code",
            json!({"deviceAuthId": "expired-id", "userCode": "ABCD-1234"}),
        )
        .unwrap();
        assert_eq!(expired["status"], "error");
        assert!(expired["message"].as_str().unwrap().contains("过期"));

        store_file::write_json(
            &device_path(&root),
            &json!({
                "deviceAuthId": "denied-id",
                "userCode": "WXYZ-9876",
                "status": "denied",
                "createdAtMs": 0,
                "expiresAtMs": now_ms() + DEVICE_TTL_MS,
            }),
        )
        .unwrap();
        let denied = call(
            &root,
            "codex_poll_device_code",
            json!({"deviceAuthId": "denied-id", "userCode": "WXYZ-9876"}),
        )
        .unwrap();
        assert_eq!(denied["status"], "error");
        assert!(denied["message"].as_str().unwrap().contains("拒绝"));
    }

    #[test]
    fn poll_device_code_passes_success_tokens_through_without_fabricating() {
        let root = temp_root("device-success");
        // 成功记录只能来自上游回调落盘；测试直接写一条，验证令牌原样透传。
        store_file::write_json(
            &device_path(&root),
            &json!({
                "deviceAuthId": "ok-id",
                "userCode": "SUCC-ESS1",
                "status": "success",
                "createdAtMs": 0,
                "expiresAtMs": now_ms() + DEVICE_TTL_MS,
                "accessToken": "at-123",
                "refreshToken": "rt-456",
                "accountId": "acc-789",
            }),
        )
        .unwrap();
        let success = call(
            &root,
            "codex_poll_device_code",
            json!({"deviceAuthId": "ok-id", "userCode": "SUCC-ESS1"}),
        )
        .unwrap();
        assert_eq!(success["status"], "success");
        assert_eq!(success["accessToken"], "at-123");
        assert_eq!(success["refreshToken"], "rt-456");
        assert_eq!(success["accountId"], "acc-789");
        assert!(success["idToken"].is_null());
    }

    #[test]
    fn poll_device_code_rejects_missing_arguments() {
        let root = temp_root("device-bad");
        assert!(call(&root, "codex_poll_device_code", json!({}))
            .unwrap_err()
            .contains("缺少 deviceAuthId"));
        assert!(call(
            &root,
            "codex_poll_device_code",
            json!({"deviceAuthId": "id", "userCode": "  "})
        )
        .unwrap_err()
        .contains("缺少 userCode"));
    }
}
