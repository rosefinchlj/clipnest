# 启动 UI 预览 → 等待渲染 → 截图 → 关闭进程（可重复的 UI 验收脚本）
# 用法：
#   .\tools\ui-shot.ps1 -Preview picker -Out "E:\...\clipnest\target\ui-picker.png"
#   .\tools\ui-shot.ps1 -Preview manager -Out "...\ui-manager.png" -WaitSeconds 4
param(
    [string]$Preview = "picker",
    [string]$Out = "target\ui-shot.png",
    [int]$WaitSeconds = 3,
    [string]$Exe = ""
)

$root = Split-Path $PSScriptRoot -Parent
if (-not $Exe) { $Exe = Join-Path $root "target\debug\clipnest.exe" }
if (-not (Test-Path $Exe)) { Write-Error "找不到可执行文件：$Exe（先运行 cargo build）"; exit 1 }

$full = [System.IO.Path]::GetFullPath($Out)
$dir = [System.IO.Path]::GetDirectoryName($full)
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }

Get-Process -Name clipnest -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Process -FilePath $Exe -ArgumentList @("--ui-preview", $Preview) | Out-Null
Start-Sleep -Seconds $WaitSeconds
& (Join-Path $PSScriptRoot "screenshot.ps1") -Out $full -ProcessName clipnest
Start-Sleep -Milliseconds 300
Get-Process -Name clipnest -ErrorAction SilentlyContinue | Stop-Process -Force
Write-Host "ui-shot done: $full (preview=$Preview)"
