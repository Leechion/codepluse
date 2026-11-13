$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$existing = git -C $root config --local --get core.hooksPath 2>$null
if ($LASTEXITCODE -eq 0 -and $existing -and $existing.Trim() -ne '.githooks') {
    throw "此仓库已将 core.hooksPath 配置为 '$($existing.Trim())'。请先合并现有 Git hooks，再启用 Codepluse 自动更新。"
}
git -C $root config --local core.hooksPath .githooks
if ($LASTEXITCODE -ne 0) { throw '无法启用仓库的自动更新 hook。' }
Write-Host '已启用 Codepluse 拉取后自动更新。之后执行 git pull 会自动构建，并在 Codepluse 正运行时重启它。'
