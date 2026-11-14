//! 杂项单条命令：`pick_directory`（server.go:518 → pickdir.go）。
//!
//! 用 PowerShell 的 FolderBrowserDialog 弹出系统文件夹选择窗口，输出强制
//! UTF-8 避免中文路径经 stdout 乱码。对话框是模态的——用户可以停几分钟，
//! 所以分发层必须把它放进阻塞池。

use std::path::Path;

use serde_json::Value;

pub fn owns(command: &str) -> bool {
    matches!(command, "pick_directory")
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    let _ = (root, args);
    Some(run(command))
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

fn run(_command: &str) -> Result<Value, String> {
    use std::io::Read;
    use std::process::Command;
    use std::os::windows::process::CommandExt;
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
    let status = child.wait().map_err(|e| format!("无法打开系统文件夹选择器：{e}"))?;
    if !status.success() {
        // Go 的 Output()：非零退出 → "exit status N"。
        let code = status.code().unwrap_or(1);
        return Err(format!("无法打开系统文件夹选择器：exit status {code}"));
    }
    let _ = stderr;
    let text = String::from_utf8_lossy(&stdout).trim().to_string();
    Ok(Value::String(text))
}
