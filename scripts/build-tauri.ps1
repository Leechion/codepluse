<#
    Builds the codepluse desktop shell.

    Layout: one binary. Tauri is the window, the Rust backend in
    src-tauri/src/backend/ serves the UI, the HTTP API and the event
    WebSocket on 127.0.0.1 — there is no sidecar any more (Phase 4).
    See docs/TAURI-SHELL.md.

    Requires: Rust (x86_64-pc-windows-msvc) + Visual Studio Build Tools and
    pnpm 11. Run scripts/build-tauri.ps1 -SkipFrontend to rebuild only the
    shell after the first full build.
#>
[CmdletBinding()]
param(
    [switch] $SkipFrontend,
    [switch] $DebugBuild
)

$ErrorActionPreference = 'Stop'

# Native tools write progress and warnings to stderr. PowerShell 5.1 turns any
# such line into a terminating error while ErrorActionPreference is Stop, so
# every external call runs with it relaxed and is judged by its exit code.
function Invoke-Native([scriptblock] $Run) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $Run } finally { $ErrorActionPreference = $previous }
    return $LASTEXITCODE
}

$root = Split-Path -Parent $PSScriptRoot
$ui = Join-Path $root 'codeg-ui'
$tauriDir = Join-Path $root 'src-tauri'

# Rust may live outside the default location (this machine installs it to
# E:\rust). Resolve the shim explicitly and export the home variables so
# `cargo`/`rustc` children work even when the session started before the
# environment was configured.
function Resolve-Cargo {
    if ($env:CARGO_HOME) {
        $candidate = Join-Path $env:CARGO_HOME 'bin\cargo.exe'
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    foreach ($home_ in @('E:\rust\.cargo', (Join-Path $env:USERPROFILE '.cargo'))) {
        $candidate = Join-Path $home_ 'bin\cargo.exe'
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    $cmd = Get-Command cargo -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    return $null
}

$cargoExe = Resolve-Cargo
if (!$cargoExe) {
    throw @'
未检测到 Rust 工具链。Tauri 构建需要：

  1. rustup        https://win.rustup.rs   （默认 x86_64-pc-windows-msvc）
  2. Visual Studio Build Tools，勾选「使用 C++ 的桌面开发」
     —— 提供 link.exe；只装 rustup 会在链接阶段报 linker not found

安装后重开终端，确认 `rustc -vV` 的 host 为 x86_64-pc-windows-msvc。
如果只想用原来的 WebView2 外壳，继续用 pnpm codepluse:desktop:build:webview2。
'@
}

$cargoDir = Split-Path -Parent $cargoExe
if (($env:Path -split ';') -notcontains $cargoDir) { $env:Path = "$cargoDir;$env:Path" }
if (!$env:CARGO_HOME) { $env:CARGO_HOME = Split-Path -Parent $cargoDir }
if (!$env:RUSTUP_HOME) {
    $candidate = Join-Path (Split-Path -Parent $env:CARGO_HOME) '.rustup'
    if (Test-Path -LiteralPath $candidate) { $env:RUSTUP_HOME = $candidate }
}

$cli = Join-Path $ui 'node_modules\@tauri-apps\cli\tauri.js'
if (!(Test-Path -LiteralPath $cli)) {
    throw "缺少 @tauri-apps/cli，请先执行：pnpm --dir codeg-ui install"
}

if (!$SkipFrontend) {
    Write-Host '[tauri] 构建前端静态产物…'
    if ((Invoke-Native { pnpm --dir $ui build }) -ne 0) { throw '前端构建失败' }
}

if (!(Test-Path -LiteralPath (Join-Path $ui 'out\workspace.html'))) {
    throw '缺少 codeg-ui\out\workspace.html，请先构建前端'
}

& (Join-Path $PSScriptRoot 'make-tauri-icons.ps1')

# The installed shell resolves its workspace root from this manifest (see
# resolve_root in src/main.rs). Forward slashes keep the JSON readable and are
# accepted by Windows path APIs.
$resources = Join-Path $tauriDir 'resources'
New-Item -ItemType Directory -Path $resources -Force | Out-Null
@{ root = ($root -replace '\\', '/') } | ConvertTo-Json |
    Set-Content -LiteralPath (Join-Path $resources 'workspace.json') -Encoding utf8

# Run the CLI from the repository root so it finds ./src-tauri.
Push-Location $root
try {
    $tauriArgs = @($cli, 'build')
    if ($DebugBuild) { $tauriArgs += '--debug' }
    # The CLI writes progress to stderr. With ErrorActionPreference=Stop,
    # PowerShell 5.1 turns any native stderr line into a terminating error and
    # the build "fails" after two seconds. Relax it for this call and judge by
    # the exit code instead.
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & node @tauriArgs
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousPreference
    }
    if ($code -ne 0) {
        Write-Warning '若错误含 "linker link.exe not found"，说明只装了 Rust、缺少 VS Build Tools（见 docs/TAURI-SHELL.md）。'
        throw "Tauri 构建失败（exit $code）"
    }
} finally { Pop-Location }

$outDir = if ($DebugBuild) { 'target\debug\bundle' } else { 'target\release\bundle' }
Write-Host "[tauri] 完成，安装包位于 $tauriDir\$outDir"
