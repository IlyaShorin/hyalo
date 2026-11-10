# Captures the launcher window, or the settings window, plus a margin (to
# show the shadow and what the glass blurs) into a PNG, for checking the
# look without a human.
#
#   powershell -ExecutionPolicy Bypass -File scripts/snap.ps1 [-Out snap.png] [-Margin 48] [-Stripes] [-Type text] [-Settings] [-Exe path]
#
# The launcher is shown the way a user's second start shows it: by
# starting hyalo.exe again (-Exe, target/debug/hyalo.exe by default),
# which asks the running one to show, whatever its hotkey. -Settings
# captures the settings window: it presses the hotkey config.toml names
# first (only once it is sure hyalo holds it, so the keys reach no other
# app), which lets hyalo take the foreground as a key press would, then
# starts hyalo with --settings; the window opens in front, active.
# -Stripes puts bright colour stripes behind the launcher first, so the
# capture shows whether the glass really blurs what is underneath.
# -Type types into the launcher before the capture, which then covers the
# list too: {DOWN}, {UP}, {ENTER} and {ESC} press those keys, the rest is
# typed. Keys go to the window with SendMessageW, so they reach it
# whatever has the focus, and a slow debug search finishes before the
# capture.
param(
    [string]$Out = "target/snap.png",
    [int]$Margin = 48,
    [switch]$Stripes,
    [string]$Type = "",
    [switch]$Settings,
    [string]$Exe = "target/debug/hyalo.exe"
)
$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Snap {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr SendMessageW(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool RegisterHotKey(IntPtr hwnd, int id, uint modifiers, uint key);
    [DllImport("user32.dll")] public static extern bool UnregisterHotKey(IntPtr hwnd, int id);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
}
"@
[Snap]::SetProcessDPIAware() | Out-Null

$class = if ($Settings) { "hyalo.settings" } else { "hyalo.launcher" }

# [NullString]::Value, not $null: PowerShell turns $null into "" for a
# string parameter, and FindWindow would then look for an empty title.
function Find-Window { [Snap]::FindWindow($class, [NullString]::Value) }

function Get-Area($hwnd) {
    $r = New-Object Snap+RECT
    [Snap]::GetWindowRect($hwnd, [ref]$r) | Out-Null
    @{ X = $r.Left - $Margin; Y = $r.Top - $Margin
       W = $r.Right - $r.Left + 2 * $Margin; H = $r.Bottom - $r.Top + 2 * $Margin
       Bar = $r.Bottom - $r.Top }
}

# Starts hyalo again, which asks the running one to show (or to open its
# settings), and waits for the window.
function Show-Window {
    $arguments = if ($Settings) { @("--settings") } else { @() }
    Start-Process -FilePath $Exe -ArgumentList $arguments | Out-Null
    for ($i = 0; $i -lt 50; $i++) {
        $hwnd = Find-Window
        if ($hwnd -ne [IntPtr]::Zero -and [Snap]::IsWindowVisible($hwnd)) { return $hwnd }
        Start-Sleep -Milliseconds 100
    }
    throw "hyalo did not show $class"
}

# The hotkey config.toml names, as RegisterHotKey's modifiers and key.
function Get-Hotkey {
    $config = Join-Path $env:APPDATA "hyalo\config.toml"
    $name = "Alt+Space"
    if ((Test-Path $config) -and ((Get-Content -Raw $config) -match '(?m)^\s*hotkey\s*=\s*["'']([^"'']+)["'']')) { $name = $Matches[1] }
    $bits = @{ "alt" = 1; "ctrl" = 2; "control" = 2; "shift" = 4; "win" = 8; "windows" = 8 }
    $modifiers = 0; $key = 0
    foreach ($part in $name.Split("+") | ForEach-Object { $_.Trim() } | Where-Object { $_ }) {
        $lower = $part.ToLower()
        if ($bits.ContainsKey($lower)) { $modifiers = $modifiers -bor $bits[$lower] }
        elseif ($lower -eq "space") { $key = 0x20 }
        elseif ($lower -match "^f([0-9]+)$") { $key = 0x6F + [int]$Matches[1] }
        elseif ($part.Length -eq 1) { $key = [int][char]$part.ToUpper() }
        else { throw "snap.ps1 cannot press $name" }
    }
    @{ Name = $name; Modifiers = $modifiers; Key = $key }
}

# Presses the hotkey, once sure someone holds it (hyalo, as it runs here):
# a key no one holds would go to whatever app is in front.
function Press-Hotkey {
    $hotkey = Get-Hotkey
    if ([Snap]::RegisterHotKey([IntPtr]::Zero, 0x5A5A, $hotkey.Modifiers, $hotkey.Key)) {
        [Snap]::UnregisterHotKey([IntPtr]::Zero, 0x5A5A) | Out-Null
        throw "no one holds $($hotkey.Name): not pressing it"
    }
    $held = @(@(8, 0x5B), @(2, 0x11), @(1, 0x12), @(4, 0x10)) | Where-Object { $hotkey.Modifiers -band $_[0] }
    foreach ($m in $held) { [Snap]::keybd_event($m[1], 0, 0, [UIntPtr]::Zero) }
    [Snap]::keybd_event($hotkey.Key, 0, 0, [UIntPtr]::Zero)
    [Snap]::keybd_event($hotkey.Key, 0, 2, [UIntPtr]::Zero)
    foreach ($m in $held) { [Snap]::keybd_event($m[1], 0, 2, [UIntPtr]::Zero) }
    Start-Sleep -Milliseconds 300
}

if ($Settings) {
    $launcher = [Snap]::FindWindow("hyalo.launcher", [NullString]::Value)
    if ($launcher -ne [IntPtr]::Zero -and -not [Snap]::IsWindowVisible($launcher)) { Press-Hotkey }
}
$hwnd = Find-Window
$form = $null
if ($Stripes -and -not $Settings) {
    if ($hwnd -eq [IntPtr]::Zero) { $hwnd = Show-Window }
    $area = Get-Area $hwnd
    $x = $area.X; $y = $area.Y; $w = $area.W; $h = $area.H
    # Typing grows the window: stripes cover the tallest list (64 + 16 + 8 * 48
    # DIPs against the bar's 64), assuming the window shows the bar alone now.
    if ($Type) { $h = [int]($area.Bar * 464 / 64) + 2 * $Margin }
    $form = New-Object System.Windows.Forms.Form
    # TopMost: a background process may not raise its windows above the
    # active one. The launcher shows after this, so it still ends up on top.
    $form.FormBorderStyle = "None"; $form.ShowInTaskbar = $false; $form.TopMost = $true
    $form.StartPosition = "Manual"; $form.Bounds = New-Object System.Drawing.Rectangle $x, $y, $w, $h
    $colors = "#FF3B30", "#FF9500", "#FFCC00", "#34C759", "#007AFF", "#AF52DE", "#FFFFFF", "#000000"
    $stripe = [Math]::Ceiling($w / $colors.Count)
    for ($i = 0; $i -lt $colors.Count; $i++) {
        $panel = New-Object System.Windows.Forms.Panel
        $panel.BackColor = [System.Drawing.ColorTranslator]::FromHtml($colors[$i])
        $panel.Bounds = New-Object System.Drawing.Rectangle ($i * $stripe), 0, $stripe, $h
        $form.Controls.Add($panel)
    }
    $form.Show(); [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 200
}
if ($hwnd -eq [IntPtr]::Zero -or -not [Snap]::IsWindowVisible($hwnd) -or $form -or $Settings) { $hwnd = Show-Window }
if ($Settings) {
    for ($i = 0; $i -lt 30 -and [Snap]::GetForegroundWindow() -ne $hwnd; $i++) { Start-Sleep -Milliseconds 100 }
    if ([Snap]::GetForegroundWindow() -ne $hwnd) { throw "the settings window is not in front" }
}
Start-Sleep -Milliseconds 500

if ($Type) {
    $keys = @{ "DOWN" = 0x28; "UP" = 0x26; "ENTER" = 0x0D; "ESC" = 0x1B }
    foreach ($part in [regex]::Split($Type, "(\{[A-Z]+\})")) {
        if ($part -match "^\{([A-Z]+)\}$") {
            [Snap]::SendMessageW($hwnd, 0x0100, [IntPtr]$keys[$Matches[1]], [IntPtr]::Zero) | Out-Null
        } else {
            foreach ($unit in $part.ToCharArray()) {
                [Snap]::SendMessageW($hwnd, 0x0102, [IntPtr][int]$unit, [IntPtr]::Zero) | Out-Null
            }
        }
    }
    Start-Sleep -Milliseconds 400
}

$area = Get-Area $hwnd
$x = $area.X; $y = $area.Y; $w = $area.W; $h = $area.H
$bitmap = New-Object System.Drawing.Bitmap $w, $h
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($x, $y, 0, 0, $bitmap.Size)
New-Item -ItemType Directory -Force -Path (Split-Path $Out) | Out-Null
$bitmap.Save((Join-Path (Get-Location) $Out), [System.Drawing.Imaging.ImageFormat]::Png)
if ($form) { $form.Close() }
"saved $Out (${w}x${h} at $x,$y)"
