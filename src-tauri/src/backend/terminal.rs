//! 终端会话：`server-go/internal/codeg/terminal.go` + `terminal_windows.go` 的
//! Rust 移植，ConPTY 走 kernel32 裸 FFI（等价于 Go 侧引用的
//! UserExistsError/conpty v0.1.4，包括它逐字节→UTF-16 码元的环境块怪癖）。
//!
//! 翻转 8 条：`terminal_spawn/write/resize/kill/list/snapshot`、
//! `get_available_terminal_shells`、`probe_terminal_shell_path`。
//! 画布删除节点原先把 `terminal_kill` 转发回 Go，现在由 mod.rs 在 forward
//! 出口拦截，改道到 [`kill_for_canvas`]。
//!
//! 生命周期与 Go 一致：spawn 起两个线程（读输出 / 等退出），退出时发
//! `terminal://exit/{id}` 事件并修剪退会话表（上限 64）；输出按块发
//! `terminal://output/{id}`，缓冲上限 2 MB、按 rune 边界截断。

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::{mem, ptr};

use serde_json::{json, Value};

use crate::backend::git::resolve;
use crate::backend::{emit_event, goformat, store_file};

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "terminal_spawn"
            | "terminal_write"
            | "terminal_resize"
            | "terminal_kill"
            | "terminal_list"
            | "terminal_snapshot"
            | "get_available_terminal_shells"
            | "probe_terminal_shell_path"
            | "get_system_terminal_settings"
            | "update_system_terminal_settings"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn txt(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Go `number()`：JSON 数值 → int，NaN/超界按 Go 的钳制处理。
fn num(args: &Value, key: &str) -> i64 {
    match args.get(key).and_then(|v| v.as_f64()) {
        Some(f) if f.is_finite() => {
            if f >= 9.223_372_036_854_776e18 {
                i64::MAX
            } else if f <= -9.223_372_036_854_776e18 {
                i64::MIN
            } else {
                f as i64
            }
        }
        _ => 0,
    }
}

// ── shell 解析（terminal.go:terminalShell + exec.LookPath Windows 语义）─────

/// Go `exec.LookPath`：含分隔符按直查处理，否则逐 PATH 目录、无扩展名时按
/// PATHEXT 逐个尝试。找不到时复刻 Go 的错误文案。
fn look_path(name: &str) -> Result<String, String> {
    let executable = |path: &Path| -> bool {
        std::fs::metadata(path)
            .map(|m| m.is_file())
            .unwrap_or(false)
    };
    if name.contains('/') || name.contains('\\') {
        if executable(Path::new(name)) {
            return Ok(name.to_string());
        }
        return Err(format!(
            "exec: \"{name}\": executable file not found in %PATH%"
        ));
    }
    let pathext: Vec<String> = match std::env::var("PATHEXT") {
        Ok(v) if !v.trim().is_empty() => v
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
        _ => [".COM", ".EXE", ".BAT", ".CMD"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(';') {
        let dir = if dir.is_empty() { "." } else { dir };
        let base = Path::new(dir).join(name);
        let has_ext = Path::new(name)
            .extension()
            .map(|e| !e.is_empty())
            .unwrap_or(false);
        if !has_ext {
            for ext in &pathext {
                let candidate = PathBuf::from(format!("{}{}", base.to_string_lossy(), ext));
                if executable(&candidate) {
                    return Ok(candidate.to_string_lossy().to_string());
                }
            }
            continue;
        }
        if executable(&base) {
            return Ok(base.to_string_lossy().to_string());
        }
    }
    Err(format!(
        "exec: \"{name}\": executable file not found in %PATH%"
    ))
}

/// Go `filepath.Base`（\ 与 / 都算分隔符）。
fn shell_base(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    match normalized.rsplit('/').next() {
        Some(base) if !base.is_empty() => base.to_string(),
        // 以分隔符结尾的路径：Go 回退到原样（对真实 shell 路径不出现）。
        _ => path.to_string(),
    }
}

/// terminal.go:terminalShell。错误文案逐条复刻。
fn terminal_shell(shell: &str) -> Result<String, String> {
    let resolved = if shell.is_empty() {
        let mut found = None;
        for candidate in ["pwsh.exe", "powershell.exe", "cmd.exe"] {
            if let Ok(path) = look_path(candidate) {
                found = Some(path);
                break;
            }
        }
        match found {
            Some(path) => path,
            None => return Err("找不到可用的终端 Shell".into()),
        }
    } else {
        if shell.contains('\r') || shell.contains('\n') || shell.contains('\0') {
            return Err("Shell 路径无效".into());
        }
        look_path(shell)?
    };
    // 自定义路径只放行已知交互 shell，防止把任意可执行文件当 shell 拉起。
    match shell_base(&resolved).to_lowercase().as_str() {
        "pwsh.exe" | "pwsh" | "powershell.exe" | "powershell" | "cmd.exe" | "cmd" | "bash.exe"
        | "bash" | "wsl.exe" | "sh" | "zsh" => Ok(resolved),
        other => Err(format!("不支持的自定义 Shell：{other}")),
    }
}

fn saved_default_shell(root: &Path) -> Option<String> {
    let doc = store_file::read_json(&root.join(".data").join("codeg-workspace.json")).ok()??;
    doc.get("settings")?
        .get("get_system_terminal_settings")?
        .get("default_shell")?
        .as_str()
        .map(|s| s.to_string())
}

fn system_terminal_settings(root: &Path) -> Result<Value, String> {
    let path = root.join(".data").join("codeg-workspace.json");
    let doc = store_file::read_json(&path)?.unwrap_or_else(|| json!({}));
    let saved = doc
        .get("settings")
        .and_then(|settings| settings.get("get_system_terminal_settings"));
    Ok(json!({
        "default_shell": saved.and_then(|value| value.get("default_shell")).cloned().unwrap_or(Value::Null),
        "colorize_command_output": saved
            .and_then(|value| value.get("colorize_command_output"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }))
}

fn update_system_terminal_settings(root: &Path, args: &Value) -> Result<Value, String> {
    let settings = args
        .get("settings")
        .and_then(Value::as_object)
        .ok_or("终端设置无效")?;
    let default_shell = match settings.get("default_shell") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(shell)) => json!(shell),
        _ => return Err("默认 Shell 设置无效".into()),
    };
    let colorize_command_output = settings
        .get("colorize_command_output")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let updated = json!({
        "default_shell": default_shell,
        "colorize_command_output": colorize_command_output,
    });

    let path = root.join(".data").join("codeg-workspace.json");
    let _lock = store_file::lock(&path)?;
    let mut doc = store_file::read_json(&path)?.unwrap_or_else(|| json!({}));
    let doc_object = doc.as_object_mut().ok_or("工作区设置格式无效")?;
    let workspace_settings = doc_object.entry("settings").or_insert_with(|| json!({}));
    let workspace_settings = workspace_settings
        .as_object_mut()
        .ok_or("工作区设置格式无效")?;
    workspace_settings.insert("get_system_terminal_settings".into(), updated.clone());
    store_file::write_json(&path, &doc)?;
    Ok(updated)
}

fn shell_options(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let mut shell = txt(args, "path");
    if command == "get_available_terminal_shells" {
        if let Some(saved) = saved_default_shell(root) {
            shell = saved;
        }
    }
    let resolved = terminal_shell(&shell)?;
    let mut options = vec![json!({
        "id": "default",
        "label_key": "terminalSystemDefault",
        "value": Value::Null,
        "exists": true,
        "accepts_custom_path": false,
    })];
    for name in ["pwsh.exe", "powershell.exe", "cmd.exe"] {
        // Go：LookPath 失败时 path 是 ""，value 始终是字符串。
        let (value, exists) = match look_path(name) {
            Ok(p) => (json!(p), true),
            Err(_) => (json!(""), false),
        };
        let label = match name {
            "pwsh.exe" => "terminalPowerShell7",
            "powershell.exe" => "terminalWindowsPowerShell",
            _ => "terminalCmd",
        };
        options.push(json!({
            "id": name,
            "label_key": label,
            "value": value,
            "exists": exists,
            "accepts_custom_path": false,
        }));
    }
    options.push(json!({
        "id": "custom",
        "label_key": "terminalShellCustom",
        "value": shell,
        "exists": true,
        "accepts_custom_path": true,
    }));
    Ok(json!({"options": options, "resolved_shell": resolved}))
}

// ── ConPTY FFI（等价 UserExistsError/conpty v0.1.4）─────────────────────────

type HANDLE = *mut core::ffi::c_void;
type HPCON = *mut core::ffi::c_void;

const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const WAIT_TIMEOUT: u32 = 0x0000_0102;
const STILL_ACTIVE: u32 = 259;

#[repr(C)]
struct Coord {
    x: i16,
    y: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    cb_reserved2: u16,
    lp_reserved2: *mut u8,
    h_std_input: HANDLE,
    h_std_output: HANDLE,
    h_std_error: HANDLE,
}

#[repr(C)]
struct StartupInfoExW {
    startup_info: StartupInfoW,
    attribute_list: *mut core::ffi::c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessInformation {
    process: HANDLE,
    thread: HANDLE,
    process_id: u32,
    thread_id: u32,
}

extern "system" {
    fn CreatePipe(
        read_pipe: *mut HANDLE,
        write_pipe: *mut HANDLE,
        attrs: *const core::ffi::c_void,
        size: u32,
    ) -> i32;
    fn CreatePseudoConsole(
        size: Coord,
        input: HANDLE,
        output: HANDLE,
        flags: u32,
        hpc: *mut HPCON,
    ) -> i32;
    fn ResizePseudoConsole(hpc: HPCON, size: Coord) -> i32;
    fn ClosePseudoConsole(hpc: HPCON);
    fn InitializeProcThreadAttributeList(
        list: *mut core::ffi::c_void,
        count: u32,
        flags: u32,
        size: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        list: *mut core::ffi::c_void,
        flags: u32,
        attribute: usize,
        value: *const core::ffi::c_void,
        size: usize,
        previous: *mut *mut core::ffi::c_void,
        ret: *mut *mut core::ffi::c_void,
    ) -> i32;
    fn CreateProcessW(
        application: *const u16,
        command_line: *mut u16,
        process_attrs: *const core::ffi::c_void,
        thread_attrs: *const core::ffi::c_void,
        inherit_handles: i32,
        flags: u32,
        environment: *const core::ffi::c_void,
        current_directory: *const u16,
        startup_info: *const StartupInfoW,
        info: *mut ProcessInformation,
    ) -> i32;
    fn WaitForSingleObject(handle: HANDLE, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(handle: HANDLE, exit_code: *mut u32) -> i32;
    fn CloseHandle(handle: HANDLE) -> i32;
    fn GetLastError() -> u32;
    fn FormatMessageW(
        flags: u32,
        source: *const core::ffi::c_void,
        message_id: u32,
        language_id: u32,
        buffer: *mut u16,
        size: u32,
        arguments: *mut *mut u8,
    ) -> u32;
}

const FORMAT_MESSAGE_FROM_SYSTEM: u32 = 0x0000_1000;

/// Go 侧 `syscall.Errno` 的错误文案（conpty 库把 CreateProcess 的底层消息
/// 原样上抛）。
fn win32_message(code: u32) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM,
            ptr::null(),
            code,
            0,
            buf.as_mut_ptr(),
            buf.len() as u32,
            ptr::null_mut(),
        )
    };
    if n == 0 {
        return format!("错误 {code}");
    }
    String::from_utf16_lossy(&buf[..n as usize])
        .trim_end()
        .to_string()
}

fn wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// conpty 库的 `createEnvBlock`：把 UTF-8 字节逐字节当 UTF-16 码元编码
/// （非 ASCII 环境值按同样方式展开——库的既有怪癖，逐行为对齐）。
fn env_block() -> Vec<u16> {
    let mut raw = String::new();
    for (k, v) in std::env::vars_os() {
        raw.push_str(&k.to_string_lossy());
        raw.push('=');
        raw.push_str(&v.to_string_lossy());
        raw.push('\0');
    }
    // Go: append(os.Environ(), "TERM=xterm-256color") —— 重复键照加。
    raw.push_str("TERM=xterm-256color\0");
    raw.push('\0');
    raw.bytes().map(|b| b as u16).collect()
}

/// spawn 出的底层句柄包：close 时一次性全部释放。
struct PtyHandles {
    hpc: HPCON,
    pty_in: RawHandle,
    pty_out: RawHandle,
    cmd_in: File,
    process: RawHandle,
    thread: RawHandle,
}

// SAFETY：PtyHandles 里的裸句柄是数值而非借用，Windows 句柄本就允许跨线程
// 使用；会话被 Arc 在读/等/命令线程间移动正是预期。
unsafe impl Send for PtyHandles {}

impl PtyHandles {
    /// Go `ConPty.Close`：先 ClosePseudoConsole（杀掉附着进程），再关句柄。
    /// 进程句柄一并关掉是库的行为——等待线程下一次 WaitForSingleObject 会
    /// 得到 WAIT_FAILED，于是 exit 事件的 data 为空串，与 Go 观察一致。
    fn close(&mut self) {
        unsafe { ClosePseudoConsole(self.hpc) };
        unsafe { CloseHandle(self.pty_in as HANDLE) };
        unsafe { CloseHandle(self.pty_out as HANDLE) };
        unsafe { CloseHandle(self.process as HANDLE) };
        unsafe { CloseHandle(self.thread as HANDLE) };
        // cmd_in 的 File 随 self 释放。
    }
}

fn close_pipes_early(pty_in: HANDLE, pty_out: HANDLE, cmd_in: HANDLE, cmd_out: HANDLE) {
    unsafe { CloseHandle(pty_in) };
    unsafe { CloseHandle(pty_out) };
    unsafe { CloseHandle(cmd_in) };
    unsafe { CloseHandle(cmd_out) };
}

fn start_native_pty(
    shell: &str,
    cwd: &str,
    cols: i32,
    rows: i32,
) -> Result<(PtyHandles, File), String> {
    let mut command = format!("\"{shell}\"");
    let lower = shell.to_lowercase();
    if lower.contains("powershell") || shell_base(shell).eq_ignore_ascii_case("pwsh.exe") {
        command += " -NoLogo";
    }

    let mut pty_in: HANDLE = ptr::null_mut();
    let mut cmd_in: HANDLE = ptr::null_mut();
    if unsafe { CreatePipe(&mut pty_in, &mut cmd_in, ptr::null(), 0) } == 0 {
        return Err("CreatePipe: 句柄创建失败".into());
    }
    let mut cmd_out: HANDLE = ptr::null_mut();
    let mut pty_out: HANDLE = ptr::null_mut();
    if unsafe { CreatePipe(&mut cmd_out, &mut pty_out, ptr::null(), 0) } == 0 {
        unsafe { CloseHandle(pty_in) };
        unsafe { CloseHandle(cmd_in) };
        return Err("CreatePipe: 句柄创建失败".into());
    }

    let mut hpc: HPCON = ptr::null_mut();
    let status = unsafe {
        CreatePseudoConsole(
            Coord {
                x: cols as i16,
                y: rows as i16,
            },
            pty_in,
            pty_out,
            0,
            &mut hpc,
        )
    };
    if status != 0 {
        close_pipes_early(pty_in, pty_out, cmd_in, cmd_out);
        return Err(format!(
            "CreatePseudoConsole() failed with status 0x{status:x}"
        ));
    }

    // 属性列表挂 pseudoconsole，与 conpty 库同一流程（库不做 Delete 清理）。
    let mut size: usize = 0;
    unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size) };
    let list = vec![0u8; size];
    let list_ptr = list.as_ptr() as *mut core::ffi::c_void;
    if unsafe { InitializeProcThreadAttributeList(list_ptr, 1, 0, &mut size) } == 0 {
        close_pipes_early(pty_in, pty_out, cmd_in, cmd_out);
        unsafe { ClosePseudoConsole(hpc) };
        return Err("InitializeProcThreadAttributeList failed".into());
    }
    if unsafe {
        UpdateProcThreadAttribute(
            list_ptr,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            hpc as *const core::ffi::c_void,
            mem::size_of::<HPCON>(),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    } == 0
    {
        close_pipes_early(pty_in, pty_out, cmd_in, cmd_out);
        unsafe { ClosePseudoConsole(hpc) };
        return Err("UpdateProcThreadAttribute failed".into());
    }

    let cmd_line = wide(&command);
    let dir = wide(cwd);
    let env = env_block();
    let startup = StartupInfoExW {
        startup_info: StartupInfoW {
            cb: (mem::size_of::<StartupInfoW>() + mem::size_of::<*const u16>()) as u32,
            reserved: ptr::null_mut(),
            desktop: ptr::null_mut(),
            title: ptr::null_mut(),
            x: 0,
            y: 0,
            x_size: 0,
            y_size: 0,
            x_count_chars: 0,
            y_count_chars: 0,
            fill_attribute: 0,
            flags: STARTF_USESTDHANDLES,
            show_window: 0,
            cb_reserved2: 0,
            lp_reserved2: ptr::null_mut(),
            h_std_input: ptr::null_mut(),
            h_std_output: ptr::null_mut(),
            h_std_error: ptr::null_mut(),
        },
        attribute_list: list_ptr,
    };
    let mut info = ProcessInformation {
        process: ptr::null_mut(),
        thread: ptr::null_mut(),
        process_id: 0,
        thread_id: 0,
    };
    let ok = unsafe {
        CreateProcessW(
            ptr::null(),
            cmd_line.as_ptr() as *mut u16,
            ptr::null(),
            ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            env.as_ptr() as *const core::ffi::c_void,
            dir.as_ptr(),
            &startup.startup_info,
            &mut info,
        )
    };
    if ok == 0 {
        let code = unsafe { GetLastError() };
        close_pipes_early(pty_in, pty_out, cmd_in, cmd_out);
        unsafe { ClosePseudoConsole(hpc) };
        return Err(format!(
            "Failed to create console process: {}",
            win32_message(code)
        ));
    }

    Ok((
        PtyHandles {
            hpc,
            pty_in: pty_in as RawHandle,
            pty_out: pty_out as RawHandle,
            cmd_in: unsafe { File::from_raw_handle(cmd_in as RawHandle) },
            process: info.process as RawHandle,
            thread: info.thread as RawHandle,
        },
        // cmd_out 是输出管道的读端，交给读线程独占（写端 pty_out 已交给 ConPTY）。
        unsafe { File::from_raw_handle(cmd_out as RawHandle) },
    ))
}

// ── 会话表 ──────────────────────────────────────────────────────────────────

struct Inner {
    tid: String,
    generation: String,
    title: String,
    output: String,
    seq: u64,
    alive: bool,
    exit_code: Option<u32>,
    cols: i64,
    rows: i64,
}

struct TerminalSession {
    inner: Mutex<Inner>,
    /// Go 的 writeMu：序列化 pty 写入。
    write_mu: Mutex<()>,
    handles: Mutex<Option<PtyHandles>>,
    close_once: Once,
    process: OnceLock<SendPtr>,
    /// 读线程收尾时发送（Go 的 readDone），等待线程收下。
    read_done: Mutex<Option<Sender<()>>>,
    hpc: OnceLock<SendPtr>,
}

/// 裸句柄的线程安全包装：Windows 句柄本身可跨线程使用（ ConPTY 的句柄
/// 会被读线程/等待线程/命令线程分别触碰）。
#[derive(Clone, Copy)]
struct SendPtr(*mut core::ffi::c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

impl SendPtr {
    fn raw(&self) -> *mut core::ffi::c_void {
        self.0
    }
}

fn sessions() -> &'static Mutex<HashMap<String, Arc<TerminalSession>>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, Arc<TerminalSession>>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Go `maxRetainedTerminals`：退会话表上限 64。
const MAX_RETAINED_TERMINALS: usize = 64;

fn prune(keep: &str) {
    let mut map = sessions().lock().unwrap();
    if map.len() <= MAX_RETAINED_TERMINALS {
        return;
    }
    let ids: Vec<String> = map.keys().cloned().collect();
    for id in ids {
        if map.len() <= MAX_RETAINED_TERMINALS {
            return;
        }
        if id == keep {
            continue;
        }
        let alive = map.get(&id).unwrap().inner.lock().unwrap().alive;
        if !alive {
            map.remove(&id);
        }
    }
}

/// 关闭会话（幂等）：Go `TerminalSession.close`。
fn close_session(t: &TerminalSession) {
    t.close_once.call_once(|| {
        if let Some(h) = t.handles.lock().unwrap().as_mut() {
            h.close();
        }
    });
}

fn spawn_session(tid: String, shell: &str, cwd: &str, initial: &str) -> Result<(), String> {
    let (pty, cmd_out) = start_native_pty(shell, cwd, 120, 30)?;
    let process_raw = pty.process;
    let hpc = pty.hpc;
    let (done_tx, done_rx): (Sender<()>, Receiver<()>) = channel();
    let session = Arc::new(TerminalSession {
        inner: Mutex::new(Inner {
            tid: tid.clone(),
            generation: goformat::hex_id(),
            title: shell_base(shell),
            output: String::new(),
            seq: 0,
            alive: true,
            exit_code: None,
            cols: 120,
            rows: 30,
        }),
        write_mu: Mutex::new(()),
        handles: Mutex::new(Some(pty)),
        close_once: Once::new(),
        process: OnceLock::new(),
        read_done: Mutex::new(Some(done_tx)),
        hpc: OnceLock::new(),
    });
    let _ = session.process.set(SendPtr(process_raw));
    let _ = session.hpc.set(SendPtr(hpc));

    // 先登记再起线程：output/exit 事件只有在会话进表后才对得上门牌。
    sessions()
        .lock()
        .unwrap()
        .insert(tid.clone(), Arc::clone(&session));

    // 读输出线程：8 KB 块、rune 边界、2 MB 截断，逐块发 output 事件。
    let reader = Arc::clone(&session);
    std::thread::spawn(move || read_loop(reader, cmd_out));

    // 等退出线程：轮询进程句柄 → 关闭 → 等读线程收尾 → 发 exit 事件并修剪。
    let waiter = Arc::clone(&session);
    let wait_tid = tid.clone();
    std::thread::spawn(move || {
        let process = waiter
            .process
            .get()
            .map(SendPtr::raw)
            .unwrap_or(ptr::null_mut());
        loop {
            let r = unsafe { WaitForSingleObject(process, 1000) };
            if r != WAIT_TIMEOUT {
                break;
            }
        }
        let mut code: u32 = STILL_ACTIVE;
        let ok = unsafe { GetExitCodeProcess(process, &mut code) } != 0;
        close_session(&waiter);
        let _ = done_rx.recv();
        let mut exit_code: Option<u32> = None;
        let generation;
        {
            let mut inner = waiter.inner.lock().unwrap();
            inner.alive = false;
            if ok {
                inner.exit_code = Some(code);
                exit_code = Some(code);
            }
            generation = inner.generation.clone();
        }
        let exit_text = match exit_code {
            Some(c) => c.to_string(),
            None => String::new(),
        };
        emit_event(
            &format!("terminal://exit/{wait_tid}"),
            json!({
                "terminal_id": wait_tid,
                "data": exit_text,
                "seq": 0,
                "generation": generation,
            }),
        );
        prune(&wait_tid);
    });

    if !initial.is_empty() {
        let writer = Arc::clone(&session);
        let text = format!("{initial}\r").into_bytes();
        std::thread::spawn(move || {
            let _guard = writer.write_mu.lock().unwrap();
            if let Some(pty) = writer.handles.lock().unwrap().as_mut() {
                let _ = (&pty.cmd_in).write_all(&text);
            }
        });
    }
    Ok(())
}

/// 读输出线程：Go `readTerminal`。
fn read_loop(t: Arc<TerminalSession>, mut out: File) {
    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let (n, err) = match out.read(&mut buf) {
            Ok(n) => (n, n == 0),
            Err(_) => (0, true),
        };
        pending.extend_from_slice(&buf[..n]);
        let mut end = 0usize;
        while end < pending.len() {
            match decode_go(&pending[end..]) {
                Some(size) => end += size,
                None => break, // 需要更多字节
            }
        }
        if end > 0 {
            let chunk = go_lossy_bytes(&pending[..end]);
            pending.drain(..end);
            let (seq, generation, tid) = {
                let mut inner = t.inner.lock().unwrap();
                inner.output.push_str(&chunk);
                if inner.output.len() > 2 << 20 {
                    let mut cut = inner.output.len() - (2 << 20);
                    let bytes = inner.output.as_bytes();
                    while cut < bytes.len() && (bytes[cut] & 0xC0) == 0x80 {
                        cut += 1;
                    }
                    inner.output.drain(..cut);
                }
                inner.seq += 1;
                (inner.seq, inner.generation.clone(), inner.tid.clone())
            };
            // emit 在锁外（Go 注释：不能在持有会话锁时拿事件总线锁）。
            emit_event(
                &format!("terminal://output/{tid}"),
                json!({
                    "terminal_id": tid,
                    "data": chunk,
                    "seq": seq,
                    "generation": generation,
                }),
            );
        }
        if err {
            break;
        }
    }
    // readDone：读线程已结束。
    if let Some(tx) = t.read_done.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

/// Go `utf8.DecodeRune` 的尺寸语义：合法序列给字节数，非法序列一律消费
/// 1 字节，前缀不完整（还可能补全）返回 None。
fn decode_go(bytes: &[u8]) -> Option<usize> {
    let b0 = bytes[0];
    if b0 < 0x80 {
        return Some(1);
    }
    if b0 < 0xC2 {
        return Some(1); // 散置续字节 0x80-0xBF、过长编码 0xC0/0xC1 → RuneError
    }
    let (need, lo, hi) = match b0 {
        0xC2..=0xDF => (2, 0x80u8, 0xBFu8),
        0xE0 => (3, 0xA0, 0xBF),
        0xE1..=0xEC => (3, 0x80, 0xBF),
        0xED => (3, 0x80, 0x9F),
        0xEE..=0xEF => (3, 0x80, 0xBF),
        0xF0 => (4, 0x90, 0xBF),
        0xF1..=0xF3 => (4, 0x80, 0xBF),
        0xF4 => (4, 0x80, 0x8F),
        _ => return Some(1), // 0xF5..0xFF
    };
    if bytes.len() < 2 {
        return None;
    }
    if bytes[1] < lo || bytes[1] > hi {
        return Some(1);
    }
    if need == 2 {
        return Some(2);
    }
    if bytes.len() < 3 {
        return None;
    }
    if bytes[2] < 0x80 || bytes[2] > 0xBF {
        return Some(1);
    }
    if need == 3 {
        return Some(3);
    }
    if bytes.len() < 4 {
        return None;
    }
    if bytes[3] < 0x80 || bytes[3] > 0xBF {
        return Some(1);
    }
    Some(4)
}

/// Go `strings.ToValidUTF8`：每个非法字节替换为一个 U+FFFD。
pub(crate) fn go_lossy_bytes(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                break;
            }
            Err(e) => {
                let (valid, remainder) = rest.split_at(e.valid_up_to());
                if let Ok(s) = std::str::from_utf8(valid) {
                    out.push_str(s);
                }
                let skip = e.error_len().unwrap_or(1);
                for _ in 0..skip {
                    out.push('\u{FFFD}');
                }
                rest = &remainder[skip..];
            }
        }
    }
    out
}

