# Dev hot-reload launcher (no full build per edit).
#
# Runs `next dev` for frontend HMR, and the Rust backend with
# HARNESS_HUB_DEV_FRONTEND so it reverse-proxies every non-/api request to the
# dev server — while /api and /ws stay local and same-origin. Open
# http://127.0.0.1:<Port>/workspace: edits hot-reload, no CORS, real WebSocket.
#
#   powershell -ExecutionPolicy Bypass -File scripts/dev.ps1            # browser
#   powershell -ExecutionPolicy Bypass -File scripts/dev.ps1 -Shell     # desktop window
#
# Backend changes still need a rebuild/restart (frontend HMR only). Ctrl+C stops both.

param(
  [int]$Port = 8840,
  [int]$WebPort = 3000,
  [switch]$Shell
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$webRoot = Join-Path $root "codeg-ui"

# Kill the whole tree: Stop-Process only ends the named process, which leaves
# pnpm/next children holding the dev port.
function Stop-ProcessTree([int]$ProcessId) {
  & taskkill.exe /PID $ProcessId /T /F 2>$null | Out-Null
}

# Stop any running instance so the dev backend can own $Port.
Get-Process -Name codepluse -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-ProcessTree $_.Id }
Start-Sleep -Seconds 1

$env:HARNESS_HUB_CODEG = "1"
$env:CODEPLUSE_PORT = "$Port"
$env:HOST = "127.0.0.1"
$env:HARNESS_HUB_WARM = "0"
$env:HARNESS_HUB_DEV_FRONTEND = "http://127.0.0.1:$WebPort"

# Frontend dev server (HMR).
$web = Start-Process -FilePath "cmd.exe" `
  -ArgumentList "/c", "pnpm --dir codeg-ui dev --port $WebPort" `
  -WorkingDirectory $root -PassThru -WindowStyle Hidden
Write-Host "[dev] next dev -> http://127.0.0.1:$WebPort (HMR)"

$back = $null
if ($Shell) {
  # The shell inherits HARNESS_HUB_DEV_FRONTEND and passes it to its own spawned
  # backend, so no separate backend is started here.
  Start-Process -FilePath (Join-Path $root "dist\codepluse\codepluse.exe") -WorkingDirectory $root
  Write-Host "[dev] desktop shell launched -> http://127.0.0.1:$Port/workspace (proxied, HMR)"
} else {
  # The backend is the shell binary itself; CODEPLUSE_HEADLESS=1 keeps the
  # window closed so the browser can own the page.
  Push-Location (Join-Path $root "src-tauri")
  try {
    cargo build
    if ($LASTEXITCODE -ne 0) { throw "Rust 后台构建失败" }
  } finally { Pop-Location }
  $env:CODEPLUSE_HEADLESS = "1"
  $back = Start-Process -FilePath (Join-Path $root "src-tauri\target\debug\codepluse.exe") -WorkingDirectory $root -PassThru -WindowStyle Hidden
  Write-Host "[dev] backend -> http://127.0.0.1:$Port (proxy /api,/ws local)"
  Write-Host "[dev] open: http://127.0.0.1:$Port/workspace"
}

Write-Host "[dev] Ctrl+C to stop."
try {
  if ($back) { Wait-Process -Id $back.Id } else { Wait-Process -Id $web.Id }
} finally {
  if ($back) { Stop-ProcessTree $back.Id }
  Stop-ProcessTree $web.Id
  Get-Process -Name codepluse -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-ProcessTree $_.Id }
}
