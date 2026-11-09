# Captures the launcher window (plus a margin, to show the shadow and what
# the glass blurs) into a PNG, for checking the look without a human.
#
#   powershell -ExecutionPolicy Bypass -File scripts/snap.ps1 [-Out snap.png] [-Margin 48] [-Toggle] [-Stripes]
#
# -Toggle presses the hotkey (Alt+Space) twice first, so the window comes
# back through the real show path and owns the foreground.
# -Stripes puts bright colour stripes behind the launcher first, so the
# capture shows whether the glass really blurs what is underneath.
param(
    [string]$Out = "target/snap.png",
    [int]$Margin = 48,
    [switch]$Toggle,
    [switch]$Stripes
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
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    public static void AltSpace() {
        keybd_event(0x12, 0, 0, UIntPtr.Zero);  // Alt down
        keybd_event(0x20, 0, 0, UIntPtr.Zero);  // Space down
        keybd_event(0x20, 0, 2, UIntPtr.Zero);  // Space up
        keybd_event(0x12, 0, 2, UIntPtr.Zero);  // Alt up
    }
}
"@
[Snap]::SetProcessDPIAware() | Out-Null

# [NullString]::Value, not $null: PowerShell turns $null into "" for a
# string parameter, and FindWindow would then look for an empty title.
$hwnd = [Snap]::FindWindow("hyalo.launcher", [NullString]::Value)
if ($hwnd -eq [IntPtr]::Zero) { throw "hyalo is not running" }

$r = New-Object Snap+RECT
[Snap]::GetWindowRect($hwnd, [ref]$r) | Out-Null
$x = $r.Left - $Margin; $y = $r.Top - $Margin
$w = $r.Right - $r.Left + 2 * $Margin; $h = $r.Bottom - $r.Top + 2 * $Margin

$form = $null
if ($Stripes) {
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

if ($Toggle -or $Stripes) {
    [Snap]::AltSpace(); Start-Sleep -Milliseconds 300
    if (-not [Snap]::IsWindowVisible($hwnd)) { [Snap]::AltSpace() }
    Start-Sleep -Milliseconds 500
}
if (-not [Snap]::IsWindowVisible($hwnd)) { if ($form) { $form.Close() }; throw "hyalo is hidden" }

$bitmap = New-Object System.Drawing.Bitmap $w, $h
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($x, $y, 0, 0, $bitmap.Size)
New-Item -ItemType Directory -Force -Path (Split-Path $Out) | Out-Null
$bitmap.Save((Join-Path (Get-Location) $Out), [System.Drawing.Imaging.ImageFormat]::Png)
if ($form) { $form.Close() }
"saved $Out (${w}x${h} at $x,$y)"
