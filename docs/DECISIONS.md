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

## Organisation

- **2026-07-23 — Convention de rangement héritée de Toolbox** : le cadrage vit dans `Programe Cue Video/` (dossier de travail), le code dans le repo `videocuelist/` (GitHub : pymenvert/videocuelist). Docs normatives : DECISIONS / SPEC / ARCHITECTURE / PLAN.
- **2026-07-23 — Chaque phase du PLAN livre un outil utilisable en l'état** avec un critère de sortie testable. *Pourquoi : de la valeur à chaque étape, correction de cap possible.*

## En attente / à trancher plus tard

- Nombre max de sorties simultanées et résolutions cibles (dimensionne le bench phase 0) — à préciser par Pym.
- Audio : lecture du son des vidéos + volume par cue suffit ? (routing multi-sorties audio = probablement jamais, QLab fait ça très bien).
- Timecode (chase LTC/MTC) : besoin réel ou confort lointain — phase tardive de toute façon.
- MIDI Show Control (MSC) : prévu, à confirmer quelle console en face.
- Partage de crates avec Lanterne : dépendance git vs copie — trancher en phase 0 crate par crate (voir ARCHITECTURE).
- Nom définitif du produit (VideoCuelist est le nom de travail).
