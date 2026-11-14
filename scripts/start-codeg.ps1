param([int]$Port = 8840, [switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$workspaceRoot = Split-Path -Parent $PSScriptRoot
if (!(Test-Path -LiteralPath (Join-Path $workspaceRoot 'codeg-ui\out\workspace.html'))) {
    throw '请先执行 npm run codeg:build'
}
Push-Location (Join-Path $workspaceRoot 'src-tauri')
try {
    # The backend is the shell binary itself; CODEPLUSE_HEADLESS=1 serves the
    # HTTP surface without opening a window.
    $backend = Join-Path $workspaceRoot 'src-tauri\target\release\codepluse.exe'
    if (!$SkipBuild) {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw 'Rust 后台构建失败' }
    } elseif (!(Test-Path -LiteralPath $backend)) {
        $backend = Join-Path $workspaceRoot 'src-tauri\target\debug\codepluse.exe'
        if (!(Test-Path -LiteralPath $backend)) { throw '未找到已编译的后台，请先执行 cargo build' }
        Write-Warning '未重新编译后台，使用已有的 debug 构建。'
    }
    $previousMode = $env:HARNESS_HUB_CODEG
    $previousPort = $env:CODEPLUSE_PORT
    $previousAnyPath = $env:HARNESS_HUB_ALLOW_ANY_PATH
    $previousAnyBrowse = $env:HARNESS_HUB_ALLOW_ANY_DIR_BROWSE
    $previousWarm = $env:HARNESS_HUB_WARM
    try {
        $env:HARNESS_HUB_CODEG = '1'
        $env:CODEPLUSE_PORT = "$Port"
        $env:CODEPLUSE_HEADLESS = '1'
        # 允许把服务根之外的文件夹添加为工作空间（目录浏览器可浏览整机）。
        # 两个开关分开：ANY_PATH 决定能否加入工作空间（进而读写），
        # ANY_DIR_BROWSE 只决定只读目录列举。桌面/本地开发同时打开。
        $env:HARNESS_HUB_ALLOW_ANY_PATH = '1'
        $env:HARNESS_HUB_ALLOW_ANY_DIR_BROWSE = '1'
        # 默认关闭连接预热，避免低内存机器启动时 "out of memory" 崩溃；已显式设置则尊重其值。
        if ([string]::IsNullOrEmpty($env:HARNESS_HUB_WARM)) { $env:HARNESS_HUB_WARM = '0' }
        Set-Location -LiteralPath $workspaceRoot
        & $backend
        # Native non-zero exits are not terminating errors, so npm start used
        # to report success even when the backend died (port busy, bad config).
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } finally {
        $env:HARNESS_HUB_CODEG = $previousMode
        $env:CODEPLUSE_PORT = $previousPort
        $env:HARNESS_HUB_ALLOW_ANY_PATH = $previousAnyPath
        $env:HARNESS_HUB_ALLOW_ANY_DIR_BROWSE = $previousAnyBrowse
        $env:HARNESS_HUB_WARM = $previousWarm
    }
} finally { Pop-Location }
