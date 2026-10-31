# ClipNest 内存/资源采样（验收用）
# 用法：
#   .\tools\measure.ps1                          # 采样 clipnest 10 次（1s 间隔）
#   .\tools\measure.ps1 -Samples 30 -Out "E:\...\target\mem.csv"
param(
    [string]$ProcessName = "clipnest",
    [int]$Samples = 10,
    [int]$IntervalSeconds = 1,
    [string]$Out = "target\measure.csv"
)

$rows = @()
for ($i = 0; $i -lt $Samples; $i++) {
    $p = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $p) { Write-Error "进程 $ProcessName 未运行"; exit 1 }
    $rows += [pscustomobject]@{
        Time        = (Get-Date).ToString("HH:mm:ss")
        WorkingSetMB = [math]::Round($p.WorkingSet64 / 1MB, 1)
        PrivateMB   = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
        Threads     = $p.Threads.Count
        Handles     = $p.HandleCount
    }
    Start-Sleep -Seconds $IntervalSeconds
}

$rows | Format-Table -AutoSize
$full = [System.IO.Path]::GetFullPath($Out)
$dir = [System.IO.Path]::GetDirectoryName($full)
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
$rows | Export-Csv -Path $full -NoTypeInformation -Encoding UTF8

$ws = $rows | Measure-Object -Property WorkingSetMB -Average -Maximum -Minimum
$priv = $rows | Measure-Object -Property PrivateMB -Average -Maximum -Minimum
Write-Host ("WorkingSet: avg {0:N1} MB / max {1:N1} MB / min {2:N1} MB" -f $ws.Average, $ws.Maximum, $ws.Minimum)
Write-Host ("Private:    avg {0:N1} MB / max {1:N1} MB / min {2:N1} MB" -f $priv.Average, $priv.Maximum, $priv.Minimum)
Write-Host "saved: $full"
