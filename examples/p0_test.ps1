# ClipNest P0 能力验证测试脚本（配合 examples/p0_spike.rs）
#
# 用法（Windows PowerShell 5.1 或 PowerShell 7 均可；本文件为 UTF-8 BOM）：
#   pwsh -NoProfile -ExecutionPolicy Bypass -File examples\p0_test.ps1
#   pwsh -NoProfile -ExecutionPolicy Bypass -File examples\p0_test.ps1 -SkipGui
#
# 验证项：
#   A. 窗口截图（仅截 spike 自身窗口）+ RegisterHotKey（Ctrl+` / Win+V / 测试组合）结果提示
#   B. WH_KEYBOARD_LL 吞 Win+V（swallow_v）：开始菜单是否误弹、Win 键状态
#   C. WH_KEYBOARD_LL 吞 Win+V + 补偿吞 Win up（swallow_v_winup）：开始菜单是否误弹
#   D. tray-icon 创建 + 模拟托盘点击后菜单能否响应（headless，Sys 线程创建）
#
# 安全约定：键盘注入仅发给 spike 自身窗口（无法聚焦时会显示警告）；不截全桌面；
#           每次运行结束清理钩子/热键/托盘。
param(
    [string]$Exe = "",
    [string]$OutDir = "",
    [switch]$SkipGui,
    [switch]$SkipHook
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if (-not $Exe) { $Exe = Join-Path $Root "target\debug\examples\p0_spike.exe" }
if (-not $OutDir) { $OutDir = Join-Path $Root "target\p0" }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
if (-not (Test-Path $Exe)) {
    throw "找不到 spike 可执行文件：$Exe（请先运行 cargo build --example p0_spike）"
}

Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class P0Native {
    // x64 下 INPUT = type(4)+pad(4)+union(32) = 40 字节。用扁平显式布局直接编码键盘事件，
    // 避免 PowerShell 无法原地修改嵌套结构体字段的问题（尺寸错会得到 ERROR_INVALID_PARAMETER）。
    [StructLayout(LayoutKind.Explicit, Size = 40)]
    public struct INPUT {
        [FieldOffset(0)] public uint type;
        [FieldOffset(8)] public ushort wVk;
        [FieldOffset(10)] public ushort wScan;
        [FieldOffset(12)] public uint dwFlags;
        [FieldOffset(16)] public uint time;
        [FieldOffset(24)] public IntPtr dwExtraInfo;
    }

    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern uint SendInput(uint nInputs, INPUT[] pInputs, int cbSize);
    [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int vKey);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr SetFocus(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hWnd, StringBuilder buf, int max);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder buf, int max);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);

    /// 枚举可见顶层窗口，返回 "pid|class|title" 列表。
    public static List<string> VisibleWindows() {
        var list = new List<string>();
        EnumWindows(delegate(IntPtr h, IntPtr l) {
            if (!IsWindowVisible(h)) return true;
            var cls = new StringBuilder(256);
            GetClassName(h, cls, 256);
            var txt = new StringBuilder(512);
            GetWindowText(h, txt, 512);
            uint pid;
            GetWindowThreadProcessId(h, out pid);
            list.Add(pid.ToString() + "|" + cls.ToString() + "|" + txt.ToString());
            return true;
        }, IntPtr.Zero);
        return list;
    }
}
'@

# ---------------------------------------------------------------------------
# 注入按键（SendInput）
# ---------------------------------------------------------------------------

function Send-Vk {
    param([int]$Vk, [bool]$KeyUp)
    $inp = New-Object P0Native+INPUT
    $inp.type = [uint32]1
    $inp.wVk = [uint16]$Vk
    $inp.dwFlags = if ($KeyUp) { [uint32]2 } else { [uint32]0 }
    $arr = New-Object P0Native+INPUT[] 1
    $arr[0] = $inp
    $size = [System.Runtime.InteropServices.Marshal]::SizeOf($inp)
    if ($size -ne 40) { throw "INPUT 结构体尺寸异常：$size（期望 40）" }
    $sent = [P0Native]::SendInput(1, $arr, $size)
    if ($sent -ne 1) {
        throw ("SendInput 失败 vk=0x{0:X2} err={1}" -f $Vk, [System.Runtime.InteropServices.Marshal]::GetLastWin32Error())
    }
}

