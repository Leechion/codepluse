#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! codepluse desktop shell.
//!
//! Tauri owns the window; the Rust backend in `backend/` owns the HTTP and
//! WebSocket surface. Since Phase 4 there is no sidecar: this process serves
//! `127.0.0.1:<port>` itself, the window loads it, and the page keeps using
//! its HTTP/WebSocket transport — the IPC channel here only drives window
//! chrome (drag / minimize / maximize / close / open). Resizing uses the JS
//! window API directly (see `go-desktop.ts`): `WebviewWindow` has no
//! `start_resize_dragging`, and the enum lives in `tauri_runtime`, not the
//! facade crate.
//!
//! `SHELL_MARKER` is what tells the frontend which shell it is talking to;
//! without it the app would switch to the full-Tauri transport and `invoke()`
//! commands that this repository does not implement.

use std::{
    fs,
    net::TcpStream,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use serde::Deserialize;
use tauri::{Manager, RunEvent, State, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

mod backend;

const SHELL_MARKER: &str = "window.__CODEG_SHELL__='tauri-shell';window.__CODEG_TAURI_SHELL__=true;";
const DEFAULT_PORT: u16 = 8840;
const READY_TIMEOUT: Duration = Duration::from_secs(30);

struct Backend {
    port: u16,
    root: PathBuf,
    next_window: AtomicUsize,
}

#[derive(Deserialize)]
struct WorkspaceConfig {
    root: String,
}

/// Mirrors `cmd/desktop/main_windows.go`: an explicit env var wins, then a
/// `workspace.json` (beside the executable for repo builds, in the bundled
/// resource dir for installed builds), then the executable's own directory.
fn resolve_root(exe_dir: &Path, resource_dir: Option<&Path>) -> PathBuf {
    if let Ok(explicit) = std::env::var("CODEPLUSE_ROOT") {
        if !explicit.trim().is_empty() {
            return PathBuf::from(explicit);
        }
    }
    let mut candidates = vec![exe_dir.join("workspace.json")];
    if let Some(dir) = resource_dir {
        // Bundled resources keep their path relative to the app root, so an
        // NSIS install puts the manifest at <install>/resources/workspace.json.
        candidates.push(dir.join("resources").join("workspace.json"));
        candidates.push(dir.join("workspace.json"));
    }
    for config in candidates {
        if let Ok(bytes) = fs::read(&config) {
            if let Ok(parsed) = serde_json::from_slice::<WorkspaceConfig>(&bytes) {
                if !parsed.root.trim().is_empty() {
                    return PathBuf::from(parsed.root);
                }
            }
        }
    }
    exe_dir.to_path_buf()
}

fn ensure_frontend(root: &Path) -> Result<(), String> {
    if root.join("codeg-ui").join("out").join("workspace.html").is_file() {
        return Ok(());
    }
    Err(format!(
        "界面尚未构建：{}\n请先执行 pnpm --dir codeg-ui build（或运行 scripts/build-tauri.ps1）。",
        root.join("codeg-ui").join("out").join("workspace.html").display()
    ))
}

fn port_open(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

/// The one port this process serves. The migration used to add one (the
/// proxy in front of the sidecar); a single process needs a single port.
fn backend_port() -> u16 {
    std::env::var("CODEPLUSE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT)
}

fn wait_port(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if port_open(port) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    false
}

/// Headless mode (`CODEPLUSE_HEADLESS=1`): serve the backend without a
/// window. Used by the smoke tests and the migration harnesses, which need
/// the HTTP surface but not a webview.
fn run_headless() {
    let port = backend_port();
    let root = std::env::var("CODEPLUSE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });
    eprintln!("[headless] 127.0.0.1:{port} (root {})", root.display());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the tokio runtime");
    runtime.block_on(async move {
        let cfg = backend::Config {
            listen_port: port,
            upstream_port: None,
            root,
        };
        if let Err(err) = backend::serve(cfg).await {
            eprintln!("[headless] backend stopped: {err}");
            std::process::exit(1);
        }
    });
}

fn build_window(
    app: &tauri::AppHandle,
    label: &str,
    path: &str,
    port: u16,
    primary: bool,
) -> Result<(), String> {
    if !path.starts_with('/') {
        // Keep the destination on this server: a path is appended to the
        // loopback origin, never used as a URL of its own.
        return Err("窗口路径必须是站内绝对路径".into());
    }
    let url: Url = format!("http://127.0.0.1:{port}{path}")
        .parse()
        .map_err(|e| format!("窗口地址无效：{e}"))?;
    let (width, height) = if primary {
        (1440.0, 900.0)
    } else {
        (1200.0, 860.0)
    };
    WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title("codepluse")
        .inner_size(width, height)
        .min_inner_size(880.0, 560.0)
        .decorations(false)
        .initialization_script(SHELL_MARKER)
        .build()
        .map_err(|e| format!("窗口创建失败：{e}"))?;
    Ok(())
}

#[tauri::command]
fn window_close(window: WebviewWindow) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

#[tauri::command]
fn window_minimize(window: WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
fn window_toggle_maximize(window: WebviewWindow) -> Result<bool, String> {
    let maximized = window.is_maximized().map_err(|e| e.to_string())?;
    if maximized {
        window.unmaximize().map_err(|e| e.to_string())?;
    } else {
        window.maximize().map_err(|e| e.to_string())?;
    }
    Ok(!maximized)
}

#[tauri::command]
fn window_begin_drag(window: WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(|e| e.to_string())
}

#[tauri::command]
fn window_open(app: tauri::AppHandle, state: State<'_, Backend>, path: String) -> Result<(), String> {
    let index = state.next_window.fetch_add(1, Ordering::SeqCst) + 1;
    build_window(&app, &format!("app-{index}"), &path, state.port, false)
}

fn main() {
    // The MCP companion the agents spawn: `codepluse mcp-codeg <endpoint> <token>`.
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() > 1 && argv[1] == "mcp-codeg" {
        std::process::exit(backend::mcp_companion::main_companion(&argv[2..]));
    }
    if std::env::var("CODEPLUSE_HEADLESS").as_deref() == Ok("1") {
        run_headless();
        return;
    }
    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            window_close,
            window_minimize,
            window_toggle_maximize,
            window_begin_drag,
            window_open
        ])
        .setup(|app| {
            let port = backend_port();
            let exe = std::env::current_exe().map_err(|e| format!("无法定位可执行文件：{e}"))?;
            let exe_dir = exe
                .parent()
                .ok_or_else(|| "无法定位可执行文件目录".to_string())?;
            let resource_dir = app.path().resource_dir().ok();
            let root = resolve_root(exe_dir, resource_dir.as_deref());
            ensure_frontend(&root)?;

            // The backend runs inside this process: same binary, same event
            // loop, no sidecar to start, stop or roll back to.
            let server_root = root.clone();
            tauri::async_runtime::spawn(async move {
                let cfg = backend::Config {
                    listen_port: port,
                    upstream_port: None,
                    root: server_root,
                };
                if let Err(err) = backend::serve(cfg).await {
                    eprintln!("[backend] stopped: {err}");
                }
            });
            if !wait_port(port, READY_TIMEOUT) {
                return Err(format!(
                    "后台未在 {} 秒内就绪（端口 {port}）",
                    READY_TIMEOUT.as_secs()
                )
                .into());
            }

            app.manage(Backend {
                port,
                root,
                next_window: AtomicUsize::new(0),
            });
            build_window(app.handle(), "main", "/workspace", port, true)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the codepluse shell");

    app.run(|_app_handle, _event| {});
}
