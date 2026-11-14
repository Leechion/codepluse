//! Computer tools, ported from `server-go/internal/codeg/computer.go`.
//!
//! The management layer (settings, window enumeration, thumbnails, share
//! bookkeeping) is real; the input driver (synthetic mouse/keyboard by the
//! agent) is a desktop-shell feature and reports clearly as not implemented,
//! exactly like the Go build. State is session-level (`computerState`), kept
//! in process memory only — nothing under `.data/`.
//!
//! The Win32 surface is hand-rolled `extern "system"` declarations instead of
//! the `windows` crate: the calls needed are a dozen user32/gdi32 entry points
//! the MSVC toolchain links natively. Adding them via the crate would pull in
//! the Win32_UI_WindowsAndMessaging + Win32_Graphics_Gdi features (a windows
//! crate recompile) for code a dozen declarations cover.

use std::path::Path;
use std::sync::Mutex;

use base64::Engine;
use serde_json::{json, Map, Value};

type BOOL = i32;
type HWND = isize;
type HDC = isize;
type HBITMAP = isize;
type LPARAM = isize;

#[repr(C)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct BITMAPINFOHEADER {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_ppm: i32,
    y_ppm: i32,
    clr_used: u32,
    clr_important: u32,
}

// Go built the header plus one colour entry inline; BITMAPINFO needs the
// trailing bmiColors slot for GetDIBits even when unused.
#[repr(C)]
struct BITMAPINFO {
    header: BITMAPINFOHEADER,
    colors: [u32; 1],
}

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(
        callback: Option<unsafe extern "system" fn(HWND, LPARAM) -> BOOL>,
        l_param: LPARAM,
    ) -> BOOL;
    fn GetWindowTextW(hwnd: HWND, buffer: *mut u16, max: i32) -> i32;
    fn GetClassNameW(hwnd: HWND, buffer: *mut u16, max: i32) -> i32;
    fn IsWindowVisible(hwnd: HWND) -> BOOL;
    fn GetWindowRect(hwnd: HWND, rect: *mut RECT) -> BOOL;
    fn GetWindowThreadProcessId(hwnd: HWND, pid: *mut u32) -> u32;
    fn GetDC(hwnd: HWND) -> HDC;
    fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
    fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> BOOL;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(hdc: HDC) -> HDC;
    fn CreateCompatibleBitmap(hdc: HDC, width: i32, height: i32) -> HBITMAP;
    fn SelectObject(hdc: HDC, object: HBITMAP) -> HBITMAP;
    fn DeleteObject(object: HBITMAP) -> BOOL;
    fn DeleteDC(hdc: HDC) -> BOOL;
    fn GetDIBits(
        hdc: HDC,
        bitmap: HBITMAP,
        start: u32,
        lines: u32,
        bits: *mut u8,
        info: *mut BITMAPINFO,
        usage: u32,
    ) -> i32;
}

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "computer_available"
            | "get_computer_tools_settings"
            | "set_computer_tools_enabled"
            | "set_computer_tools_preferences"
            | "computer_status"
            | "computer_shared_state"
            | "computer_request_permission"
            | "computer_open_permission_settings"
            | "computer_reveal_helper"
            | "computer_list_shareable_windows"
            | "computer_window_thumbnail"
            | "computer_share_window"
            | "computer_share_windows"
            | "computer_share_app"
            | "computer_share_screen"
            | "computer_revoke_all"
            | "computer_stop"
            | "computer_stop_key_status"
            | "computer_indicator_fit"
            | "computer_driver_info"
            | "computer_driver_uninstall"
    )
}

/// Command outcome: the HTTP body plus the frames this server synthesises
/// (`computer://state`, `computer://agent-grant`) for the WS relay.
#[derive(Debug)]
pub struct Outcome {
    pub result: Value,
    pub events: Vec<(String, Value)>,
}

#[derive(Default)]
struct ComputerState {
    enabled: bool,
    prefs: Option<Map<String, Value>>,
    shared: Option<Vec<Value>>,
}

static STATE: Mutex<Option<ComputerState>> = Mutex::new(None);

