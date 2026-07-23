# Décisions actées

Source de vérité du projet. Format : date — décision — pourquoi.

## Produit & scope

- **2026-07-23 — VideoCuelist est un produit distinct de Lanterne.** Lanterne (repo `toolbox`) = node/player multimédia généraliste ; VideoCuelist = **régie de spectacle centrée cuelist** (scènes configurées → rappelées par cues, avec transitions). On réutilise les acquis, on ne fusionne pas les produits. *Pourquoi : demande explicite de Pym (« un outil spécifique spectacle cette fois ») ; les deux outils n'ont pas le même utilisateur au même moment (installation autonome vs régisseur en jeu).*
- **2026-07-23 — Cibles : Windows, macOS, Ubuntu, Raspberry Pi 4/5.** macOS est un ajout par rapport à Lanterne → à dérisquer en phase 0. Pi 3 exclu. *Pourquoi : parc de Pym + usage régie sur laptop Mac/Windows fréquent en spectacle.*
- **2026-07-23 — Modèle spatial : plusieurs slices par sortie ET plusieurs sorties si besoin.** Un show = N outputs, chaque output porte ses slices, chaque slice reçoit un contenu (vidéo ou matériau). *Pourquoi : demande explicite ; c'est le modèle MadMapper que Pym pratique déjà.*
- **2026-07-23 — « IFS » = ISF (Interactive Shader Format).** Vérifié dans le projet local « Materiaux IFS » : c'est du ISF (DomePack `.fs` + index.json, format MadMapper/Resolume). Support natif ISF + GLSL brut ; le DomePack de Pym doit se charger tel quel. *Pourquoi : réutilisation directe de sa bibliothèque existante + accès aux milliers de shaders d'editor.isf.video ; les inputs déclarés en JSON s'exposent automatiquement au MIDI/OSC/DMX.*
- **2026-07-23 — Licence : MIT.** *Pourquoi : cohérent avec Lanterne ; permet l'emprunt de code entre les deux repos (même auteur). Conséquence identique : aucune copie de code GPL (HPlayer2/3, VLink…), patterns seulement.*

## Stack technique

