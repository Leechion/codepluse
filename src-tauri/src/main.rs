#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! codepluse desktop shell.
//!
//! Tauri owns the window; the Go backend (`codepluse-backend` sidecar) still
//! serves the UI over HTTP on 127.0.0.1 and owns every workspace command.
//! The page therefore keeps using its HTTP/WebSocket transport — the IPC
//! channel here only drives window chrome (drag / minimize / maximize / close
//! / open). Resizing uses the JS window API directly (see `go-desktop.ts`):
//! `WebviewWindow` has no `start_resize_dragging`, and the enum lives in
//! `tauri_runtime`, not the facade crate.
//!
//! `SHELL_MARKER` is what tells the frontend which shell it is talking to;
//! without it the app would switch to the full-Tauri transport and `invoke()`
//! commands that this repository does not implement.

use std::{
    fs,
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use serde::Deserialize;
use tauri::{Manager, RunEvent, State, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_shell::{process::CommandChild, ShellExt};

mod backend;

const SHELL_MARKER: &str = "window.__CODEG_SHELL__='tauri-sidecar';window.__CODEG_TAURI_SHELL__=true;";
const DEFAULT_PORT: u16 = 8840;
const READY_TIMEOUT: Duration = Duration::from_secs(30);

struct Backend {
    child: Mutex<Option<CommandChild>>,
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

/// Port the migration proxy listens on. Defaults to the backend port + 1 so a
/// running old instance on the base port is never disturbed.
fn proxy_port_for(go_port: u16) -> u16 {
    std::env::var("CODEPLUSE_PROXY_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or_else(|| go_port.saturating_add(1))
}

/// `CODEPLUSE_BACKEND=go` bypasses the proxy and points the window straight at
/// the sidecar — the rollback switch for the migration.
fn proxy_enabled() -> bool {
    !matches!(
        std::env::var("CODEPLUSE_BACKEND").unwrap_or_default().as_str(),
        "go"
    )
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

/// True only for *our* backend. A bare TCP probe would happily attach the
/// window to any unrelated program that happens to hold 8840; the Go server
/// answers `/api/health` with `"backend":"go"`.
fn probe_backend(port: u16) -> bool {
    use std::io::{Read, Write};

    let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    let request = format!(
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut body = String::new();
    let _ = stream.take(4096).read_to_string(&mut body);
    body.contains("\"backend\":\"go\"")
}

fn start_backend(app: &tauri::AppHandle) -> Result<Backend, String> {
    let port = std::env::var("CODEPLUSE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);

    // Resolve the workspace first: both the reuse path and the spawn path need
    // it, and the proxy answers some commands straight off the same files.
    let exe = std::env::current_exe().map_err(|e| format!("无法定位可执行文件：{e}"))?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| "无法定位可执行文件目录".to_string())?;
    let resource_dir = app.path().resource_dir().ok();
    let root = resolve_root(exe_dir, resource_dir.as_deref());

    // A ready backend is reused: that is the dev loop, and it also stops a
    // second window from spawning a competing process.
    if probe_backend(port) {
        return Ok(Backend {
            child: Mutex::new(None),
            port,
            root,
            next_window: AtomicUsize::new(0),
        });
    }
    if port_open(port) {
        return Err(format!(
            "端口 {port} 已被其他程序占用。请关闭占用者，或设置 CODEPLUSE_PORT 换一个端口。"
        ));
    }
    ensure_frontend(&root)?;

    let (mut events, child) = app
        .shell()
        .sidecar("codepluse-backend")
        .map_err(|e| format!("未找到后台 sidecar（binaries/codepluse-backend-<triple>.exe）：{e}"))?
        .current_dir(&root)
        .env("HARNESS_HUB_CODEG", "1")
        .env("HOST", "127.0.0.1")
        .env("HARNESS_HUB_WARM", "0")
        .env("PORT", port.to_string())
        .spawn()
        .map_err(|e| format!("后台启动失败：{e}"))?;

    // Drain the pipe: a full stdout buffer would block the backend mid-request.
    tauri::async_runtime::spawn(async move { while events.recv().await.is_some() {} });

    let deadline = Instant::now() + READY_TIMEOUT;
    let mut ready = false;
    while Instant::now() < deadline {
        if probe_backend(port) {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    if !ready {
        let _ = child.kill();
        return Err(format!(
            "后台未在 {} 秒内就绪：请查看 {}/.data/codeg-desktop-backend.log",
            READY_TIMEOUT.as_secs(),
            root.display()
        ));
    }

    Ok(Backend {
        child: Mutex::new(Some(child)),
        port,
        root,
        next_window: AtomicUsize::new(0),
    })
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

/// Headless front door: `CODEPLUSE_PROXY_ONLY=1` serves the migration proxy
/// without building a window. Used by `scripts/parity.mjs` so a diff can be
/// checked without launching the desktop shell.
///
///   CODEPLUSE_PROXY_ONLY=1 CODEPLUSE_PORT=8840 CODEPLUSE_PROXY_PORT=8841 \
///     CODEPLUSE_ROOT=<workspace> codepluse.exe
fn run_proxy_only() {
    let upstream = std::env::var("CODEPLUSE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let listen = proxy_port_for(upstream);
    let root = std::env::var("CODEPLUSE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });
    eprintln!(
        "[proxy-only] 127.0.0.1:{listen} -> 127.0.0.1:{upstream} (root {})",
        root.display()
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the tokio runtime");
    runtime.block_on(async move {
        let cfg = backend::Config {
            listen_port: listen,
            upstream_port: upstream,
            root,
        };
        if let Err(err) = backend::serve(cfg).await {
            eprintln!("[proxy-only] front door stopped: {err}");
            std::process::exit(1);
        }
    });
}

fn main() {
    if std::env::var("CODEPLUSE_PROXY_ONLY").as_deref() == Ok("1") {
        run_proxy_only();
        return;
    }
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            window_close,
            window_minimize,
            window_toggle_maximize,
            window_begin_drag,
            window_open
        ])
        .setup(|app| {
            let backend = start_backend(app.handle())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let go_port = backend.port;
            let root = backend.root.clone();
            app.manage(backend);

            // Phase 1: put the Rust front door in front of the sidecar so
            // commands can be ported one at a time. The window loads the proxy;
            // unported traffic is forwarded unchanged.
            let page_port = if proxy_enabled() {
                let proxy_port = proxy_port_for(go_port);
                tauri::async_runtime::spawn(async move {
                    let cfg = backend::Config {
                        listen_port: proxy_port,
                        upstream_port: go_port,
                        root,
                    };
                    if let Err(err) = backend::serve(cfg).await {
                        eprintln!("[proxy] front door stopped: {err}");
                    }
                });
                if !wait_port(proxy_port, Duration::from_secs(10)) {
                    eprintln!("[proxy] port {proxy_port} did not open; falling back to the sidecar");
                    go_port
                } else {
                    proxy_port
                }
            } else {
                go_port
            };

            build_window(app.handle(), "main", "/workspace", page_port, true)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the codepluse shell");

    app.run(|app_handle, event| {
        if let RunEvent::Exit = event {
            if let Some(state) = app_handle.try_state::<Backend>() {
                if let Ok(mut guard) = state.child.lock() {
                    if let Some(child) = guard.take() {
                        let _ = child.kill();
                    }
                }
            }
        }
    });
}
