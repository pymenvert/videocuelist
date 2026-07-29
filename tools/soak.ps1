# Soak d'endurance Conduite — formalise le rituel des soaks archivés dans
# docs/bench/ (mêmes colonnes que soak-premium-10min.csv, enrichies).
#
# Usage : powershell -File tools/soak.ps1
#         [-Minutes 480] [-Port 9820]
#         [-ReleaseExe target\release\conduite.exe]
#         [-Cues "1,2,2.5,3,4"] [-SampleEverySec 30] [-GotoEverySec 4]
#
# Déroulé : lance le build release, attend /health = ok, ouvre le WebSocket
# /ws, boucle des GOTO sur les cues demandées + balayage du master
# (param_set master/intensity), échantillonne le process toutes les 30 s
# (working set, mémoire privée, threads, handles) dans
# docs/bench/soak-<date>-<durée>min.csv, rend un verdict automatique
# PLATE / CROISSANTE / ECHEC, puis vérifie qu'il ne reste AUCUN process.
#
# Codes de sortie : 0 = PLATE, 2 = CROISSANTE, 3 = ECHEC (moteur figé,
# crash, ou pré-conditions non remplies).
#
# RÈGLES ANTIVIRUS — ne pas « améliorer » ce script en les contournant :
#   - l'exécutable est lancé DIRECTEMENT depuis le dossier du dépôt,
#     jamais copié vers %TEMP% ni ailleurs ;
#   - aucune fenêtre cachée (pas de -WindowStyle Hidden) ;
#   - une seule instance de conduite.exe à la fois : le script REFUSE de
#     démarrer si une instance tourne déjà ;
#   - arrêt propre (commande quit) puis, au besoin, Stop-Process par Id —
#     jamais de kill par nom ;
#   - aucun téléchargement.

param(
    [int]$Minutes = 480,
    [int]$Port = 9820,
    [string]$ReleaseExe = "target\release\conduite.exe",
    [string]$Cues = "1,2,2.5,3,4",
    [int]$SampleEverySec = 30,
    [double]$GotoEverySec = 4.0
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# ---------------------------------------------------------- pré-conditions
$exe = Join-Path $root $ReleaseExe
if (-not (Test-Path $exe)) {
    Write-Host "Introuvable : $exe" -ForegroundColor Red
    Write-Host "Compiler d'abord : cargo build --release -p conduite"
    exit 3
}
$already = Get-Process -Name "conduite" -ErrorAction SilentlyContinue
if ($null -ne $already) {
    Write-Host "REFUS : conduite.exe tourne déjà (PID $($already.Id -join ', ')) — une seule instance à la fois." -ForegroundColor Red
    exit 3
}

$benchDir = Join-Path $root "docs\bench"
if (-not (Test-Path $benchDir)) { New-Item -ItemType Directory -Force $benchDir | Out-Null }
$stamp = Get-Date -Format "yyyy-MM-dd-HHmm"
$csv = Join-Path $benchDir ("soak-{0}-{1}min.csv" -f $stamp, $Minutes)
Set-Content -Path $csv -Encoding utf8 -Value "timestamp,elapsed_s,working_set_mb,private_mb,threads,handles,ws_msgs,gotos"

# Cues -> millièmes (contrat CueNumber : "2.5" = 2500).
$cueList = @()
foreach ($tok in $Cues.Split(",")) {
    $t = $tok.Trim()
    if ($t.Length -gt 0) { $cueList += [int][math]::Round([double]::Parse($t, [Globalization.CultureInfo]::InvariantCulture) * 1000) }
}
if ($cueList.Count -eq 0) { Write-Host "Aucune cue valide dans -Cues." -ForegroundColor Red; exit 3 }

# ------------------------------------------------------------- lancement
Write-Host ("Soak {0} min — {1} (port {2})" -f $Minutes, $ReleaseExe, $Port)
$proc = Start-Process -FilePath $exe -ArgumentList @("--port", "$Port") -WorkingDirectory $root -PassThru
Write-Host ("conduite.exe PID {0}" -f $proc.Id)

# Attendre /health = ok (30 s max).
# 127.0.0.1 et pas « localhost » : Windows résout localhost en ::1 d'abord,
# or le moteur écoute en IPv4 (0.0.0.0) — le poll de santé expirerait.
$healthUrl = "http://127.0.0.1:$Port/health"
$ok = $false
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Seconds 1
    if ($proc.HasExited) { Write-Host "Le moteur s'est arrêté au démarrage (code $($proc.ExitCode))." -ForegroundColor Red; exit 3 }
    try {
        $h = Invoke-RestMethod -Uri $healthUrl -TimeoutSec 2
        if ($h.status -eq "ok") { $ok = $true; break }
    } catch {}
}
if (-not $ok) {
    Write-Host "/health ne répond pas ok après 30 s — abandon." -ForegroundColor Red
    try { Stop-Process -Id $proc.Id -Force -ErrorAction Stop } catch {}
    exit 3
}
Write-Host "/health ok — version $($h.version)"

