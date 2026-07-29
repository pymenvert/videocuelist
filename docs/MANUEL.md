# Conduite — manuel de l'utilisateur (v1)

Régie vidéo de spectacle : on prépare des **cues** (des scènes complètes), on les envoie au **GO**, tout est pilotable en **OSC / MIDI / Art-Net**.

## 1. Démarrage

1. Lancer `conduite.exe` (dossier portable) ou `cargo run -p conduite`.
2. Ouvrir **http://localhost:9820** — c'est l'interface de régie (utilisable depuis un
   autre poste ou une tablette du même réseau : `http://<ip-de-la-machine>:9820`).
3. Déposer les vidéos dans `media/`, les shaders ISF dans `shaders/`, puis onglet
   **Médias → Re-scanner**.

Un show de démonstration est créé au premier lancement (mires, matériaux, une vidéo).
`ffmpeg` doit être disponible (dossier `bin/` du portable, sinon dans le PATH).

## 2. Les concepts en 30 secondes

| Terme | Ce que c'est |
|---|---|
| **Sortie** | Un écran/vidéoprojecteur physique (jusqu'à 4). Fenêtré ou plein écran. |
| **Slice** | Une surface mappée sur une sortie (4 coins étirables). Plusieurs slices par sortie. |
| **Média** | Vidéo, image ou **matériau** ISF/GLSL. Se pose sur un slice. |
| **Cue** | Une scène complète enregistrée : quoi sur quel slice + valeurs des paramètres + transition d'entrée. |
| **Conduite** | La liste des cues, dans l'ordre du spectacle. GO = envoyer la suivante. |
| **Paramètre** | Toute valeur réglable (opacité, vitesse, uniform de shader…). Pilotable OSC/MIDI/DMX et enregistrée dans les cues. |

## 3. Préparer un spectacle (workflow type)

1. **Sorties** : déclarer les sorties (moniteur, résolution, plein écran). Bouton
   « Identifier » pour afficher le numéro sur chaque écran.
2. **Mapping** : créer les slices, tirer les 4 coins à la souris (flèches = réglage fin,
   Maj = ×10, Alt = ×0,1). Mires de calage par slice ou globales.
3. **Médias / Matériaux** : assigner un contenu au slice sélectionné. Les paramètres
   des shaders ISF apparaissent automatiquement (sliders, couleurs…).
4. **Cues** : régler la scène comme voulu, puis « Enregistrer l'état dans la cue ».
   Numérotation décimale (1, 2, 2.5…) pour insérer sans renuméroter. Régler la
   **transition** d'entrée (cut, fondu, par le noir + durée + courbe) et le **follow**
   (manuel, à la fin du média, ou minuterie).
5. **Patch** (si pilotage externe) : ajouter des mappings MIDI, patcher des canaux
   Art-Net, relever les adresses OSC.
6. **Mode Show** (Réglages) : verrouille toute l'édition pour le spectacle.

## 4. En jeu (onglet Live)

- **GO** (ou Espace) envoie la cue en standby. **BACK** revient. **GOTO** saute à un numéro.
- Barre de progression sur la cue active (temps média / minuterie de follow).
- **Master** = intensité générale. **DBO** = noir d'urgence (appui maintenu — sécurité anti-fausse manip).
- Moniteurs **Program** (ce qui sort) et **Préview** (la cue en standby).
- Bandeau santé : fps par sortie, drops, CPU, températures, état des protocoles.
- La cue suivante est **toujours préchargée** : le GO est instantané.

## 5. Modulation (LFO & audio)

Onglet **Modulation** : créer des modulateurs et les brancher sur n'importe quel paramètre.

- **LFO** : sinus, triangle, carré, dent de scie, random, drift — fréquence en Hz
  ou synchronisée au **BPM** (tap tempo : bouton ou touche T, ou OSC/MIDI).
- **Bande audio** : entrée micro/ligne de la machine, bande de fréquences définie en Hz
  (ex. 60–120 Hz pour le kick), enveloppe attack/release → module ce que vous voulez.
- La profondeur des branchements s'enregistre **par cue** (une cue peut activer/couper une modulation).

## 6. Pilotage externe

### OSC (port 9000 par défaut, réponses/feedback configurables)

```
/conduite/cue/go            GO
/conduite/cue/back          retour
/conduite/cue/goto 12.5     aller à la cue (float ou chaîne)
/conduite/param/<adresse> f n'importe quel paramètre (ex. /conduite/param/slice/1/opacity 0.5)
/conduite/master f          intensité générale (0..1)
/conduite/dbo f             noir d'urgence avec temps de fondu en secondes
/conduite/bpm f | /conduite/bpm/tap
```
Feedback sortant : `/conduite/status/active`, `/status/standby`, `/status/progress`, `/status/remaining`.
Compatible Chataigne, TouchOSC, Open Stage Control, Bitfocus Companion (module OSC générique).

### MIDI

- Mapping dans l'onglet Patch (ajout manuel : canal, note/CC, cible). Notes → commandes
  (GO, DBO…), CC 7/14 bits → paramètres avec **soft-takeover** (pas de saut de valeur).
  Le bouton *Learn* (mapper en bougeant le contrôleur) arrive dans une prochaine version.
- **MIDI Show Control** : GO/STOP/RESUME/LOAD reçus d'une console lumière ou de QLab.

### Timecode (MTC) — chase de cues

Conduite peut suivre un **MIDI Time Code** entrant et déclencher les cues toute
seule, comme QLab ou un média-serveur : idéal quand la bande-son (ou la console)
fait foi et que la vidéo doit se caler dessus.

**Brancher une source :**
1. La source MTC (DAW, QLab, console…) doit arriver sur le **port MIDI d'entrée
   utilisé par Conduite** — le même que le pilotage MIDI (état visible dans
   l'onglet Patch). Cadences reconnues : 24, 25, 29,97 DF et 30 i/s.
2. Dans **Réglages → Chase timecode**, cochez la case.
3. Dans l'onglet **Cues**, colonne **Timecode**, donnez à chaque cue concernée
   sa position de déclenchement au format `HH:MM:SS:FF` (ex. `00:05:30:00`).
   Champ vide = cue manuelle, jamais touchée par le chase. Les deux types de
   cues coexistent librement dans la même conduite.

Le timecode reçu s'affiche près de l'horloge, en bas à droite : **vert** =
signal verrouillé (la cadence est dans l'infobulle), **orange** = signal perdu
(dernier timecode figé), **gris** = aucun signal reçu.

**Sémantique du calage :**
- **Avancée normale** : chaque cue dont le déclencheur passe est jouée (GO
  direct, sa transition est respectée).
- **Saut avant ou arrière** (relecture, scrub) : Conduite se cale par GOTO sur
  la **dernière** cue dont le déclencheur est ≤ au timecode courant (noir si
  aucune).
- **Perte de signal** : 2 s de **roue libre** (le temps continue d'avancer en
  interne), puis le chase se met en pause — **les cues actives continuent**,
  rien n'est coupé ni éteint. Au retour du signal, re-calage comme après un
  saut. Chaque verrouillage/perte est signalé par un toast et une ligne de
  Journal.

Le chase fonctionne bien sûr en **mode Show** — c'est son usage principal.
Note : l'option « Timecode » du popover d'animation des *paramètres* reste
grisée ; le chase pilote les **cues** (l'animation de paramètres au timecode
viendra plus tard).

### Art-Net (port 6454)

Le logiciel se comporte comme un récepteur DMX : patchez univers/canal → paramètre
(8 ou 16 bits, plage, lissage). Le nœud répond à ArtPoll (visible des consoles).

## 7. Le dossier portable

```
Conduite/
├── conduite.exe      ← le logiciel (tout est dedans, interface comprise)
├── bin/ffmpeg.exe    ← décodage vidéo (optionnel si ffmpeg est dans le PATH)
├── media/            ← vos vidéos et images
├── shaders/          ← vos matériaux ISF (.fs)
├── shows/            ← vos conduites (JSON lisible + backups automatiques)
├── logs/             ← journaux horodatés
├── licenses/         ← licences : Conduite (MIT), FFmpeg, composants tiers
├── LISEZMOI.txt      ← démarrage express (pare-feu, raccourcis)
├── MANUEL.md         ← ce manuel
└── config.toml       ← réglages machine (créé au premier lancement)
```

`config.toml` (réglages machine, distincts des réglages du show) :
`http_port` (défaut 9820), `http_bind` (défaut `0.0.0.0`), `audio_input`
(nom du périphérique d'entrée pour la modulation audio, absent = désactivé),
`last_show` (show chargé au démarrage), `target_fps` (cadence de rendu,
défaut 60).

Tout est relatif : copiez le dossier sur une clé USB, il repart ailleurs.
« Collecter le show » (Réglages) rassemble show + médias dans un dossier autonome.

## 8. Conseils spectacle

- **Codec** : préférez le **HAP** (`ffmpeg -c:v hap`) — décodage léger, scrub instantané,
  multi-couches. H.264/HEVC fonctionnent aussi.
- Un média manquant n'empêche jamais le show de se charger : il apparaît en damier
  + alerte rouge dans Médias (re-link possible).
- Sauvegardes : autosave permanent + backups rotatifs dans le dossier du show
  (`shows/<nom-du-show>/backups/`) ; après un crash, un fichier de
  récupération est proposé au démarrage.
- **Mode Show** avant le public : édition verrouillée, fermeture double-confirmée.
- Les logs (`logs/`) horodatent tout : GO, erreurs, protocoles — utile au débrief.

## 9. Checklist « machine de spectacle »

À dérouler sur la machine qui joue, avant la première. Conduite empêche
déjà la veille pendant le show, mais en salle on met **ceinture et
bretelles** — et l'application ne modifie jamais vos réglages système :
c'est vous qui décidez.

- [ ] **Windows Update en pause** pendant la période d'exploitation
      (Paramètres → Windows Update → Suspendre). Un redémarrage forcé à
      l'entracte est le pire scénario.
- [ ] **Veille et écran de veille désactivés** (Alimentation : écran
      « jamais », veille « jamais »). L'app l'empêche pendant le show,
      mais un réglage propre couvre aussi l'avant-spectacle.
- [ ] **Mode d'alimentation « Performances élevées »** (et sur un
      portable : secteur branché, jamais sur batterie).
- [ ] **Driver GPU figé** : pas de mise à jour automatique du pilote en
      exploitation ; on ne change pas un pilote qui marche la veille
      d'une première.
- [ ] **Notifications coupées** (Assistant de concentration / Ne pas
      déranger) : aucun toast Windows par-dessus une sortie.
- [ ] **Antivirus : exclure le dossier de Conduite** (l'exe scanne à
      chaque lancement sinon, et un scan planifié en plein show coûte des
      frames). L'exécutable n'est pas encore signé — l'exclusion évite
      aussi les faux positifs SmartScreen ; la signature Authenticode est
      prévue.
- [ ] **Réseau** : Wi-Fi coupé si la régie est câblée ; pare-feu : autoriser
      conduite.exe (ports 9820 web, 9000 OSC, 6454 Art-Net).
- [ ] **Un GO de contrôle** : dérouler la conduite complète une fois
      (les médias se préchargent, les erreurs se voient dans « État du
      show », pas devant le public).

## 10. Sécurité en conduite

Ce que Conduite fait pour que la fausse manip ou le pépin technique ne
se voie pas depuis la salle — chaque garde-fou existe parce qu'un
incident réel l'a rendu nécessaire quelque part :

- **Anti double-GO** : après un GO, tout GO reçu pendant le délai minimal
  (réglable, 300 ms par défaut, `min_go_interval_ms`) est **ignoré**,
  quelle que soit la source — clic, Espace, OSC, MIDI, MSC. Le bouton
  montre le délai ; un GO refusé produit un avertissement, jamais une
  cue sautée deux fois.
- **Échap = panic universel** : un appui = fondu au noir de conduite
  (durée réglable), double appui = arrêt sec. Jamais désactivable, même
  en mode Show. Le GO suivant reprend la conduite.
- **DBO (dead blackout)** : touche **B maintenue** (l'appui maintenu
  empêche le coude sur le clavier) — voile noir maître par-dessus tout,
  relâché sans toucher à la conduite. C'est le « noir salle » d'urgence.
- **Cues armées/désarmées** : une cue désarmée est grisée et **sautée**
  au GO et au follow — on retire un tableau en répétition sans détruire
  la conduite.
- **Mode Show** : toute édition verrouillée (UI, OSC, MIDI), fermeture
  double-confirmée, sortie du mode par geste volontaire. Restent actifs :
  GO/BACK/GOTO, master, DBO, panic, sauvegarde.
- **Récupération** : autosave permanent + backups rotatifs par show ;
  après un arrêt sale, la version récupérable est **proposée** au
  démarrage (jamais imposée). Toutes les écritures disque sont atomiques.
- **Un média manquant n'annule jamais un show** : damier + alerte, le
  reste de la cue joue.
- **Verrou mono-instance** : un deuxième lancement s'arrête net avec un
  message clair — jamais deux moteurs qui se disputent le MIDI.
- **Supervision** : `GET /health` répond `ok`/`stalled` (moteur « vivant
  mais figé » détecté), codes de sortie documentés (0 = quitté, 10 =
  déjà lancé, 11 = perte GPU) — de quoi brancher un watchdog qui relance
  en quelques secondes.

## 11. Raccourcis

| Touche | Action |
|---|---|
| Espace | GO (jamais dans un champ de saisie) |
| B (maintenu) | Noir d'urgence (DBO) |
| T | Tap tempo |
| O | Notes de régie de la cue en standby |
| Flèches | Nudge du coin sélectionné (Maj ×10, Alt ×0,1) |
| 1–9, 0 | Onglets (0 = Réglages) |

Les raccourcis de conduite se remappent dans **Patch → Clavier** (cliquer
un raccourci, presser la touche voulue). Les raccourcis **système** —
Espace (GO), Échap (panic), B (DBO) — restent prioritaires et ne sont pas
remappables : ce sont des organes de sécurité.

## 12. Dépannage rapide

| Symptôme | Piste |
|---|---|
| « ffmpeg introuvable » | Copier `ffmpeg.exe`/`ffprobe.exe` dans `bin/`, ou installer ffmpeg dans le PATH. |
| Pas d'image sur la sortie | Onglet Sorties : sortie activée ? bon moniteur ? Master > 0 ? DBO relâché ? |
| L'UI ne répond plus | La page se reconnecte seule (bandeau). Sinon : recharger le navigateur — le moteur, lui, n'a pas bougé. |
| Vidéo saccadée | Transcoder en HAP ; vérifier le disque (SSD conseillé) ; fps affichés dans Santé. |
| Port déjà pris | `conduite --port 9821`, ou libérer 9820. |

## 13. Rapport de diagnostic

Pour joindre à un mail de support : **Réglages → Rapport de diagnostic**
(ou commande `diagnostic_report`). Conduite écrit un zip horodaté dans
`logs/diagnostic-<date>.zip`.

**Ce qu'il contient** : les 500 dernières lignes de chaque fichier de
journal, `config.toml`, le `show.json` courant, les versions (Conduite,
`ffmpeg -version`) et l'instantané santé.

**Vie privée** : les chemins personnels sont expurgés avant écriture
(`C:\Users\<votre-nom>` devient `~`), et **rien n'est envoyé nulle part** —
le fichier reste sur votre disque, c'est vous qui l'attachez (ou pas) à
votre message. Vous pouvez l'ouvrir pour vérifier son contenu : c'est un
zip ordinaire.

## 14. Diagnostic avancé (optionnel) : crash dumps Windows

Si un crash reproductible résiste au support, Windows peut conserver un
*dump* du process au moment du crash (WER LocalDumps). C'est une
procédure **système, volontaire et réversible** : Conduite ne modifie
JAMAIS le registre — c'est vous qui exécutez ceci, dans un PowerShell
**administrateur** :

```powershell
# Activer les dumps locaux pour conduite.exe (dans logs\crash du dossier portable)
$k = "HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\conduite.exe"
New-Item -Path $k -Force | Out-Null
Set-ItemProperty $k -Name DumpFolder -Value "C:\chemin\vers\Conduite\logs\crash" -Type ExpandString
Set-ItemProperty $k -Name DumpCount  -Value 5 -Type DWord   # rétention : 5 dumps
Set-ItemProperty $k -Name DumpType   -Value 1 -Type DWord   # 1 = minidump
```

Après le crash suivant, un fichier `.dmp` apparaît dans `logs\crash` —
joignez-le au rapport de diagnostic. Pour tout désactiver :

```powershell
Remove-Item "HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\conduite.exe" -Recurse
```

Les dumps restent **100 % locaux** (ce sont des extraits mémoire :
ne les publiez pas). Même famille de réglages avancés, même principe :
la désactivation des MPO (`OverlayTestMode`) ou le `TdrDelay` peuvent
aider sur certains pilotes GPU capricieux, mais ne les touchez qu'en
dépannage accompagné — jamais la veille d'une première.
