//! Workspace background image storage.

use std::path::Path;

use base64::Engine;
use serde_json::{json, Value};

use crate::backend::store_file;

const MAX_BG_BYTES: usize = 64 * 1024 * 1024;
const FILE: &str = "codeg-workspace-background.json";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "background_read"
            | "background_set"
            | "background_clear"
            | "background_market_search"
            | "background_market_asset"
            | "background_market_download"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn path(root: &Path) -> std::path::PathBuf {
    root.join(".data").join(FILE)
}

fn mime(bytes: &[u8]) -> Option<&'static str> {
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

fn write_asset(root: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_BG_BYTES {
        return Err("背景图片不能为空且不能超过 64 MiB".into());
    }
    let mime = mime(bytes).ok_or("仅支持 PNG、JPEG、WebP 或 GIF 图片")?;
    let value = json!({
        "mime": mime,
        "dataBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
    });
    let file = path(root);
    let _lock = store_file::lock(&file)?;
    store_file::write_json(&file, &value)
}

fn market_url(url: &str) -> Result<url::Url, String> {
    let parsed = url::Url::parse(url).map_err(|_| "壁纸地址无效")?;
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if parsed.scheme() != "https"
        || !(host == "wallhaven.cc" || host.ends_with(".wallhaven.cc"))
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("壁纸地址不属于安全的 Wallhaven HTTPS 域名".into());
    }
    Ok(parsed)
}

fn fetch_market_asset(url: &str) -> Result<Vec<u8>, String> {
    let parsed = market_url(url)?;
    let response = crate::backend::net::get(parsed.as_str(), std::time::Duration::from_secs(60))
        .map_err(|error| format!("下载壁纸失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("下载壁纸失败：HTTP {}", response.status()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BG_BYTES as u64)
    {
        return Err("背景图片不能超过 64 MiB".into());
    }
    let bytes = response
        .bytes()
        .map_err(|error| format!("读取壁纸失败：{error}"))?;
    if bytes.len() > MAX_BG_BYTES {
        return Err("背景图片不能超过 64 MiB".into());
    }
    if mime(&bytes).is_none() {
        return Err("Wallhaven 返回了不支持的图片格式".into());
    }
    Ok(bytes.to_vec())
}

fn market_search(args: &Value) -> Result<Value, String> {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");
    let category = args
        .get("category")
        .and_then(Value::as_str)
        .unwrap_or("all");
    let categories = match category {
        "all" => "111",
        "general" => "100",
        "anime" => "010",
        "people" => "001",
        _ => return Err("壁纸分类无效".into()),
    };
    let page = args
        .get("page")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 1000);
    let mut url =
        url::Url::parse("https://wallhaven.cc/api/v1/search").map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("q", query.trim())
        .append_pair("categories", categories)
        .append_pair("purity", "100")
        .append_pair("sorting", "relevance")
        .append_pair("order", "desc")
        .append_pair("page", &page.to_string());
    let response = crate::backend::net::get(url.as_str(), std::time::Duration::from_secs(25))
        .map_err(|error| format!("连接 Wallhaven 失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("Wallhaven 搜索失败：HTTP {}", response.status()));
    }
    let data: Value = response
        .json()
        .map_err(|error| format!("解析 Wallhaven 搜索结果失败：{error}"))?;
    let rows = data
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Wallhaven 返回的数据格式无效")?;
    let items: Vec<Value> = rows
        .iter()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?;
            let full_url = row.pointer("/path")?.as_str()?;
            let source_url = row.get("url")?.as_str()?;
            let thumb_url = row.pointer("/thumbs/large")?.as_str()?;
            Some(json!({
                "id": id,
                "thumbUrl": thumb_url,
                "fullUrl": full_url,
                "sourceUrl": source_url,
                "width": row.get("dimension_x").and_then(Value::as_u64).unwrap_or(0),
                "height": row.get("dimension_y").and_then(Value::as_u64).unwrap_or(0),
                "fileSizeBytes": row.get("file_size").and_then(Value::as_u64).unwrap_or(0),
                "category": row.get("category").and_then(Value::as_str).unwrap_or(""),
            }))
        })
        .collect();
    let meta = data.get("meta");
    Ok(json!({
        "items": items,
        "page": meta.and_then(|value| value.get("current_page")).and_then(Value::as_u64).unwrap_or(page),
        "lastPage": meta.and_then(|value| value.get("last_page")).and_then(Value::as_u64).unwrap_or(page),
    }))
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let file = path(root);
    match command {
        "background_read" => {
            let Some(value) = store_file::read_json(&file)? else {
                return Ok(Value::Null);
            };
            Ok(value)
        }
        "background_set" => {
            let encoded = args
                .get("imageBase64")
                .and_then(Value::as_str)
                .ok_or("背景图片数据无效")?;
            if encoded.len() > MAX_BG_BYTES.div_ceil(3) * 4 {
                return Err("背景图片不能超过 64 MiB".into());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| "背景图片 Base64 数据无效")?;
            write_asset(root, &bytes)?;
            Ok(Value::Null)
        }
        "background_market_search" => market_search(args),
        "background_market_asset" => {
            let url = args.get("url").and_then(Value::as_str).unwrap_or("");
            let bytes = fetch_market_asset(url)?;
            Ok(json!({
                "mime": mime(&bytes).unwrap(),
                "dataBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
            }))
        }
        "background_market_download" => {
            let url = args.get("url").and_then(Value::as_str).unwrap_or("");
            let source_url = args.get("sourceUrl").and_then(Value::as_str).unwrap_or("");
            let _ = market_url(source_url)?;
            let bytes = fetch_market_asset(url)?;
            write_asset(root, &bytes)?;
            Ok(Value::Null)
        }
        "background_clear" => {
            let _lock = store_file::lock(&file)?;
            match std::fs::remove_file(&file) {
                Ok(()) => Ok(Value::Null),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
                Err(error) => Err(format!("清除背景图片失败：{error}")),
            }
        }
        _ => unreachable!("owned command must have a handler"),
    }
}