# ------------------------------------------------------------- WebSocket
$ct = [System.Threading.CancellationToken]::None
$ws = New-Object System.Net.WebSockets.ClientWebSocket
$ws.ConnectAsync([Uri]("ws://127.0.0.1:{0}/ws" -f $Port), $ct).GetAwaiter().GetResult() | Out-Null
Write-Host "WebSocket /ws connecté."

$recvBuf = New-Object byte[] 262144
$recvSeg = New-Object "System.ArraySegment[byte]" -ArgumentList @(, $recvBuf)
$pendingRecv = $null
$wsMsgs = [long]0
$gotos = [long]0

function Send-Json([string]$json) {
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
    $seg = New-Object "System.ArraySegment[byte]" -ArgumentList @(, $bytes)
    $script:ws.SendAsync($seg, [System.Net.WebSockets.WebSocketMessageType]::Text, $true, $ct).GetAwaiter().GetResult() | Out-Null
}

# Contrat WS entrant (docs/INTERFACES.md) : {"type":"cmd","cmd":{...Command}}.
# Une commande envoyée NUE (sans l'enveloppe) est IGNORÉE en silence par le
# serveur — le soak tournerait alors à vide sans déclencher un seul GOTO.
function Send-Cmd([string]$inner) {
    Send-Json ('{"type":"cmd","cmd":' + $inner + '}')
}

# ------------------------------------------------------------ boucle soak
$deadline = (Get-Date).AddMinutes($Minutes)
$t0 = Get-Date
$nextGoto = $t0
$nextSample = $t0.AddSeconds($SampleEverySec)
$cueIdx = 0
$verdict = ""
$stallSeen = $false

