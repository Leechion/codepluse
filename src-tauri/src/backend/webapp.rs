//! Phase 3 — the surfaces the Go sidecar used to own outside the command
//! dispatch: static assets, `/api/health`, attachment upload, the companion
//! endpoint and the local-request guard.
//!
//! Everything here mirrors the Go implementation byte for byte where the
//! client can observe it (status codes, JSON bodies, header sets, error
//! texts). The Go originals live in `server-go/internal/codeg/server.go`
//! (`static`, `respond`), `guard.go` and `attachments.go`.
//!
//! ```text
//! GET  /                     → codeg-ui/out/index.html
//! GET  /api/health           → synthesised locally (no sidecar needed)
//! POST /api/upload_attachment→ multipart upload into .data/uploads/<hex>/
//! POST /api/codeg_mcp        → 401 (the delegation registry is not ported)
//! POST /api/<command>        → command dispatch
//! GET  /ws, /ws/events       → the event protocol (events_ws.rs)
//! ```

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use serde_json::{json, Value};

use crate::backend::goformat;

/// Go caps POST bodies at 2MB (`MaxBytesReader`).
pub const MAX_COMMAND_BODY: usize = 2 << 20;
/// Go caps attachment uploads at 20MB plus 1MB of multipart overhead.
pub const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_ATTACHMENT_REQUEST: usize = MAX_ATTACHMENT_BYTES + 1024 * 1024;

/// Go's `respond` envelope: compact JSON, HTML escaping, trailing newline and
/// the two headers Go sets (`application/json; charset=utf-8`, `no-store`).
pub fn respond(status: StatusCode, value: &Value) -> Response {
    let mut body = goformat::marshal_preserve(&goformat::canon(value));
    body.push(b'\n');
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .expect("静态部件构造不可能失败")
}

/// Go's error envelope for the command surface: `{"code","message"}`.
pub fn error_envelope(status: StatusCode, code: &str, message: &str) -> Response {
    respond(status, &json!({ "code": code, "message": message }))
}

// ---------------------------------------------------------------------------
// A2 — /api/health
// ---------------------------------------------------------------------------

/// Go `server.go:280` synthesised the health row locally. The Rust front door
/// used to proxy it; now it answers by itself, so the app keeps working with
/// no sidecar. The shape is unchanged, only `backend` flips to `rust`.
///
/// `version` / `codeg_version` keep Go's literals on purpose: the frontend
/// compares `codeg_version` against its own build, and the updater contract
/// uses its own `VERSION` constant — changing either value here would move
/// behaviour that this migration is not supposed to move.
pub fn health() -> Response {
    let body = json!({
        "ok": true,
        "app": "codepluse",
        "data": {"status": "ok", "version": "2.0.0-rust", "sessionRuntime": "acp-v1"},
        "backend": "rust",
        "codeg_version": "0.42.0",
        "path_boundary": {
            "allow_any_path": env_flag("HARNESS_HUB_ALLOW_ANY_PATH"),
            "allow_any_browse": env_flag("HARNESS_HUB_ALLOW_ANY_BROWSE"),
        },
    });
    respond(StatusCode::OK, &body)
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).map(|v| v == "1").unwrap_or(false)
}

// ---------------------------------------------------------------------------
// A4 — /api/codeg_mcp (the companion endpoint)
// ---------------------------------------------------------------------------

/// Go `delegation.go:73` `companionRequest`.
pub fn companion_method_not_allowed() -> Response {
    respond(StatusCode::METHOD_NOT_ALLOWED, &json!({"error": "POST required"}))
}

/// The companion token did not match a live caller.
pub fn companion_unauthorized() -> Response {
    respond(StatusCode::UNAUTHORIZED, &json!({"error": "Invalid companion token"}))
}

/// The tool call itself failed (Go answers 400 with the error text).
pub fn companion_bad_request(message: &str) -> Response {
    respond(StatusCode::BAD_REQUEST, &json!({"error": message}))
}

