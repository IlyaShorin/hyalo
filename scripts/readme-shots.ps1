# The README's screenshots, step two: puts the frames the renderer drew
# (premultiplied RGBA PNGs in target/readme, from
# `cargo test -p hyalo -- --ignored readme_shots`) over a wallpaper drawn
# here, the glass under each window blurred and tinted as DWM does it,
# with a shadow, and the settings window under a title bar.
#
#   powershell -STA -ExecutionPolicy Bypass -File scripts/readme-shots.ps1
#
# Writes docs/images/<name>.png. Look at each before committing it.
param([string]$Frames = "target/readme", [string]$Out = "docs/images")
$Frames = (Resolve-Path $Frames).Path
$Out = [IO.Path]::GetFullPath($Out)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName PresentationCore, PresentationFramework, WindowsBase

function Wallpaper([int]$w, [int]$h) {
    $canvas = New-Object System.Windows.Controls.Canvas
    $canvas.Width = $w; $canvas.Height = $h
    $bg = New-Object System.Windows.Media.LinearGradientBrush
    $bg.StartPoint = "0,0"; $bg.EndPoint = "1,1"
    $bg.GradientStops.Add((New-Object System.Windows.Media.GradientStop ([System.Windows.Media.Color]::FromRgb(10, 14, 38)), 0))
    $bg.GradientStops.Add((New-Object System.Windows.Media.GradientStop ([System.Windows.Media.Color]::FromRgb(28, 12, 48)), 1))
    $canvas.Background = $bg
    $blobs = @(
        @(0.18, 0.30, 0.55, 59, 130, 246),
        @(0.70, 0.22, 0.50, 168, 85, 247),
        @(0.85, 0.80, 0.55, 236, 72, 153),
        @(0.25, 0.85, 0.50, 20, 184, 166),
        @(0.50, 0.55, 0.35, 99, 102, 241)
    )
    foreach ($b in $blobs) {
        $size = $b[2] * $w
        $e = New-Object System.Windows.Shapes.Ellipse
        $e.Width = $size; $e.Height = $size * 0.8
        $brush = New-Object System.Windows.Media.RadialGradientBrush
        $c = [System.Windows.Media.Color]::FromArgb(230, [byte]$b[3], [byte]$b[4], [byte]$b[5])
        $brush.GradientStops.Add((New-Object System.Windows.Media.GradientStop $c, 0))
        $brush.GradientStops.Add((New-Object System.Windows.Media.GradientStop ([System.Windows.Media.Color]::FromArgb(0, [byte]$b[3], [byte]$b[4], [byte]$b[5])), 1))
        $e.Fill = $brush
        [System.Windows.Controls.Canvas]::SetLeft($e, $b[0] * $w - $size / 2)
        [System.Windows.Controls.Canvas]::SetTop($e, $b[1] * $h - $size * 0.4)
        $canvas.Children.Add($e) | Out-Null
    }
    $blur = New-Object System.Windows.Media.Effects.BlurEffect
    $blur.Radius = 120; $blur.KernelType = "Gaussian"; $blur.RenderingBias = "Quality"
    $canvas.Effect = $blur
    Render $canvas $w $h
}

function Render($element, [int]$w, [int]$h) {
    $element.Measure((New-Object System.Windows.Size $w, $h))
    $element.Arrange((New-Object System.Windows.Rect 0, 0, $w, $h))
    $element.UpdateLayout()
    $bitmap = New-Object System.Windows.Media.Imaging.RenderTargetBitmap $w, $h, 96, 96, ([System.Windows.Media.PixelFormats]::Pbgra32)
    $bitmap.Render($element)
    $bitmap.Freeze()
    $bitmap
}

# A frame's bytes are premultiplied already: taken as Pbgra32, not Bgra32.
function Frame([string]$path, [int]$cropHeight = 0) {
    $decoder = New-Object System.Windows.Media.Imaging.PngBitmapDecoder ([Uri]$path), "PreservePixelFormat", "OnLoad"
    $source = New-Object System.Windows.Media.Imaging.FormatConvertedBitmap $decoder.Frames[0], ([System.Windows.Media.PixelFormats]::Bgra32), $null, 0
    $w = $source.PixelWidth; $h = $source.PixelHeight
    if ($cropHeight -gt 0) { $h = [Math]::Min($h, $cropHeight) }
    $stride = $w * 4
    $pixels = New-Object byte[] ($stride * $h)
    $source.CopyPixels((New-Object System.Windows.Int32Rect 0, 0, $w, $h), $pixels, $stride, 0)
    $bitmap = New-Object System.Windows.Media.Imaging.WriteableBitmap $w, $h, 96, 96, ([System.Windows.Media.PixelFormats]::Pbgra32), $null
    $bitmap.WritePixels((New-Object System.Windows.Int32Rect 0, 0, $w, $h), $pixels, $stride, 0)
    $bitmap.Freeze()
    $bitmap
}

function Place($canvas, $element, [double]$x, [double]$y) {
    [System.Windows.Controls.Canvas]::SetLeft($element, $x)
    [System.Windows.Controls.Canvas]::SetTop($element, $y)
    $canvas.Children.Add($element) | Out-Null
}

