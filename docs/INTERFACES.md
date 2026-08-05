# Contrat d'interfaces (v1)

Document de coordination pour l'implémentation. **Toute divergence par rapport à ce contrat doit être justifiée dans DECISIONS.md.** Identifiants en anglais, UI/docs en français. Nom produit (travail) : **Conduite** ; binaire `conduite`.

## Graphe de dépendances des crates

```
core          (modèle de show, commandes, persistance, chemins)
params        (dépend de core::ParamValue uniquement — machinerie des paramètres)
cue           (dépend de core + params — moteur de conduite, PUR, sans GL ni IO)
modulation    (autonome — LFO, BPM, bandes audio ; reçoit les FFT, ne capture pas)
engine        (autonome — trait Player + backend ffmpeg subprocess)
isf           (autonome — parseur ISF + génération GLSL 330)
compositor    (dépend de core + engine::FrameRgba + isf::CompiledIsf — tout le GL)
control-osc   (dépend de core — traduit OSC ↔ Command/feedback)
control-midi  (dépend de core — MIDI learn, pickup, MSC)
control-artnet(dépend de core — nœud Art-Net, patch DMX)
control-http  (dépend de core — axum, WS, assets webui embarqués)
media-library (dépend de core — scan, ffprobe, vignettes)
system        (autonome — santé machine)
app           (binaire : assemble tout, winit + GL + boucle de session)
```

## Règles de code (non négociables)

1. `unwrap()`/`expect()` interdits hors `#[cfg(test)]` et init de démarrage ; les erreurs runtime → `tracing::error!` + dégradation propre (jamais de panic pendant un show).
2. **Zéro allocation et zéro IO dans le chemin de rendu** (tout préalloué ; les uploads GPU réutilisent les textures).
3. Chaque crate logique livre des **tests unitaires** (`cargo test -p <crate>` vert avant de conclure).
4. Code copié/adapté de Lanterne (`C:\Users\pymenvert\Claude\Projects\Toolbox\toolbox`) : en-tête `// Adapté de Lanterne (pymenvert/toolbox), MIT.`
5. Logs : `tracing` partout, target = nom du module. Pas de `println!`.
6. Chemins de médias validés (relatifs au dossier portable, pas de `..`) ; écritures disque atomiques (tmp + rename).

## core

```rust
pub type OutputId = u32; pub type SliceId = u32; pub type MediaId = u32;
pub type MaterialId = u32; pub type ModId = u32;

/// Numéro de cue en millièmes : 1000 = "1", 1500 = "1.5", 12340 = "12.34".
/// Ordre total, insertion sans renumérotation, pas de float.
pub struct CueNumber(pub u32);      // Display + FromStr ("1.5" ↔ 1500)

pub enum ParamValue { F(f32), I(i64), B(bool), Color([f32; 4]), P2([f32; 2]), S(String) }

pub enum Content {
    None,
    Media(MediaId),
    Material(MaterialId),
    Pattern(PatternKind),           // Grid, Checker, Ident (nom+numéro du slice), Bars
    Color([f32; 4]),
}

pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }   // normalisé 0..1

pub struct OutputCfg { pub id: OutputId, pub name: String, pub monitor_index: Option<usize>,
    pub width: u32, pub height: u32, pub fullscreen: bool, pub enabled: bool }

pub struct Slice { pub id: SliceId, pub name: String, pub output: OutputId,
    pub corners: [[f32; 2]; 4],     // espace sortie normalisé 0..1, ordre TL,TR,BR,BL
    pub src: Rect, pub z: i32, pub enabled: bool }
// opacité/gains/blend/etc. = paramètres (adresses stables), pas des champs

pub struct MediaRef { pub id: MediaId, pub path: String /* relatif à media/ */, pub name: String,
    pub duration_s: Option<f64>, pub fps: Option<f64>, pub width: u32, pub height: u32, pub missing: bool }

pub struct MaterialRef { pub id: MaterialId, pub path: String /* relatif à shaders/ */, pub name: String }

pub struct Playback { pub in_s: f64, pub out_s: Option<f64>, pub speed: f32, pub end: EndMode }
pub enum EndMode { Loop, PingPong, Hold, Black, FollowNext }

pub struct SliceState { pub slice: SliceId, pub content: Content,
    pub playback: Option<Playback>, pub params: BTreeMap<String, ParamValue> }

pub enum TransitionKind { Cut, Crossfade, ThroughBlack }
pub struct Transition { pub kind: TransitionKind, pub dur_s: f32, pub curve: Curve }
pub enum Curve { Linear, EaseIn, EaseOut, EaseInOut, SCurve }

pub enum FollowMode { Manual, AfterMedia, Wait(f32) }
pub struct Cue { pub number: CueNumber, pub name: String, pub color: Option<String>,
    pub notes: String, pub transition: Transition, pub follow: FollowMode,
    pub goto_after: Option<CueNumber>,               // boucles de section
    pub states: Vec<SliceState>,
    pub mod_routes: Vec<ModRouteState>,              // profondeurs de modulation par cue
    pub triggers: CueTriggers }                      // note MIDI / adresse OSC dédiées
pub struct Show { pub format_version: u32, pub name: String, pub outputs: Vec<OutputCfg>,
    pub slices: Vec<Slice>, pub media: Vec<MediaRef>, pub materials: Vec<MaterialRef>,
    pub cues: Vec<Cue>, pub patch: PatchTable, pub modulators: Vec<ModulatorCfg>,
    pub settings: ShowSettings }
```

