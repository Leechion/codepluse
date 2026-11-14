<#
    Generates the icon set Tauri expects (32/128/256 PNG + multi-size ICO)
    from the single app icon PNG. The ICO comes from make-codeg-icon.ps1 so
    Explorer and the installer both get alpha-correct frames.
#>
[CmdletBinding()]
param(
    [string] $SourcePng = (Join-Path (Split-Path -Parent $PSScriptRoot) 'codeg-ui\public\icon-512x512.png')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$icons = Join-Path $root 'src-tauri\icons'
New-Item -ItemType Directory -Path $icons -Force | Out-Null

if (!(Test-Path -LiteralPath $SourcePng)) {
    throw "缺少图标源：$SourcePng"
}

$image = [System.Drawing.Image]::FromFile($SourcePng)
try {
    foreach ($pair in @(
            @(32, '32x32.png'),
            @(128, '128x128.png'),
            @(256, '128x128@2x.png'),
            @(256, 'icon.png'))) {
        $size = [int]$pair[0]
        $name = [string]$pair[1]
        $bmp = New-Object System.Drawing.Bitmap($size, $size)
        try {
            $gfx = [System.Drawing.Graphics]::FromImage($bmp)
            try {
                $gfx.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                $gfx.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
                $gfx.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
                $gfx.Clear([System.Drawing.Color]::Transparent)
                $gfx.DrawImage($image, 0, 0, $size, $size)
            } finally { $gfx.Dispose() }
            $bmp.Save((Join-Path $icons $name), [System.Drawing.Imaging.ImageFormat]::Png)
        } finally { $bmp.Dispose() }
    }
} finally { $image.Dispose() }

& (Join-Path $PSScriptRoot 'make-codeg-icon.ps1') -SourcePng $SourcePng -OutIco (Join-Path $icons 'icon.ico')
Write-Host "[tauri] 图标已生成：$icons"
