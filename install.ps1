$ErrorActionPreference = "Stop"

$projectDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$installDir = Join-Path $env:LOCALAPPDATA "Programs\System Monitor Overlay"
$exePath = Join-Path $installDir "system-monitor-overlay.exe"

$builtExe = Join-Path $projectDir "target\release\system-monitor-overlay.exe"
if (-not (Test-Path $builtExe)) {
    throw "Build the app first with: cargo build --locked --release"
}

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
Copy-Item $builtExe $exePath -Force

$startupDir = [Environment]::GetFolderPath("Startup")
$programsDir = [Environment]::GetFolderPath("Programs")
$shell = New-Object -ComObject WScript.Shell

$startupShortcut = $shell.CreateShortcut((Join-Path $startupDir "Dave.lnk"))
$startupShortcut.TargetPath = $exePath
$startupShortcut.Arguments = "--background"
$startupShortcut.WorkingDirectory = $installDir
$startupShortcut.WindowStyle = 7
$startupShortcut.Save()

$startMenuShortcut = $shell.CreateShortcut((Join-Path $programsDir "Dave.lnk"))
$startMenuShortcut.TargetPath = $exePath
$startMenuShortcut.WorkingDirectory = $installDir
$startMenuShortcut.WindowStyle = 7
$startMenuShortcut.Save()

Write-Host "Installed $exePath"
Write-Host "Dave will start at your next sign-in. Launch it now from the Start menu."
