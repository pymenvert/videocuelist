# Conduite

[![CI](https://github.com/pymenvert/videocuelist/actions/workflows/ci.yml/badge.svg)](https://github.com/pymenvert/videocuelist/actions/workflows/ci.yml)
[![Licence : MIT](https://img.shields.io/badge/licence-MIT-blue.svg)](LICENSE)

**Régie vidéo de spectacle.** La conduite par cues de QLab, le mapping de
MadMapper et le pilotage d'un média-serveur — dans un seul outil, piloté
depuis un navigateur, pensé pour le plateau : **en spectacle, l'outil ne
surprend jamais.**

> Capture d'écran à venir.

## Fonctionnalités

| | |
|---|---|
| **Conduite** | Cuelist avec GO/BACK/GOTO, numérotation décimale, transitions (cut, fondu, par le noir + courbes), follow (manuel, fin de média, minuterie), notes de régie, mode Show verrouillé |
| **Mapping** | Jusqu'à 4 sorties (fenêtré/plein écran), plusieurs slices par sortie, warp 4 coins au pixel près, mires de calage, identification des écrans |
| **Médias** | Vidéos HAP / H.264 / HEVC (FFmpeg), images, préchargement systématique — le GO est instantané ; média manquant = damier, jamais un show qui refuse de se charger |
| **Matériaux** | Shaders ISF / GLSL avec paramètres exposés automatiquement (sliders, couleurs), pack DomePack livré |
| **Modulation** | LFO (6 formes, Hz ou BPM + tap tempo), bandes audio temps réel (FFT, enveloppe), profondeur enregistrée par cue |
| **Pilotage** | Chaque paramètre adressable en **OSC**, mappable **MIDI** (soft-takeover, MIDI Show Control), patchable **Art-Net** ; feedback sortant |
| **Régie web** | Interface complète dans le navigateur — localement ou depuis une tablette du même réseau, aucune installation ; **bilingue français / anglais**, bascule immédiate même en pleine conduite, langue enregistrée dans le show |
| **Fiabilité** | Écritures atomiques, autosave + backups rotatifs + récupération après crash, verrou mono-instance, journaux horodatés, bandeau santé |

**Cibles** : Windows · macOS · Ubuntu · Raspberry Pi 4/5.

## Installation

### Version portable Windows

Dézipper `Conduite-{version}-win64.zip`, lancer `conduite.exe`, ouvrir
**http://localhost:9820**. Aucune installation, aucun registre : le dossier
se copie sur une clé USB. Voir le `LISEZMOI.txt` livré (pare-feu, raccourcis).

### Depuis les sources

```
cargo run -p conduite
```

Puis ouvrir **http://localhost:9820**. Au premier lancement, un show de
démonstration est créé ; déposez vos vidéos dans `media/`, vos shaders ISF
dans `shaders/`.

- `ffmpeg`/`ffprobe` requis (dans le PATH, ou copiés dans `bin/` du dossier portable).
- OSC en écoute sur le port 9000 (`/conduite/cue/go`, `/conduite/param/...`),
  Art-Net sur 6454, MIDI depuis l'onglet Patch.
- Options : `conduite --help` (`--headless`, `--port`, `--show`).
- Packaging portable : `powershell -File tools/package.ps1` →
  `dist/Conduite-{version}-win64.zip` + SHA-256 (FFmpeg LGPL embarqué depuis
  `tools/ffmpeg-lgpl/`, notices copiées depuis `licenses/`).

## Endurance et fiabilité

Un logiciel de spectacle se juge sur la durée, pas sur la démo. Ce que
Conduite met sur la table — et qui se vérifie :

- **Soaks archivés** : chaque campagne d'endurance (GO en boucle +
  balayage du master sur le build release) échantillonne mémoire, threads
  et handles toutes les 30 s ; les CSV sont commités dans
  [docs/bench/](docs/bench/). Le rituel est scripté :
  `powershell -File tools/soak.ps1` (8 h par défaut, verdict automatique
  PLATE / CROISSANTE, zéro process résiduel exigé). Le soak 8 h est la
  **gate de release** : verdict PLATE ou pas de publication.
- **Supervision** : `GET /health` répond `{status: ok|stalled,
  tick_age_ms, version}` — un moteur « vivant mais figé » se détecte de
  l'extérieur.
- **Watchdog-friendly** : codes de sortie documentés (0 = arrêt normal,
  10 = instance/port déjà pris — ne pas relancer, 11 = perte GPU —
  relancer), verrou mono-instance, écritures atomiques, autosave +
  récupération après crash.

L'endurance est mesurée sur Windows (la plateforme livrée aujourd'hui) ;
les autres cibles seront benchées quand leurs builds existeront.

Le processus complet de publication est documenté dans
[docs/RELEASE.md](docs/RELEASE.md).

## Documentation

| Fichier | Contenu |
|---|---|
| [docs/MANUEL.md](docs/MANUEL.md) | **Manuel de l'utilisateur** (régie, protocoles, dépannage) |
| [docs/RELEASE.md](docs/RELEASE.md) | Checklist de release (tests, packaging, soak 8 h, publication) |
| [CHANGELOG.md](CHANGELOG.md) | Versions et changements |
| [docs/SPEC.md](docs/SPEC.md) | Spécification fonctionnelle : concepts, features, conduite |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Stack, découpage en crates, réutilisation Lanterne |
| [docs/DECISIONS.md](docs/DECISIONS.md) | Décisions actées (date + pourquoi) — source de vérité |
| [docs/PLAN.md](docs/PLAN.md) | Plan par phases avec critères de sortie |

## Liens avec Lanterne (repo `toolbox`)

Conduite réutilise les acquis du projet [Lanterne](https://github.com/pymenvert/toolbox)
(moteur Rust + GL, crates OSC/MIDI/Art-Net, bus de commandes, état JSON
versionné) mais c'est un **produit distinct** : Lanterne est un node/player
généraliste, Conduite est une régie de spectacle centrée sur la cuelist.

## Licences

- Conduite : **MIT** — voir [LICENSE](LICENSE).
- FFmpeg (programme séparé appelé en sous-processus) : **LGPL v3** —
  [licenses/FFMPEG.txt](licenses/FFMPEG.txt).
- Dépendances Rust : [licenses/THIRD-PARTY-NOTICES.html](licenses/THIRD-PARTY-NOTICES.html).
- Shaders livrés : [shaders/CREDITS.txt](shaders/CREDITS.txt).
