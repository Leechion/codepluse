$ErrorActionPreference = 'Stop'
$workspaceRoot = Split-Path -Parent $PSScriptRoot
$bundleRoot = Join-Path $workspaceRoot 'dist\Harness Hub'
$executable = Join-Path $bundleRoot 'Harness Hub.exe'
if (!(Test-Path -LiteralPath $executable)) { throw '请先运行 npm run desktop:build' }
$shortcutPath = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Harness Hub.lnk'
$shortcutShell = New-Object -ComObject WScript.Shell
$shortcut = $shortcutShell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $executable
$shortcut.WorkingDirectory = $workspaceRoot
$shortcut.IconLocation = (Join-Path $bundleRoot 'Harness Hub.ico') + ',0'
$shortcut.Description = 'Harness Hub desktop workspace'
$shortcut.Save()
Write-Output $shortcutPath
