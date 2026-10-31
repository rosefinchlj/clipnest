# 自动粘贴冒烟用的「受控目标窗口」：一个带 TextBox 的 WinForms 窗口，
# 把 TextBox 内容实时写入文件，供 paste_smoke.ps1 校验自动粘贴是否成功。
#
# 用法（由 examples\paste_smoke.ps1 启动，也可手动）：
#   pwsh -NoProfile -ExecutionPolicy Bypass -File examples\paste_target.ps1 -OutFile target\paste-target\result.txt
param(
    [string]$OutFile = "target\paste-target\result.txt"
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$fullPath = [System.IO.Path]::GetFullPath($OutFile)
$dir = [System.IO.Path]::GetDirectoryName($fullPath)
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
[System.IO.File]::WriteAllText($fullPath, "")

$form = New-Object System.Windows.Forms.Form
$form.Text = "ClipNestPasteTarget"
$form.Width = 520
$form.Height = 240
$form.StartPosition = "CenterScreen"

$textBox = New-Object System.Windows.Forms.TextBox
$textBox.Multiline = $true
$textBox.Dock = "Fill"
$textBox.Font = New-Object System.Drawing.Font "Consolas", 12
$form.Controls.Add($textBox)

$textBox.Add_TextChanged({
        try {
            [System.IO.File]::WriteAllText($fullPath, $textBox.Text)
        } catch {}
    })

$form.Add_Shown({
        try {
            [System.IO.File]::WriteAllText($fullPath + ".ready", "ready")
            $form.Activate()
            [void]$textBox.Focus()
        } catch {}
    })

[void]$form.ShowDialog()