/// A successful tool call answers with the tool's own payload.
pub fn companion_ok(value: &Value) -> Response {
    respond(StatusCode::OK, value)
}

// ---------------------------------------------------------------------------
// A6 — the local request guard
// ---------------------------------------------------------------------------

/// The local guard (Go `cmd/serve/guard.go`): the API grants process-level
/// power (terminal, ACP, file writes), so a web page must not be able to
/// drive it. Three cheap checks:
///
/// 1. **Host allowlist** — a rebound domain has `Origin == Host`, so only a
///    host whitelist stops it.
/// 2. **Origin allowlist** (plus Fetch Metadata for requests without Origin).
/// 3. **`application/json` on POST** — a cross-origin `text/plain` fetch is a
///    "simple request" and skips the CORS preflight, so it would reach the
///    handler anyway.
///
/// Uploads are multipart and are allowed that media type explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deny {
    ForbiddenHost,
    ForbiddenOrigin,
    CrossSite,
    UnsupportedContentType,
}

impl Deny {
    /// The code the Go side reports (`denyLocal`).
    pub fn code(self) -> &'static str {
        match self {
            Deny::ForbiddenHost => "forbidden_host",
            Deny::ForbiddenOrigin => "forbidden_origin",
            Deny::CrossSite => "cross_site_blocked",
            Deny::UnsupportedContentType => "unsupported_content_type",
        }
    }

    pub fn status(self) -> StatusCode {
        match self {
            Deny::UnsupportedContentType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            _ => StatusCode::FORBIDDEN,
        }
    }
}

/// The denial body — Go writes it raw, without a trailing newline.
pub fn denied(reason: Deny) -> Response {
    Response::builder()
        .status(reason.status())
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(format!(
            "{{\"code\":\"{}\",\"message\":\"请求被本地安全策略拒绝\"}}",
            reason.code()
        )))
        .expect("静态部件构造不可能失败")
}

/// Reads one header as text.
fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