function Send-WinV {
    Send-Vk 0x5B $false; Start-Sleep -Milliseconds 30
    Send-Vk 0x56 $false; Start-Sleep -Milliseconds 30
    Send-Vk 0x56 $true;  Start-Sleep -Milliseconds 30
    Send-Vk 0x5B $true
}

function Send-CtrlBacktick {
    Send-Vk 0x11 $false; Start-Sleep -Milliseconds 25
    Send-Vk 0xC0 $false; Start-Sleep -Milliseconds 25
    Send-Vk 0xC0 $true;  Start-Sleep -Milliseconds 25
    Send-Vk 0x11 $true
}

# Ctrl+Alt+Shift+F10（本机 Ctrl+` 被 Ditto 占用，用空闲组合验证热键通路）
function Send-TestHotkey {
    Send-Vk 0x11 $false; Send-Vk 0x12 $false; Send-Vk 0x10 $false
    Start-Sleep -Milliseconds 25
    Send-Vk 0x79 $false; Start-Sleep -Milliseconds 25
    Send-Vk 0x79 $true;  Start-Sleep -Milliseconds 25
    Send-Vk 0x10 $true; Send-Vk 0x12 $true; Send-Vk 0x11 $true
}

function Send-Escape { Send-Vk 0x1B $false; Start-Sleep -Milliseconds 20; Send-Vk 0x1B $true }

# 注入一次「Win 单击」（用于探测注入的 Win 键能否触发开始菜单；也用于清掉粘住的 Win 状态）
function Send-WinTap {
    Send-Vk 0x5B $false; Start-Sleep -Milliseconds 60
    Send-Vk 0x5B $true
}

function Reset-WinKeyState {
    if (Get-WinKeyState) {
        Send-Vk 0x5B $true
        Start-Sleep -Milliseconds 150
    }
}

# ---------------------------------------------------------------------------
# 窗口 / 前台 / 系统 UI 探测
# ---------------------------------------------------------------------------

function Get-ForegroundHwnd { [P0Native]::GetForegroundWindow() }

function Focus-Window {
    param([IntPtr]$Hwnd)
    if ($Hwnd -eq [IntPtr]::Zero) { return $false }
    if ((Get-ForegroundHwnd) -eq $Hwnd) { return $true }
    [void][P0Native]::ShowWindow($Hwnd, 9)   # SW_RESTORE
    [void][P0Native]::SetForegroundWindow($Hwnd)
    Start-Sleep -Milliseconds 150
    if ((Get-ForegroundHwnd) -eq $Hwnd) { return $true }
    # 前台锁定：AttachThreadInput 到当前前台线程后再 BringWindowToTop / SetForegroundWindow
    $fg = Get-ForegroundHwnd
    $fgPid = [uint32]0
    $fgThread = [P0Native]::GetWindowThreadProcessId($fg, [ref]$fgPid)
    $my = [P0Native]::GetCurrentThreadId()
    [void][P0Native]::AttachThreadInput($my, $fgThread, $true)
    try {
        [void][P0Native]::ShowWindow($Hwnd, 9)
        [void][P0Native]::SetForegroundWindow($Hwnd)
        [void][P0Native]::BringWindowToTop($Hwnd)
        [void][P0Native]::SetFocus($Hwnd)
        Start-Sleep -Milliseconds 250
    } finally {
        [void][P0Native]::AttachThreadInput($my, $fgThread, $false)
    }
    return ((Get-ForegroundHwnd) -eq $Hwnd)
}

