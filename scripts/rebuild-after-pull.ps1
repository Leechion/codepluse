param()
$ErrorActionPreference = 'Stop'

# Post-pull rebuild. The desktop shell is Tauri; the WebView2 build is kept as
# a fallback for machines without the Rust toolchain, so a pull never leaves
# the user with nothing runnable.

$root = Split-Path -Parent $PSScriptRoot
$uiRoot = Join-Path $root 'codeg-ui'
$tauriExe = Join-Path $root 'src-tauri\target\release\codepluse.exe'
$webviewExe = Join-Path $root 'dist\codepluse\codepluse.exe'
$desktopCandidates = @(
    [IO.Path]::GetFullPath($tauriExe),
    [IO.Path]::GetFullPath($webviewExe)
) | Select-Object -Unique
$backendPaths = @(
    [IO.Path]::GetFullPath((Join-Path $root 'src-tauri\target\release\codepluse.exe')),
    [IO.Path]::GetFullPath((Join-Path $root 'src-tauri\target\debug\codepluse.exe'))
)

if (!(Get-Command pnpm -ErrorAction SilentlyContinue)) { throw '自动更新需要 pnpm；请先安装 Node.js 和 pnpm。' }

Write-Host '[update] 同步前端依赖…'
pnpm --dir $uiRoot install --frozen-lockfile
if ($LASTEXITCODE -ne 0) { throw '前端依赖安装失败。' }

$processes = @(Get-CimInstance Win32_Process)
$desktopProcesses = @($processes | Where-Object {
    $_.ExecutablePath -and ($desktopCandidates -contains [IO.Path]::GetFullPath($_.ExecutablePath))
})
$backendProcesses = @($processes | Where-Object {
    if (!$_.ExecutablePath) { return $false }
    $backendPaths -contains [IO.Path]::GetFullPath($_.ExecutablePath)
})

$desktopLaunches = @()
foreach ($process in $desktopProcesses) {
    $port = 8840
    $page = '/workspace'
    if ($process.CommandLine -match '--port\s+(\d+)') { $port = [int]$Matches[1] }
    if ($process.CommandLine -match '--page\s+"([^"]+)"') { $page = $Matches[1] }
    elseif ($process.CommandLine -match '--page\s+(\S+)') { $page = $Matches[1] }
    $desktopLaunches += [pscustomobject]@{ Port = $port; Page = $page }
}

$backendPorts = @()
foreach ($process in $backendProcesses) {
    try {
        $backendPorts += Get-NetTCPConnection -State Listen -OwningProcess $process.ProcessId -ErrorAction Stop |
            Select-Object -ExpandProperty LocalPort
    } catch {
        Write-Warning "读取后台服务 $($process.ProcessId) 的监听端口失败；桌面窗口仍会按原端口重启。"
    }
}
$backendPorts = @($backendPorts | Sort-Object -Unique)

foreach ($process in $desktopProcesses) {
    $app = Get-Process -Id $process.ProcessId -ErrorAction SilentlyContinue
    if ($app -and $app.MainWindowHandle -ne [IntPtr]::Zero) { [void]$app.CloseMainWindow() }
}
$closeDeadline = (Get-Date).AddSeconds(20)
while ($desktopProcesses.Count -gt 0 -and (Get-Date) -lt $closeDeadline) {
    $remaining = @($desktopProcesses | Where-Object { Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue })
    if ($remaining.Count -eq 0) { break }
    Start-Sleep -Milliseconds 300
}
$remaining = @($desktopProcesses | Where-Object { Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue })
if ($remaining.Count -gt 0) {
    throw 'Codepluse 未能正常关闭，自动更新已停止。请先关闭窗口，再重新执行 pnpm codepluse:refresh。'
}

foreach ($process in $backendProcesses) {
    Stop-Process -Id $process.ProcessId -Force -ErrorAction SilentlyContinue
}
foreach ($process in $backendProcesses) {
    Wait-Process -Id $process.ProcessId -Timeout 10 -ErrorAction SilentlyContinue
}

