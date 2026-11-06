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
if ($InstallShortcut) {
    $shortcutShell = New-Object -ComObject WScript.Shell
    $shortcutPath = Join-Path ([Environment]::GetFolderPath('Desktop')) 'codepluse.lnk'
    $shortcut = $shortcutShell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = Join-Path $bundleRoot 'codepluse.exe'
    $shortcut.WorkingDirectory = $workspaceRoot
    $shortcut.Description = 'codepluse · 多 Harness 工作台'
    $shortcut.Save()
    Write-Output $shortcutPath
}
Write-Output (Join-Path $bundleRoot 'codepluse.exe')
