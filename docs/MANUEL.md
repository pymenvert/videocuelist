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

## 9. Raccourcis

| Touche | Action |
|---|---|
| Espace | GO (jamais dans un champ de saisie) |
| B (maintenu) | Noir d'urgence (DBO) |
| T | Tap tempo |
| O | Notes de régie de la cue en standby |
| Flèches | Nudge du coin sélectionné (Maj ×10, Alt ×0,1) |
| 1–9, 0 | Onglets (0 = Réglages) |

## 10. Dépannage rapide

| Symptôme | Piste |
|---|---|
| « ffmpeg introuvable » | Copier `ffmpeg.exe`/`ffprobe.exe` dans `bin/`, ou installer ffmpeg dans le PATH. |
| Pas d'image sur la sortie | Onglet Sorties : sortie activée ? bon moniteur ? Master > 0 ? DBO relâché ? |
| L'UI ne répond plus | La page se reconnecte seule (bandeau). Sinon : recharger le navigateur — le moteur, lui, n'a pas bougé. |
| Vidéo saccadée | Transcoder en HAP ; vérifier le disque (SSD conseillé) ; fps affichés dans Santé. |
| Port déjà pris | `conduite --port 9821`, ou libérer 9820. |
