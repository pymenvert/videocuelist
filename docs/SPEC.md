# Spécification fonctionnelle

Ce document décrit **ce que fait** VideoCuelist. Le comment est dans [ARCHITECTURE.md](ARCHITECTURE.md), l'ordre de réalisation dans [PLAN.md](PLAN.md).

## 1. Vocabulaire (normatif)

| Terme | Définition |
|---|---|
| **Média** | Vidéo, image, ou **matériau** (shader ISF/GLSL). Tout média expose des paramètres. |
| **Slice** | Surface mappée dans l'espace d'une sortie : quad 4 coins (puis mesh warp), masque, opacité, colorimétrie, ordre z. **Plusieurs slices par sortie.** |
| **Layer** | Contenu posé sur un slice : média + chaîne d'effets + mode de fusion. V1 : un layer par slice ; multi-layers ensuite. |
| **Output** | Sortie physique (écran, projecteur). **Plusieurs sorties par show si besoin.** Chaque slice appartient à une sortie. |
| **Paramètre** | Toute valeur contrôlable (opacité, vitesse, uniform de shader, master…). Adresse OSC stable, mappable MIDI, patchable Art-Net, enregistrable en cue. |
| **Cue** | Snapshot complet d'une scène : contenus par slice, valeurs des paramètres scénarisés, réglages de lecture, transition d'entrée. |
| **Cuelist** | La conduite : cues numérotées (décimales), follow/wait, boucles de section, notes de régie. |
| **Patch** | Table MIDI/DMX/OSC ↔ paramètres, avec courbes, plages, lissage. |
| **Master / DBO** | Intensité générale et blackout d'urgence, accessibles en permanence (UI + protocoles). |

## 2. Médias & lecture