// ── 命令分发（terminal.go:terminalCommand）──────────────────────────────────

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    if command == "get_system_terminal_settings" {
        return system_terminal_settings(root);
    }
    if command == "update_system_terminal_settings" {
        return update_system_terminal_settings(root, args);
    }
    let tid = txt(args, "terminalId");
    if command == "get_available_terminal_shells" || command == "probe_terminal_shell_path" {
        return shell_options(root, command, args);
    }
    if command == "terminal_spawn" {
        let cwd = resolve(root, &txt(args, "workingDir"))?;
        let mut shell = txt(args, "shell");
        if shell.is_empty() {
            if let Some(saved) = saved_default_shell(root) {
                shell = saved;
            }
        }
        let shell = terminal_shell(&shell)?;
        let mut tid = tid;
        if tid.is_empty() {
            tid = goformat::hex_id();
        }
        {
            let map = sessions().lock().unwrap();
            if let Some(old) = map.get(&tid) {
                if old.inner.lock().unwrap().alive {
                    return Ok(json!(tid));
                }
            }
        }
        // 锁已释放：spawn_session 内部完成登记（避免重入死锁）。
        let initial = txt(args, "initialCommand");
        spawn_session(tid.clone(), &shell, &cwd.to_string_lossy(), &initial)?;
        return Ok(json!(tid));
    }

    let existing = sessions().lock().unwrap().get(&tid).cloned();
    match command {
        "terminal_list" => {
            let map = sessions().lock().unwrap();
            let out: Vec<Value> = map
                .values()
                .map(|t| {
                    let inner = t.inner.lock().unwrap();
                    json!({"id": inner.tid, "title": inner.title})
                })
                .collect();
            Ok(Value::Array(out))
        }
        "terminal_snapshot" => match existing {
            None => Ok(json!({
                "exists": false, "alive": false, "data": "", "seq": 0,
                "generation": Value::Null, "exit_code": Value::Null,
                "cols": Value::Null, "rows": Value::Null,
            })),
            Some(t) => {
                let inner = t.inner.lock().unwrap();
                Ok(json!({
                    "exists": true,
                    "alive": inner.alive,
                    "data": inner.output,
                    "seq": inner.seq,
                    "generation": inner.generation,
                    "exit_code": inner.exit_code,
                    "cols": inner.cols,
                    "rows": inner.rows,
                }))
            }
        },
        "terminal_kill" => {
            if let Some(t) = existing {
                // Go：keepOutput 为 true 时保留会话表项（快照仍可读），否则删除。
                if args.get("keepOutput").and_then(|v| v.as_bool()) != Some(true) {
                    sessions().lock().unwrap().remove(&tid);
                }
                close_session(&t);
            }
            Ok(Value::Null)
        }
        "terminal_write" => {
            let t = existing.ok_or("终端不存在")?;
            let data = txt(args, "data");
            let _guard = t.write_mu.lock().unwrap();
            let mut slot = t.handles.lock().unwrap();
            let pty = slot.as_mut().ok_or("终端不存在")?;
            // Go：写失败返回错误，无返回值。
            pty.cmd_in
                .write_all(data.as_bytes())
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "terminal_resize" => {
            let t = existing.ok_or("终端不存在")?;
            let cols = num(args, "cols");
            let rows = num(args, "rows");
            if !(1..=1000).contains(&cols) || !(1..=1000).contains(&rows) {
                return Err("终端尺寸无效".into());
            }
            let hpc = t.hpc.get().map(|p| p.raw()).unwrap_or(ptr::null_mut());
            let status = unsafe {
                ResizePseudoConsole(
                    hpc,
                    Coord {
                        x: cols as i16,
                        y: rows as i16,
                    },
                )
            };
            if status != 0 {
                return Err(format!(
                    "ResizePseudoConsole failed with status 0x{status:x}"
                ));
            }
            let mut inner = t.inner.lock().unwrap();
            inner.cols = cols;
            inner.rows = rows;
            Ok(Value::Null)
        }
        _ => Err(format!("未知终端接口：{command}")),
    }
}

