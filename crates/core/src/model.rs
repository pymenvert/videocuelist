//! Types du modèle de show — voir docs/INTERFACES.md (§ core, normatif).
//!
//! Tout type sérialisé dans [`Show`] vit ici (ou dans `modulation.rs` /
//! `patch.rs` de cette crate) : les crates `modulation`/`control-*`
//! n'exportent que de la machinerie, jamais de types de modèle.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::modulation::{ModRoute, ModRouteState, ModulatorCfg};
use crate::patch::PatchTable;

pub type OutputId = u32;
pub type SliceId = u32;
pub type MediaId = u32;
pub type MaterialId = u32;
pub type ModId = u32;

/// Version courante du format de fichier show (hook de migration dans
/// `persist::load_show`).
pub const FORMAT_VERSION: u32 = 1;

/// Numéro de cue en millièmes : 1000 = "1", 1500 = "1.5", 12340 = "12.34".
/// Ordre total, insertion sans renumérotation, pas de float.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct CueNumber(pub u32);

impl CueNumber {
    /// Construit depuis partie entière + millièmes (0..=999).
    pub fn new(int: u32, thousandths: u32) -> Self {
        CueNumber(int.saturating_mul(1000).saturating_add(thousandths.min(999)))
    }
}

impl fmt::Display for CueNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let int = self.0 / 1000;
        let frac = self.0 % 1000;
        if frac == 0 {
            write!(f, "{int}")
        } else {
            let mut digits = [b'0'; 3];
            digits[0] += (frac / 100) as u8;
            digits[1] += (frac / 10 % 10) as u8;
            digits[2] += (frac % 10) as u8;
            let mut len = 3;
            while len > 1 && digits[len - 1] == b'0' {
                len -= 1;
            }
            // Les octets sont des chiffres ASCII : toujours de l'UTF-8 valide.
            let s = std::str::from_utf8(&digits[..len]).map_err(|_| fmt::Error)?;
            write!(f, "{int}.{s}")
        }
    }
}

impl FromStr for CueNumber {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let raw = s.trim();
        let err = || CoreError::InvalidCueNumber(s.to_string());
        if raw.is_empty() {
            return Err(err());
        }
        let (int_part, frac_part) = match raw.split_once('.') {
            // "1." (point sans décimales) est refusé.
            Some((_, "")) => return Err(err()),
            Some((i, f)) => (i, f),
            None => (raw, ""),
        };
        // Chiffres ASCII uniquement (parse::<u32> accepterait un '+' de tête).
        if int_part.is_empty()
            || !int_part.bytes().all(|b| b.is_ascii_digit())
            || !frac_part.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(err());
        }
        if frac_part.len() > 3 {
            return Err(err());
        }
        let int: u32 = int_part.parse().map_err(|_| err())?;
        let mut frac: u32 = 0;
        for (i, b) in frac_part.bytes().enumerate() {
            frac += u32::from(b - b'0') * 10u32.pow(2 - i as u32);
        }
        int.checked_mul(1000)
            .and_then(|v| v.checked_add(frac))
            .map(CueNumber)
            .ok_or_else(err)
    }
}

/// Valeur d'un paramètre (voir crate `params` pour la machinerie).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamValue {
    F(f32),
    I(i64),
    B(bool),
    Color([f32; 4]),
    P2([f32; 2]),
    S(String),
}

/// Mire de test intégrée (calage projecteur sans média).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    /// Grille de convergence.
    Grid,
    /// Damier.
    Checker,
    /// Mire d'identification : nom + numéro du slice.
    Ident,
    /// Barres de couleurs.
    Bars,
}

/// Contenu posé sur un slice pour une cue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    None,
    Media(MediaId),
    Material(MaterialId),
    Pattern(PatternKind),
    Color([f32; 4]),
}

/// Rectangle normalisé 0..1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// Le rectangle plein cadre (0,0,1,1).
    pub fn full() -> Self {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }
}

/// Configuration d'une sortie physique (écran, projecteur).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputCfg {
    pub id: OutputId,
    pub name: String,
    pub monitor_index: Option<usize>,
    pub width: u32,
    pub height: u32,
    pub fullscreen: bool,
    pub enabled: bool,
}

/// Surface mappée dans l'espace d'une sortie (quad 4 coins).
/// Opacité/gains/blend/etc. = paramètres (adresses stables), pas des champs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slice {
    pub id: SliceId,
    pub name: String,
    pub output: OutputId,
    /// Espace sortie normalisé 0..1, ordre TL, TR, BR, BL.
    pub corners: [[f32; 2]; 4],
    pub src: Rect,
    pub z: i32,
    pub enabled: bool,
}