```rust
pub enum Source { Ui, Osc, Midi, ArtNet, Cue, Modulation, Internal }

pub enum Command {
    ParamSet { addr: String, value: ParamValue, source: Source },
    ParamNudge { addr: String, delta: f32, source: Source },
    CueGo, CueBack, CueGoto(CueNumber), CueStandby(CueNumber), CuePanic { fade_s: f32 },
    Dbo { fade_s: f32 }, DboRelease,
    TapTempo, BpmSet(f32),
    Edit(EditOp),                    // toute mutation du modèle (undo-able)
    ShowSave, ShowSaveAs(String), ShowLoad(String), ShowNew,
    MediaRescan, ShowCollect,
    ModeSet(AppMode),                // Edit | Show (verrouillé)
}
// EditOp couvre : add/remove/update slice, output, cue, media, material,
// corner move, patch add/remove, modulator add/remove/update, cue reorder…
```

**Règle de propriété des types** : tout type sérialisé dans `Show` vit dans `core`
(`ModulatorCfg`, `ModRoute`, `Wave`, `Freq`, `ModKind`, `PatchTable`, `PatchEntry`,
`MidiBinding`, `CueTriggers`, `ShowSettings`, `AppMode`, `PatternKind`…). Les crates
`modulation`/`control-*` n'exportent que de la machinerie, jamais de types de modèle.

Persistance : `save_show_atomic(dir, &Show)`, backups rotatifs (`backups/show-YYYYMMDD-HHMMSS.json`, garder 20), `load_show(dir) -> (Show, Vec<LoadWarning>)` — un média manquant ⇒ `missing: true` + warning, **jamais d'échec de chargement**.

## params