$portsToStart = @($backendPorts)
foreach ($launch in $desktopLaunches) { $portsToStart += $launch.Port }
$portsToStart = @($portsToStart | Sort-Object -Unique)
$backendExe = Join-Path $root 'src-tauri\target\release\codepluse.exe'

# The app is already stopped; if the build fails, bring the previous version
# back up instead of leaving the user with nothing running.
try {
    $buildScript = 'build-tauri.ps1'
    Write-Host "[update] 构建桌面端（$buildScript）…"
    & (Join-Path $PSScriptRoot $buildScript)
    if (-not $?) { throw '桌面端构建失败' }
} catch {
    Write-Warning "[update] 构建失败：$($_.Exception.Message)"
    Write-Warning '[update] 用现有版本恢复运行…'
    foreach ($port in $portsToStart) {
        $env:HARNESS_HUB_CODEG = '1'
        $env:CODEPLUSE_PORT = "$port"
        $env:CODEPLUSE_HEADLESS = '1'
        $env:HOST = '127.0.0.1'
        Start-Process -FilePath $backendExe -WorkingDirectory $root -WindowStyle Hidden | Out-Null
    }
    foreach ($launch in $desktopLaunches) {
        if (Test-Path -LiteralPath $tauriExe) {
            $env:CODEPLUSE_ROOT = $root
            $env:CODEPLUSE_PORT = "$($launch.Port)"
            Start-Process -FilePath $tauriExe -WorkingDirectory $root | Out-Null
        } elseif (Test-Path -LiteralPath $webviewExe) {
            Start-Process -FilePath $webviewExe -WorkingDirectory $root -ArgumentList @(
                '--root', "`"$root`"", '--port', "$($launch.Port)", '--page', "`"$($launch.Page)`""
            ) | Out-Null
        }
    }
    throw
}

foreach ($port in $portsToStart) {
    $envNames = @('HARNESS_HUB_CODEG', 'CODEPLUSE_PORT', 'CODEPLUSE_HEADLESS', 'HOST', 'HARNESS_HUB_WARM', 'HARNESS_HUB_ALLOW_ANY_PATH')
    $oldValues = @{}
    foreach ($name in $envNames) { $oldValues[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
    try {
        $env:HARNESS_HUB_CODEG = '1'
        $env:CODEPLUSE_PORT = "$port"
        $env:CODEPLUSE_HEADLESS = '1'
        $env:HOST = '127.0.0.1'
        $env:HARNESS_HUB_WARM = '0'
        $env:HARNESS_HUB_ALLOW_ANY_PATH = '1'
        Start-Process -FilePath $backendExe -WorkingDirectory $root -WindowStyle Hidden | Out-Null
    } finally {
        foreach ($name in $envNames) { [Environment]::SetEnvironmentVariable($name, $oldValues[$name], 'Process') }
    }
}

foreach ($launch in $desktopLaunches) {
    if (Test-Path -LiteralPath $tauriExe) {
        # The Tauri shell resolves its root from CODEPLUSE_ROOT, then from a
        # workspace.json beside the exe, then from its own directory.
        $env:CODEPLUSE_ROOT = $root
        $env:CODEPLUSE_PORT = "$($launch.Port)"
        Start-Process -FilePath $tauriExe -WorkingDirectory $root | Out-Null
    } else {
        Start-Process -FilePath $webviewExe -WorkingDirectory $root -ArgumentList @(
            '--root', "`"$root`"", '--port', "$($launch.Port)", '--page', "`"$($launch.Page)`""
        ) | Out-Null
    }
}

if ($desktopLaunches.Count -gt 0) {
    Write-Host "[update] Codepluse 已更新并重新打开（$($desktopLaunches.Count) 个窗口）。"
} elseif ($portsToStart.Count -gt 0) {
    Write-Host '[update] 后台服务已更新并重新启动。'
} else {
    Write-Host '[update] 构建完成；Codepluse 未运行，下次打开时将使用新版。'
}
