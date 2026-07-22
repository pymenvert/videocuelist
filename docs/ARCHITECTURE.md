# Architecture

## Vue d'ensemble

Un **binaire unique** (`videocuelist`) par machine : moteur vidéo (GStreamer + GL), système de paramètres, moteur de cues, serveurs de contrôle (OSC/MIDI/Art-Net) et serveur web (UI + API). L'UI tourne dans un navigateur — sur la même machine ou à distance (laptop qui pilote un Pi, tablette pour le GO).

```
  MIDI/MSC ─────▶ ┌──────────────────────────────────────────────┐
  OSC ──────────▶ │ control-* ──▶ patch ──▶ params (colonne       │
  Art-Net ──────▶ │                          vertébrale)          │
  Web UI (WS) ──▶ │                    ▲          │                │
                  │                    │          ▼                │
                  │ show model ──▶ cue engine (decks A/B,          │
                  │ (slices, cues)     transitions, préchargement) │
                  │                    │                            │
                  │                    ▼                            │
  fichiers ─────▶ │ players (GStreamer / ISF) ──▶ compositor GL    │
  HAP/H264/HEVC   │        slices → homographie → effets → blend   │
                  │                    │                            │
                  └────────────────────┼────────────────────────────┘
                                       ▼
                        Output 1..n (plein écran natif) + Preview (UI)
```

## Principes non négociables (hérités de Lanterne, étendus)

1. **Tout passe par le bus de commandes** : web UI, OSC, MIDI, DMX et cuelist émettent les *mêmes* commandes internes. Une feature = une commande = automatiquement disponible partout.
2. **Tout paramètre passe par `params`** : valeur, plage, courbe, lissage, adresse OSC, mappings, valeur par cue. Une feature = un paramètre = automatiquement pilotable et scénarisable.
3. **Zéro copie CPU↔GPU autant que possible** : DMABUF → EGLImage → texture GL sur Pi ; upload DXT direct pour le HAP.
4. **État sérialisable** : le show entier est un document JSON versionné (cues, slices, patch, géométrie) → autosave, presets, diff, réparation manuelle.
5. **Le thread de rendu ne bloque jamais** : pas d'I/O, pas de compilation, pas d'allocation lourde en mode show.

## Structure du repo (workspace Rust)

```
videocuelist/
├── Cargo.toml               # workspace
├── crates/
│   ├── core/                # bus de commandes, modèle de show, validation, écritures atomiques
│   ├── params/              # système de paramètres : plages, courbes, lissage, adressage
│   ├── cue/                 # cuelist, decks A/B, transitions, follow, préchargement (testé unitairement)
│   ├── engine/              # players vidéo derrière trait PlayerBackend (v1 : ffmpeg subprocess ; GStreamer en phase Pi), horloges média
│   ├── modulation/          # LFO, BPM maître, analyse audio d'entrée (FFT) → modulateurs de paramètres
│   ├── isf/                 # parseur ISF + traduction GLSL 330/ES300 + wrapper GLSL brut
│   ├── compositor/          # GL : slices, homographie, effets, blend A/B, multi-outputs, mires
│   ├── control-osc/         # serveur/client OSC (rosc)
│   ├── control-midi/        # midir : learn, soft-takeover, feedback, MSC
│   ├── control-artnet/      # nœud Art-Net : ArtPoll/ArtDMX, patch, lissage 44 Hz
│   ├── control-http/        # axum : REST + WebSocket + sert la web UI embarquée
│   ├── media-library/       # scan, vignettes, re-link, « collecter le show », transcodage ffmpeg
│   ├── system/              # santé : FPS, drops, CPU/GPU, température Pi
│   └── app/                 # binaire final : assemble le tout selon la config
├── webui/                   # SPA embarquée dans le binaire (vanilla d'abord, Svelte si ça grossit)
├── deploy/                  # systemd + watchdog (Pi), installeurs Win/Mac, image Pi (phase tardive)
├── docs/
└── tools/                   # bench (phase 0), génération de médias de test
```

Règle : **une fonctionnalité = un crate**, nommé par ce qu'il fait.

## Réutilisation depuis Lanterne (`github.com/pymenvert/toolbox`, MIT)

