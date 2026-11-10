# Writes each reference image (or failed frame) over a solid backdrop, for
# looking at it before it is blessed. The PNGs hold the frames' colours
# premultiplied by their alpha, as the swap chain does, so an image viewer,
# which takes them as straight alpha, shows them too dark; this adds the
# backdrop the way DWM composes: colour + backdrop * (1 - alpha).
#
#   powershell -ExecutionPolicy Bypass -File scripts/frames.ps1 -Files crates/hyalo/reference/settings-100.png,target/frames/list-150.png
#
# Writes target/look/<name>-over.png; -R -G -B pick the backdrop (a dark
# grey by default, near the dark Mica and Acrylic).
param([string]$Files, [string]$Out = "target/look", [int]$R = 32, [int]$G = 32, [int]$B = 32)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @"
using System; using System.Drawing; using System.Drawing.Imaging; using System.Runtime.InteropServices;
public static class Over {
  public static Bitmap Run(string path, int r, int g, int b) {
    var src = new Bitmap(path);
    var w = src.Width; var h = src.Height;
    var dst = new Bitmap(w, h, PixelFormat.Format24bppRgb);
    var s = src.LockBits(new Rectangle(0, 0, w, h), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
    var d = dst.LockBits(new Rectangle(0, 0, w, h), ImageLockMode.WriteOnly, PixelFormat.Format24bppRgb);
    var sb = new byte[s.Stride * h]; var db = new byte[d.Stride * h];
    Marshal.Copy(s.Scan0, sb, 0, sb.Length);
    for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
      int i = y * s.Stride + x * 4; int j = y * d.Stride + x * 3; int a = sb[i + 3];
      db[j]     = (byte)Math.Min(255, sb[i]     + b * (255 - a) / 255);
      db[j + 1] = (byte)Math.Min(255, sb[i + 1] + g * (255 - a) / 255);
      db[j + 2] = (byte)Math.Min(255, sb[i + 2] + r * (255 - a) / 255);
    }
    Marshal.Copy(db, 0, d.Scan0, db.Length);
    src.UnlockBits(s); dst.UnlockBits(d); src.Dispose();
    return dst;
  }
}
"@
New-Item -ItemType Directory -Force -Path $Out | Out-Null
foreach ($file in ($Files -split ",")) {
  $bitmap = [Over]::Run((Resolve-Path $file.Trim()).Path, $R, $G, $B)
  $name = [IO.Path]::GetFileNameWithoutExtension($file.Trim()) + "-over.png"
  $bitmap.Save((Join-Path (Resolve-Path $Out).Path $name), [Drawing.Imaging.ImageFormat]::Png)
  $bitmap.Dispose()
  "wrote $Out/$name"
}