function Shot([string]$name, [int]$margin, [int]$offsetX, [int]$offsetY, [double]$tint, [int]$cropHeight = 0, [string]$title = "") {
    $frame = Frame (Join-Path $Frames "$name.png") $cropHeight
    $bar = if ($title) { 48 } else { 0 }
    $fw = $frame.PixelWidth; $fh = $frame.PixelHeight + $bar
    $W = $fw + 2 * $margin; $H = $fh + 2 * $margin
    $radius = 12
    $wall = New-Object System.Windows.Media.Imaging.CroppedBitmap $script:wallpaper, (New-Object System.Windows.Int32Rect $offsetX, $offsetY, $W, $H)
    $canvas = New-Object System.Windows.Controls.Canvas
    $canvas.Width = $W; $canvas.Height = $H
    $back = New-Object System.Windows.Controls.Image; $back.Source = $wall; $back.Width = $W; $back.Height = $H
    Place $canvas $back 0 0
    # The shadow, under the window only: the glass above covers its middle.
    $shadow = New-Object System.Windows.Controls.Border
    $shadow.Width = $fw; $shadow.Height = $fh; $shadow.CornerRadius = $radius
    $shadow.Background = [System.Windows.Media.Brushes]::Black
    $drop = New-Object System.Windows.Media.Effects.DropShadowEffect
    $drop.BlurRadius = 60; $drop.ShadowDepth = 14; $drop.Direction = 270; $drop.Opacity = 0.6
    $shadow.Effect = $drop
    Place $canvas $shadow $margin $margin
    # The glass: the wallpaper under the window, blurred, tinted dark.
    $glass = New-Object System.Windows.Controls.Canvas
    $glass.Width = $W; $glass.Height = $H
    $glass.Clip = New-Object System.Windows.Media.RectangleGeometry (New-Object System.Windows.Rect $margin, $margin, $fw, $fh), $radius, $radius
    $blurred = New-Object System.Windows.Controls.Image; $blurred.Source = $wall; $blurred.Width = $W; $blurred.Height = $H
    $blur = New-Object System.Windows.Media.Effects.BlurEffect
    $blur.Radius = 70; $blur.KernelType = "Gaussian"; $blur.RenderingBias = "Quality"
    $blurred.Effect = $blur
    Place $glass $blurred 0 0
    $shade = New-Object System.Windows.Shapes.Rectangle
    $shade.Width = $W; $shade.Height = $H
    $shade.Fill = New-Object System.Windows.Media.SolidColorBrush ([System.Windows.Media.Color]::FromArgb([byte](255 * $tint), 24, 24, 28))
    Place $glass $shade 0 0
    Place $canvas $glass 0 0
    if ($title) {
        $text = New-Object System.Windows.Controls.TextBlock
        $text.Text = $title; $text.FontFamily = "Segoe UI Variable Text"; $text.FontSize = 18
        $text.Foreground = New-Object System.Windows.Media.SolidColorBrush ([System.Windows.Media.Color]::FromArgb(230, 255, 255, 255))
        Place $canvas $text ($margin + 24) ($margin + 12)
        $glyphs = [char]0xE921, [char]0xE922, [char]0xE8BB
        for ($i = 0; $i -lt 3; $i++) {
            $g = New-Object System.Windows.Controls.TextBlock
            $g.Text = $glyphs[$i]; $g.FontFamily = "Segoe Fluent Icons"; $g.FontSize = 15
            $g.Foreground = $text.Foreground
            Place $canvas $g ($margin + $fw - 207 + 69 * $i + 27) ($margin + 16)
        }
    }
    $image = New-Object System.Windows.Controls.Image; $image.Source = $frame
    $image.Width = $frame.PixelWidth; $image.Height = $frame.PixelHeight
    $image.Clip = New-Object System.Windows.Media.RectangleGeometry (New-Object System.Windows.Rect 0, (-$bar), $fw, $fh), $radius, $radius
    Place $canvas $image $margin ($margin + $bar)
    $bitmap = Render $canvas $W $H
    $encoder = New-Object System.Windows.Media.Imaging.PngBitmapEncoder
    $encoder.Frames.Add([System.Windows.Media.Imaging.BitmapFrame]::Create($bitmap))
    $stream = [IO.File]::Create((Join-Path $Out "$name.png"))
    $encoder.Save($stream); $stream.Close()
    "{0}: {1} x {2}" -f $name, $W, $H
}

New-Item -ItemType Directory -Force -Path $Out | Out-Null
$script:wallpaper = Wallpaper 2600 2200
Shot "search" 110 300 120 0.55
Shot "clipboard" 70 900 700 0.55
Shot "windows" 70 200 900 0.55
Shot "commands" 70 1300 300 0.55
Shot "calculator" 50 500 1500 0.55
Shot "currency" 50 1100 1600 0.55
Shot "web" 50 1300 1800 0.55
Shot "settings" 70 1400 200 0.78 1132 "hyalo settings"