| Acquis Lanterne | Usage ici | Mode |
|---|---|---|
| `artnet` (crate) | base de `control-artnet` | copie puis divergence (on ajoute patch/lissage) |
| `control-osc`, `control-midi` | patterns + code serveurs | emprunt ciblé |
| trait `PlayerBackend` + `MemoryBackend` | contrat des players + tests sans vidéo | reprise directe |
| shaders homographie, gains RGB, mires | base du `compositor` | reprise directe (validés/testés) |
| bus de commandes, état JSON versionné | patterns de `core` | emprunt ciblé |
| `validate_media_path`, écritures atomiques | sécurité des chemins | reprise directe |
| pipelines GStreamer Pi 4/Pi 5 (H.264/HEVC HW) | `engine` | reprise directe |

Mode d'emprunt tranché crate par crate en phase 0 (copie vs dépendance git). Par défaut : **copie documentée** — les deux produits divergent librement, même auteur, même licence.

## Ce qui est nouveau (le cœur de VideoCuelist)

- **`params`** : la colonne vertébrale. Chaque `Parameter` (float/int/bool/enum/color/point2D) porte : valeur courante, défaut, plage, courbe de réponse, lissage, adresse OSC stable, mappings MIDI/DMX, mode live/scénarisé, et sa valeur par cue. Interpolation typée pour les transitions (continu = fondu, discret = bascule au cut point).
- **`cue`** : machine à états de conduite. Decks A/B : la cue active vit en A, la cue en standby se précharge en B (players ouverts, première frame décodée, shaders compilés) ; transition = blend A→B piloté par l'horloge moteur ; à la fin, B devient A. **Continuité** : si un média reste sur le même slice, son player est transféré de A vers B sans redémarrer.
- **`isf`** : parseur de l'en-tête JSON, génération des uniforms standard (TIME, RENDERSIZE, coordonnées normalisées), traduction vers GLSL desktop et ES, exposition des INPUTS en `params`. Single-pass d'abord ; multi-pass/PERSISTENT ensuite. Critère : le DomePack de Pym se charge tel quel.
- **`compositor` multi-sorties** : chaque output a sa swapchain ; les slices sont rendus dans l'espace de leur output ; le blend A/B se fait par slice (pour la continuité et les overrides de transition). Preview = rendu offscreen → flux MJPEG/WebRTC léger vers l'UI.

## Threads & temps réel

- **Thread de rendu** : cadencé vsync (sortie principale), applique les changements de paramètres en début de frame (cohérence), exécute transitions et compositing.
- **Threads de décodage** : gérés par GStreamer par player, frames poussées dans des ring buffers.
- **Threads de contrôle** : sockets OSC/Art-Net, callbacks MIDI, WebSocket — tous traduisent en commandes vers le bus (file lock-free).
- **Horloges** : horloge moteur monotone ; horloge média par player ; les transitions et follows tickent sur l'horloge moteur.

## Choix par plateforme

| | Pi 4 | Pi 5 | Ubuntu | Windows | macOS |
|---|---|---|---|---|---|
| Décodage | v4l2 H.264 HW | v4l2 HEVC HW, H.264 soft | VA-API | D3D11 | VideoToolbox |
| HAP | CPU (snappy) + upload DXT | idem | idem | idem | idem |
| Sortie | KMS/DRM (sans bureau) | idem | GL fenêtre/KMS | GL fenêtre | GL fenêtre |
| Audio | ALSA | ALSA | Pulse/ALSA | WASAPI | CoreAudio |

**Risque identifié — macOS** : nouvelle cible par rapport à Lanterne (GStreamer + GL y sont fonctionnels mais moins éprouvés ; OpenGL est déprécié par Apple mais stable). À dérisquer en phase 0 ; sortie de secours : backend rendu via ANGLE/Metal, sans impact sur le reste (tout passe par le compositor).

## Format de show

- Dossier `MonSpectacle.vcl/` : `show.json` (versionné, migrable) + `media/` + `shaders/` + `thumbnails/` + `backups/`.
- Écritures atomiques (temp + rename + fsync), autosave, backups rotatifs.
- « Collecter le show » rend le dossier autonome (tout média copié en relatif) → clé USB, autre machine, archive zip.
