$ErrorActionPreference = "Stop"

$projectDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$installDir = Join-Path $env:LOCALAPPDATA "Programs\System Monitor Overlay"
$exePath = Join-Path $installDir "system-monitor-overlay.exe"

$builtExe = Join-Path $projectDir "target\release\system-monitor-overlay.exe"
if (-not (Test-Path $builtExe)) {
    throw "Build the app first from the MSYS2 UCRT64 shell with: cargo build --release"
}

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
Copy-Item $builtExe $exePath -Force

$msysRoot = if ($env:MSYS2_ROOT) { $env:MSYS2_ROOT } else { "C:\msys64" }
$gtkBin = Join-Path $msysRoot "ucrt64\bin"
$launcherPath = Join-Path $installDir "run-system-monitor-overlay.cmd"
@"
@echo off
set "PATH=$gtkBin;%PATH%"
cd /d "%~dp0"
start "" /b system-monitor-overlay.exe %*
"@ | Set-Content -Encoding ascii $launcherPath

$startupDir = [Environment]::GetFolderPath("Startup")
$programsDir = [Environment]::GetFolderPath("Programs")
$shell = New-Object -ComObject WScript.Shell

$startupShortcut = $shell.CreateShortcut((Join-Path $startupDir "Dave.lnk"))
$startupShortcut.TargetPath = $launcherPath
$startupShortcut.Arguments = "--background"
$startupShortcut.WorkingDirectory = $installDir
$startupShortcut.WindowStyle = 7
$startupShortcut.Save()

$startMenuShortcut = $shell.CreateShortcut((Join-Path $programsDir "Dave.lnk"))
$startMenuShortcut.TargetPath = $launcherPath
$startMenuShortcut.WorkingDirectory = $installDir
$startMenuShortcut.WindowStyle = 7
$startMenuShortcut.Save()

Write-Host "Installed $exePath"
Write-Host "Dave will start at your next sign-in. Launch it now from the Start menu."