/// 把刚 spawn 的会话登记进表。spawn_session 已经持有 Arc，这里再从注册表
/// 取回——spawn_session 里插入而不是事后查找，避免并发 spawn 丢会话。
fn session_for(tid: &str) -> Arc<TerminalSession> {
    let map = sessions().lock().unwrap();
    Arc::clone(map.get(tid).expect("刚插入的会话必须在表里"))
}

/// 画布删除节点用的 terminal_kill（等价 keepOutput 缺省 → 删除并关闭）。
pub(crate) fn kill_for_canvas(terminal_id: &str) {
    let existing = sessions().lock().unwrap().get(terminal_id).cloned();
    if let Some(t) = existing {
        sessions().lock().unwrap().remove(terminal_id);
        close_session(&t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("codepluse-term-{tag}-{}", goformat::hex_id()));
        std::fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    #[test]
    fn shell_resolution_mirrors_go() {
        // 空串回退默认候选（本机必有 powershell 或 cmd）。
        let resolved = terminal_shell("").unwrap();
        let base = shell_base(&resolved).to_lowercase();
        assert!(
            base.starts_with("pwsh") || base.starts_with("powershell") || base == "cmd.exe",
            "unexpected: {resolved}"
        );
        // 白名单外拒绝。
        let err = terminal_shell("notepad.exe").unwrap_err();
        assert_eq!(err, "不支持的自定义 Shell：notepad.exe");
        // 含控制字符拒绝。
        assert_eq!(terminal_shell("a\r\nb").unwrap_err(), "Shell 路径无效");
        // LookPath 找不到的文案。
        assert_eq!(
            look_path("definitely-not-here-9x.exe").unwrap_err(),
            "exec: \"definitely-not-here-9x.exe\": executable file not found in %PATH%"
        );
    }

    #[test]
    fn shell_options_shape() {
        let root = temp_root("opts");
        let value =
            shell_options(&root, "get_available_terminal_shells", &json!({"path": ""})).unwrap();
        let options = value["options"].as_array().unwrap();
        assert_eq!(options[0]["id"], json!("default"));
        assert_eq!(options[0]["label_key"], json!("terminalSystemDefault"));
        assert_eq!(options.last().unwrap()["id"], json!("custom"));
        assert!(value["resolved_shell"].as_str().is_some());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_terminal_snapshot_shape() {
        let root = temp_root("missing");
        let value = run(&root, "terminal_snapshot", &json!({"terminalId": "ghost"})).unwrap();
        assert_eq!(
            value,
            json!({
                "exists": false, "alive": false, "data": "", "seq": 0,
                "generation": Value::Null, "exit_code": Value::Null,
                "cols": Value::Null, "rows": Value::Null,
            })
        );
        // 不存在的终端 write/resize 报错文案与 Go 一致。
        assert_eq!(
            run(
                &root,
                "terminal_write",
                &json!({"terminalId": "ghost", "data": "x"})
            )
            .unwrap_err(),
            "终端不存在"
        );
        assert_eq!(
            run(
                &root,
                "terminal_resize",
                &json!({"terminalId": "ghost", "cols": 80, "rows": 24})
            )
            .unwrap_err(),
            "终端不存在"
        );
        // 尺寸越界：Go 先查会话存在性，missing 终端永远是「终端不存在」。
        assert_eq!(
            run(
                &root,
                "terminal_resize",
                &json!({"terminalId": "ghost", "cols": 0, "rows": 24})
            )
            .unwrap_err(),
            "终端不存在"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn conpty_session_round_trip() {
        // 与 Go 的 TestTerminalIsRealInteractivePTYAndRetainsExitOutput 同型：
        // 真 ConPTY 会话跑 cmd.exe，写命令、等退出码。
        let root = temp_root("pty");
        let cwd = root.to_string_lossy().to_string();
        spawn_session("test-pty".into(), "cmd.exe", &cwd, "").unwrap();
        run(
            &root,
            "terminal_write",
            &json!({"terminalId": "test-pty", "data": "exit /b 7\r"}),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
        loop {
            let snap = run(
                &root,
                "terminal_snapshot",
                &json!({"terminalId": "test-pty"}),
            )
            .unwrap();
            if snap["alive"] == json!(false) {
                assert_eq!(snap["exit_code"], json!(7), "{snap}");
                assert!(snap["generation"].as_str().is_some(), "{snap}");
                assert!(snap["seq"].as_i64().unwrap_or(0) >= 1, "{snap}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "PTY did not exit: {snap}"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        run(&root, "terminal_kill", &json!({"terminalId": "test-pty"})).unwrap();
        let snap = run(
            &root,
            "terminal_snapshot",
            &json!({"terminalId": "test-pty"}),
        )
        .unwrap();
        assert_eq!(snap["exists"], json!(false));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn go_lossy_replaces_each_invalid_byte() {
        // 3 字节中文字符被截 2 字节 → Go 语义是 2 个 FFFD。
        let bytes: &[u8] = &[0xE4, 0xB8]; // "中" 的前两字节
        assert_eq!(go_lossy_bytes(bytes), "\u{FFFD}\u{FFFD}");
        assert_eq!(go_lossy_bytes("中文".as_bytes()), "中文");
    }
}