```rust
pub enum ParamKind { Float { min: f32, max: f32 }, Int { min: i64, max: i64 },
    Bool, Color, Point2, Enum(Vec<String>) }
pub struct ParamSpec { pub addr: String, pub label: String, pub kind: ParamKind,
    pub default: ParamValue, pub smoothing_ms: f32, pub scriptable: bool }

pub struct Registry;                 // toute l'API prend &str pour addr
impl Registry {
    pub fn register(&mut self, spec: ParamSpec);
    pub fn unregister_prefix(&mut self, prefix: &str);          // ex. au retrait d'un slice
    pub fn set(&mut self, addr: &str, v: ParamValue, source: Source);
    pub fn set_live_override(&mut self, addr: &str, on: bool);  // fader "live" non écrasé par les cues
    pub fn value(&self, addr: &str) -> Option<ParamValue>;      // valeur lissée courante
    pub fn value_f32(&self, addr: &str) -> f32;                 // 0.0 si absent (log warn)
    pub fn snapshot_scripted(&self) -> BTreeMap<String, ParamValue>;
    /// Fondu vers un snapshot cible : alpha 0..1 (appelé chaque frame par le moteur de cues).
    pub fn blend_toward(&mut self, target: &BTreeMap<String, ParamValue>, alpha: f32);
    pub fn apply_modulation(&mut self, offsets: &[(String, f32)]);  // post-blend, non persistant
    pub fn tick(&mut self, dt_s: f32);                          // lissage
    pub fn set_smoothing_override(&mut self, addr: &str, ms: Option<f32>);  // ex. patch DMX
    pub fn specs(&self) -> impl Iterator<Item = &ParamSpec>;
}
```

Adresses stables (schéma normatif) :
```
master/intensity            master/dbo
slice/{id}/opacity          slice/{id}/gain/r|g|b       slice/{id}/gamma
slice/{id}/blendmode (enum) slice/{id}/media/speed      slice/{id}/media/position
material/{id}/{isf_input}   mod/{id}/freq|depth|offset  bpm
```

## cue (pur, testé à fond — c'est le cœur du produit)

