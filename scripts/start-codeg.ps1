param([int]$Port = 8840, [switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$workspaceRoot = Split-Path -Parent $PSScriptRoot
if (!(Test-Path -LiteralPath (Join-Path $workspaceRoot 'codeg-ui\out\workspace.html'))) {
    throw '请先执行 npm run codeg:build'
}
Push-Location (Join-Path $workspaceRoot 'server-go')
try {
    if (!$SkipBuild) {
        go build -o serve-codeg.exe ./cmd/serve
        if ($LASTEXITCODE -ne 0) { throw 'Go 后台构建失败' }
    }
    $previousMode = $env:HARNESS_HUB_CODEG
    $previousPort = $env:PORT
    try {
        $env:HARNESS_HUB_CODEG = '1'
        $env:PORT = "$Port"
        Set-Location -LiteralPath $workspaceRoot
        & (Join-Path $workspaceRoot 'server-go\serve-codeg.exe')
    } finally {
        $env:HARNESS_HUB_CODEG = $previousMode
        $env:PORT = $previousPort
    }
} finally { Pop-Location }
