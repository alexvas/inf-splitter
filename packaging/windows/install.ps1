#Requires -RunAsAdministrator

$ErrorActionPreference = "Stop"

$targetDir = "$env:ProgramData\inf-splitter"
$logsDir = "$targetDir\logs"
$secretsDir = "$targetDir\secrets"
$winswUrl = "https://github.com/winsw/winsw/releases/download/v3.0.0/WinSW-x64.exe"
$winswExe = "$targetDir\inf-splitter-service.exe"

Write-Host "=== Inf-Splitter Windows Install ==="

Write-Host "Creating $targetDir ..."
New-Item -ItemType Directory -Force -Path $targetDir | Out-Null
New-Item -ItemType Directory -Force -Path $logsDir | Out-Null
New-Item -ItemType Directory -Force -Path $secretsDir | Out-Null

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
Write-Host "Set API keys: create a file in $secretsDir with the key name"
Write-Host "and put the key value inside. Examples:"
Write-Host "  DEEPSEEK_API_KEY -> sk-..."
Write-Host "  MAAS_API_KEY     -> sk-..."
Write-Host "Then restart: Restart-Service inf-splitter"