fn with_state<T>(f: impl FnOnce(&mut ComputerState) -> T) -> T {
    let mut guard = STATE.lock().unwrap();
    let state = guard.get_or_insert_with(ComputerState::default);
    f(state)
}

/// Go `number()` for one JSON value: numbers truncate toward zero, the rest is 0.
fn number_of(value: &Value) -> i64 {
    match value {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

pub fn handle(_root: &Path, command: &str, args: &Value) -> Option<Result<Outcome, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(command, args))
}

fn run(command: &str, a: &Value) -> Result<Outcome, String> {
    match command {
        "computer_available" => Ok(Outcome {
            result: json!({
                "available": true,
                "platform": "windows",
                "driver": false,
                "note": "窗口枚举与截图可用；输入驱动层待实现",
            }),
            events: Vec::new(),
        }),
        "get_computer_tools_settings" => {
            let prefs = with_state(|st| {
                // Go stamps the defaults into state, so later preference
                // writes replace them rather than merging.
                if st.prefs.is_none() {
                    let mut map = Map::new();
                    map.insert("grantTtlMinutes".into(), json!(15));
                    map.insert("stopShortcut".into(), json!("ctrl+alt+q"));
                    map.insert("showIndicator".into(), json!(true));
                    st.prefs = Some(map);
                }
                Value::Object(st.prefs.clone().unwrap())
            });
            Ok(Outcome {
                result: json!({
                    "enabled": with_state(|st| st.enabled),
                    "preferences": prefs,
                    "available": true,
                    "driver": false,
                }),
                events: Vec::new(),
            })
        }
        "set_computer_tools_enabled" => {
            let enabled = a["enabled"] == json!(true);
            with_state(|st| st.enabled = enabled);
            Ok(Outcome {
                result: Value::Null,
                events: Vec::new(),
            })
        }
        "set_computer_tools_preferences" => {
            if let Value::Object(prefs) = &a["prefs"] {
                let snapshot = prefs.clone();
                with_state(|st| st.prefs = Some(snapshot));
            }
            Ok(Outcome {
                result: Value::Null,
                events: Vec::new(),
            })
        }
        "computer_status" | "computer_shared_state" => Ok(Outcome {
            result: json!({
                "enabled": with_state(|st| st.enabled),
                "shared": with_state(|st| st.shared.clone())
                    .map(|shared| json!(shared))
                    .unwrap_or(Value::Null),
                "available": true,
                "driver": false,
            }),
            events: Vec::new(),
        }),
        "computer_request_permission" | "computer_open_permission_settings" => {
            Err("电脑操作权限申请需要桌面助手，当前不可用".into())
        }
        "computer_reveal_helper" => Err("桌面助手未安装".into()),
        "computer_list_shareable_windows" => Ok(Outcome {
            result: json!({ "windows": list_shareable_windows() }),
            events: Vec::new(),
        }),
        "computer_window_thumbnail" => {
            let data = window_thumbnail(&text_of(&a["targetId"]))?;
            Ok(Outcome {
                result: json!({ "mime_type": "image/png", "base64": data }),
                events: Vec::new(),
            })
        }
        "computer_share_window" => {
            let (entry, events) = computer_share(&text_of(&a["targetId"]), &text_of(&a["level"]));
            Ok(Outcome {
                result: entry,
                events,
            })
        }
        "computer_share_windows" => {
            let mut events = Vec::new();
            if let Value::Array(ids) = &a["targetIds"] {
                for id in ids {
                    let (_, mut added) = computer_share(&text_of(id), &text_of(&a["level"]));
                    events.append(&mut added);
                }
            }
            Ok(Outcome {
                result: Value::Null,
                events,
            })
        }
        "computer_share_app" | "computer_share_screen" => {
            Err("整屏/应用共享需要桌面助手，当前不可用".into())
        }
        "computer_revoke_all" => {
            with_state(|st| st.shared = None);
            Ok(Outcome {
                result: Value::Null,
                events: vec![("computer://state".into(), json!({ "kind": "revoked" }))],
            })
        }
        "computer_stop" | "computer_stop_key_status" => Ok(Outcome {
            result: json!({ "stopped": true }),
            events: Vec::new(),
        }),
        "computer_indicator_fit" => Ok(Outcome {
            result: json!({
                "width": number_of(&a["width"]),
                "height": number_of(&a["height"]),
            }),
            events: Vec::new(),
        }),
        "computer_driver_info" | "computer_driver_uninstall" => {
            Err("输入驱动层待实现：本构建提供窗口枚举与截图，不含合成输入".into())
        }
        _ => Err(format!("未知的电脑操作命令：{command}")),
    }
}

