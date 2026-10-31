# 截取当前桌面（或指定窗口）画面，用于验证 GUI 渲染效果。
# 用法：
#   .\tools\screenshot.ps1 -Out shot.png                # 全屏
#   .\tools\screenshot.ps1 -Out shot.png -WindowTitle "右键菜单管理器"   # 指定窗口
param(
    [string]$Out = "shot.png",
    [string]$WindowTitle = "",
    [string]$ProcessName = ""
)

Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Shot {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

$rect = $null
$hwnd = [IntPtr]::Zero

if ($ProcessName) {
    $p = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
    if ($p) { $hwnd = $p.MainWindowHandle }
} elseif ($WindowTitle) {
    $p = Get-Process | Where-Object { $_.MainWindowTitle -like "*$WindowTitle*" } | Select-Object -First 1
    if ($p) { $hwnd = $p.MainWindowHandle }
}

if ($hwnd -ne [IntPtr]::Zero) {
    [void][Win32Shot]::ShowWindow($hwnd, 9)   # SW_RESTORE
    [void][Win32Shot]::SetForegroundWindow($hwnd)
    Start-Sleep -Milliseconds 700
    $r = New-Object Win32Shot+RECT
    if ([Win32Shot]::GetWindowRect($hwnd, [ref]$r)) {
        $rect = @{ X = $r.Left; Y = $r.Top; W = ($r.Right - $r.Left); H = ($r.Bottom - $r.Top) }
    }
}

if ($null -eq $rect) {
    Add-Type -AssemblyName System.Windows.Forms
    $vs = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $rect = @{ X = $vs.Left; Y = $vs.Top; W = $vs.Width; H = $vs.Height }
}

if ($rect.W -le 0 -or $rect.H -le 0) { Write-Error "无效的截图区域"; exit 1 }

$bmp = New-Object System.Drawing.Bitmap $rect.W, $rect.H
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($rect.X, $rect.Y, 0, 0, (New-Object System.Drawing.Size $rect.W, $rect.H))
$full = [System.IO.Path]::GetFullPath($Out)
$dir = [System.IO.Path]::GetDirectoryName($full)
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
$bmp.Save($full, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Host "saved: $full ($($rect.W)x$($rect.H)) hwnd=$hwnd"
