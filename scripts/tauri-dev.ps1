<#
    Runs the Tauri shell in development mode.

    The backend now lives in this binary, so there is nothing to build but
    the frontend and the shell itself. For frontend hot reload, run
    scripts/dev.ps1 in another terminal instead (CODEPLUSE_PORT must match).
#>
[CmdletBinding()]
param(
    [switch] $SkipFrontend
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$ui = Join-Path $root 'codeg-ui'

# Rust may live outside the default location (this machine installs it to
# E:\rust); resolve it explicitly and export the home variables.
if (!$env:CARGO_HOME) {
    foreach ($home_ in @('E:\rust\.cargo', (Join-Path $env:USERPROFILE '.cargo'))) {
        if (Test-Path -LiteralPath (Join-Path $home_ 'bin\cargo.exe')) {
            $env:CARGO_HOME = $home_
            $env:Path = (Join-Path $home_ 'bin') + ';' + $env:Path
            break
        }
    }
}
if ($env:CARGO_HOME -and !$env:RUSTUP_HOME) {
    $candidate = Join-Path (Split-Path -Parent $env:CARGO_HOME) '.rustup'
    if (Test-Path -LiteralPath $candidate) { $env:RUSTUP_HOME = $candidate }
}

if (!(Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw '未检测到 Rust 工具链，请先安装 rustup 与 Visual Studio Build Tools（见 docs/TAURI-SHELL.md）。'
}

if (!$SkipFrontend) {
    pnpm --dir $ui build
    if ($LASTEXITCODE -ne 0) { throw '前端构建失败' }
}

& (Join-Path $PSScriptRoot 'make-tauri-icons.ps1')

$env:CODEPLUSE_ROOT = $root
$cli = Join-Path $ui 'node_modules\@tauri-apps\cli\tauri.js'
Push-Location $root
try {
    & node $cli dev
} finally { Pop-Location }
