# Plan par phases

Règle : **chaque phase livre un outil utilisable en l'état**, avec un critère de sortie testable. On ne passe pas à la phase suivante sans l'avoir validé.

> **État au 2026-08-05** : v1 desktop construite en une itération nocturne (phases 0 à 4
> compressées, backend vidéo ffmpeg, Windows validé). **Ubuntu x86-64 vérifié** depuis :
> build, tests, exécution `--headless`, paquet `.deb` installé et lancé sous son
> utilisateur système. Restent de la phase 0 : la validation **macOS** réelle (la CI y
> compile et y passe les tests, mais rien n'a été lancé sur un Mac) et le **bench
> Raspberry Pi** — le rendu GL et le décodage n'ont jamais tourné sur un Pi, et le
> backend GStreamer reste à brancher derrière `PlayerBackend`. De la phase 5, restent
> le transcodeur HAP intégré, l'installeur Windows et le `.app` macOS. Les critères de
> sortie ci-dessous restent la référence pour valider chaque périmètre en conditions
> réelles.

## Phase 0 — Socle & dérisquage

Workspace Rust, CI GitHub Actions (build 4 plateformes), reprise des acquis Lanterne (shaders homographie, PlayerBackend, pipelines GStreamer), une sortie plein écran, lecture vidéo HAP + H.264, bench.

- Décoder : quelle forme d'emprunt pour chaque crate Lanterne (copie vs dépendance git).
- Valider macOS (GStreamer + GL) — c'est LE risque de la phase.
- Bench Pi 4 / Pi 5 : nombre de couches HAP 1080p tenables depuis SSD USB3, HEVC 4K HW sur Pi 5.

**Sortie : 2 vidéos HAP 1080p en boucle sur Pi 4 pendant 30 min sans drop, et la même app qui tourne sur Mac, Windows, Ubuntu.**

## Phase 1 — Mapping & sorties

Slices corner-pin (plusieurs par sortie), multi-sorties, éditeur web (poignées, nudge clavier, zoom/pan, vues source/sortie), mires d'identification, réglages par slice (opacité, gains RGB, flip, blend, ordre z), snapshots de géométrie.

**Sortie : mapper 4 vidéos sur 4 surfaces réelles au projecteur, dont 2 sorties simultanées, au clavier-souris uniquement.**

## Phase 2 — Paramètres & matériaux ISF

Crate `params` complet (plages, courbes, lissage, adresses OSC, interpolation typée), parseur ISF + GLSL brut, matériau comme source et comme effet, hot-reload en édition, précompilation.

**Sortie : le DomePack de Pym chargé tel quel, ses paramètres animés depuis l'UI, un ISF d'editor.isf.video importé sans modification.**

## Phase 3 — Contrôle MIDI / OSC / Art-Net

OSC in/out (adresses stables + feedback), MIDI learn + soft-takeover + feedback + MSC, nœud Art-Net + patch 8/16 bits + lissage + personality DMX documentée, modes live/scénarisé.

**Sortie : un fondu d'opacité propre piloté depuis une console DMX, un GO déclenché en MIDI, le tout simultanément.**

## Phase 4 — Cues & conduite

Crate `cue` : snapshots, decks A/B, transitions (cut/crossfade/par le noir, durées, courbes), continuité de lecture, numérotation décimale, follow/wait/boucles, préchargement systématique, vue Live (cuelist + progressions + program/preview + santé), master/DBO, mode show verrouillé.

**Sortie : filer un « spectacle » de 20 cues (vidéos + matériaux, 2 sorties) au clavier seul, sans toucher la souris, avec un média volontairement manquant qui ne casse rien.**

## Phase 5 — Robustesse & finition

Autosave + récupération après crash, « Collecter le show », transcodeur HAP intégré, infobulles complètes + mode « ? », bilingue FR/EN, thème finalisé, undo/redo, mode player headless Pi (systemd + watchdog), packaging (installeur Windows, .app macOS, .deb, zip portable), manuel utilisateur.

**Sortie : une vraie date en conditions réelles, montage → filage → démontage, sans intervention dans le code.**

## v2+ (backlog assumé, rien de tout ça en v1)

- Mesh warp bézier + masques à bord doux
- Edge blending multi-projecteurs
- NDI in/out (plugin séparé — licence), Spout/Syphon
- Timecode LTC/MTC (chase)
- Web remote dédiée tablette (GO + cuelist en miroir)
- Cues partielles / overlays (n'affecter que certains slices)
- ISF multi-pass + buffers persistants (feedback)
- Multi-layers par slice
- Édition à distance complète d'un player Pi
- Main/backup synchronisés