fn text_of(value: &Value) -> String {
    value.as_str().unwrap_or("").to_string()
}

/// Adds or updates one share entry; emits `computer://agent-grant` only for a
/// new entry, mirroring Go's `computerShare`.
fn computer_share(target_id: &str, level: &str) -> (Value, Vec<(String, Value)>) {
    let mut event = Vec::new();
    let entry = with_state(|st| {
        if let Some(shared) = &mut st.shared {
            for entry in shared.iter_mut() {
                if entry["target_id"].as_str() == Some(target_id) {
                    entry["level"] = json!(level);
                    return entry.clone();
                }
            }
        }
        let entry = json!({ "target_id": target_id, "level": level });
        st.shared.get_or_insert_with(Vec::new).push(entry.clone());
        event.push((
            "computer://agent-grant".into(),
            json!({ "target_id": target_id, "level": level }),
        ));
        entry
    });
    (entry, event)
}

// ----------------------------- Win32 surface -----------------------------

/// Enumerates visible top-level windows with titles, mirroring Go's
/// `listShareableWindows` (same filters, same field names — `bounds` keeps the
/// Go struct's exported-field keys `Left/Top/Right/Bottom`).
fn list_shareable_windows() -> Value {
    let mut out: Vec<Value> = Vec::new();
    unsafe {
        EnumWindows(Some(enum_proc), &mut out as *mut Vec<Value> as LPARAM);
    }
    json!(out)
}

unsafe extern "system" fn enum_proc(hwnd: HWND, l_param: LPARAM) -> BOOL {
    let out = &mut *(l_param as *mut Vec<Value>);
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let title = window_text(hwnd);
    if title.trim().is_empty() {
        return 1;
    }
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    let rect = window_rect(hwnd);
    out.push(json!({
        "target_id": (hwnd as usize).to_string(),
        "title": title,
        "class": window_class(hwnd),
        "process_id": pid,
        "bounds": {
            "Left": rect.left,
            "Top": rect.top,
            "Right": rect.right,
            "Bottom": rect.bottom,
        },
    }));
    1
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), 256) };
    utf16(&buf, n.max(0) as usize)
}

fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), 128) };
    utf16(&buf, n.max(0) as usize)
}

fn utf16(buf: &[u16], len: usize) -> String {
    String::from_utf16_lossy(&buf[..len.min(buf.len())])
}

fn window_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetWindowRect(hwnd, &mut rect) };
    rect
}

/// Captures one window via PrintWindow and encodes PNG, mirroring Go's
/// `windowThumbnail` (32bpp top-down DIB, BGRA→RGBA swap, base64 body).
fn window_thumbnail(target_id: &str) -> Result<String, String> {
    // Go parses with Sscanf("%d"), which accepts a decimal prefix; target ids
    // are always bare digits, so a leading-digit parse covers both.
    let digits: String = target_id
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let hwnd = digits.parse::<usize>().unwrap_or(0) as HWND;
    if hwnd == 0 {
        return Err("窗口不存在".into());
    }
    let rect = window_rect(hwnd);
    let width = (rect.right - rect.left) as usize;
    let height = (rect.bottom - rect.top) as usize;
    if width == 0 || height == 0 || width > 8000 || height > 8000 {
        return Err("窗口尺寸异常".into());
    }
    unsafe {
        let screen_dc = GetDC(0);
        if screen_dc == 0 {
            return Err("无法获取设备上下文".into());
        }
        let result = capture(screen_dc, hwnd, width, height);
        ReleaseDC(0, screen_dc);
        result
    }
}

