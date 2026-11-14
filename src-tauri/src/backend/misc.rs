//! 杂项单条命令：`pick_directory`（server.go:518 → pickdir.go）。
//!
//! 用 PowerShell 的 FolderBrowserDialog 弹出系统文件夹选择窗口，输出强制
//! UTF-8 避免中文路径经 stdout 乱码。对话框是模态的——用户可以停几分钟，
//! 所以分发层必须把它放进阻塞池。

use std::path::Path;

use serde_json::Value;

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "pick_directory" | "create_folder_directory" | "open_in_code" | "detect_package_manager"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    let _ = (root, args);
    Some(run(root, command, args))
}

const PICK_SCRIPT: &str = r#"[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName System.Windows.Forms
$owner = New-Object System.Windows.Forms.Form
$owner.TopMost = $true
$owner.ShowInTaskbar = $false
$owner.WindowState = 'Minimized'
$owner.Show()
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = 'Select workspace folder'
$dialog.ShowNewFolderButton = $true
$dialog.RootFolder = [System.Environment+SpecialFolder]::MyComputer
$result = $dialog.ShowDialog($owner)
$owner.Close()
if ($result -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($dialog.SelectedPath) }"#;

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    if command == "detect_package_manager" {
        return detect_package_manager(args);
    }
    if command == "create_folder_directory" {
        let raw = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if raw.is_empty() {
            return Err("目录路径不能为空".into());
        }
        let path = Path::new(raw);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        std::fs::create_dir_all(&path).map_err(|e| format!("创建目录失败：{e}"))?;
        return Ok(Value::Null);
    }
    if command == "open_in_code" {
        let raw = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if raw.is_empty() {
            return Err("路径不能为空".into());
        }
        let path = Path::new(raw);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        if !path.exists() {
            return Err("要打开的路径不存在".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let local = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);
            let program = local
                .into_iter()
                .flat_map(|p| {
                    [
                        p.join("Programs/Microsoft VS Code/Code.exe"),
                        p.join("Programs/Microsoft VS Code Insiders/Code - Insiders.exe"),
                    ]
                })
                .find(|p| p.is_file());
            let mut cmd = if let Some(exe) = program {
                std::process::Command::new(exe)
            } else {
                std::process::Command::new("code.exe")
            };
            cmd.arg(&path)
                .creation_flags(0x0800_0000)
                .spawn()
                .map_err(|e| format!("无法启动 Visual Studio Code：{e}"))?;
        }
        #[cfg(not(windows))]
        {
            std::process::Command::new("code")
                .arg(&path)
                .spawn()
                .map_err(|e| format!("无法启动 Visual Studio Code：{e}"))?;
        }
        return Ok(Value::Null);
    }
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    // CREATE_NO_WINDOW：Go 的 exec.Command.Output() 走 HideWindow 语义。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", PICK_SCRIPT])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            // Go：exec.Command().Output() 找不到可执行文件时是 LookPath 错误。
            if e.kind() == std::io::ErrorKind::NotFound {
                "无法打开系统文件夹选择器：exec: \"powershell.exe\": executable file not found in %PATH%".to_string()
            } else {
                format!("无法打开系统文件夹选择器：{e}")
            }
        })?;
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr);
    }
    let status = child
        .wait()
        .map_err(|e| format!("无法打开系统文件夹选择器：{e}"))?;
    if !status.success() {
        // Go 的 Output()：非零退出 → "exit status N"。
        let code = status.code().unwrap_or(1);
        return Err(format!("无法打开系统文件夹选择器：exit status {code}"));
    }
    let _ = stderr;
    let text = String::from_utf8_lossy(&stdout).trim().to_string();
    Ok(Value::String(text))
}

fn detect_package_manager(args: &Value) -> Result<Value, String> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !matches!(name, "npm" | "pnpm" | "yarn" | "bun") {
        return Err("不支持的包管理器；可选 npm、pnpm、yarn 或 bun".into());
    }
    #[cfg(windows)]
    let output = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd.exe")
            .args(["/d", "/c", name, "--version"])
            .creation_flags(0x0800_0000)
            .output()
    };
    #[cfg(not(windows))]
    let output = std::process::Command::new(name).arg("--version").output();
    let output = output.ok();
    let version = output
        .as_ref()
        .filter(|result| result.status.success())
        .map(|result| String::from_utf8_lossy(&result.stdout).trim().to_string())
        .filter(|version| !version.is_empty());
    Ok(serde_json::json!({
        "name": name,
        "installed": version.is_some(),
        "version": version,
    }))
}
