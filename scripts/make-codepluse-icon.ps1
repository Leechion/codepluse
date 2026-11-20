<#
    Builds a Windows .ico from the app icon PNG.

    The previous inline version used Bitmap::GetHicon() + Icon::Save(). That
    writes a single frame with no alpha channel, which Explorer refuses to
    render — the desktop shortcut fell back to the generic application icon.
    Windows Vista and later also accept PNG-compressed ICO frames, so the
    frames are encoded here and the ICONDIR / ICONDIRENTRY records are written
    by hand: multi-size, 32-bit, alpha preserved.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $SourcePng,
    [Parameter(Mandatory = $true)] [string] $OutIco,
    [int[]] $Sizes = @(16, 24, 32, 48, 64, 128, 256)
)

Add-Type -AssemblyName System.Drawing

if (!(Test-Path -LiteralPath $SourcePng)) {
    Write-Warning "缺少图标源 $SourcePng，未生成 $OutIco"
    exit 0
}

$frames = @()
$image = [System.Drawing.Image]::FromFile($SourcePng)
try {
    foreach ($size in $Sizes) {
        $bmp = New-Object System.Drawing.Bitmap([int]$size, [int]$size)
        try {
            $gfx = [System.Drawing.Graphics]::FromImage($bmp)
            try {
                $gfx.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                $gfx.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
                $gfx.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
                $gfx.Clear([System.Drawing.Color]::Transparent)
                $gfx.DrawImage($image, 0, 0, [int]$size, [int]$size)
            } finally { $gfx.Dispose() }
            $ms = New-Object System.IO.MemoryStream
            try {
                $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
                $frames += , @{ Size = $size; Bytes = $ms.ToArray() }
            } finally { $ms.Dispose() }
        } finally { $bmp.Dispose() }
    }
} finally { $image.Dispose() }

$out = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter($out)
try {
    $writer.Write([UInt16]0)                  # reserved
    $writer.Write([UInt16]1)                  # type: icon
    $writer.Write([UInt16]$Frames.Count)
    $offset = 6 + (16 * $Frames.Count)
    foreach ($frame in $Frames) {
        # 256 is encoded as 0 in both width and height bytes.
        $edge = if ($frame.Size -ge 256) { 0 } else { $frame.Size }
        $writer.Write([Byte]$edge)
        $writer.Write([Byte]$edge)
        $writer.Write([Byte]0)                # palette colours
        $writer.Write([Byte]0)                # reserved
        $writer.Write([UInt16]1)              # colour planes
        $writer.Write([UInt16]32)             # bits per pixel
        $writer.Write([UInt32]$frame.Bytes.Length)
        $writer.Write([UInt32]$offset)
        $offset += $frame.Bytes.Length
    }
    foreach ($frame in $Frames) { $writer.Write($frame.Bytes) }
    $writer.Flush()
    $bytes = $out.ToArray()
} finally {
    $writer.Dispose()
    $out.Dispose()
}

$tmp = "$OutIco.tmp"
[System.IO.File]::WriteAllBytes($tmp, $bytes)
Move-Item -LiteralPath $tmp -Destination $OutIco -Force
Write-Output ("{0} ({1} bytes, {2} frames)" -f $OutIco, $bytes.Length, $Frames.Count)
