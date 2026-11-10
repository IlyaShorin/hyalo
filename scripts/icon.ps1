# Draws hyalo's icon, a glass lens, and writes crates/hyalo/assets/hyalo.ico
# with every size Windows asks for, plus a preview of them on dark and light
# backgrounds in target/icon-preview.png.
#
#   powershell -ExecutionPolicy Bypass -File scripts/icon.ps1
#
# Each size is drawn on its own, four times as large, and scaled down, so
# small sizes stay sharp. The drawing is on a 256-unit grid.
param(
    [string]$Out = "crates/hyalo/assets/hyalo.ico",
    [string]$Preview = "target/icon-preview.png"
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
$root = Split-Path -Parent $PSScriptRoot
$sizes = 16, 20, 24, 32, 40, 48, 64, 256

function Color([string]$hex, [int]$alpha = 255) {
    $c = [System.Drawing.ColorTranslator]::FromHtml($hex)
    [System.Drawing.Color]::FromArgb($alpha, $c)
}

function Circle([float]$x, [float]$y, [float]$r) {
    New-Object System.Drawing.RectangleF ($x - $r), ($y - $r), (2 * $r), (2 * $r)
}

function Gradient($rect, [float]$angle, $colors, [float[]]$stops) {
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, $colors[0], $colors[-1], $angle
    $blend = New-Object System.Drawing.Drawing2D.ColorBlend $colors.Count
    $blend.Colors = $colors
    $blend.Positions = $stops
    $brush.InterpolationColors = $blend
    $brush
}

# The lens: centre and radii of the rim and the glass inside it.
$cx = 104; $cy = 104; $rim = 88; $glass = 71

function Draw-Lens($g) {
    # The handle, under the lens: a short thick bar, light to dark.
    $handle = New-Object System.Drawing.RectangleF 150, 150, 90, 90
    $brush = Gradient $handle 45 @((Color "#E6ECFA"), (Color "#7D8BB0"), (Color "#46527A")) @(0, 0.5, 1)
    $pen = New-Object System.Drawing.Pen $brush, 38
    $pen.StartCap = "Round"; $pen.EndCap = "Round"
    $g.DrawLine($pen, 168, 168, 222, 222)

    # The rim: bright glass edge, outlined so it stands on light ground too.
    $outer = Circle $cx $cy $rim
    $g.FillEllipse((Gradient $outer 50 @((Color "#FFFFFF"), (Color "#BFE3FF"), (Color "#8FA8FF")) @(0, 0.55, 1)), $outer)
    $g.DrawEllipse((New-Object System.Drawing.Pen (Color "#24307A" 150), 6), (Circle $cx $cy ($rim - 2)))

    # The glass: cyan to blue to violet.
    $inner = Circle $cx $cy $glass
    $colors = @((Color "#9CF4FF"), (Color "#45A0FF"), (Color "#3A5BF0"), (Color "#6B3FE0"))
    $g.FillEllipse((Gradient $inner 55 $colors @(0, 0.35, 0.7, 1)), $inner)
    $g.DrawEllipse((New-Object System.Drawing.Pen (Color "#1E2A80" 90), 4), $inner)

    # A crescent of light along the upper left of the glass: the glass
    # minus itself moved down and right.
    $crescent = New-Object System.Drawing.Region (New-Object System.Drawing.Drawing2D.GraphicsPath)
    $lit = New-Object System.Drawing.Drawing2D.GraphicsPath
    $lit.AddEllipse((Circle $cx $cy ($glass - 9)))
    $shade = New-Object System.Drawing.Drawing2D.GraphicsPath
    $shade.AddEllipse((Circle ($cx + 16) ($cy + 20) ($glass - 4)))
    $crescent = New-Object System.Drawing.Region $lit
    $crescent.Exclude($shade)
    $fade = Gradient $inner 45 @((Color "#FFFFFF" 230), (Color "#FFFFFF" 120), (Color "#FFFFFF" 0), (Color "#FFFFFF" 0)) @(0, 0.4, 0.75, 1)
    $g.FillRegion($fade, $crescent)
}

function Draw-Size([int]$size) {
    $n = $size * 4
    $big = New-Object System.Drawing.Bitmap $n, $n, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($big)
    $g.SmoothingMode = "AntiAlias"
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.ScaleTransform($n / 256, $n / 256)
    Draw-Lens $g
    $g.Dispose()
    $small = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($small)
    $g.InterpolationMode = "HighQualityBilinear"
    $g.PixelOffsetMode = "HighQuality"
    $g.DrawImage($big, (New-Object System.Drawing.Rectangle 0, 0, $size, $size))
    $g.Dispose()
    $big.Dispose()
    $small
}

$images = foreach ($size in $sizes) { Draw-Size $size }

# The .ico: a directory of entries, then each image as a PNG.
$pngs = foreach ($image in $images) {
    $stream = New-Object System.IO.MemoryStream
    $image.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    , $stream.ToArray()
}
$file = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $file
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $side = $sizes[$i] % 256
    $w.Write([byte]$side); $w.Write([byte]$side); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$pngs[$i].Length); $w.Write([uint32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($png in $pngs) { $w.Write($png) }
$w.Flush()
$path = Join-Path $root $Out
[System.IO.Directory]::CreateDirectory((Split-Path -Parent $path)) | Out-Null
[System.IO.File]::WriteAllBytes($path, $file.ToArray())
"wrote $path ($($file.Length) bytes)"

# The preview: each size at 1x and 4x (nearest neighbour) on dark and light.
$sheet = New-Object System.Drawing.Bitmap 1180, 640
$g = [System.Drawing.Graphics]::FromImage($sheet)
$g.InterpolationMode = "NearestNeighbor"
$g.PixelOffsetMode = "Half"
$g.FillRectangle((New-Object System.Drawing.SolidBrush (Color "#202020")), 0, 0, 1180, 320)
$g.FillRectangle((New-Object System.Drawing.SolidBrush (Color "#F3F3F3")), 0, 320, 1180, 320)
foreach ($band in 0, 320) {
    $x = 16
    for ($i = 0; $i -lt $sizes.Count - 1; $i++) {
        $s = $sizes[$i]
        $g.DrawImage($images[$i], $x, $band + 16, $s, $s)
        $g.DrawImage($images[$i], $x, $band + 100, $s * 3, $s * 3)
        $x += [Math]::Max($s * 3, 40) + 16
    }
    $g.DrawImage($images[-1], 900, $band + 32, 256, 256)
}
$g.Dispose()
$previewPath = Join-Path $root $Preview
$sheet.Save($previewPath, [System.Drawing.Imaging.ImageFormat]::Png)
"wrote $previewPath"
