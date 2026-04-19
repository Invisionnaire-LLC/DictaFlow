$wsh = New-Object -ComObject WScript.Shell
$lnkPath = [Environment]::GetFolderPath('Startup') + '\DictaFlow.lnk'
$sc = $wsh.CreateShortcut($lnkPath)
$sc.TargetPath = 'D:\GeneralApps\DictaFlow\src-tauri\target\debug\dictaflow.exe'
$sc.WorkingDirectory = 'D:\GeneralApps\DictaFlow\src-tauri\target\debug'
$sc.WindowStyle = 7
$sc.Description = 'DictaFlow dictation'
$sc.Save()
Write-Host "Updated: $lnkPath -> debug build"
