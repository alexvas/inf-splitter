#Requires -RunAsAdministrator

$ErrorActionPreference = "Stop"

$targetDir = "$env:ProgramData\inf-splitter"
$logsDir = "$targetDir\logs"
$winswUrl = "https://github.com/winsw/winsw/releases/download/v3.0.0/WinSW-x64.exe"
$winswExe = "$targetDir\inf-splitter-service.exe"

Write-Host "=== Inf-Splitter Windows Install ==="

Write-Host "Creating $targetDir ..."
New-Item -ItemType Directory -Force -Path $targetDir | Out-Null
New-Item -ItemType Directory -Force -Path $logsDir | Out-Null

Write-Host "Copying files..."
Copy-Item -Path "$PSScriptRoot\inf-splitter.exe" -Destination $targetDir -Force
Copy-Item -Path "$PSScriptRoot\winsw.xml" -Destination $targetDir -Force
if (-not (Test-Path "$targetDir\config.toml")) {
    Copy-Item -Path "$PSScriptRoot\config.toml" -Destination $targetDir
    Write-Host "  config.toml installed (first time)"
} else {
    Write-Host "  config.toml already present, skipping"
}

Write-Host "Downloading WinSW..."
Invoke-WebRequest -Uri $winswUrl -OutFile $winswExe

Write-Host "Installing service..."
& $winswExe install
& $winswExe start

Write-Host "=== Done ==="
Write-Host "Service status:"
& $winswExe status
Write-Host ""
Write-Host "Edit config: $targetDir\config.toml"
Write-Host "Set API keys via WinSW env vars, e.g.:"
Write-Host "  & `"$winswExe`" set DEEPSEEK_API_KEY=sk-..."
Write-Host "  & `"$winswExe`" restart"