- **Codecs** : HAP / HAP-Q / HAP-Alpha recommandés (décodage quasi gratuit, scrub instantané, multi-couches) ; H.264 / HEVC acceptés (décodage matériel par plateforme). Images PNG/JPEG avec alpha.
- **Réglages par cue** : points IN/OUT, vitesse, volume, mode de fin — **boucle, ping-pong, gel sur dernière image, noir, ou follow** (fin de média → cue suivante).
- **Préchargement** : la cue en standby est totalement prête (fichiers ouverts, première frame décodée, shaders compilés). Le GO est instantané.
- **Pool de médias** : vignettes, tags, état OK/manquant, re-link, **« Collecter le show »** (copie de tous les médias dans un dossier autonome → clé USB, autre machine).
- **Transcodeur intégré** « Optimiser pour le spectacle » (ffmpeg → HAP, file d'attente, progression).
- **Audio** : son des vidéos lu, choix du périphérique de sortie, volume par cue + master audio.

## 3. Matériaux — ISF & GLSL

- **Import ISF** : parsing de l'en-tête JSON ; les inputs (`float`, `bool`, `long`, `color`, `point2D`, `image`) deviennent automatiquement des paramètres → pilotables MIDI/OSC/DMX immédiatement, enregistrables en cue. **Le DomePack existant de Pym doit se charger tel quel.**
- **GLSL brut** : wrapper minimal (TIME, RESOLUTION, texture d'entrée) pour adapter rapidement un shader type Shadertoy.
- Un matériau s'utilise **comme source** (à la place d'une vidéo sur un slice) **ou comme effet** dans la chaîne d'un layer (blur, strobe, colorize, kaléido… eux-mêmes des ISF). Un shader avec input `image` est de fait un effet/mixeur vidéo.
- **Édition** : hot-reload à la sauvegarde du fichier, erreurs de compilation affichées clairement. **En mode show : tout est précompilé, aucune compilation en live.**
- Plus tard : ISF multi-pass et buffers persistants (feedback).

## 4. Mapping

- V1 : **corner-pin 4 coins** par slice (homographie perspective-correcte — shaders déjà validés dans Lanterne). Ensuite : **mesh warp** (grille/bézier) pour surfaces courbes, **masques polygonaux** à bord doux.
- **Éditeur** : poignées à la souris, **nudge clavier** (flèches ±1 px, Maj ±10, Alt ±0,1), zoom/pan, sélection multiple, snap.
- Deux vues, comme MadMapper : **espace source** (quelle portion du média part) et **espace sortie** (où elle atterrit).
- Par slice : opacité, luminosité/contraste/gamma, gains RGB, flip/rotation, mode de fusion (normal, add, screen, multiply), ordre z.
- **Mires** : grille, damier, mire d'identification (nom + numéro du slice), par slice ou global — celles de Lanterne en base.
- **Snapshots de géométrie** : sauvegarder/rappeler des calages complets. En tournée, on re-cale la géométrie salle par salle **sans toucher aux cues**.

## 5. Sorties

- **Plusieurs slices par sortie, plusieurs sorties par show.** Sorties plein écran sans bordure, choix du display, bouton « identifier ».
- **Preview program** dans l'UI, indépendante des vraies sorties.
- Plus tard : edge blending multi-projecteurs, NDI in/out, Spout (Windows) / Syphon (macOS).

## 6. Contrôle — MIDI / OSC / Art-Net

- **Tout paramètre** a une adresse OSC lisible et stable : `/cue/go`, `/cue/goto 12`, `/slice/3/opacity`, `/slice/3/media/speed`, `/material/kaleido/sides`…
- **MIDI** : learn en un clic ; CC 7/14 bits, notes, pitch bend ; **soft-takeover** (pas de saut de valeur quand le fader physique n'est pas à la position mémorisée) ; **feedback** vers les surfaces (LED, faders motorisés) ; **MIDI Show Control** (GO/STOP/RESUME depuis console lumière ou QLab).
- **Art-Net** : nœud conforme (ArtPoll/ArtDMX) ; patch par paramètre (univers, canal, 8/16 bits) ; **lissage configurable** (le DMX arrive à ~44 Hz, on interpole pour des fondus propres) ; **personality DMX documentée** façon média-serveur, pilotable depuis GrandMA/Chamsys/Dot2. Esquisse par slice :

| Canal | Fonction |
|---|---|
| 1 | Intensité du slice |
| 2 | Sélection de dossier/banque |
| 3 | Sélection de média |
| 4 | Vitesse (128 = 1×, plage 0,25–4×) |
| 5–6 | Position de lecture (16 bits) |
| 7 | Volume |
| 8 | Contrôle (play / pause / restart / noir) |
| 9–10 | Effet 1 / Effet 2 (amount) |

- **OSC sortant** (feedback) : cue active, temps restant, états → TouchOSC, Open Stage Control, Bitfocus Companion (Stream Deck).
- Par mapping : courbe de réponse, min/max, inversion, mode **live** (override temps réel, non écrasé par les cues, avec indicateur visuel) vs **scénarisé** (enregistré dans les cues). Règle de priorité : dernière action gagne.
- **Déclencheurs par cue** : GO manuel, note MIDI dédiée, adresse OSC, MSC.

## 7. Cues & conduite

- **Contenu d'une cue** : assignations média/matériau → slice, valeurs des paramètres scénarisés, réglages de lecture (IN/OUT, vitesse, mode de fin), transition d'entrée.
- **Transition par cue** : durée, courbe (linéaire, ease in/out, S), type — **cut, crossfade, fondu par le noir** ; overrides par slice possibles.
- **Continuité** : même média sur même slice entre deux cues → la lecture continue, seuls les paramètres fondent.
- **Numérotation décimale** (1, 2, 2.5, 3…) pour insérer sans renuméroter ; labels, couleurs, notes de régie.
- **Follow** : GO manuel, auto-follow (fin de média), wait chronométré ; boucles de section (fin de cue 8 → retour cue 5).
- GO / BACK / GOTO ; **standby** toujours visible ; préchargement systématique de la cue suivante.
- **Master intensité** + **DBO** (fondu au noir d'urgence, temps réglable) toujours accessibles, y compris par protocoles.

## 8. Vue Live (en jeu)

- **Cuelist en grand** : cue active surlignée avec **barre de progression** (temps média restant / compte à rebours de follow / progression de transition), cue suivante en standby, notes de régie visibles.
- Moniteurs **Program** (ce qui sort réellement) et **Preview** (la cue suivante, rendue à blanc).
- Gros **GO** (Espace), BACK, GOTO ; master ; DBO ; horloge + chrono de spectacle.
- **Bandeau santé** : FPS par sortie, frames perdues, CPU/GPU/RAM, température (Pi), heartbeat MIDI/OSC/Art-Net.
- **Mode show verrouillé** : édition impossible, fermeture confirmée deux fois, sorties insensibles aux clics.

## 9. Fiabilité (doctrine, non négociable)

1. Le thread de rendu ne fait **ni I/O disque, ni compilation shader, ni allocation lourde** pendant le show.
2. Chargement **tolérant** : média manquant → placeholder visible + avertissement ; le show se charge toujours.
3. **Autosave** + backups rotatifs + écritures atomiques. Récupération après crash : « Reprendre à la cue 12 ? »
4. Fichier show en **JSON lisible** (diffable, réparable à la main un soir de première), versionné avec migrations.
5. Logs horodatés + console interne ; compteurs de drops par sortie.
6. **Mode player headless** (Pi) : boot direct sur le show, piloté OSC/MSC/Art-Net/web, watchdog systemd.

## 10. UX

- UI web moderne servie par le moteur : panneaux **Médias, Mapping, Cuelist, Paramètres, Patch, Sorties, Santé** ; layouts préréglés **Édition / Calage / Show**.
- **Infobulles partout** (description + raccourci clavier), plus un mode « ? » : on l'active puis on survole n'importe quel élément pour une explication détaillée.
- Thème sombre par défaut (régie dans le noir), accent coloré, iconographie cohérente ; **bilingue FR/EN**.
- Raccourcis remappables (Espace = GO, B = DBO, flèches = nudge…).
- **Undo/redo** complet en édition (jamais actif en mode show).
- Utilisable au clavier seul pour les opérations de conduite.
