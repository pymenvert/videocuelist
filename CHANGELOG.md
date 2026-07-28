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
  répond à ArtPoll) ; interface web de régie complète (10 onglets, FR),
  utilisable depuis une tablette.
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
- **Produit** : version portable Windows (`tools/package.ps1`) — zip
  versionné avec SHA-256, FFmpeg **LGPL** embarqué avec notices
  (`licenses/FFMPEG.txt`), attributions des dépendances Rust
  (`licenses/THIRD-PARTY-NOTICES.html` via cargo-about), crédits shaders
  (`shaders/CREDITS.txt`), LISEZMOI ; icône et métadonnées VERSIONINFO de
  l'exe ; `--help`/`--version` (avec hash git) ; endpoint `/about` ;
  garde-fous CI (cargo-deny : licences + advisories).

[0.1.0]: https://github.com/pymenvert/videocuelist/releases/tag/v0.1.0