# 系统 Shell 弹出层（开始菜单 / 剪贴板历史 / 搜索）——按进程名识别
$ShellProcessNames = @('StartMenuExperienceHost', 'ShellExperienceHost', 'TextInputHost', 'SearchHost', 'SearchApp', 'explorer')
function Get-ShellFlyouts {
    $result = @()
    foreach ($entry in [P0Native]::VisibleWindows()) {
        $parts = $entry.Split('|', 3)
        $procId = 0
        if (-not [int]::TryParse($parts[0], [ref]$procId)) { continue }
        $proc = Get-Process -Id $procId -ErrorAction SilentlyContinue
        if (-not $proc) { continue }
        if ($ShellProcessNames -contains $proc.ProcessName) {
            $result += ("{0}|{1}|{2}" -f $proc.ProcessName, $parts[1], $parts[2])
        }
    }
    return $result
}

function Test-StartMenuOpen {
    $flyouts = Get-ShellFlyouts
    return [bool]($flyouts | Where-Object { $_ -like 'StartMenuExperienceHost|*' -or $_ -like 'SearchHost|*' })
}

function Get-WinKeyState {
    $state = [P0Native]::GetAsyncKeyState(0x5B)
    return [bool]($state -band 0x8000)
}

# ---------------------------------------------------------------------------
# spike 进程管理
# ---------------------------------------------------------------------------

function Start-Spike {
    param([string]$Name, [string[]]$ExtraArgs)
    $log = Join-Path $OutDir "$Name.log"
    $ready = Join-Path $OutDir "$Name.ready"
    $result = Join-Path $OutDir "$Name.json"
    $stdout = Join-Path $OutDir "$Name.stdout.log"
    $stderr = Join-Path $OutDir "$Name.stderr.log"
    Remove-Item $log, $ready, $result, $stdout, $stderr -ErrorAction SilentlyContinue
    $allArgs = @("--log", $log, "--ready", $ready, "--result", $result) + $ExtraArgs
    $proc = Start-Process -FilePath $Exe -ArgumentList $allArgs -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    return [pscustomobject]@{
        Name = $Name; Proc = $proc; Log = $log; Ready = $ready; Result = $result
    }
}

function Wait-Ready {
    param($Spike, [int]$TimeoutSec = 20)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path $Spike.Ready) { return $true }
        if ($Spike.Proc.HasExited) {
            throw "spike 提前退出：$($Spike.Name) exit=$($Spike.Proc.ExitCode)，日志见 $($Spike.Log)"
        }
        Start-Sleep -Milliseconds 80
    }
    throw "等待 READY 超时：$($Spike.Name)"
}

function Wait-Log {
    param($Spike, [string]$Pattern, [int]$TimeoutSec = 6)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path $Spike.Log) {
            $text = Get-Content $Spike.Log -Raw -ErrorAction SilentlyContinue
            if ($text -and $text -match $Pattern) { return $text }
        }
        Start-Sleep -Milliseconds 80
    }
    return $null
}

function Get-LogLine {
    param($Spike, [string]$Pattern)
    if (-not (Test-Path $Spike.Log)) { return "" }
    $line = Get-Content $Spike.Log -ErrorAction SilentlyContinue | Where-Object { $_ -match $Pattern } | Select-Object -Last 1
    return $line
}

function Wait-Exit {
    param($Spike, [int]$TimeoutSec = 40)
    if (-not $Spike.Proc.WaitForExit($TimeoutSec * 1000)) {
        try { $Spike.Proc.Kill() } catch {}
        throw "spike 未按时退出（已强制结束）：$($Spike.Name)"
    }
    return $Spike.Proc.ExitCode
}

function Read-Result {
    param($Spike)
    if (Test-Path $Spike.Result) {
        return (Get-Content $Spike.Result -Raw | ConvertFrom-Json)
    }
    return $null
}

function Wait-MainWindow {
    param($Proc, [int]$TimeoutSec = 8)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        $Proc.Refresh()
        if ($Proc.MainWindowHandle -ne [IntPtr]::Zero) { return $Proc.MainWindowHandle }
        Start-Sleep -Milliseconds 100
    }
    return [IntPtr]::Zero
}

