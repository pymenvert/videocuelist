# Packaging portable Windows — produit dist/Conduite-portable-win64/
# + dist/Conduite-{version}-win64.zip + SHA-256.
#
# Usage : powershell -File tools/package.ps1
#         [-FfmpegDir <dossier contenant ffmpeg.exe/ffprobe.exe>]
#         [-DomePack <dossier des .fs DomePack>] [-AllowGpl]
#
# ffmpeg : par défaut, le build LGPL est pris dans tools/ffmpeg-lgpl/bin/
# (gitignoré — voir tools/ffmpeg-lgpl/VERSION.txt pour le re-télécharger).
# Le script REFUSE d'embarquer un build GPL (ex. gyan.dev "full") sauf si
# -AllowGpl est passé explicitement : produit vendu, la variante LGPL est
# la seule redistribuable sans obligations GPL.
param(
    [string]$FfmpegDir = "",
    [string]$DomePack = "C:\Users\pymenvert\Claude\Projects\Materiaux IFS\dist\ISF",
    [switch]$AllowGpl
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# --- Version (source de vérité : [workspace.package] de Cargo.toml) --------
$cargoToml = Get-Content (Join-Path $root "Cargo.toml") -Raw
if ($cargoToml -notmatch '(?ms)\[workspace\.package\].*?version\s*=\s*"([^"]+)"') {
    throw "Version introuvable dans Cargo.toml ([workspace.package])"
}
$version = $Matches[1]
Write-Host "Conduite v$version — build release..."

cargo build --release -p conduite
if ($LASTEXITCODE -ne 0) { throw "Build échoué" }

# Contrôle VERSIONINFO : l'exe doit porter la version (build.rs/winresource).
$exe = Join-Path $root "target\release\conduite.exe"
$vi = (Get-Item $exe).VersionInfo
if ($vi.ProductVersion -notlike "$version*") {
    throw "VERSIONINFO de conduite.exe ('$($vi.ProductVersion)') != version Cargo ($version) — build.rs/winresource en panne ?"
}

# --- Arborescence -----------------------------------------------------------
$dist = Join-Path $root "dist\Conduite-portable-win64"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
foreach ($d in @("", "media", "shows", "shaders", "logs", "bin", "licenses")) {
    New-Item -ItemType Directory -Force (Join-Path $dist $d) | Out-Null
}

Copy-Item $exe $dist

# --- Shaders (exemples MIT + DomePack (c) Pym) + crédits --------------------
if (Test-Path "shaders\examples") {
    Copy-Item -Recurse "shaders\examples" (Join-Path $dist "shaders\examples")
}
if (Test-Path $DomePack) {
    Copy-Item "$DomePack\*.fs" (Join-Path $dist "shaders") -ErrorAction SilentlyContinue
    Write-Host "DomePack copié depuis $DomePack"
}
Copy-Item "shaders\CREDITS.txt" (Join-Path $dist "shaders\CREDITS.txt")

# --- Médias de démo (le premier GO doit produire une image) -----------------
Get-ChildItem "media\demo-*" -File -ErrorAction SilentlyContinue |
    Copy-Item -Destination (Join-Path $dist "media")

# --- ffmpeg (LGPL par défaut) + contrôle de variante ------------------------
if (-not $FfmpegDir) { $FfmpegDir = Join-Path $root "tools\ffmpeg-lgpl\bin" }
$ffmpeg = Join-Path $FfmpegDir "ffmpeg.exe"
if (Test-Path $ffmpeg) {
    $banner = (& $ffmpeg -version 2>$null) -join "`n"
    if ($banner -match "--enable-gpl" -and -not $AllowGpl) {
        throw ("Le ffmpeg de $FfmpegDir est un build GPL (--enable-gpl) : " +
               "non redistribuable sans obligations GPL. Utiliser le build " +
               "LGPL (tools/ffmpeg-lgpl/, voir VERSION.txt) ou passer -AllowGpl en connaissance de cause.")
    }
    if ($banner -notmatch "libsnappy") {
        Write-Warning "Ce ffmpeg semble sans libsnappy : vérifier le décodage HAP avant livraison."
    }
    Copy-Item $ffmpeg (Join-Path $dist "bin\ffmpeg.exe")
    $ffprobe = Join-Path $FfmpegDir "ffprobe.exe"
    if (Test-Path $ffprobe) { Copy-Item $ffprobe (Join-Path $dist "bin\ffprobe.exe") }
    Write-Host "ffmpeg embarqué depuis $FfmpegDir"
} else {
    Write-Warning "ffmpeg non embarqué ($ffmpeg absent) : il sera cherché dans le PATH. Voir tools/ffmpeg-lgpl/VERSION.txt."
}

# --- Licences (obligation d'attribution en redistribution binaire) ----------
$licSrc = Join-Path $root "licenses"
if (-not (Test-Path (Join-Path $licSrc "THIRD-PARTY-NOTICES.html"))) {
    throw "licenses/THIRD-PARTY-NOTICES.html manquant — générer avec : cargo about generate about.hbs -o licenses/THIRD-PARTY-NOTICES.html"
}
Copy-Item "$licSrc\*" (Join-Path $dist "licenses")
Copy-Item (Join-Path $root "LICENSE") (Join-Path $dist "licenses\LICENSE-CONDUITE.txt")

# --- Manuel + LISEZMOI (source UTF-8, écrit avec BOM : lisible partout) -----
Copy-Item (Join-Path $root "docs\MANUEL.md") (Join-Path $dist "MANUEL.md")
$lisezmoi = Get-Content (Join-Path $root "tools\LISEZMOI.txt") -Raw -Encoding UTF8
$lisezmoi = $lisezmoi.Replace("{VERSION}", $version)
# PS 5.1 : -Encoding UTF8 d'Out-File écrit un BOM — c'est voulu (Notepad, etc.)
$lisezmoi | Out-File -Encoding UTF8 (Join-Path $dist "LISEZMOI.txt")

# --- Zip versionné + empreinte SHA-256 --------------------------------------
$zip = Join-Path $root "dist\Conduite-$version-win64.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $dist -DestinationPath $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash *$(Split-Path $zip -Leaf)" | Out-File -Encoding ascii "$zip.sha256"

Write-Host ""
Write-Host "OK -> $dist"
Write-Host "OK -> $zip"
Write-Host "SHA-256 : $hash (aussi dans $zip.sha256)"