/// The hosts the server answers to (Go `newLocalGuard`).
fn allowed_hosts(port: u16) -> Vec<String> {
    vec![
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
}

fn allowed_origins(port: u16) -> Vec<String> {
    vec![
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
        format!("http://[::1]:{port}"),
    ]
}

/// The guard itself. `path` decides whether the content-type rule applies.
pub fn guard(
    headers: &HeaderMap,
    host: &str,
    listen_port: u16,
    method: &Method,
    path: &str,
) -> Result<(), Deny> {
    let hosts = allowed_hosts(listen_port);
    if !hosts.contains(&host.to_lowercase()) {
        return Err(Deny::ForbiddenHost);
    }
    let origin = header_str(headers, header::ORIGIN).unwrap_or_default();
    let origin = origin.trim_end_matches('/').to_lowercase();
    if !origin.is_empty() {
        if !allowed_origins(listen_port).contains(&origin) {
            return Err(Deny::ForbiddenOrigin);
        }
    } else if header_str(headers, header::HeaderName::from_static("sec-fetch-site"))
        .map(|site| site.eq_ignore_ascii_case("cross-site"))
        .unwrap_or(false)
    {
        return Err(Deny::CrossSite);
    }
    if *method == Method::POST && path.starts_with("/api/") {
        let content_type = header_str(headers, header::CONTENT_TYPE).unwrap_or_default();
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let want = if path == "/api/upload_attachment" {
            "multipart/form-data"
        } else {
            "application/json"
        };
        if media_type != want {
            return Err(Deny::UnsupportedContentType);
        }
    }
    Ok(())
}

/// The WebSocket upgrade check: the browser's Origin must be one of ours.
pub fn origin_allowed_for_ws(origin: &str, request_host: &str, listen_port: u16) -> bool {
    let origin = origin.trim_end_matches('/').to_lowercase();
    let allowed = allowed_origins(listen_port);
    if allowed.contains(&origin) {
        return true;
    }
    // A native client sends no Origin; a mismatching Host is still refused.
    origin.is_empty() && allowed_hosts(listen_port).contains(&request_host.to_lowercase())
}

// ---------------------------------------------------------------------------
// A3 — /api/upload_attachment
// ---------------------------------------------------------------------------

/// Go `attachments.go` `uploadAttachment`.
pub fn upload_attachment(root: &Path, method: &Method, headers: &HeaderMap, body: &[u8]) -> Response {
    if method != Method::POST {
        return error_envelope(
            StatusCode::METHOD_NOT_ALLOWED,
            "invalid_input",
            "Use POST",
        );
    }
    if body.len() > MAX_ATTACHMENT_REQUEST {
        return error_envelope(StatusCode::BAD_REQUEST, "invalid_input", "无法读取附件");
    }
    let boundary = match multipart_boundary(headers) {
        Some(boundary) => boundary,
        None => return error_envelope(StatusCode::BAD_REQUEST, "invalid_input", "无法读取附件"),
    };
    let part = match multipart_file(body, &boundary) {
        Some(part) => part,
        None => return error_envelope(StatusCode::BAD_REQUEST, "invalid_input", "无法读取附件"),
    };
    // Go: filepath.Base(strings.ReplaceAll(header.Filename, "\\", "/"))
    let raw_name = part.filename.replace('\\', "/");
    let name = match raw_name.rsplit('/').next() {
        Some(base) => base.to_string(),
        None => String::new(),
    };
    if name.is_empty() || name == "." || name == ".." {
        return error_envelope(StatusCode::BAD_REQUEST, "invalid_input", "附件名称无效");
    }

    let dir = uploads_root(root).join(goformat::hex_id());
    if std::fs::create_dir_all(&dir).is_err() {
        return error_envelope(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upload_failed",
            "创建上传目录失败",
        );
    }
    let path = dir.join(&name);
    // Exclusive create: an existing file is never overwritten.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(_) => {
            let _ = std::fs::remove_dir_all(&dir);
            return error_envelope(
                StatusCode::INTERNAL_SERVER_ERROR,
                "upload_failed",
                "写入附件失败",
            );
        }
    };
    use std::io::Write;
    let size = part.data.len();
    let too_large = size > MAX_ATTACHMENT_BYTES;
    let written = if too_large {
        0
    } else {
        match file.write_all(&part.data) {
            Ok(()) => match file.flush() {
                Ok(()) => size,
                Err(_) => 0,
            },
            Err(_) => 0,
        }
    };
    drop(file);
    if written == 0 || too_large {
        // Go removes the half-written file and the directory it lives in.
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        return error_envelope(
            StatusCode::BAD_REQUEST,
            "invalid_input",
            "附件为空、过大或读取失败",
        );
    }
    respond(
        StatusCode::OK,
        &json!({
            "path": path.to_string_lossy(),
            "name": name,
            "size": written,
            "mimeType": part.mime,
        }),
    )
}

fn uploads_root(root: &Path) -> PathBuf {
    root.join(".data").join("uploads")
}

struct MultipartPart {
    filename: String,
    mime: String,
    data: Vec<u8>,
}

fn multipart_boundary(headers: &HeaderMap) -> Option<String> {
    let content_type = header_str(headers, header::CONTENT_TYPE)?;
    let lower = content_type.to_lowercase();
    if !lower.starts_with("multipart/form-data") {
        return None;
    }
    for piece in content_type.split(';').skip(1) {
        let piece = piece.trim();
        if let Some(value) = piece.strip_prefix("boundary=") {
            let value = value.trim().trim_matches('"');
            if value.is_empty() {
                return None;
            }
            return Some(value.to_string());
        }
    }
    None
}