- **2026-07-23 — Rust + GStreamer + rendu OpenGL/GLES custom, comme Lanterne.** Réutilisation ciblée des crates/du code de `toolbox` : `artnet`, patterns `control-osc`/`control-midi`, trait `PlayerBackend` + `MemoryBackend`, shaders homographie/gains RGB/mires, bus de commandes, validation des chemins, écritures atomiques. *Pourquoi : décisions déjà instruites (état de l'art + audit dans Toolbox/docs/research), code testé, asymétrie décodage Pi 4/Pi 5 déjà traitée par GStreamer ; repartir de zéro en C++ jetterait tout ça.*
- **2026-07-23 — UI : web UI embarquée (axum + WebSocket), rendu vidéo natif plein écran.** L'éditeur ET la conduite tournent dans le navigateur (localhost ou distant) ; le moteur affiche les sorties en natif. *Pourquoi : (1) même UI moderne sur les 4 plateformes ; (2) contrôle à distance gratuit — éditer sur laptop un player Pi, GO depuis une tablette ; (3) « interface sexy + infobulles partout » bien plus atteignable en HTML/CSS qu'en egui/ImGui ; (4) c'est l'architecture Lanterne, déjà éprouvée. Fallback assumé : si la latence ou le canvas de mapping posent problème, panneau natif egui pour l'éditeur de mapping uniquement.*
- **2026-07-23 — Codecs : HAP recommandé pour le show, H.264/HEVC acceptés** (décodage matériel par plateforme, asymétrie Pi 4 = H.264 HW / Pi 5 = HEVC HW héritée des décisions Lanterne). Outil d'import avec conversion ffmpeg « Optimiser pour le spectacle ». *Pourquoi : HAP = décodage quasi gratuit GPU, scrub instantané, multi-couches — standard VJ ; mais gourmand en débit disque (SSD USB3 sur Pi).*

## Modèle de conduite

- **2026-07-23 — Une cue = snapshot complet de la scène** (contenus par slice, valeurs des paramètres scénarisés, réglages de lecture, transition d'entrée). Pas de tracking façon GrandMA en v1 ; cues partielles/overlays envisagées en v2. *Pourquoi : simple, prévisible, correspond au modèle mental « scène » de Pym.*
- **2026-07-23 — Moteur de transitions en double scène (decks A/B).** La cue active vit en A, la suivante se précharge en B ; une transition = blend A→B (cut, crossfade, fondu par le noir). *Pourquoi : modèle des mélangeurs vidéo — rend le préchargement et la preview naturels.*
- **2026-07-23 — Continuité de lecture : si le même média reste sur le même slice d'une cue à l'autre, la lecture ne redémarre pas** (seuls les paramètres fondent). *Pourquoi : essentiel pour des enchaînements propres en spectacle.*
- **2026-07-23 — Tout paramètre passe par un système unifié** : adresse OSC stable, MIDI learn avec soft-takeover, patch Art-Net 8/16 bits avec lissage (le DMX arrive à ~44 Hz), courbes de réponse, mode « live » (override non écrasé par les cues) vs « scénarisé ». *Pourquoi : une feature = un paramètre = automatiquement pilotable partout et enregistrable en cue — c'est la colonne vertébrale du produit.*

## Réponses de Pym du 2026-07-23 (soir)

- **2026-07-23 — Sorties : 4 simultanées maximum.** Résolution 1080p par défaut, 4K selon les vidéoprojecteurs — le moteur doit tenir 4×1080p confortablement, 4K à valider au bench par machine.
- **2026-07-23 — Audio : AUCUNE sortie audio.** À la place, un module de **modulation** de qualité : générateurs de signaux basse fréquence (LFO sinus/triangle/carré/dent de scie/random S&H — fréquence en Hz ou synchro BPM, phase, profondeur, lissage) + **analyse audio d'entrée** (FFT fenêtrée Hann, bandes log réglables, enveloppes attack/release) pour faire évoluer n'importe quel paramètre sur des fréquences définies proprement. *Interprétation de « module de synthétisation du son » = synthèse de signaux de contrôle + audio-réactif, puisque pas de sortie son — à reconfirmer avec Pym.*
- **2026-07-23 — Timecode : option utile confirmée** (chase MTC/LTC) — backlog v2, l'architecture des déclencheurs de cue doit le prévoir.
- **2026-07-23 — MIDI : mapping 100 % générique + learn.** Aucune personnalité de console spécifique : « les consoles vont changer ».
- **2026-07-23 — Partage de crates avec Lanterne : copie documentée** (en-tête de provenance dans chaque fichier repris). *Pourquoi : deux produits qui divergent, un seul mainteneur — une dépendance git créerait un enfer de versions croisées ; la copie est simple, fiable, maintenable. Décision demandée à Claude par Pym.*
- **2026-07-23 — Backend vidéo v1 : ffmpeg en sous-processus** (frames brutes par pipe, HAP/H.264/HEVC décodés par le ffmpeg full build), derrière le trait `PlayerBackend`. *Pourquoi : pas de GStreamer sur la machine de dev, ffmpeg déjà présent et embarquable dans le dossier portable, zéro problème de linkage. GStreamer reste le backend visé pour la phase Raspberry Pi.*
- **2026-07-23 — Rendu : OpenGL 3.3 (glow + winit/glutin).** *Pourquoi : ISF = GLSL compilé nativement par le driver (pas de traduction hasardeuse), shaders Lanterne repris tels quels, multi-fenêtres pour 4 sorties.*
- **2026-07-23 — Distribution : dossier portable d'abord.** Binaire + web UI embarquée + `config.toml` + dossiers `media/ shows/ shaders/ logs/` relatifs à l'exécutable. Aucune installation requise.

## Organisation

- **2026-07-23 — Convention de rangement héritée de Toolbox** : le cadrage vit dans `Programe Cue Video/` (dossier de travail), le code dans le repo `videocuelist/` (GitHub : pymenvert/videocuelist). Docs normatives : DECISIONS / SPEC / ARCHITECTURE / PLAN.
- **2026-07-23 — Chaque phase du PLAN livre un outil utilisable en l'état** avec un critère de sortie testable. *Pourquoi : de la valeur à chaque étape, correction de cap possible.*

- **2026-07-23 — Nom proposé : « Conduite »** (à valider par Pym). Recherche de collisions menée sur 7 candidats (Filage, Conduite, Servante, Poursuite, Girandole, Luciole, CueLight) : Conduite gagne — c'est littéralement le document de régie que le logiciel incarne, aucun logiciel du spectacle ne porte ce nom, handle GitHub libre, duo cohérent avec Lanterne. Réserves connues : SEO (permis de conduire) → communiquer « Conduite app », lecture anglophone « Conduit ». Dauphins : Filage (très bon mais voisin de « Filmage », apps vidéo Mac), Poursuite. À éviter : CueLight (produit commercial NuDelta du même secteur), Luciole (logiciel vidéo libre français existant).

## Assemblage `app` (binaire conduite)

- **2026-07-23 — Un seul contexte GL, une surface par fenêtre de sortie** (au lieu de contextes partagés via `with_sharing`). *Pourquoi : les VAO et FBO du compositor ne sont PAS partagés entre contextes GL (seuls textures/buffers le sont) — un contexte unique rendu tour à tour sur chaque surface (`make_current` par fenêtre) partage tout sans piège. Vsync posé sur la première surface uniquement, conforme à INTERFACES.*
- **2026-07-23 — Ajout à `core` des commandes `Undo`/`Redo` et `MidiLearnStart`/`MidiLearnCancel`** (JSON figé par test, retouche core explicitement autorisée par la mission). *Pourquoi : l'undo est une pile de snapshots côté app mais doit être pilotable par l'UI/raccourcis via le vocabulaire commun ; le learn MIDI est déclenché depuis la page Patch.*
- **2026-07-23 — DBO implémenté comme fondu maître dédié dans la session** (niveau 0..1 poussé dans `OutputView::dbo`), distinct de `CuePanic` (qui reste le noir de conduite du moteur de cues). *Pourquoi : `DboRelease` doit relever le voile sans toucher à la conduite, alors que le panic du moteur n'est relâché que par un GO.*
- **2026-07-23 — Entrée audio (cpal + rustfft) : stub propre en v1 de l'app** — `FftFrame::empty()` chaque tick, warn au démarrage si `audio_input` est configuré. *Pourquoi : les LFO fonctionnent sans ; l'intégration cpal/FFT (thread dédié, Hann 2048/hop 1024, triple buffer) reste à brancher sans changer l'architecture.*
- **2026-07-23 — Édition des cues en cours de conduite : `CueEngine::load` puis re-standby de l'ancienne cue active.** *Pourquoi : le moteur repart proprement de zéro au rechargement ; on repositionne la standby pour que le GO suivant reprenne où on en était (l'édition lourde se fait en mode Edit).*

## En attente / à trancher plus tard

- Validation du nom « Conduite » par Pym (et renommage éventuel du repo).
- Confirmer l'interprétation du « module de synthétisation du son » (LFO + audio-réactif, sans sortie son).
- Machine(s) de show réelles pour le bench 4×1080p / 4K.
