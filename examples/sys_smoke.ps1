# Sys 域最小冒烟测试：真实启动 clipnest.exe（临时数据目录），验证
#   1) Sys 线程启动 / 隐藏窗口 / 剪贴板监听
#   2) 备用快捷键冲突降级（本机 Ctrl+` 被 Ditto 占用 → Ctrl+Shift+V）
#   3) Win+V L2 钩子状态
#   4) 托盘创建
#   5) 复制文本 → Event::Captured → 日志（不入真实用户库）
#   6) 备用热键唤起 Picker（注入 Ctrl+Shift+V）+ 截自身窗口图 + Esc 关闭
#
# 用法：pwsh -NoProfile -ExecutionPolicy Bypass -File examples\sys_smoke.ps1
# 安全：使用临时 CLIPNEST_DATA_DIR；测试结束 kill 进程（热键/钩子由 OS 随进程回收）。
param(
    [string]$Exe = "",
    [string]$OutDir = "", 
    [switch]$Quiet   # 静默模式：不注入全局按键、不改系统剪贴板（回归专用）
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if (-not $Exe) { $Exe = Join-Path $Root "target\debug\clipnest.exe" }
if (-not $OutDir) { $OutDir = Join-Path $Root "target\sys-smoke" }
$DataDir = Join-Path $OutDir "data"
Remove-Item $OutDir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
$LogPath = Join-Path $DataDir "logs\app.log"

if (-not (Test-Path $Exe)) { throw "找不到 clipnest.exe：$Exe（请先 cargo build）" }

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class P0Sys {
    [StructLayout(LayoutKind.Explicit, Size = 40)]
    public struct INPUT {
        [FieldOffset(0)] public uint type;
        [FieldOffset(8)] public ushort wVk;
        [FieldOffset(10)] public ushort wScan;
        [FieldOffset(12)] public uint dwFlags;
        [FieldOffset(16)] public uint time;
        [FieldOffset(24)] public IntPtr dwExtraInfo;
    }
    [DllImport("user32.dll", SetLastError = true)]
    public static extern uint SendInput(uint n, INPUT[] p, int size);
}
'@

function Send-Vk([int]$Vk, [bool]$KeyUp) {
    $inp = New-Object P0Sys+INPUT
    $inp.type = [uint32]1
    $inp.wVk = [uint16]$Vk
    $inp.dwFlags = if ($KeyUp) { [uint32]2 } else { [uint32]0 }
    $arr = New-Object P0Sys+INPUT[] 1
    $arr[0] = $inp
    $n = [P0Sys]::SendInput(1, $arr, 40)
    if ($n -ne 1) { throw ("SendInput 失败 vk={0}" -f $Vk) }
}

function Send-CtrlShiftV {
    Send-Vk 0x11 $false; Send-Vk 0x10 $false
    Start-Sleep -Milliseconds 30
    Send-Vk 0x56 $false; Start-Sleep -Milliseconds 30
    Send-Vk 0x56 $true; Start-Sleep -Milliseconds 30
    Send-Vk 0x10 $true; Send-Vk 0x11 $true
}

function Send-Escape { Send-Vk 0x1B $false; Start-Sleep -Milliseconds 20; Send-Vk 0x1B $true }

$results = [ordered]@{}
try {
    Write-Host "启动 clipnest.exe（临时数据目录 $DataDir）…"
    $env:CLIPNEST_DATA_DIR = $DataDir
    $proc = Start-Process -FilePath $Exe -PassThru `
        -RedirectStandardOutput (Join-Path $OutDir "stdout.log") `
        -RedirectStandardError (Join-Path $OutDir "stderr.log")
    try {
        Start-Sleep -Seconds 3

        if (-not (Test-Path $LogPath)) { throw "启动 3s 后没有日志文件：$LogPath" }
        $log = Get-Content $LogPath -Raw

        $results["sys_thread_started"] = [bool]($log -match 'Sys 线程已启动，隐藏窗口 hwnd=')
        $results["clipboard_listener"] = [bool]($log -match '已注册剪贴板监听')
        $hotkeyLine = ($log -split "`n" | Where-Object { $_ -match '备用快捷键：' } | Select-Object -Last 1)
        $results["hotkey_line"] = $hotkeyLine
        $results["hotkey_ok"] = [bool]($log -match '备用快捷键：([^\n]*?) ok=true')
        $results["hotkey_fallback"] = [bool]($log -match '生效=Ctrl\+Shift\+V')
        $winvLine = ($log -split "`n" | Where-Object { $_ -match 'Win\+V 状态：' } | Select-Object -Last 1)
        $results["winv_line"] = $winvLine
        $results["winv_hook"] = [bool]($log -match 'Win\+V 状态：键盘钩子生效')
        $results["tray_ok"] = [bool]($log -match '托盘创建成功')

        if ($Quiet) {
            $results["quiet_mode"] = "跳过剪贴板/按键操作（-Quiet）"
            Write-Host "Quiet 模式：跳过 Set-Clipboard / 热键注入 / 截图"
        } else {
        # 复制文本 → 采集
        $text = "ClipNest sys smoke $(Get-Date -Format 'HH:mm:ss.fff')"
        Set-Clipboard -Value $text
        Start-Sleep -Milliseconds 900
        $log = Get-Content $LogPath -Raw
        $captured = ($log -split "`n" | Where-Object { $_ -match '采集剪贴板：' } | Select-Object -Last 1)
        $results["captured_line"] = $captured
        $results["captured_text"] = [bool]($log -match '采集剪贴板：类型=文本')

        # 备用热键唤起 Picker（全局热键，不依赖焦点）
        Send-CtrlShiftV
        Start-Sleep -Milliseconds 1200
        $log = Get-Content $LogPath -Raw
        $pickLine = ($log -split "`n" | Where-Object { $_ -match '呼出面板' } | Select-Object -Last 1)
        $results["picker_line"] = $pickLine
        $results["picker_requested"] = [bool]($log -match '呼出面板（备用快捷键）')

        # 检查窗口是否可见 + 截自身窗口
        $proc.Refresh()
        $results["main_window_handle"] = [string]$proc.MainWindowHandle
        $results["window_visible"] = ($proc.MainWindowHandle -ne [IntPtr]::Zero)
        if ($proc.MainWindowHandle -ne [IntPtr]::Zero) {
            $shot = Join-Path $OutDir "picker.png"
            & (Join-Path $Root "tools\screenshot.ps1") -ProcessName "clipnest" -Out $shot 2>$null | Out-Null
            $results["screenshot"] = $shot
            $results["screenshot_exists"] = (Test-Path $shot)
        }

        # Esc 关闭 Picker（不退出程序），随后验证进程仍存活
        Send-Escape
        Start-Sleep -Milliseconds 600
        $proc.Refresh()
        $results["alive_after_esc"] = -not $proc.HasExited
        }
    } finally {
        # 结束进程（Sys 资源随进程回收；不长期占用热键/钩子）
        if (-not $proc.HasExited) {
            Start-Sleep -Milliseconds 300
            try { $proc.Kill() } catch {}
            $proc.WaitForExit(5000) | Out-Null
        }
        Remove-Item Env:CLIPNEST_DATA_DIR -ErrorAction SilentlyContinue
    }
} catch {
    $results["error"] = "$_"
} finally {
    Write-Host "`n===== Sys 冒烟结果 =====" -ForegroundColor Yellow
    foreach ($key in $results.Keys) { Write-Host ("{0} = {1}" -f $key, $results[$key]) }
    $results | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $OutDir "result.json") -Encoding UTF8
    Write-Host "`n日志：$LogPath"
}
