package codeg

import (
	"fmt"
	"os/exec"
	"strings"
)

// pickDirectory 弹出系统文件夹选择窗口（原生资源管理器对话框），把选中的
// 绝对路径返回给前端。服务与用户在同一桌面会话时窗口可见；用户取消时返回
// 空字符串。
//
// 用 PowerShell 的 FolderBrowserDialog 而不是自绘目录树：本项目的 Web 部署
// 跑在本机，用户期望点一下就看到 Windows 的选择窗口。输出强制 UTF-8，避免
// 中文路径经 stdout 变成乱码。
func (s *Server) pickDirectory() (any, error) {
	const script = `[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
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
if ($result -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($dialog.SelectedPath) }`
	out, err := exec.Command("powershell.exe", "-NoProfile", "-STA", "-Command", script).Output()
	if err != nil {
		return nil, fmt.Errorf("无法打开系统文件夹选择器：%v", err)
	}
	return strings.TrimSpace(string(out)), nil
}