unsafe fn capture(
    screen_dc: HDC,
    hwnd: HWND,
    width: usize,
    height: usize,
) -> Result<String, String> {
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc == 0 {
        return Err("无法创建兼容设备上下文".into());
    }
    let bitmap = CreateCompatibleBitmap(screen_dc, width as i32, height as i32);
    if bitmap == 0 {
        DeleteDC(mem_dc);
        return Err("无法创建兼容位图".into());
    }
    let old = SelectObject(mem_dc, bitmap);
    // PW_RENDERFULLCONTENT
    PrintWindow(hwnd, mem_dc, 2);
    SelectObject(mem_dc, old);

    let mut info = BITMAPINFO {
        header: BITMAPINFOHEADER {
            size: 40,
            width: width as i32,
            height: -(height as i32), // top-down
            planes: 1,
            bit_count: 32,
            compression: 0,
            size_image: 0,
            x_ppm: 0,
            y_ppm: 0,
            clr_used: 0,
            clr_important: 0,
        },
        colors: [0],
    };
    let mut pixels = vec![0u8; width * height * 4];
    GetDIBits(
        mem_dc,
        bitmap,
        0,
        height as u32,
        pixels.as_mut_ptr(),
        &mut info,
        0,
    );
    DeleteObject(bitmap);
    DeleteDC(mem_dc);

    // Windows DIB rows are BGRA; the PNG encoder wants RGBA.
    let mut rgba = vec![0u8; width * height * 4];
    for (src, dst) in pixels.chunks_exact_mut(4).zip(rgba.chunks_exact_mut(4)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = src[3];
    }

    let mut png_bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut png_bytes, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("PNG 编码失败：{e}"))?;
    writer
        .write_image_data(&rgba)
        .map_err(|e| format!("PNG 编码失败：{e}"))?;
    writer.finish().map_err(|e| format!("PNG 编码失败：{e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&png_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(command: &str, args: Value) -> Result<Outcome, String> {
        run(command, &args)
    }

    /// The module state is process-global; the stateful tests must not
    /// interleave (cargo runs tests on parallel threads).
    static TEST_SERIALIZER: Mutex<()> = Mutex::new(());

    fn serialized() -> std::sync::MutexGuard<'static, ()> {
        TEST_SERIALIZER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn unknown_command_message_matches_go() {
        assert_eq!(
            call("computer_nope", json!({})).unwrap_err(),
            "未知的电脑操作命令：computer_nope"
        );
        assert!(!owns("computer_nope"));
    }

    #[test]
    fn available_reports_windows_without_driver() {
        let out = call("computer_available", json!({})).unwrap();
        assert_eq!(out.result["available"], json!(true));
        assert_eq!(out.result["platform"], json!("windows"));
        assert_eq!(out.result["driver"], json!(false));
    }

    #[test]
    fn settings_default_then_update() {
        let _guard = serialized();
        let out = call("get_computer_tools_settings", json!({})).unwrap();
        assert_eq!(out.result["preferences"]["grantTtlMinutes"], json!(15));
        assert_eq!(
            out.result["preferences"]["stopShortcut"],
            json!("ctrl+alt+q")
        );
        assert_eq!(out.result["preferences"]["showIndicator"], json!(true));
        assert_eq!(out.result["enabled"], json!(false));

        call("set_computer_tools_enabled", json!({ "enabled": true })).unwrap();
        call(
            "set_computer_tools_preferences",
            json!({ "prefs": { "grantTtlMinutes": 30 } }),
        )
        .unwrap();
        let out = call("computer_status", json!({})).unwrap();
        assert_eq!(out.result["enabled"], json!(true));
        // status 不带 preferences（Go 的 computer_status 只有四个键）。
        assert!(out.result.get("preferences").is_none());
        let out = call("get_computer_tools_settings", json!({})).unwrap();
        assert_eq!(out.result["preferences"]["grantTtlMinutes"], json!(30));
        // 非对象 prefs 被忽略，与 Go 的类型断言一致。
        call("set_computer_tools_preferences", json!({ "prefs": "x" })).unwrap();
        let out = call("get_computer_tools_settings", json!({})).unwrap();
        assert_eq!(out.result["preferences"]["grantTtlMinutes"], json!(30));
    }

    #[test]
    fn share_flow_and_events() {
        let _guard = serialized();
        call("computer_revoke_all", json!({})).unwrap();
        let out = call("computer_status", json!({})).unwrap();
        assert_eq!(
            out.result["shared"],
            Value::Null,
            "Go 的 nil slice 序列化为 null"
        );

        let out = call(
            "computer_share_window",
            json!({ "targetId": "101", "level": "window" }),
        )
        .unwrap();
        assert_eq!(out.result["target_id"], json!("101"));
        assert_eq!(
            out.events,
            vec![(
                "computer://agent-grant".to_string(),
                json!({ "target_id": "101", "level": "window" })
            )]
        );

        // 已存在的 target 只改 level，不再发事件。
        let out = call(
            "computer_share_window",
            json!({ "targetId": "101", "level": "screen" }),
        )
        .unwrap();
        assert_eq!(out.result["level"], json!("screen"));
        assert!(out.events.is_empty());

        let out = call(
            "computer_share_windows",
            json!({ "targetIds": ["102", "103"], "level": "window" }),
        )
        .unwrap();
        assert_eq!(out.result, Value::Null);
        assert_eq!(out.events.len(), 2);

        let out = call("computer_shared_state", json!({})).unwrap();
        let shared = out.result["shared"].as_array().unwrap();
        assert_eq!(shared.len(), 3);
        assert_eq!(shared[0]["level"], json!("screen"));

        let out = call("computer_revoke_all", json!({})).unwrap();
        assert_eq!(out.result, Value::Null);
        assert_eq!(
            out.events,
            vec![("computer://state".to_string(), json!({ "kind": "revoked" }))]
        );
        let out = call("computer_shared_state", json!({})).unwrap();
        assert_eq!(out.result["shared"], Value::Null);
    }

    #[test]
    fn indicator_fit_coerces_like_go_number() {
        let out = call(
            "computer_indicator_fit",
            json!({ "width": 800.9, "height": "600" }),
        )
        .unwrap();
        assert_eq!(out.result["width"], json!(800));
        assert_eq!(
            out.result["height"],
            json!(0),
            "字符串在 Go 的 number() 里是 0"
        );
    }

    #[test]
    fn stubs_report_clearly() {
        assert_eq!(
            call("computer_request_permission", json!({})).unwrap_err(),
            "电脑操作权限申请需要桌面助手，当前不可用"
        );
        assert_eq!(
            call("computer_reveal_helper", json!({})).unwrap_err(),
            "桌面助手未安装"
        );
        assert_eq!(
            call("computer_share_screen", json!({})).unwrap_err(),
            "整屏/应用共享需要桌面助手，当前不可用"
        );
        assert_eq!(
            call("computer_driver_info", json!({})).unwrap_err(),
            "输入驱动层待实现：本构建提供窗口枚举与截图，不含合成输入"
        );
        let out = call("computer_stop", json!({})).unwrap();
        assert_eq!(out.result, json!({ "stopped": true }));
    }

    #[test]
    fn window_enumeration_and_thumbnail_flow() {
        let out = call("computer_list_shareable_windows", json!({})).unwrap();
        let windows = out.result["windows"].as_array().unwrap();
        for entry in windows {
            assert!(entry["target_id"].as_str().is_some());
            assert!(entry["process_id"].is_u64());
            assert!(entry["bounds"]["Left"].is_i64());
            assert!(entry["bounds"]["Bottom"].is_i64());
        }
        // 可解析但无效的 hwnd：GetWindowRect 失败留零矩形 → Go 的「窗口尺寸异常」。
        assert_eq!(window_thumbnail("999999999").unwrap_err(), "窗口尺寸异常");
        // 解析不出数字 → hwnd 0 → 「窗口不存在」。
        assert_eq!(window_thumbnail("not-a-number").unwrap_err(), "窗口不存在");
        // 真实窗口（若枚举到了）应当能截图；没有窗口的机器上跳过。
        if let Some(entry) = windows.first() {
            let id = entry["target_id"].as_str().unwrap();
            if let Ok(data) = window_thumbnail(id) {
                assert!(!data.is_empty());
                // PNG 魔数（base64 前缀 iVBORw0KGgo = 89 50 4E 47 0D 0A 1A 0A）。
                assert!(data.starts_with("iVBORw0KGgo"), "缩略图不是 PNG");
            }
        }
    }
}
