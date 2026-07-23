# Packaging portable Windows — produit dist/Conduite-portable-win64/ (+ zip)
# Usage : powershell -File tools/package.ps1 [-FfmpegPath C:\chemin\ffmpeg.exe]
param(
    [string]$FfmpegPath = "",
    [string]$DomePack = "C:\Users\pymenvert\Claude\Projects\Materiaux IFS\dist\ISF"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Write-Host "Build release..."
cargo build --release -p conduite
if ($LASTEXITCODE -ne 0) { throw "Build échoué" }

$dist = Join-Path $root "dist\Conduite-portable-win64"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
foreach ($d in @("", "media", "shows", "shaders", "logs", "bin")) {
    New-Item -ItemType Directory -Force (Join-Path $dist $d) | Out-Null
}

Copy-Item "target\release\conduite.exe" $dist
if (Test-Path "shaders\examples") { Copy-Item -Recurse "shaders\examples" (Join-Path $dist "shaders\examples") }
if (Test-Path $DomePack) {
    Copy-Item "$DomePack\*.fs" (Join-Path $dist "shaders") -ErrorAction SilentlyContinue
    Write-Host "DomePack copié depuis $DomePack"
}
if ($FfmpegPath -and (Test-Path $FfmpegPath)) {
    Copy-Item $FfmpegPath (Join-Path $dist "bin\ffmpeg.exe")
    $ffprobe = Join-Path (Split-Path $FfmpegPath) "ffprobe.exe"
    if (Test-Path $ffprobe) { Copy-Item $ffprobe (Join-Path $dist "bin\ffprobe.exe") }
} else {
    Write-Host "ffmpeg non embarqué (sera cherché dans le PATH). Passe -FfmpegPath pour l'inclure."
}

@"
Conduite — régie vidéo de spectacle (version portable)

1. Lancer conduite.exe
2. Ouvrir http://localhost:9820 dans un navigateur (l'interface de régie)
3. Déposer vos vidéos dans media\, vos shaders ISF dans shaders\
4. Les shows sont enregistrés dans shows\, les journaux dans logs\

ffmpeg : si bin\ffmpeg.exe est absent, ffmpeg doit être dans le PATH.
Aucune installation, aucun registre : ce dossier se copie sur une clé USB.
"@ | Out-File -Encoding utf8 (Join-Path $dist "LISEZMOI.txt")

$zip = Join-Path $root "dist\Conduite-portable-win64.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $dist -DestinationPath $zip
Write-Host "OK -> $dist et $zip"
