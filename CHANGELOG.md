# Changelog

Tous les changements notables de Conduite sont documentés ici.
Format : [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) — versionnage [SemVer](https://semver.org/lang/fr/).

## [0.1.0] — en préparation

Première version : le moteur complet, l'interface de régie web et la
version portable Windows.

### Ajouté

- **Moteur de spectacle** (13 crates Rust) : cues avec transitions
  (cut/fondu/par le noir, courbes), follow (manuel/fin de média/minuterie),
  numérotation décimale, GO/BACK/GOTO ; paramètres adressables et
  enregistrés par cue ; écritures disque atomiques, chargement de show
  tolérant (média manquant = damier, jamais un refus).
- **Sorties et mapping** : jusqu'à 4 sorties (fenêtré/plein écran), slices
  multi-surfaces à 4 coins (nudge clavier, mires de calage), préviews
  Program/Standby MJPEG.
- **Médias et matériaux** : vidéos (HAP, H.264, HEVC… via FFmpeg en
  sous-processus), images, shaders ISF/GLSL avec paramètres automatiques ;
  vignettes ; « Collecter le show ».
- **Modulation** : LFO (6 formes, Hz ou BPM avec tap tempo) et bandes
  audio (entrée micro/ligne, FFT, enveloppe attack/release), profondeur de
  branchement enregistrée par cue ; spectre en direct dans l'onglet.
- **Pilotage** : OSC in/out (`/conduite/...`), MIDI (notes, CC 7/14 bits
  avec soft-takeover, MIDI Show Control), Art-Net (récepteur DMX patchable,
  répond à ArtPoll) ; interface web de régie complète (10 onglets, FR/EN),
  utilisable depuis une tablette.
- **Timecode (MTC)** : réception MIDI Time Code sur le port d'entrée
  existant (quarter-frames et full-frames, 24/25/29,97 DF/30 fps,
  compensation de latence, drop-frame exact) ; **chase de cues** opt-in
  (« Chase timecode » dans Réglages, désactivé par défaut — les shows
  existants sont intacts) : chaque cue peut porter un déclencheur
  `HH:MM:SS:FF` — à l'avancée du TC la cue part en GO automatique
  (transition respectée), un saut avant/arrière ou un re-verrouillage se
  cale en GOTO sur la dernière cue passée, une perte de signal laisse
  2 s de roue libre puis suspend le chase **sans rien couper** ; cues
  manuelles et cues timecodées coexistent ; affichage du TC entrant en
  direct (Live et pied de page, vert/orange/gris), colonne Timecode et
  badge TC dans la liste des cues, toasts au verrouillage/perte.
- **Interface bilingue FR/EN** : **Réglages → Langue** bascule toute
  l'interface entre français et anglais, immédiatement et sans rechargement,
  y compris en pleine conduite ; le choix est enregistré dans le show (une
  conduite préparée en anglais s'ouvre en anglais chez le régisseur suivant).
  Suivent la langue : libellés, boutons, menus contextuels, infobulles,
  confirmations, toasts de l'interface **et** les avertissements du centre
  « État du show » (média manquant, moniteur perdu, port occupé — le moteur
  publie désormais `runtime.warnings` sous forme de gabarit + valeurs). Le
  français reste la langue source : une chaîne non traduite s'affiche en
  français, jamais en blanc. Le journal et les fichiers `logs/` restent en
  français (outil de diagnostic). Garde-fou en CI : toute chaîne française
  ajoutée à la web UI sans traduction fait échouer les tests.
- **Fiabilité spectacle** : verrou mono-instance (2ᵉ lancement refusé,
  code 10), autosave, backups rotatifs par show avec proposition de
  récupération au démarrage après arrêt sale, journaux horodatés,
  bandeau santé (fps, drops, CPU, températures), mode Show verrouillant
  l'édition, arrêt propre (bouton Quitter, Ctrl-C, codes de sortie
  0/10/11), endpoint `/health` (détecte « vivant mais figé »),
  anti-veille système pendant le show, détection de perte GPU,
  soak tests mémoire plate (40 min + 10 min build packagé).
- **Conduite sûre** : armement par cue (désarmée = grisée et sautée au
  GO/follow), protection double-GO (délai minimal réglable, bordure
  rouge), Échap = panic universel (simple = fondu, double = arrêt sec),
  garde-fou DBO, undo/redo (Ctrl+Z/Ctrl+Maj+Z) avec confirmations sur
  le destructif, état réel des protocoles (OSC in/out, MIDI, Art-Net)
  dans Patch et le pied de page, centre « État du show » (avertissements
  et cues cassées).
- **Interface premium** : refonte tokens/typo/espacement, accessibilité
  (focus visible, `color-scheme: dark`, contrastes AA), Live pro (temps
  restant en grand, notes de régie de la cue en standby, panneau cues
  actives, badges), toasts systématiques, sélecteur de shows daté,
  micro-UX des sliders (saisie exacte au clic, clic droit = défaut,
  Maj+drag = réglage fin), dialogue À propos (version, licence,
  crédits), favicon et titre dynamique, navigation tablette.
- **Confort de régie** : menus contextuels (clic droit) sur cues, slices,
  médias et paramètres ; raccourcis clavier remappables (mode « apprendre »,
  persistés dans le patch du show — Espace/Échap/B restent prioritaires et
  non remappables) ; mires premium (grilles 4/16, barres SMPTE, mire
  d'identification avec nom et résolution de la sortie incrustés).
- **Préview H.264 (WebCodecs)** : flux `ws://…/preview.h264` (config JSON
  puis frames Annex-B, encodeur Windows MediaFoundation `h264_mf` via le
  FFmpeg embarqué) — bande passante divisée par ~10 ; repli MJPEG
  automatique (Safari, encodeur absent, ou aucune frame décodée en 3 s).
- **Support** : bouton « Rapport de diagnostic » (zip horodaté dans
  `logs/` : journaux récents, config, show, versions, santé — chemins
  personnels expurgés) ; crash dumps locaux hors-process
  (`logs/crash/`, rétention 5, aucun envoi réseau) ; vérification de mise
  à jour **opt-in** (désactivée par défaut, une requête au démarrage en
  mode édition, timeout 3 s, jamais de téléchargement — badge discret).
- **Endurance** : allocateur mimalloc ; priorité process relevée en mode
  Show (option) ; résolution DNS OSC hors du thread de tick ; ré-ancrage
  des transitions après une veille machine ; soak test scripté
  (`tools/soak.ps1`) et rituel de release documenté (`docs/RELEASE.md`).
- **Linux et player Raspberry Pi** : dossier de travail explicitable
  (`--home <dir>`, ou `CONDUITE_HOME`) — le binaire peut vivre en lecture
  seule dans `/usr/bin` pendant que les shows vivent ailleurs ;
  `tools/package-linux.sh [--deb]` produit le **portable** `tar.gz` (même
  disposition que le portable Windows, ffmpeg fourni par la distribution)
  **et** un **paquet Debian** (binaire système, données dans
  `/var/lib/conduite`, utilisateur système dédié, désinstallation qui ne
  touche jamais aux shows), chacun avec son SHA-256 ; **service systemd**
  désactivé par défaut, qui relance sur perte GPU (code 11) mais jamais sur
  port occupé (code 10), et **chien de garde** `conduite-health.timer` qui
  interroge `/health` toutes les 30 s pour rattraper un moteur « vivant mais
  figé » — le cas que `Restart=on-failure` ne voit pas — sans jamais
  ressusciter un service arrêté à la main.
- **Produit** : version portable Windows (`tools/package.ps1`) — zip
  versionné avec SHA-256, FFmpeg **LGPL** embarqué avec notices
  (`licenses/FFMPEG.txt`), attributions des dépendances Rust
  (`licenses/THIRD-PARTY-NOTICES.html` via cargo-about), crédits shaders
  (`shaders/CREDITS.txt`), LISEZMOI ; icône et métadonnées VERSIONINFO de
  l'exe ; `--help`/`--version` (avec hash git) ; endpoint `/about` ;
  garde-fous CI (cargo-deny : licences + advisories).

[0.1.0]: https://github.com/pymenvert/videocuelist/releases/tag/v0.1.0
