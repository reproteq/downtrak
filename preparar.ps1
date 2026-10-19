# Descarga yt-dlp (y deno) en embed\ y compila un único exe autónomo.
# Uso:  powershell -ExecutionPolicy Bypass -File .\preparar.ps1
#       powershell -ExecutionPolicy Bypass -File .\preparar.ps1 -SinDeno   (exe más pequeño)
param([switch]$SinDeno)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
Set-Location $PSScriptRoot
New-Item -ItemType Directory -Force embed | Out-Null

Write-Host "Descargando yt-dlp.exe..."
Invoke-WebRequest "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe" -OutFile "embed\yt-dlp.exe"

if ($SinDeno) {
    Remove-Item -ErrorAction SilentlyContinue "embed\deno.exe"
} else {
    Write-Host "Descargando deno (motor JavaScript que yt-dlp necesita para YouTube)..."
    $zip = Join-Path $env:TEMP "deno.zip"
    Invoke-WebRequest "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip" -OutFile $zip
    Expand-Archive -Force $zip "embed"
    Remove-Item $zip
}

Write-Host "Compilando..."
cargo build --release
Copy-Item -Force "target\release\yt_mp3_gui.exe" ".\YT-MP3.exe"
Write-Host ""
Write-Host "Listo: YT-MP3.exe (no necesita nada más)"
