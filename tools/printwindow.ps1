# 用 PrintWindow(PW_RENDERFULLCONTENT) 抓取窗口自身内容（不受遮挡影响，适合 UI 验收截图）
# 用法：
#   .\tools\printwindow.ps1 -Out "E:\...\target\ui-manager.png" -ProcessName clipnest
#   .\tools\printwindow.ps1 -Out "..." -WindowTitle "ClipNest"
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$ProcessName = "clipnest",
    [string]$WindowTitle = ""
)

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class WinPrint {
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

$hwnd = [IntPtr]::Zero
if ($WindowTitle) {
    $p = Get-Process | Where-Object { $_.MainWindowTitle -like "*$WindowTitle*" } | Select-Object -First 1
    if ($p) { $hwnd = $p.MainWindowHandle }
} else {
    $p = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
    if ($p) { $hwnd = $p.MainWindowHandle }
}
if ($hwnd -eq [IntPtr]::Zero) { Write-Error "找不到窗口（进程 $ProcessName）"; exit 1 }

$r = New-Object WinPrint+RECT
[WinPrint]::GetWindowRect($hwnd, [ref]$r) | Out-Null
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
if ($w -le 0 -or $h -le 0) { Write-Error "无效窗口尺寸"; exit 1 }

$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok = [WinPrint]::PrintWindow($hwnd, $hdc, 2)  # PW_RENDERFULLCONTENT
$g.ReleaseHdc($hdc)
$g.Dispose()

$full = [System.IO.Path]::GetFullPath($Out)
$dir = [System.IO.Path]::GetDirectoryName($full)
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
$bmp.Save($full, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Host "printwindow saved: $full (${w}x${h}) hwnd=$hwnd ok=$ok"
