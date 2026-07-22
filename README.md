# VideoCuelist

Régie vidéo de spectacle : mapping multi-surfaces, matériaux ISF/GLSL, conduite par cues, pilotage MIDI / OSC / Art-Net.

**Cibles** : Windows · macOS · Ubuntu · Raspberry Pi 4/5.

## Ce que c'est

Un outil de **conduite vidéo** pensé pour le plateau, qui croise trois familles :

- la **conduite** de QLab : cuelist, GO, follow, transitions, notes de régie ;
- le **mapping** de MadMapper : slices (plusieurs par sortie), warp, mires, multi-sorties ;
- le **pilotage** d'un média-serveur : chaque paramètre adressable en OSC, mappable MIDI, patchable Art-Net (personality DMX documentée pour consoles lumière).

Doctrine : **en spectacle, l'outil ne surprend jamais.** Tout est préchargé avant le GO, un média manquant n'empêche jamais un show de se charger, aucune compilation ni I/O disque pendant le jeu.

## Liens avec Lanterne (repo `toolbox`)

VideoCuelist réutilise les acquis du projet [Lanterne](https://github.com/pymenvert/toolbox) — moteur Rust + GStreamer + GL (homographie testée), crates OSC/MIDI/Art-Net, bus de commandes, état JSON versionné — mais c'est un **produit distinct** : Lanterne est un node/player généraliste, VideoCuelist est une régie de spectacle centrée sur la cuelist.

## Documentation (lire dans cet ordre)

| Fichier | Contenu |
|---|---|
| [docs/DECISIONS.md](docs/DECISIONS.md) | Décisions actées (date + pourquoi) — **source de vérité** |
| [docs/SPEC.md](docs/SPEC.md) | Spécification fonctionnelle : concepts, features, conduite |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Stack, découpage en crates, réutilisation Lanterne |
| [docs/PLAN.md](docs/PLAN.md) | Plan par phases avec critères de sortie |

## État d'avancement

- 2026-07-23 : cadrage initial (spec, décisions, architecture, plan). Prochaine étape : phase 0 (socle moteur + validation macOS + bench Pi).

## Licence

MIT — voir [LICENSE](LICENSE).
