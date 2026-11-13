param()
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$uiRoot = Join-Path $root 'codeg-ui'
$desktopExe = Join-Path $root 'dist\codepluse\codepluse.exe'
$desktopExeFull = [IO.Path]::GetFullPath($desktopExe)
$backendPaths = @(
    [IO.Path]::GetFullPath((Join-Path $root 'server-go\serve-codepluse.exe')),
    [IO.Path]::GetFullPath((Join-Path $root 'server-go\serve-codeg.exe')),
    [IO.Path]::GetFullPath((Join-Path $root 'server-go\serve.exe'))
)

if (!(Get-Command pnpm -ErrorAction SilentlyContinue)) { throw '自动更新需要 pnpm；请先安装 Node.js 和 pnpm。' }
if (!(Get-Command go -ErrorAction SilentlyContinue)) { throw '自动更新需要 Go；请先安装 Go。' }

Write-Host '[update] 同步前端依赖…'
pnpm --dir $uiRoot install --frozen-lockfile
if ($LASTEXITCODE -ne 0) { throw '前端依赖安装失败。' }

$processes = @(Get-CimInstance Win32_Process)
$desktopProcesses = @($processes | Where-Object {
    $_.ExecutablePath -and [string]::Equals([IO.Path]::GetFullPath($_.ExecutablePath), $desktopExeFull, [StringComparison]::OrdinalIgnoreCase)
})
$backendProcesses = @($processes | Where-Object {
    if (!$_.ExecutablePath) { return $false }
    $processPath = [IO.Path]::GetFullPath($_.ExecutablePath)
    $backendPaths -contains $processPath
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

Write-Host '[update] 构建静态前端…'
pnpm --dir $uiRoot build
if ($LASTEXITCODE -ne 0) { throw '前端构建失败；请检查 Next.js 输出。' }

Write-Host '[update] 构建 Go 后台和桌面程序…'
& (Join-Path $PSScriptRoot 'build-codeg-desktop.ps1')
if ($LASTEXITCODE -ne 0) { throw 'Go 桌面程序构建失败。' }

$portsToStart = @($backendPorts)
foreach ($launch in $desktopLaunches) { $portsToStart += $launch.Port }
$portsToStart = @($portsToStart | Sort-Object -Unique)
$backendExe = Join-Path $root 'server-go\serve-codepluse.exe'
foreach ($port in $portsToStart) {
    $envNames = @('HARNESS_HUB_CODEG', 'PORT', 'HOST', 'HARNESS_HUB_WARM', 'HARNESS_HUB_ALLOW_ANY_PATH')
    $oldValues = @{}
    foreach ($name in $envNames) { $oldValues[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
    try {
        $env:HARNESS_HUB_CODEG = '1'
        $env:PORT = "$port"
        $env:HOST = '127.0.0.1'
        $env:HARNESS_HUB_WARM = '0'
        $env:HARNESS_HUB_ALLOW_ANY_PATH = '1'
        Start-Process -FilePath $backendExe -WorkingDirectory $root -WindowStyle Hidden | Out-Null
    } finally {
        foreach ($name in $envNames) { [Environment]::SetEnvironmentVariable($name, $oldValues[$name], 'Process') }
    }
}

foreach ($launch in $desktopLaunches) {
    Start-Process -FilePath $desktopExe -WorkingDirectory $root -ArgumentList @(
        '--root', "`"$root`"", '--port', "$($launch.Port)", '--page', "`"$($launch.Page)`""
    ) | Out-Null
}

if ($desktopLaunches.Count -gt 0) {
    Write-Host "[update] Codepluse 已更新并重新打开（$($desktopLaunches.Count) 个窗口）。"
} elseif ($portsToStart.Count -gt 0) {
    Write-Host '[update] 后台服务已更新并重新启动。'
} else {
    Write-Host '[update] 构建完成；Codepluse 未运行，下次打开时将使用新版。'
}
