# P0 按键注入/键盘状态小工具（手动实验用；不参与验收）
# 用法：
#   pwsh -File examples\p0_keys.ps1 state            # 打印 LWIN/V/LCTRL 异步按下状态
#   pwsh -File examples\p0_keys.ps1 win-tap          # 注入 Win 按下+抬起，前后打印状态
#   pwsh -File examples\p0_keys.ps1 winv             # 注入 Win+V 全序列，前后打印状态
#   pwsh -File examples\p0_keys.ps1 win-down / win-up / v-down / v-up
param([string]$Action = "state")

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class P0K {
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
    public static extern uint SendInput(uint nInputs, INPUT[] pInputs, int cbSize);
    [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int vKey);
}
'@

function Send-Vk([int]$Vk, [bool]$KeyUp) {
    $inp = New-Object P0K+INPUT
    $inp.type = [uint32]1
    $inp.wVk = [uint16]$Vk
    $inp.dwFlags = if ($KeyUp) { [uint32]2 } else { [uint32]0 }
    $arr = New-Object P0K+INPUT[] 1
    $arr[0] = $inp
    $n = [P0K]::SendInput(1, $arr, 40)
    if ($n -ne 1) { throw ("SendInput err=" + [System.Runtime.InteropServices.Marshal]::GetLastWin32Error()) }
}

function Show-State([string]$Tag) {
    $lwin = [bool]([P0K]::GetAsyncKeyState(0x5B) -band 0x8000)
    $rwin = [bool]([P0K]::GetAsyncKeyState(0x5C) -band 0x8000)
    $v = [bool]([P0K]::GetAsyncKeyState(0x56) -band 0x8000)
    $ctrl = [bool]([P0K]::GetAsyncKeyState(0x11) -band 0x8000)
    Write-Host ("[{0}] LWIN={1} RWIN={2} V={3} CTRL={4}" -f $Tag, $lwin, $rwin, $v, $ctrl)
}

Show-State "before"
switch ($Action) {
    "state" { }
    "win-down" { Send-Vk 0x5B $false; Start-Sleep -Milliseconds 150; Show-State "after win-down" }
    "win-up" { Send-Vk 0x5B $true; Start-Sleep -Milliseconds 150; Show-State "after win-up" }
    "v-down" { Send-Vk 0x56 $false; Start-Sleep -Milliseconds 150; Show-State "after v-down" }
    "v-up" { Send-Vk 0x56 $true; Start-Sleep -Milliseconds 150; Show-State "after v-up" }
    "win-tap" {
        Send-Vk 0x5B $false; Start-Sleep -Milliseconds 80
        Send-Vk 0x5B $true; Start-Sleep -Milliseconds 400
        Show-State "after win-tap"
    }
    "winv" {
        Send-Vk 0x5B $false; Start-Sleep -Milliseconds 60
        Send-Vk 0x56 $false; Start-Sleep -Milliseconds 60
        Send-Vk 0x56 $true; Start-Sleep -Milliseconds 60
        Send-Vk 0x5B $true; Start-Sleep -Milliseconds 400
        Show-State "after winv"
    }
    default { Write-Host "未知 Action: $Action" }
}