impl Slice {
    /// Coins par défaut : quad plein cadre (TL, TR, BR, BL).
    pub fn default_corners() -> [[f32; 2]; 4] {
        [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
    }
}

/// Référence à un média du pool (chemin relatif à `media/`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaRef {
    pub id: MediaId,
    /// Relatif au dossier `media/` du dossier portable.
    pub path: String,
    pub name: String,
    pub duration_s: Option<f64>,
    pub fps: Option<f64>,
    pub width: u32,
    pub height: u32,
    /// Fichier absent au chargement — placeholder visible, jamais un refus.
    #[serde(default)]
    pub missing: bool,
}

/// Référence à un matériau ISF/GLSL (chemin relatif à `shaders/`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialRef {
    pub id: MaterialId,
    /// Relatif au dossier `shaders/` du dossier portable.
    pub path: String,
    pub name: String,
}

/// Réglages de lecture d'un média pour une cue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playback {
    pub in_s: f64,
    pub out_s: Option<f64>,
    pub speed: f32,
    pub end: EndMode,
}

impl Default for Playback {
    fn default() -> Self {
        Playback {
            in_s: 0.0,
            out_s: None,
            speed: 1.0,
            end: EndMode::Loop,
        }
    }
}

/// Comportement en fin de média.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndMode {
    Loop,
    PingPong,
    Hold,
    Black,
    FollowNext,
}

/// État d'un slice dans une cue : contenu + lecture + paramètres scénarisés.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SliceState {
    pub slice: SliceId,
    pub content: Content,
    pub playback: Option<Playback>,
    /// Adresses stables (ex. `slice/1/opacity`) → valeurs scénarisées.
    #[serde(default)]
    pub params: BTreeMap<String, ParamValue>,
}

/// Type de transition d'entrée d'une cue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    Cut,
    Crossfade,
    ThroughBlack,
}

/// Transition d'entrée d'une cue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: TransitionKind,
    pub dur_s: f32,
    pub curve: Curve,
}

impl Default for Transition {
    fn default() -> Self {
        Transition {
            kind: TransitionKind::Cut,
            dur_s: 0.0,
            curve: Curve::Linear,
        }
    }
}

/// Courbe d'interpolation d'une transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    SCurve,
}

impl Curve {
    /// Applique la courbe à un temps normalisé `t` (clampé 0..1).
    /// Garanties : `apply(0) == 0`, `apply(1) == 1`, monotone croissante.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Curve::Linear => t,
            Curve::EaseIn => t * t,
            Curve::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Curve::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    let u = -2.0 * t + 2.0;
                    1.0 - u * u / 2.0
                }
            }
            // Smoothstep : dérivée nulle aux deux bouts.
            Curve::SCurve => t * t * (3.0 - 2.0 * t),
        }
    }
}

/// Enchaînement après une cue.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowMode {
    /// GO manuel.
    Manual,
    /// Auto-follow en fin de média.
    AfterMedia,
    /// Wait chronométré (secondes après le début de la cue).
    Wait(f32),
}

/// Déclencheurs dédiés d'une cue (en plus du GO manuel).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CueTriggers {
    /// (canal 0..15, note 0..127).
    #[serde(default)]
    pub midi_note: Option<(u8, u8)>,
    /// Adresse OSC dédiée (ex. `/conduite/cue/ouverture`).
    #[serde(default)]
    pub osc: Option<String>,
}

/// Valeur par défaut du champ `armed` (cue armée) — les shows existants
/// sans le champ restent tous armés.
fn default_true() -> bool {
    true
}

/// Une cue : snapshot complet d'une scène.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub number: CueNumber,
    pub name: String,
    /// Couleur d'étiquette UI (ex. `"#3fa9f5"`).
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub notes: String,
    /// Cue armée : désarmée = grisée dans l'UI et SAUTÉE par GO/BACK/follow
    /// (retirer un tableau en répétition sans détruire la conduite).
    /// Un GOTO explicite la joue quand même. Défaut `true` (contrat serde :
    /// les shows antérieurs restent tous armés).
    #[serde(default = "default_true")]
    pub armed: bool,
    pub transition: Transition,
    pub follow: FollowMode,
    /// Boucles de section : fin de cue → retour à ce numéro.
    #[serde(default)]
    pub goto_after: Option<CueNumber>,
    pub states: Vec<SliceState>,
    /// Profondeurs de modulation par cue.
    #[serde(default)]
    pub mod_routes: Vec<ModRouteState>,
    #[serde(default)]
    pub triggers: CueTriggers,
}