while ((Get-Date) -lt $deadline) {
    $now = Get-Date

    # Drainer les messages entrants (état ~10 Hz) sans jamais bloquer.
    for ($d = 0; $d -lt 200; $d++) {
        if ($null -eq $pendingRecv) { $pendingRecv = $ws.ReceiveAsync($recvSeg, $ct) }
        if ($pendingRecv.IsCompleted) {
            $wsMsgs++
            $pendingRecv = $null
        } else { break }
    }

    # GOTO en boucle + balayage du master (triangle sur 60 s, 0.05..1).
    if ($now -ge $nextGoto) {
        $cue = $cueList[$cueIdx % $cueList.Count]
        $cueIdx++
        Send-Cmd ('{"cmd":"cue_goto","cue":' + $cue + '}')
        $gotos++
        $phase = (($now - $t0).TotalSeconds % 60.0) / 60.0
        if ($phase -gt 0.5) { $phase = 1.0 - $phase }
        $master = [math]::Round(0.05 + 1.9 * $phase, 3)   # 0.05 -> 1.0 -> 0.05
        Send-Cmd ('{"cmd":"param_set","addr":"master/intensity","value":{"f":' + $master.ToString([Globalization.CultureInfo]::InvariantCulture) + '},"source":"ui"}')
        $nextGoto = $now.AddSeconds($GotoEverySec)
    }

    # Échantillon process + santé.
    if ($now -ge $nextSample) {
        if ($proc.HasExited) { $verdict = "ECHEC (le moteur s'est arrêté, code $($proc.ExitCode))"; break }
        $p = Get-Process -Id $proc.Id
        $elapsed = [int]($now - $t0).TotalSeconds
        $row = "{0},{1},{2},{3},{4},{5},{6},{7}" -f `
            (Get-Date -Format "yyyy-MM-ddTHH:mm:ss"), $elapsed, `
            ([math]::Round($p.WorkingSet64 / 1MB, 2).ToString([Globalization.CultureInfo]::InvariantCulture)), `
            ([math]::Round($p.PrivateMemorySize64 / 1MB, 2).ToString([Globalization.CultureInfo]::InvariantCulture)), `
            $p.Threads.Count, $p.HandleCount, $wsMsgs, $gotos
        Add-Content -Path $csv -Encoding utf8 -Value $row
        Write-Host ("  {0,6} s  WS={1,8:n1} Mo  priv={2,8:n1} Mo  thr={3,3}  hnd={4,5}  gotos={5}" -f `
            $elapsed, ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB), $p.Threads.Count, $p.HandleCount, $gotos)
        try {
            $h = Invoke-RestMethod -Uri $healthUrl -TimeoutSec 3
            if ($h.status -ne "ok") {
                if ($stallSeen) { $verdict = "ECHEC (moteur figé : /health = $($h.status), tick_age_ms = $($h.tick_age_ms))"; break }
                $stallSeen = $true   # tolérer UN échantillon (GC, antivirus…), deux d'affilée = figé
            } else { $stallSeen = $false }
        } catch {
            if ($stallSeen) { $verdict = "ECHEC (/health injoignable deux fois d'affilée)"; break }
            $stallSeen = $true
        }
        $nextSample = $now.AddSeconds($SampleEverySec)
    }

    Start-Sleep -Milliseconds 100
}

# --------------------------------------------------------------- arrêt
Write-Host "Arrêt propre (cmd quit)…"
try { Send-Cmd '{"cmd":"quit"}' } catch {}
try { $ws.Dispose() } catch {}
if (-not $proc.WaitForExit(15000)) {
    Write-Host "Pas d'arrêt en 15 s — Stop-Process Id $($proc.Id)." -ForegroundColor Yellow
    try { Stop-Process -Id $proc.Id -Force -ErrorAction Stop } catch {}
    $proc.WaitForExit(5000) | Out-Null
}

# Zéro résiduel : ni conduite, ni ffmpeg lancé depuis ce dépôt.
Start-Sleep -Seconds 2
$leftC = @(Get-Process -Name "conduite" -ErrorAction SilentlyContinue)
$leftF = @(Get-Process -Name "ffmpeg" -ErrorAction SilentlyContinue | Where-Object {
    try { $_.Path -like "$root*" } catch { $false }
})
foreach ($z in ($leftC + $leftF)) {
    Write-Host "Résiduel : $($z.Name) PID $($z.Id) — Stop-Process." -ForegroundColor Yellow
    try { Stop-Process -Id $z.Id -Force -ErrorAction Stop } catch {}
}
if (($leftC.Count + $leftF.Count) -eq 0) { Write-Host "Zéro process résiduel." }

# --------------------------------------------------------------- verdict
if ($verdict -eq "") {
    $rows = @(Import-Csv $csv)
    if ($rows.Count -lt 8) {
        $verdict = "ECHEC (trop peu d'échantillons : $($rows.Count))"
    } else {
        # Fenêtres de comparaison : après chauffe (25-50 %) vs fin (75-100 %).
        function Median([double[]]$v) {
            $s = $v | Sort-Object
            $n = $s.Count
            if ($n % 2 -eq 1) { return $s[[int](($n - 1) / 2)] }
            return ($s[$n / 2 - 1] + $s[$n / 2]) / 2.0
        }
        $n = $rows.Count
        $base = $rows[[int]($n * 0.25)..[int]($n * 0.5 - 1)]
        $fin = $rows[[int]($n * 0.75)..($n - 1)]
        $wsBase = Median(@($base | ForEach-Object { [double]$_.working_set_mb }))
        $wsFin = Median(@($fin | ForEach-Object { [double]$_.working_set_mb }))
        $hndBase = Median(@($base | ForEach-Object { [double]$_.handles }))
        $hndFin = Median(@($fin | ForEach-Object { [double]$_.handles }))
        $hours = ([double]$fin[-1].elapsed_s - [double]$base[0].elapsed_s) / 3600.0
        if ($hours -le 0) { $hours = 0.001 }
        $growth = [math]::Round($wsFin - $wsBase, 1)
        $slope = [math]::Round(($wsFin - $wsBase) / $hours, 2)
        $hndGrowth = $hndFin - $hndBase
        Write-Host ""
        Write-Host ("Mémoire (working set) : {0:n1} Mo -> {1:n1} Mo  (delta {2:n1} Mo, pente {3:n2} Mo/h)" -f $wsBase, $wsFin, $growth, $slope)
        Write-Host ("Handles               : {0:n0} -> {1:n0}" -f $hndBase, $hndFin)
        $memOk = ($growth -le [math]::Max(10.0, $wsBase * 0.05)) -and ($slope -le 3.0)
        $hndOk = ($hndGrowth -le [math]::Max(100.0, $hndBase * 0.25))
        if ($memOk -and $hndOk) { $verdict = "PLATE" } else { $verdict = "CROISSANTE" }
    }
}

Write-Host ""
Write-Host ("VERDICT : {0}" -f $verdict) -ForegroundColor $(if ($verdict -eq "PLATE") { "Green" } else { "Red" })
Write-Host ("CSV : {0}" -f $csv)
if ($verdict -eq "PLATE") { exit 0 }
if ($verdict -like "ECHEC*") { exit 3 }
exit 2