/// Minimal multipart reader: enough for the single `file` part Go reads.
/// Hand-rolled on purpose — the axum `multipart` feature would pull a new
/// crate into an offline build.
fn multipart_file(body: &[u8], boundary: &str) -> Option<MultipartPart> {
    let delimiter = format!("--{boundary}");
    let mut cursor = 0usize;
    while let Some(start) = find(body, delimiter.as_bytes(), cursor) {
        let after = start + delimiter.len();
        // "--" right after the boundary marks the end of the multipart body.
        if body.len() >= after + 2 && &body[after..after + 2] == b"--" {
            return None;
        }
        let mut part_start = after;
        while part_start < body.len() && (body[part_start] == b'\r' || body[part_start] == b'\n') {
            part_start += 1;
        }
        let header_end = find(body, b"\r\n\r\n", part_start)?;
        let header_text = String::from_utf8_lossy(&body[part_start..header_end]).to_string();
        let content_start = header_end + 4;
        let next = find(body, delimiter.as_bytes(), content_start)?;
        // The CRLF in front of the delimiter belongs to the framing.
        let content_end = next.saturating_sub(2).max(content_start);
        let data = body[content_start..content_end].to_vec();

        let mut name = String::new();
        let mut filename = String::new();
        let mut mime = String::new();
        for line in header_text.lines() {
            let (key, value) = match line.split_once(':') {
                Some(pair) => pair,
                None => continue,
            };
            let key = key.trim().to_lowercase();
            let value = value.trim();
            if key == "content-disposition" {
                for piece in value.split(';').skip(1) {
                    let piece = piece.trim();
                    if let Some(raw) = piece.strip_prefix("name=") {
                        name = raw.trim().trim_matches('"').to_string();
                    } else if let Some(raw) = piece.strip_prefix("filename=") {
                        filename = raw.trim().trim_matches('"').to_string();
                    }
                }
            } else if key == "content-type" {
                mime = value.to_string();
            }
        }
        if name == "file" {
            return Some(MultipartPart {
                filename,
                mime,
                data,
            });
        }
        cursor = next;
    }
    None
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

// ---------------------------------------------------------------------------
// A1 — static assets
// ---------------------------------------------------------------------------

/// Go `server.go:362` `static`: serve `codeg-ui/out` with the same lookup
/// order and the same headers `http.ServeFile` sets (Content-Type by
/// extension, Content-Length, Last-Modified, ETag/If-None-Match, Range).
pub fn static_asset(root: &Path, uri_path: &str, headers: &HeaderMap) -> Response {
    let base = root.join("codeg-ui").join("out");
    let request = if uri_path == "/" { "/index.html" } else { uri_path };
    // Go: filepath.Clean("/"+request) — collapses `..` and never escapes the
    // root, so `/../../etc/passwd` becomes `<out>/etc/passwd`.
    let cleaned = clean_absolute(request);
    let mut file = base.join(cleaned.trim_start_matches('/'));
    let mut content_type_override: Option<&str> = None;

    if header_str(headers, header::HeaderName::from_static("rsc")).as_deref() == Some("1") {
        // RSC flight data: Go tries `<file>.txt` first and does NOT fall back
        // to the HTML variant when it is missing.
        let candidate = with_extension(&file, "txt");
        if candidate.is_file() {
            file = candidate;
            content_type_override = Some("text/x-component");
        }
    } else {
        let is_dir = file.is_dir();
        if !file.is_file() || is_dir {
            let candidate = with_extension(&file, "html");
            if candidate.is_file() {
                file = candidate;
            } else {
                file = file.join("index.html");
            }
        }
    }

    if !file.is_file() {
        return not_found();
    }
    let metadata = match std::fs::metadata(&file) {
        Ok(metadata) => metadata,
        Err(_) => return not_found(),
    };
    let size = metadata.len();
    let modified = metadata.modified().ok();
    let etag = etag_for(modified, size);
    let content_type = content_type_override
        .map(|value| value.to_string())
        .unwrap_or_else(|| mime_for(&file));

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, etag.clone());
    if let Some(modified) = modified {
        builder = builder.header(header::LAST_MODIFIED, http_date(modified));
    }

    // Preconditions: If-None-Match wins over If-Modified-Since (Go's
    // `checkPreconditions`).
    if let Some(requested) = header_str(headers, header::IF_NONE_MATCH) {
        if etag_matches(&requested, &etag) {
            return builder.status(StatusCode::NOT_MODIFIED).body(Body::empty()).unwrap();
        }
    } else if let (Some(since), Some(modified)) = (
        header_str(headers, header::IF_MODIFIED_SINCE).and_then(|value| parse_http_date(&value)),
        modified,
    ) {
        if modified <= since {
            return builder.status(StatusCode::NOT_MODIFIED).body(Body::empty()).unwrap();
        }
    }

    // Range requests (PDF preview, video scrubbing).
    if let Some(range) = header_str(headers, header::RANGE) {
        if let Some((start, end)) = parse_range(&range, size) {
            let slice = match read_slice(&file, start, end) {
                Some(slice) => slice,
                None => return range_not_satisfiable(size),
            };
            return builder
                .status(StatusCode::PARTIAL_CONTENT)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{}", size.saturating_sub(1)),
                )
                .header(header::CONTENT_LENGTH, slice.len().to_string())
                .body(Body::from(slice))
                .unwrap();
        }
        return range_not_satisfiable(size);
    }

    let data = match std::fs::read(&file) {
        Ok(data) => data,
        Err(_) => return not_found(),
    };
    builder
        .header(header::CONTENT_LENGTH, data.len().to_string())
        .body(Body::from(data))
        .unwrap()
}

fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("404 page not found\n"))
        .unwrap()
}

fn range_not_satisfiable(size: u64) -> Response {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::CONTENT_RANGE, format!("bytes */{size}"))
        .body(Body::empty())
        .unwrap()
}

/// `filepath.Clean("/"+request)` — the leading slash makes the result
/// absolute, so `..` can never climb above the root.
fn clean_absolute(request: &str) -> String {
    let joined = format!("/{request}");
    let mut parts: Vec<&str> = Vec::new();
    for piece in joined.split('/') {
        match piece {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return "/".to_string();
    }
    format!("/{}", parts.join("/"))
}

fn with_extension(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    name.push('.');
    name.push_str(extension);
    path.with_file_name(name)
}

/// Go's `mime.TypeByExtension` for the types a web UI actually ships.
fn mime_for(path: &Path) -> String {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "text/xml; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/vnd.microsoft.icon",
        "avif" => "image/avif",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Go's `http.ServeContent` ETag: modtime nanos and size.
fn etag_for(modified: Option<SystemTime>, size: u64) -> String {
    let nanos = modified
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|delta| delta.as_nanos() as u64)
        .unwrap_or(0);
    format!("\"{nanos:x}-{size:x}\"")
}

fn etag_matches(requested: &str, etag: &str) -> bool {
    let requested = requested.trim();
    if requested == "*" {
        return true;
    }
    requested.split(',').any(|candidate| {
        let candidate = candidate.trim();
        let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
        let etag = etag.strip_prefix("W/").unwrap_or(etag);
        candidate == etag
    })
}

/// `Mon, 02 Jan 2006 15:04:05 GMT`
fn http_date(time: SystemTime) -> String {
    let seconds = time.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format_http_date(seconds)
}

fn format_http_date(seconds: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = seconds / 86_400;
    let rem = seconds % 86_400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    let weekday = DAYS[(days % 7) as usize];
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{weekday}, {day:02} {} {year} {hour:02}:{minute:02}:{second:02} GMT",
        MONTHS[(month - 1) as usize]
    )
}