# ---------------------------------------------------------------------------
# 测试 A：热键 + 托盘（主线程）+ 窗口截图
# ---------------------------------------------------------------------------

function Test-AcrylicAndHotkey {
    Write-Host "`n=== A. 窗口 / RegisterHotKey / 托盘（eframe 主线程） ===" -ForegroundColor Cyan
    $spike = Start-Spike "a_hotkey" @("--seconds", "7", "--register-ctrl", "--register-winv", "--register-test")
    Wait-Ready $spike
    $hwnd = Wait-MainWindow $spike.Proc
    Write-Host "主窗口 HWND = $hwnd"

    $shotPath = Join-Path $OutDir "p0-spike-window.png"
    if ($hwnd -ne [IntPtr]::Zero) {
        # 只截 spike 自身窗口，不截全桌面
        & (Join-Path $Root "tools\screenshot.ps1") -ProcessName "p0_spike" -Out $shotPath | Out-Host
    } else {
        Write-Warning "未找到 spike 主窗口，跳过截图"
    }

    [void](Wait-Log $spike 'HOTKEY_REG\|id=3' 5)
    $winvOk = (Get-LogLine $spike 'HOTKEY_REG\|id=2') -match 'result=ok'
    $ctrlOk = (Get-LogLine $spike 'HOTKEY_REG\|id=1') -match 'result=ok'
    $testOk = (Get-LogLine $spike 'HOTKEY_REG\|id=3') -match 'result=ok'
    Write-Host ("Ctrl+` 注册：{0}" -f (Get-LogLine $spike 'HOTKEY_REG\|id=1'))
    Write-Host ("Win+V  注册：{0}" -f (Get-LogLine $spike 'HOTKEY_REG\|id=2'))
    Write-Host ("测试组合注册：{0}" -f (Get-LogLine $spike 'HOTKEY_REG\|id=3'))

    $focused = Focus-Window $hwnd
    Write-Host "spike 窗口获得焦点：$focused"
    $hitCtrl = $false
    $hitWinV = $false
    $hitTest = $false
    # 探针：注入 Win 单击，观察系统是否弹开始菜单（判断注入 Win 键能否复现开始菜单行为）
    $winTapStartMenu = $false
    if ($focused) {
        Send-WinTap
        Start-Sleep -Milliseconds 700
        $winTapStartMenu = Test-StartMenuOpen
        if ($winTapStartMenu) { Send-Escape; Start-Sleep -Milliseconds 300 }
        Reset-WinKeyState
        Write-Host "注入 Win 单击 → 开始菜单弹出：$winTapStartMenu"
    }
    if ($focused) {
        if ($testOk) {
            Send-TestHotkey
            $hitTest = [bool](Wait-Log $spike 'HOTKEY_HIT\|id=3' 4)
        }
        if ($ctrlOk) {
            Send-CtrlBacktick
            $hitCtrl = [bool](Wait-Log $spike 'HOTKEY_HIT\|id=1' 4)
        }
        if ($winvOk) {
            Send-WinV
            $hitWinV = [bool](Wait-Log $spike 'HOTKEY_HIT\|id=2' 4)
        }
    } else {
        Write-Warning "无法让 spike 窗口取得焦点，跳过注入按键测试（避免误发到用户窗口）"
    }

    $startMenuAfter = Test-StartMenuOpen
    [void](Wait-Exit $spike)
    return [pscustomobject]@{
        test = "A: window/hotkey/tray"
        window_hwnd = [string]$hwnd
        screenshot = $shotPath
        focused = $focused
        hotkey_ctrl_registered = $ctrlOk
        hotkey_ctrl_hit = $hitCtrl
        hotkey_test_registered = $testOk
        hotkey_test_hit = $hitTest
        winv_l1_registered = $winvOk
        winv_l1_hit = $hitWinV
        win_tap_opens_startmenu = $winTapStartMenu
        start_menu_after = $startMenuAfter
        result = (Read-Result $spike)
    }
}

# ---------------------------------------------------------------------------
# 测试 B/C：Win+V 低级键盘钩子（吞键 / 开始菜单补偿）
# ---------------------------------------------------------------------------