/// Mode de l'application : édition libre ou show verrouillé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppMode {
    #[default]
    Edit,
    Show,
}

/// Gabarits de cue : défauts appliqués à toute nouvelle cue (`CueAdd`)
/// quand le champ correspondant est resté à sa valeur de type par défaut.
/// `None` = pas de gabarit pour ce champ. Référence : QLab Cue Templates.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CueDefaults {
    /// Transition d'entrée par défaut des nouvelles cues.
    pub transition: Option<Transition>,
    /// Mode d'enchaînement par défaut.
    pub follow: Option<FollowMode>,
    /// Couleur d'étiquette par défaut.
    pub color: Option<String>,
}

/// Réglages persistés du show (ports, Art-Net, langue, préview, autosave).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShowSettings {
    /// Port OSC entrant (défaut 9000).
    pub osc_in_port: u16,
    /// Port OSC sortant par défaut pour le feedback.
    pub osc_out_port: u16,
    /// Nœud Art-Net actif ?
    pub artnet_enabled: bool,
    /// Univers Art-Net écoutés.
    pub artnet_universes: Vec<u16>,
    /// Langue de l'UI ("fr" / "en").
    pub language: String,
    /// Cadence de la préview MJPEG (images/s).
    pub mjpeg_fps: u8,
    pub mjpeg_width: u32,
    pub mjpeg_height: u32,
    /// Débounce d'autosave après édition (secondes).
    pub autosave_debounce_s: f32,
    /// Autosave périodique si dirty (secondes).
    pub autosave_interval_s: f32,
    /// Périphérique d'entrée audio pour l'analyse FFT : `"default"` = entrée
    /// par défaut de l'OS, nom exact sinon, `None` = repli sur le réglage
    /// machine (`config.toml`) ou capture coupée. Modifiable à chaud par
    /// `SettingsUpdate`.
    pub audio_input: Option<String>,
    /// Anti double-GO : délai minimal entre deux GO, en millisecondes,
    /// appliqué dans la session à TOUTES les sources (UI/OSC/MIDI/MSC).
    /// Un GO refusé émet un `StateEvent::Warning` throttlé. 0 = désactivé.
    pub min_go_interval_ms: u32,
    /// Gabarits appliqués aux nouvelles cues (`CueAdd`).
    pub cue_defaults: CueDefaults,
}

impl Default for ShowSettings {
    fn default() -> Self {
        ShowSettings {
            osc_in_port: 9000,
            osc_out_port: 9001,
            artnet_enabled: false,
            artnet_universes: vec![0],
            language: "fr".to_string(),
            mjpeg_fps: 8,
            mjpeg_width: 640,
            mjpeg_height: 360,
            autosave_debounce_s: 2.0,
            autosave_interval_s: 60.0,
            audio_input: None,
            min_go_interval_ms: 300,
            cue_defaults: CueDefaults::default(),
        }
    }
}

/// Le show complet — JSON lisible, versionné, chargé avec tolérance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Show {
    pub format_version: u32,
    pub name: String,
    #[serde(default)]
    pub outputs: Vec<OutputCfg>,
    #[serde(default)]
    pub slices: Vec<Slice>,
    #[serde(default)]
    pub media: Vec<MediaRef>,
    #[serde(default)]
    pub materials: Vec<MaterialRef>,
    #[serde(default)]
    pub cues: Vec<Cue>,
    #[serde(default)]
    pub patch: PatchTable,
    #[serde(default)]
    pub modulators: Vec<ModulatorCfg>,
    /// Branchements modulateur → paramètre (les cues les activent et en
    /// règlent la profondeur via `Cue::mod_routes`). Ajout par rapport à
    /// INTERFACES.md : les routes doivent être persistées quelque part.
    #[serde(default)]
    pub routes: Vec<ModRoute>,
    #[serde(default)]
    pub settings: ShowSettings,
}

impl Show {
    /// Show vide nommé, au format courant.
    pub fn new(name: impl Into<String>) -> Self {
        Show {
            format_version: FORMAT_VERSION,
            name: name.into(),
            outputs: Vec::new(),
            slices: Vec::new(),
            media: Vec::new(),
            materials: Vec::new(),
            cues: Vec::new(),
            patch: PatchTable::default(),
            modulators: Vec::new(),
            routes: Vec::new(),
            settings: ShowSettings::default(),
        }
    }
}