fn parse_http_date(value: &str) -> Option<SystemTime> {
    // RFC 1123: "Mon, 02 Jan 2006 15:04:05 GMT"
    let value = value.trim();
    let rest = value.split_once(", ")?.1;
    let mut pieces = rest.split(' ');
    let day: u64 = pieces.next()?.parse().ok()?;
    let month = pieces.next()?;
    let year: i64 = pieces.next()?.parse().ok()?;
    let time = pieces.next()?;
    let mut clock = time.split(':');
    let hour: u64 = clock.next()?.parse().ok()?;
    let minute: u64 = clock.next()?.parse().ok()?;
    let second: u64 = clock.next()?.parse().ok()?;
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month_index = MONTHS.iter().position(|name| *name == month)? as i64 + 1;
    let days = days_from_civil(year, month_index, day as i64);
    let seconds = days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    Some(UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `bytes=start-end` (a single range — what browsers and PDF viewers send).
fn parse_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let (start, end) = if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        if suffix == 0 || size == 0 {
            return None;
        }
        (size.saturating_sub(suffix), size - 1)
    } else {
        let start: u64 = start.parse().ok()?;
        let end = if end.is_empty() {
            size.checked_sub(1)?
        } else {
            end.parse::<u64>().ok()?.min(size.checked_sub(1)?)
        };
        (start, end)
    };
    if start > end || start >= size {
        return None;
    }
    Some((start, end))
}

fn read_slice(path: &Path, start: u64, end: u64) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let length = (end - start + 1) as usize;
    let mut buffer = vec![0u8; length];
    file.read_exact(&mut buffer).ok()?;
    Some(buffer)
}

// ---------------------------------------------------------------------------
// A8 — development hot-reload proxy
// ---------------------------------------------------------------------------

/// The `next dev` target Go proxied non-API traffic to, if any.
pub fn dev_frontend() -> Option<String> {
    std::env::var("HARNESS_HUB_DEV_FRONTEND")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Go's dev-proxy failure text: `dev frontend unavailable (<target>): <err>`.
pub fn dev_unavailable(target: &str, err: &str) -> Response {
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(format!(
            "dev frontend unavailable ({target}): {err}\n"
        )))
        .unwrap()
}

