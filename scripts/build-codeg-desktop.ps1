param([switch]$InstallShortcut)
$ErrorActionPreference = 'Stop'
$workspaceRoot = Split-Path -Parent $PSScriptRoot
$bundleRoot = Join-Path $workspaceRoot 'dist\codepluse'
New-Item -ItemType Directory -Path $bundleRoot -Force | Out-Null
if (!(Test-Path -LiteralPath (Join-Path $workspaceRoot 'codeg-ui\out\workspace.html'))) { throw '请先执行 npm run codeg:build' }
Push-Location (Join-Path $workspaceRoot 'server-go')
try {
    go build -o serve-codepluse.exe ./cmd/serve
    if ($LASTEXITCODE -ne 0) { throw 'Go 后台构建失败' }
    go build -ldflags '-H=windowsgui' -o (Join-Path $bundleRoot 'codepluse.exe') ./cmd/desktop
    if ($LASTEXITCODE -ne 0) { throw 'Go 桌面端构建失败' }
    $licenseRoot = Join-Path $bundleRoot 'licenses'
    New-Item -ItemType Directory -Path $licenseRoot -Force | Out-Null
    $modules = go list -m -f '{{.Path}}|{{.Dir}}' all
    if ($LASTEXITCODE -ne 0) { throw '无法读取依赖许可目录' }
    foreach ($module in $modules) {
        $parts = $module.Split('|')
        if ($parts[0] -eq 'harnesshub') { continue }
        $license = Join-Path $parts[1] 'LICENSE'
        if (!(Test-Path -LiteralPath $license)) { $license = Join-Path $parts[1] 'LICENSE.md' }
        if (Test-Path -LiteralPath $license) {
            Copy-Item -LiteralPath $license -Destination (Join-Path $licenseRoot ($parts[0].Replace('/','_') + '.txt')) -Force
        }
    }
} finally { Pop-Location }
$workspaceConfig = @{ root = $workspaceRoot } | ConvertTo-Json
[System.IO.File]::WriteAllText((Join-Path $bundleRoot 'workspace.json'), $workspaceConfig, (New-Object System.Text.UTF8Encoding($false)))
Copy-Item -LiteralPath (Join-Path $workspaceRoot 'THIRD_PARTY_NOTICES.md') -Destination $bundleRoot
Copy-Item -LiteralPath (Join-Path $workspaceRoot 'codeg-ui\LICENSE') -Destination (Join-Path $bundleRoot 'licenses\codeg-Apache-2.0.txt')
Copy-Item -LiteralPath (Join-Path $workspaceRoot 'bridges\account-channels\vendor\LICENSE') -Destination (Join-Path $bundleRoot 'licenses\account-channel-MIT.txt')
Copy-Item -LiteralPath (Join-Path $workspaceRoot 'bridges\account-channels\vendor\LICENSE-OFM') -Destination (Join-Path $bundleRoot 'licenses\account-channel-standalone-MIT.txt')
# codepluse.ico 供可选桌面快捷方式取图标。旧的 public/codepluse.ico 已随 Node/Electron 栈删除，
# 改由应用图标 PNG 现场生成，避免依赖已删文件。
# 生成方式换成 scripts/make-codeg-icon.ps1：GetHicon()+Icon.Save() 只会写出
# 单帧、丢 alpha 的 ICO，快捷方式在资源管理器里会退回默认图标。
$iconSource = Join-Path $workspaceRoot 'codeg-ui\public\icon-512x512.png'
$iconScript = Join-Path $PSScriptRoot 'make-codeg-icon.ps1'
if (Test-Path -LiteralPath $iconSource) {
    & $iconScript -SourcePng $iconSource -OutIco (Join-Path $bundleRoot 'codepluse.ico')
} else {
    Write-Warning "缺少图标源 $iconSource，未生成 codepluse.ico（快捷方式将无自定义图标）"
}
if ($InstallShortcut) {
    $shortcutShell = New-Object -ComObject WScript.Shell
    $shortcutPath = Join-Path ([Environment]::GetFolderPath('Desktop')) 'codepluse.lnk'
    $shortcut = $shortcutShell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = Join-Path $bundleRoot 'codepluse.exe'
    $shortcut.WorkingDirectory = $workspaceRoot
    $shortcut.IconLocation = (Join-Path $bundleRoot 'codepluse.ico') + ',0'
    $shortcut.Description = 'codepluse · 多 Harness 工作台'
    $shortcut.Save()
    Write-Output $shortcutPath
}
Write-Output (Join-Path $bundleRoot 'codepluse.exe')