function Test-WinVHook {
    param([string]$Mode)
    Write-Host "`n=== 钩子模式 $Mode ===" -ForegroundColor Cyan
    $spike = Start-Spike "hook_$Mode" @(
        "--seconds", "6", "--hook", $Mode, "--test-injected", "--no-tray"
    )
    Wait-Ready $spike
    $hwnd = Wait-MainWindow $spike.Proc
    $focused = Focus-Window $hwnd
    Write-Host "spike 窗口 HWND=$hwnd 焦点=$focused"
    if (-not $focused) {
        Write-Warning "spike 窗口未获得焦点：仍发送 Win+V（钩子为全局钩子，吞键行为不依赖目标窗口；Win+V 不输入文本）"
    }

    Reset-WinKeyState
    $flyoutsBefore = @(Get-ShellFlyouts)
    $winKeyBefore = Get-WinKeyState
    Write-Host ("发出前：shell 弹层=[{0}] LWIN按下={1}" -f ($flyoutsBefore -join '; '), $winKeyBefore)

    Send-WinV
    Start-Sleep -Milliseconds 700

    $vDown = [bool](Wait-Log $spike 'HOOK_SWALLOW\|v_down' 3)
    $vUp = [bool](Wait-Log $spike 'HOOK_SWALLOW\|v_up' 3)
    $winUpLog = [bool](Wait-Log $spike 'HOOK_SWALLOW\|win_up' 2)
    $winKeyAfterSeq = Get-WinKeyState
    $flyoutsAfter = @(Get-ShellFlyouts)
    $startMenuOpen = Test-StartMenuOpen
    $flyoutDiff = @($flyoutsAfter | Where-Object { $flyoutsBefore -notcontains $_ })

    # 清掉可能粘住的 Win 状态（避免污染下一轮测试）
    Reset-WinKeyState
    $winKeyAfterReset = Get-WinKeyState

    Write-Host ("发出后：吞 v_down={0} v_up={1} 吞 win_up={2} 开始菜单={3} LWIN粘住={4}（恢复后={5}）" -f `
            $vDown, $vUp, $winUpLog, $startMenuOpen, $winKeyAfterSeq, $winKeyAfterReset)
    if ($flyoutDiff.Count -gt 0) {
        Write-Host ("新增 shell 弹层：" + ($flyoutDiff -join '; '))
    }

    if ($startMenuOpen -or $flyoutDiff.Count -gt 0) {
        Write-Host "检测到系统弹层，发送 Esc 关闭…"
        Send-Escape
        Start-Sleep -Milliseconds 400
        Send-Escape
        Start-Sleep -Milliseconds 300
    }

    [void](Wait-Exit $spike)
    return [pscustomobject]@{
        test = "hook $Mode"
        focused = $focused
        hook_v_down = $vDown
        hook_v_up = $vUp
        win_up_swallowed_log = $winUpLog
        start_menu_open = $startMenuOpen
        shell_flyout_new = $flyoutDiff
        lwin_down_before = $winKeyBefore
        lwin_stuck_after_seq = $winKeyAfterSeq
        lwin_after_reset = $winKeyAfterReset
        result = (Read-Result $spike)
    }
}

# ---------------------------------------------------------------------------
# 测试 D：托盘创建 + 菜单响应（headless，Sys 线程创建）
# ---------------------------------------------------------------------------

function Test-TrayMenu {
    Write-Host "`n=== D. tray-icon 创建 + 菜单响应（Sys 线程） ===" -ForegroundColor Cyan
    $spike = Start-Spike "tray_menu" @(
        "--headless", "--seconds", "9", "--tray", "--synth-click-after", "2"
    )
    Wait-Ready $spike
    $clicked = [bool](Wait-Log $spike 'SYNCLICK\|' 8)
    Write-Host "模拟托盘点击：$clicked"
    Start-Sleep -Milliseconds 500

    # 弹出菜单窗口类名 = "#32768"；直接投递键盘消息（避免注入到用户窗口）
    $menuHwnd = [P0Native]::FindWindow('#32768', $null)
    Write-Host "菜单窗口 HWND = $menuHwnd"
    if ($menuHwnd -ne [IntPtr]::Zero) {
        [void][P0Native]::PostMessage($menuHwnd, 0x0100, [IntPtr]0x28, [IntPtr]0)   # WM_KEYDOWN VK_DOWN
        [void][P0Native]::PostMessage($menuHwnd, 0x0101, [IntPtr]0x28, [IntPtr]0)   # WM_KEYUP
        Start-Sleep -Milliseconds 150
        [void][P0Native]::PostMessage($menuHwnd, 0x0100, [IntPtr]0x0D, [IntPtr]0)   # WM_KEYDOWN VK_RETURN
        [void][P0Native]::PostMessage($menuHwnd, 0x0101, [IntPtr]0x0D, [IntPtr]0)
    }
    $menuEvent = [bool](Wait-Log $spike 'MENU_EVENT\|id=' 5)
    if (-not $menuEvent -and $menuHwnd -ne [IntPtr]::Zero) {
        Write-Host "菜单未响应，发送 Esc 关闭"
        [void][P0Native]::PostMessage($menuHwnd, 0x0100, [IntPtr]0x1B, [IntPtr]0)
        [void][P0Native]::PostMessage($menuHwnd, 0x0101, [IntPtr]0x1B, [IntPtr]0)
    }
    $menuLog = Get-LogLine $spike 'MENU_EVENT\|id='
    Write-Host "菜单事件：$menuLog"
    [void](Wait-Exit $spike 20)
    return [pscustomobject]@{
        test = "D: tray menu"
        synthetic_click = $clicked
        menu_window_found = ($menuHwnd -ne [IntPtr]::Zero)
        menu_event = $menuLog
        menu_event_received = $menuEvent
        result = (Read-Result $spike)
    }
}

# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------

$summary = @()
$failed = $false
try {
    if (-not $SkipGui) {
        $summary += Test-AcrylicAndHotkey
    }
    if (-not $SkipHook) {
        $summary += Test-WinVHook "swallow_v"
        $summary += Test-WinVHook "swallow_v_winup"
    }
    $summary += Test-TrayMenu
} catch {
    $failed = $true
    Write-Host "`n测试异常：$_" -ForegroundColor Red
}

Write-Host "`n===== 汇总 =====" -ForegroundColor Yellow
foreach ($item in $summary) {
    Write-Host ("--- {0}" -f $item.test)
    $item.PSObject.Properties | ForEach-Object {
        if ($_.Name -in @('test', 'PSObject', 'result')) { return }
        Write-Host ("    {0} = {1}" -f $_.Name, $_.Value)
    }
    if ($null -ne $item.result) {
        Write-Host ("    result.hotkey_ctrl = {0}" -f $item.result.hotkey_ctrl)
        Write-Host ("    result.hotkey_winv = {0}" -f $item.result.hotkey_winv)
        Write-Host ("    result.hotkey_test = {0}" -f $item.result.hotkey_test)
        Write-Host ("    result.hook = {0} hit={1} v_down={2} v_up={3} win_up={4} injected={5}" -f `
                $item.result.hook, $item.result.hook_hits, $item.result.v_down_swallowed, `
                $item.result.v_up_swallowed, $item.result.win_up_swallowed, $item.result.injected_seen)
        Write-Host ("    result.tray = {0} items={1} events={2} menu={3}" -f `
                $item.result.tray, $item.result.tray_items, $item.result.tray_events, ($item.result.menu_events -join ','))
        Write-Host ("    result.acrylic = {0} tabbed = {1} exit_clean = {2}" -f `
                $item.result.acrylic, $item.result.tabbed, $item.result.exit_clean)
    }
}

$summaryPath = Join-Path $OutDir "summary.json"
$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryPath -Encoding UTF8
Write-Host "`n汇总已写入：$summaryPath"
if ($failed) { exit 1 }
