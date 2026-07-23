# Télécharge le build FFmpeg LGPL officiel (BtbN) dans tools/ffmpeg-lgpl/
# — le dossier est gitignoré, ce script le reconstitue sur un clone frais.
# Usage : powershell -File tools/get-ffmpeg-lgpl.ps1
# Voir tools/ffmpeg-lgpl/VERSION.txt et licenses/FFMPEG.txt (pourquoi LGPL).
param(
    [string]$Asset = "ffmpeg-n8.1-latest-win64-lgpl-8.1.zip"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "tools\ffmpeg-lgpl"
$url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$Asset"

$tmp = Join-Path $env:TEMP "conduite-ffmpeg-dl"
New-Item -ItemType Directory -Force $tmp | Out-Null
$zip = Join-Path $tmp $Asset

Write-Host "Téléchargement : $url"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Invoke-WebRequest -Uri $url -OutFile $zip

Write-Host "Extraction..."
$x = Join-Path $tmp "x"
if (Test-Path $x) { Remove-Item -Recurse -Force $x }
Expand-Archive -Path $zip -DestinationPath $x

$src = Get-ChildItem $x -Directory | Select-Object -First 1
New-Item -ItemType Directory -Force (Join-Path $dest "bin") | Out-Null
Copy-Item (Join-Path $src.FullName "bin\ffmpeg.exe") (Join-Path $dest "bin\ffmpeg.exe") -Force
Copy-Item (Join-Path $src.FullName "bin\ffprobe.exe") (Join-Path $dest "bin\ffprobe.exe") -Force
Copy-Item (Join-Path $src.FullName "LICENSE.txt") (Join-Path $dest "LICENSE.txt") -Force

# Contrôle de variante : jamais de build GPL ici.
$banner = (& (Join-Path $dest "bin\ffmpeg.exe") -version 2>$null) -join "`n"
if ($banner -match "--enable-gpl") { throw "Ce build est GPL (--enable-gpl) — asset inattendu, abandon." }
if ($banner -notmatch "libsnappy") { Write-Warning "libsnappy absent : le décodage HAP doit être re-vérifié." }

Remove-Item -Recurse -Force $tmp
Write-Host "OK -> $dest (variante LGPL vérifiée)"
