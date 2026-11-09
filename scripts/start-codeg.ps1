param([int]$Port = 8840, [switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$workspaceRoot = Split-Path -Parent $PSScriptRoot
if (!(Test-Path -LiteralPath (Join-Path $workspaceRoot 'codeg-ui\out\workspace.html'))) {
    throw '请先执行 npm run codeg:build'
}
Push-Location (Join-Path $workspaceRoot 'server-go')
try {
    $backend = Join-Path $workspaceRoot 'server-go\serve-codeg.exe'
    if (!$SkipBuild -and (Get-Command go -ErrorAction SilentlyContinue)) {
        go build -o serve-codeg.exe ./cmd/serve
        if ($LASTEXITCODE -ne 0) { throw 'Go 后台构建失败' }
    } elseif (!(Test-Path -LiteralPath $backend)) {
        $backend = Join-Path $workspaceRoot 'server-go\serve.exe'
        if (!(Test-Path -LiteralPath $backend)) { throw '未找到 Go 和已编译的后台，请安装 Go 后重试' }
        Write-Warning '未重新编译后台，使用已有 serve.exe；安装 Go 后可构建最新源码。'
    } elseif (!$SkipBuild) {
        Write-Warning '未找到 Go，使用已有 serve-codeg.exe；安装 Go 后可构建最新源码。'
    }
    $previousMode = $env:HARNESS_HUB_CODEG
    $previousPort = $env:PORT
    try {
        $env:HARNESS_HUB_CODEG = '1'
        $env:PORT = "$Port"
        Set-Location -LiteralPath $workspaceRoot
        & $backend
    } finally {
        $env:HARNESS_HUB_CODEG = $previousMode
        $env:PORT = $previousPort
    }
} finally { Pop-Location }
