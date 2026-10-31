# 自动粘贴链路真机冒烟：受控目标窗口（examples/paste_target.ps1）+ 真实 clipnest.exe
#
# 验证：
#   1) 复制文本 → 采集入库（Sys 日志）
#   2) 备用热键 Ctrl+Shift+V 打开 Picker（此时前台=受控目标窗口，target_hwnd 记录正确）
#   3) Enter 触发自动粘贴：写剪贴板 → 恢复目标前台 → SendInput Ctrl+V → 目标 TextBox 收到文本
#   4) Get-Clipboard 校验剪贴板内容
#
# 用法：pwsh -NoProfile -ExecutionPolicy Bypass -File examples\paste_smoke.ps1
# 安全：临时 CLIPNEST_DATA_DIR；结束 kill 两个测试进程（热键/钩子随进程回收）。
param(
    [string]$Exe = "",
    [string]$OutDir = ""
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if (-not $Exe) { $Exe = Join-Path $Root "target\debug\clipnest.exe" }
if (-not $OutDir) { $OutDir = Join-Path $Root "target\paste-smoke" }
Remove-Item $OutDir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$DataDir = Join-Path $OutDir "data"
$TargetResult = Join-Path $OutDir "target-text.txt"
$TargetReady = "$TargetResult.ready"
$LogPath = Join-Path $DataDir "logs\app.log"
if (-not (Test-Path $Exe)) { throw "找不到 clipnest.exe：$Exe（请先 cargo build）" }

Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class PasteSmoke {
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
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr SetFocus(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hWnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool f);
}
'@

function Send-Vk([int]$Vk, [bool]$KeyUp) {
    $inp = New-Object PasteSmoke+INPUT
    $inp.type = [uint32]1
    $inp.wVk = [uint16]$Vk
    $inp.dwFlags = if ($KeyUp) { [uint32]2 } else { [uint32]0 }
    $arr = New-Object PasteSmoke+INPUT[] 1
    $arr[0] = $inp
    if ([PasteSmoke]::SendInput(1, $arr, 40) -ne 1) { throw ("SendInput 失败 vk={0}" -f $Vk) }
}

function Send-CtrlShiftV {
    Send-Vk 0x11 $false; Send-Vk 0x10 $false; Start-Sleep -Milliseconds 30
    Send-Vk 0x56 $false; Start-Sleep -Milliseconds 30
    Send-Vk 0x56 $true; Start-Sleep -Milliseconds 30
    Send-Vk 0x10 $true; Send-Vk 0x11 $true
}

function Send-EnterKey { Send-Vk 0x0D $false; Start-Sleep -Milliseconds 40; Send-Vk 0x0D $true }

# 按实际生效的快捷键标签注入（本机 Ctrl+`/Ctrl+Shift+V 可能被其它程序/另一个 clipnest 实例占用）
function Send-HotkeyByLabel([string]$Label) {
    switch ($Label) {
        "Ctrl+``" { Send-Vk 0x11 $false; Send-Vk 0xC0 $false; Start-Sleep -Milliseconds 30; Send-Vk 0xC0 $true; Send-Vk 0x11 $true; return $true }
        "Ctrl+Shift+V" { Send-CtrlShiftV; return $true }
        "Alt+``" { Send-Vk 0x12 $false; Send-Vk 0xC0 $false; Start-Sleep -Milliseconds 30; Send-Vk 0xC0 $true; Send-Vk 0x12 $true; return $true }
        "Ctrl+Alt+Shift+V" {
            Send-Vk 0x11 $false; Send-Vk 0x12 $false; Send-Vk 0x10 $false; Start-Sleep -Milliseconds 30
            Send-Vk 0x56 $false; Send-Vk 0x56 $true; Start-Sleep -Milliseconds 30
            Send-Vk 0x10 $true; Send-Vk 0x12 $true; Send-Vk 0x11 $true; return $true
        }
        default { return $false }
    }
}
function Send-Escape { Send-Vk 0x1B $false; Start-Sleep -Milliseconds 20; Send-Vk 0x1B $true }

function Focus-Window([IntPtr]$Hwnd) {
    if ($Hwnd -eq [IntPtr]::Zero) { return $false }
    for ($i = 0; $i -lt 3; $i++) {
        [void][PasteSmoke]::ShowWindow($Hwnd, 9)
        [void][PasteSmoke]::SetForegroundWindow($Hwnd)
        Start-Sleep -Milliseconds 200
        if ([PasteSmoke]::GetForegroundWindow() -eq $Hwnd) { return $true }
        $fgPid = [uint32]0
        $fgThread = [PasteSmoke]::GetWindowThreadProcessId([PasteSmoke]::GetForegroundWindow(), [ref]$fgPid)
        $my = [PasteSmoke]::GetCurrentThreadId()
        [void][PasteSmoke]::AttachThreadInput($my, $fgThread, $true)
        try {
            [void][PasteSmoke]::SetForegroundWindow($Hwnd)
            [void][PasteSmoke]::BringWindowToTop($Hwnd)
            [void][PasteSmoke]::SetFocus($Hwnd)
            Start-Sleep -Milliseconds 250
        } finally {
            [void][PasteSmoke]::AttachThreadInput($my, $fgThread, $false)
        }
        if ([PasteSmoke]::GetForegroundWindow() -eq $Hwnd) { return $true }
    }
    return $false
}

$results = [ordered]@{}
$app = $null
$target = $null
$env:CLIPNEST_DATA_DIR = $DataDir
try {
    # 1) 受控目标窗口
    Write-Host "启动受控目标窗口…"
    $target = Start-Process -FilePath "pwsh" -ArgumentList @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", (Join-Path $Root "examples\paste_target.ps1"),
        "-OutFile", $TargetResult
    ) -PassThru -RedirectStandardOutput (Join-Path $OutDir "target.out") -RedirectStandardError (Join-Path $OutDir "target.err")
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline -and -not (Test-Path $TargetReady)) { Start-Sleep -Milliseconds 150 }
    if (-not (Test-Path $TargetReady)) { throw "受控目标窗口未就绪" }
    Start-Sleep -Milliseconds 800
    # 用进程 MainWindowHandle 取窗口句柄（FindWindow($null,...) 在 PowerShell 下会把 null 变空串）
    $targetHwnd = [IntPtr]::Zero
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline) {
        $target.Refresh()
        if ($target.MainWindowHandle -ne [IntPtr]::Zero) { $targetHwnd = $target.MainWindowHandle; break }
        Start-Sleep -Milliseconds 150
    }
    $results["target_hwnd"] = [string]$targetHwnd
    if ($targetHwnd -eq [IntPtr]::Zero) { throw "找不到受控目标窗口" }
    $results["target_focused"] = Focus-Window $targetHwnd

    # 2) 启动主程序（临时数据目录）
    Write-Host "启动 clipnest.exe…"
    $app = Start-Process -FilePath $Exe -PassThru `
        -RedirectStandardOutput (Join-Path $OutDir "app.out") -RedirectStandardError (Join-Path $OutDir "app.err")
    Start-Sleep -Seconds 3
    if (-not (Test-Path $LogPath)) { throw "app 日志未生成：$LogPath" }
    $results["sys_ready"] = [bool]((Get-Content $LogPath -Raw) -match 'Sys 线程已启动')

    # 3) 复制文本 → 采集
    $expected = "PASTE_SMOKE_$(Get-Date -Format 'HHmmss')_剪贴匣自动粘贴"
    Set-Clipboard -Value $expected
    Start-Sleep -Milliseconds 1000
    $log = Get-Content $LogPath -Raw
    $results["captured"] = [bool]($log -match '采集剪贴板：类型=文本')
    if (-not $results["captured"]) { throw "文本未被采集，日志见 $LogPath" }

    # 4) 目标窗口前台 → 热键打开 Picker（按实际生效组合注入）
    $results["target_focused_before_hotkey"] = Focus-Window $targetHwnd
    $log = Get-Content $LogPath -Raw
    $effective = ""
    $m = [regex]::Match($log, '备用快捷键：请求=(.+?) 生效=(.+?) ok=')
    if ($m.Success) { $effective = $m.Groups[2].Value.Trim() }
    $results["effective_hotkey"] = $effective
    if (-not (Send-HotkeyByLabel $effective)) { throw "无法发送生效快捷键：$effective" }
    Start-Sleep -Milliseconds 1200
    $log = Get-Content $LogPath -Raw
    $pickerLine = ($log -split "`n" | Where-Object { $_ -match '呼出面板' } | Select-Object -Last 1)
    $results["picker_line"] = $pickerLine
    $results["picker_opened"] = [bool]$pickerLine
    if (-not $pickerLine) { throw "Picker 未打开" }

    # 5) 确保 Picker 拿到前台焦点后再 Enter（后台进程 SetForegroundWindow 可能被前台锁阻止）
    .Refresh()
     = .MainWindowHandle
    ["picker_hwnd"] = [string]
    if ( -ne [IntPtr]::Zero) {
        ["picker_focused"] = Focus-Window 
    }
    Start-Sleep -Milliseconds 400
    # Enter 触发自动粘贴
    Send-EnterKey
    Start-Sleep -Milliseconds 2500
    $log = Get-Content $LogPath -Raw
    $pasteLine = ($log -split "`n" | Where-Object { $_ -match '自动粘贴(完成|失败)' } | Select-Object -Last 1)
    $results["paste_line"] = $pasteLine
    $results["paste_ok_log"] = [bool]($log -match '自动粘贴完成')

    # 6) 校验受控目标收到的文本 + 剪贴板内容
    $targetText = ""
    if (Test-Path $TargetResult) { $targetText = Get-Content $TargetResult -Raw }
    $results["target_text"] = $targetText
    $results["target_text_matches"] = ($targetText -eq $expected)
    $results["clipboard_matches"] = ((Get-Clipboard -Raw).TrimEnd("`r", "`n") -eq $expected)

    if (-not $results["paste_ok_log"]) { Send-Escape }
} catch {
    $results["error"] = "$_"
} finally {
    Remove-Item Env:CLIPNEST_DATA_DIR -ErrorAction SilentlyContinue
    foreach ($p in @($app, $target)) {
        if ($null -ne $p) {
            try {
                if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit(5000) | Out-Null }
            } catch {}
        }
    }
    Write-Host "`n===== 自动粘贴冒烟结果 =====" -ForegroundColor Yellow
    foreach ($key in $results.Keys) { Write-Host ("{0} = {1}" -f $key, $results[$key]) }
    $results | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $OutDir "result.json") -Encoding UTF8
    Write-Host "`n日志：$LogPath"
}