/// Header helpers shared with the dev proxy.
pub fn copy_headers(source: &HeaderMap, target: &mut axum::http::HeaderMap) {
    for (name, value) in source.iter() {
        if name == header::HOST || name == header::CONNECTION || name == header::CONTENT_LENGTH {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            target.append(name, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn clean_absolute_cannot_escape_the_root() {
        assert_eq!(clean_absolute("/../../etc/passwd"), "/etc/passwd");
        assert_eq!(clean_absolute("/a/../../b"), "/b");
        assert_eq!(clean_absolute("/workspace"), "/workspace");
        assert_eq!(clean_absolute("/"), "/");
    }

    #[test]
    fn guard_follows_the_host_and_origin_allowlists() {
        let post = Method::POST;
        let json = headers(&[("content-type", "application/json")]);
        // No browser signals: native clients pass.
        assert!(guard(&json, "127.0.0.1:8840", 8840, &post, "/api/list_folders").is_ok());
        // A rebound host is refused.
        assert_eq!(
            guard(&json, "evil.com:8840", 8840, &post, "/api/list_folders").unwrap_err(),
            Deny::ForbiddenHost
        );
        // The right origin passes.
        assert!(guard(
            &headers(&[
                ("origin", "http://127.0.0.1:8840"),
                ("content-type", "application/json")
            ]),
            "127.0.0.1:8840",
            8840,
            &post,
            "/api/list_folders"
        )
        .is_ok());
        // A foreign origin is refused.
        assert_eq!(
            guard(
                &headers(&[
                    ("origin", "http://evil.example"),
                    ("content-type", "application/json")
                ]),
                "127.0.0.1:8840",
                8840,
                &post,
                "/api/list_folders"
            )
            .unwrap_err(),
            Deny::ForbiddenOrigin
        );
        // Without an Origin, a cross-site fetch is refused.
        assert_eq!(
            guard(
                &headers(&[
                    ("sec-fetch-site", "cross-site"),
                    ("content-type", "application/json")
                ]),
                "127.0.0.1:8840",
                8840,
                &post,
                "/api/list_folders"
            )
            .unwrap_err(),
            Deny::CrossSite
        );
        // Same-site is allowed (Go only blocks "cross-site").
        assert!(guard(
            &headers(&[
                ("sec-fetch-site", "same-site"),
                ("content-type", "application/json")
            ]),
            "127.0.0.1:8840",
            8840,
            &post,
            "/api/list_folders"
        )
        .is_ok());
    }

    #[test]
    fn json_body_is_required_for_api_posts() {
        let post = Method::POST;
        assert_eq!(
            guard(
                &headers(&[("content-type", "text/plain")]),
                "127.0.0.1:8840",
                8840,
                &post,
                "/api/list_folders"
            )
            .unwrap_err(),
            Deny::UnsupportedContentType
        );
        // Uploads are multipart.
        assert!(guard(
            &headers(&[("content-type", "multipart/form-data; boundary=X1")]),
            "127.0.0.1:8840",
            8840,
            &post,
            "/api/upload_attachment"
        )
        .is_ok());
        // Static requests are not subject to the content-type rule.
        assert!(guard(
            &headers(&[("content-type", "text/html")]),
            "127.0.0.1:8840",
            8840,
            &post,
            "/index.html"
        )
        .is_ok());
    }

    #[test]
    fn multipart_reader_finds_the_file_part() {
        let boundary = "X1";
        let body = format!(
            "--X1\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--X1--\r\n"
        );
        let part = multipart_file(body.as_bytes(), boundary).expect("file part");
        assert_eq!(part.filename, "a.txt");
        assert_eq!(part.mime, "text/plain");
        assert_eq!(part.data, b"hello");
    }

    #[test]
    fn upload_rejects_invalid_names_and_empty_files() {
        let root = std::env::temp_dir().join(format!("codepluse-upload-{}", goformat::hex_id()));
        std::fs::create_dir_all(&root).unwrap();
        let body = "--X1\r\nContent-Disposition: form-data; name=\"file\"; filename=\"..\"\r\n\r\nx\r\n--X1--\r\n";
        let response = upload_attachment(
            &root,
            &Method::POST,
            &headers(&[("content-type", "multipart/form-data; boundary=X1")]),
            body.as_bytes(),
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let empty = "--X1\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\n\r\n\r\n--X1--\r\n";
        let response = upload_attachment(
            &root,
            &Method::POST,
            &headers(&[("content-type", "multipart/form-data; boundary=X1")]),
            empty.as_bytes(),
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn upload_writes_the_file_and_reports_it() {
        let root = std::env::temp_dir().join(format!("codepluse-upload-{}", goformat::hex_id()));
        std::fs::create_dir_all(&root).unwrap();
        let body = "--X1\r\nContent-Disposition: form-data; name=\"file\"; filename=\"note.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--X1--\r\n";
        let response = upload_attachment(
            &root,
            &Method::POST,
            &headers(&[("content-type", "multipart/form-data; boundary=X1")]),
            body.as_bytes(),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let uploads = root.join(".data").join("uploads");
        let written: Vec<_> = std::fs::read_dir(&uploads).unwrap().collect();
        assert_eq!(written.len(), 1);
        assert!(written[0].as_ref().unwrap().path().join("note.txt").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn health_reports_the_rust_backend() {
        let response = health();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json; charset=utf-8"
        );
    }

    #[test]
    fn range_parsing_handles_the_common_shapes() {
        assert_eq!(parse_range("bytes=0-1023", 4096), Some((0, 1023)));
        assert_eq!(parse_range("bytes=100-", 4096), Some((100, 4095)));
        assert_eq!(parse_range("bytes=-100", 4096), Some((3996, 4095)));
        assert_eq!(parse_range("bytes=5000-6000", 4096), None);
        assert_eq!(parse_range("items=0-10", 4096), None);
    }

    #[test]
    fn http_dates_round_trip() {
        let stamp = UNIX_EPOCH + std::time::Duration::from_secs(1_763_000_000);
        let text = http_date(stamp);
        assert_eq!(text, "Thu, 13 Nov 2025 02:13:20 GMT");
        let parsed = parse_http_date(&text).expect("parse");
        assert_eq!(parsed, stamp);
    }
}