```rust
pub struct CueEngine;
pub enum DeckSlot { A, B }
pub struct EngineTick<'a> { pub now_s: f64, pub media_eof: &'a dyn Fn(SliceId) -> bool }
pub struct SceneTarget { pub per_slice: Vec<SliceTarget>, pub params: BTreeMap<String, ParamValue> }
pub struct SliceTarget { pub slice: SliceId, pub content: Content, pub playback: Option<Playback> }

impl CueEngine {
    pub fn load(&mut self, cues: &[Cue]);
    pub fn go(&mut self) / back / goto(n: CueNumber) / standby(n) / panic(fade_s);
    /// Appelé chaque frame. Retourne l'état désiré des decks + l'alpha de blend A→B
    /// + le snapshot de paramètres interpolé + les événements (cue démarrée/finie, follow armé…).
    pub fn tick(&mut self, t: EngineTick) -> CueFrame;
    pub fn status(&self) -> CueStatus;   // active, standby, progress 0..1, temps restants
}
```
Règles : préchargement = la cue standby est résolue en `SceneTarget` immédiatement ;
**continuité** = si `(slice, content)` identiques entre A et B, le player n'est pas recréé
(c'est `app` qui diffe, mais `cue` doit exposer les deux targets pendant la transition) ;
`ThroughBlack` = descente à noir (dur/2) puis montée (dur/2) avec bascule au milieu ;
follow `AfterMedia` s'appuie sur `media_eof` ; `Wait(s)` sur l'horloge moteur.

## modulation

```rust
pub enum Wave { Sine, Tri, Square { pw: f32 }, Saw, RandomSh, Drift }
// mult en cycles par temps : 1 = 1 temps, 0.25 = 1 mesure (4/4), 0.0625 = 4 mesures.
pub enum Freq { Hz(f32), BpmSync { mult: f32 } }
pub struct ModulatorCfg { pub id: ModId, pub name: String, pub kind: ModKind }
pub enum ModKind { Lfo { wave: Wave, freq: Freq, phase: f32 },
    AudioBand { low_hz: f32, high_hz: f32, gain: f32, floor: f32, attack_ms: f32,
        release_ms: f32, normalize: bool /* serde default true : AGC max glissant ~3 s */ },
    TimecodeChase {} }                                  // réservé v2 : moteur → 0.0, UI grisée
pub struct ModRoute { pub id: u32, pub source: ModId, pub target_addr: String,
    pub depth: f32, pub mode: RouteMode }               // Add | Mul | Replace
pub struct ModEngine;
impl ModEngine {
    pub fn tick(&mut self, now_s: f64, bpm: f32, fft_bands: &FftFrame) -> Vec<(String, f32)>;
    pub fn retrigger(&mut self);                        // sur GO (phase reset si configuré)
    pub fn tap(&mut self, now_s: f64) -> Option<f32>;   // tap tempo → BPM
}
pub struct FftFrame { pub bins_hz: f32, pub magnitudes: Vec<f32> }  // fourni par app (cpal+rustfft)
/// Analyseur UI (trame WS `dyn.fft.bins`) : n bins log-échelonnés low→high,
/// max des magnitudes par intervalle, compression sqrt, sortie 0..1. Pure.
pub fn spectrum_bins(fft: &FftFrame, n: usize, low_hz: f32, high_hz: f32) -> Vec<f32>;
pub const SPECTRUM_BINS_DEFAULT: usize = 64;            // 20 Hz → 16 kHz par défaut
```
Qualité : horloge monotone passée par l'appelant, phase continue lors des changements de
fréquence (pas de saut), enveloppes attack/release exponentielles, `RandomSh`/`Drift` seedés,
AGC des bandes (division par le max glissant ~3 s, silence jamais amplifié).

## engine

```rust
pub struct MediaInfo { pub duration_s: f64, pub fps: f64, pub width: u32, pub height: u32 }
pub struct FrameRgba { pub width: u32, pub height: u32, pub data: Vec<u8>, pub pts_s: f64 }
pub trait Player: Send {
    fn info(&self) -> &MediaInfo;
    fn set_playback(&mut self, pb: &Playback);
    fn play(&mut self); fn pause(&mut self); fn seek(&mut self, s: f64);
    /// Frame à afficher pour l'horloge média donnée (None = garder la précédente).
    fn poll_frame(&mut self, media_time_s: f64) -> Option<FrameRgba>;
    fn eof(&self) -> bool; fn healthy(&self) -> bool;
}
pub fn probe(path: &Path) -> anyhow::Result<MediaInfo>;               // ffprobe -of json
pub fn open_ffmpeg(path: &Path, pb: &Playback) -> anyhow::Result<Box<dyn Player>>;
pub fn resolve_ffmpeg() -> PathBuf;   // ordre : ./bin/ffmpeg(.exe) portable, puis PATH
```
Backend ffmpeg : process `ffmpeg -ss <in> -i <file> [-to <out>] [-stream_loop -1 pour Loop]
-f rawvideo -pix_fmt rgba|bgra pipe:1`, thread lecteur → ring buffer borné (4 frames, SPSC),
vitesse = cadence de consommation (dup/skip), pause = on arrête de consommer (backpressure),
seek = relance du process. `preload` = process lancé + première frame en buffer. EOF = pipe fermé.
Redémarrage automatique si le process meurt (log + compteur santé). Zéro zombie (kill on drop).
**Ordre des canaux** : `set_decode_bgra(bool)` (activé par le compositor si `GL_BGRA` est supporté)
fait sortir ffmpeg en BGRA ; chaque frame porte son ordre (`FrameRgba::pixel_order() -> PixelOrder`).
**D3D11VA (Windows)** : `-hwaccel d3d11va` tenté pour H.264/HEVC ≥ ~720p ; échec du process ⇒
repli logiciel immédiat + mémorisation pour la session (log clair, une fois).

## isf

```rust
pub struct IsfInput { pub name: String, pub label: String, pub kind: IsfInputKind, /* min/max/default */ }
pub enum IsfInputKind { Float, Bool, Long { values: Vec<i64>, labels: Vec<String> },
    Color, Point2D, Image, Event, Audio, AudioFft }     // Audio* : accepté mais nourri à zéro en v1
pub struct IsfDoc { pub meta: serde_json::Value, pub inputs: Vec<IsfInput>, pub body: String }
pub fn parse(src: &str) -> Result<IsfDoc, IsfError>;    // extrait le JSON /*{ }*/ + le GLSL
pub struct IsfSources { pub vertex: String, pub fragment: String }  // GLSL 330 core prêt à compiler
pub fn generate_glsl(doc: &IsfDoc) -> Result<IsfSources, IsfError>;
```
Préambule à générer : uniforms standard `TIME`, `TIMEDELTA`, `RENDERSIZE`, `FRAMEINDEX`, `DATE`,
varying `isf_FragNormCoord`, macros `IMG_PIXEL`, `IMG_NORM_PIXEL`, `IMG_THIS_PIXEL`,
`IMG_THIS_NORM_PIXEL`, `IMG_SIZE`, compat `texture2D`→`texture`. Multi-pass (`PASSES`) ⇒
`Err(Unsupported)` avec message clair. **Critère : les .fs du DomePack
(`C:\Users\pymenvert\Claude\Projects\Materiaux IFS\dist\ISF\`) parsent et génèrent du GLSL valide.**

## compositor (tout le GL, via glow)

```rust
pub struct Compositor;   // créé avec Arc<glow::Context> partagé entre fenêtres
impl Compositor {
    pub fn new(gl: Arc<glow::Context>) -> Result<Self>;
    pub fn ensure_slice_textures(&mut self, slice: SliceId);     // double texture A/B
    pub fn upload_frame(&mut self, slice: SliceId, deck: DeckSlot, f: &FrameRgba);
    pub fn set_material(&mut self, slice: SliceId, deck: DeckSlot, isf: Option<&IsfSources>,
                        material: MaterialId) -> Result<()>;      // compile+cache par (material)
    pub fn set_material_uniforms(&mut self, slice: SliceId, deck: DeckSlot, values: &[(String, ParamValue)]);
    /// Rend un output complet : slices triés par z, homographie, opacité/gains,
    /// blend A/B par slice (alpha transition), master intensity, DBO.
    pub fn render_output(&mut self, out: &OutputView) -> Result<()>;
    pub fn render_pattern(&mut self, ...);                        // mires
    pub fn read_preview_rgba(&mut self, w: u32, h: u32) -> Vec<u8>; // pour MJPEG (FBO dédié)
    /// Rendus de sortie soumis au GPU non terminés (0..=2) — HUD santé.
    /// La latence de présentation est bornée à 2 frames par fences (pattern mpv).
    pub fn frames_in_flight(&self) -> usize;
}
```
Homographie : reprendre le calcul 3×3 de Lanterne (`crates/render`) avec provenance.
Blend modes : Normal, Add, Screen, Multiply (dans le shader de composition).
Capacités détectées à l'init (chaque chemin garde son repli, GL 3.3/GLES 3.0 restent servis) :
upload `GL_BGRA` (desktop seulement — active `engine::set_decode_bgra`), `glTexStorage2D`
(textures persistantes immuables), PBO persistant mappé 3 tranches + fences (sinon orphaning,
sinon copie synchrone), fences de latence de présentation.

## control-osc / control-midi / control-artnet

Tous reçoivent un `crossbeam_channel::Sender<Command>` et un abonnement aux événements d'état pour le feedback.

Schéma OSC (in, port 9000 par défaut ; out configurable) :
```
/conduite/cue/go          /conduite/cue/back        /conduite/cue/goto  (float ou string "12.5")
/conduite/param/<addr>  f /conduite/master  f       /conduite/dbo  f(fade_s)
/conduite/bpm/tap         /conduite/bpm  f
Feedback out : /conduite/status/active "12" ; /conduite/status/progress f ; /conduite/status/remaining f
```
MIDI : `MidiHub` (midir) — learn (capture le prochain message), bindings sérialisables
(Note→Command, CC 7/14 bits→addr avec plage/courbe + **soft-takeover**), MSC (SysEx `F0 7F <dev> 02 <fmt> <cmd> …` : GO/STOP/RESUME/LOAD avec numéro de cue) → Commands. Feedback notes/CC out.
Art-Net : socket UDP 6454, réponse ArtPoll, réception ArtDMX multi-univers ; `PatchEntry
{ universe, channel, bits: Eight|Sixteen, addr, min, max, smoothing_ms }` ; lissage côté réception.

## control-http + webui

- `GET /` : SPA embarquée (rust-embed ou include_bytes!). **Vanilla HTML/CSS/JS, aucun toolchain npm.**
- `WS /ws` : à la connexion → `{"type":"hello", state: <état complet sérialisé>}` ; ensuite
  événements (`cue`, `edit`, `health`, `log`) + trames dynamiques 10 Hz (`dyn` : progress,
  niveaux modulateurs, valeurs params changées). Client → `{"type":"cmd", ...Command JSON}`.
- `GET /preview.mjpeg` : multipart/x-mixed-replace, ~8 fps, 640×360, program (+ `?deck=preview` pour la cue standby).
- `GET /thumb/{media_id}.jpg`.
- `GET /about` : JSON statique « À propos » construit par `app` — `{ name, description,
  version, git, license, copyright, website, credits: [{ name, role, license, url?, notice }] }`.
  L'affichage (Réglages) est à la charge de la webui.
- `runtime.warnings` : `[{level, msg, key, args, action?}]`. `msg` = la phrase
  française déjà composée (journal, rapport de diagnostic, clients anciens) ;
  `key` = le gabarit `{0}`/`{1}` — une constante de `core::warnings` — et
  `args` ses valeurs brutes (chemins, noms, messages système : jamais
  traduits). La webui recompose `trf(key, …args)` pour l'afficher dans la
  langue de l'opérateur ; à défaut de `key`, elle retombe sur `msg`.
- **Bilingue FR/EN** : le français est la langue SOURCE (chaînes en clair dans
  `app.js`), l'anglais vit dans `webui/i18n.js` — catalogue JSON strict indexé
  par la chaîne française, plus `tr(s)` (chaîne complète) et `trf(tpl, …)`
  (gabarit). La traduction se fait à UN point de passage (`appendChild` +
  attributs `title`/`placeholder`/`aria-label`, et `data-tip` à l'affichage de
  l'infobulle) : aucun site d'appel n'a à s'en soucier. Une chaîne absente du
  catalogue s'affiche en français — jamais de trou. La langue vient de
  `settings.language` et s'applique sans rechargement. Garde-fou :
  `crates/control-http/tests/webui_i18n.rs` échoue si une chaîne française de
  la web UI (ou un gabarit de `core::warnings`) n'a pas de traduction.
- UI (français + anglais, sombre, tooltips partout via un système central `data-tip`) : onglets
  **Live** (cuelist + progress + program/preview + GO/BACK/GOTO + master + DBO + santé),
  **Cues** (édition), **Mapping** (canvas coins/nudge/mires), **Médias**, **Matériaux** (params ISF),
  **Modulation**, **Patch** (OSC/MIDI learn/Art-Net), **Sorties**, **Journal** (logs), **Réglages**.
  Raccourcis : Espace=GO, B=DBO, flèches=nudge (Maj ×10, Alt ×0,1). Mode Show = verrouillage édition.

## app (binaire `conduite`)

- Dossier portable : tout relatif à l'exe — `config.toml`, `media/`, `shows/`, `shaders/`,
  `logs/`, `bin/ffmpeg.exe` (optionnel, sinon PATH).
- Boucle : thread principal winit (fenêtres de sortie GL + tick 60 Hz) ;
  runtime tokio (http/osc/artnet) ; threads midir/cpal/ffmpeg.
- Ordre du tick : drain commands → cue.tick → modulation.tick → params.tick →
  poll players → uploads → render outputs → préview → santé.
- vsync sur la sortie 1 uniquement (les autres swap sans vsync) pour éviter la sérialisation.
- Logs : `tracing-subscriber` console + `tracing-appender` fichier journalier `logs/`,
  + ring buffer 500 lignes publié à l'UI. Panic hook → log + tentative de sauvegarde du show.
- Autosave : à chaque édition (débouncé 2 s) + toutes les 60 s si dirty.
```
