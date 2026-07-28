// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Vocabulaire de commandes de Conduite.
//!
//! Principe hérité de Lanterne : la web UI, l'OSC, le MIDI et le patch DMX
//! émettent les *mêmes* [`Command`]. Le JSON (tag `cmd`, snake_case) est un
//! contrat public figé par des tests — toute rupture casse la web UI et les
//! surfaces de contrôle distantes.

use serde::{Deserialize, Serialize};

use crate::model::{
    AppMode, Cue, CueNumber, MaterialId, MaterialRef, MediaId, MediaRef, ModId, OutputCfg,
    OutputId, ParamValue, Show, ShowSettings, Slice, SliceId, SliceState,
};
use crate::modulation::{ModRoute, ModulatorCfg};
use crate::patch::{KeyBinding, MidiBinding, OscOutCfg, PatchEntry};

/// Origine d'une commande (arbitrage priorité / feedback / journal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Ui,
    Osc,
    Midi,
    #[serde(rename = "artnet")]
    ArtNet,
    Cue,
    Modulation,
    Internal,
}

/// Une commande adressée au moteur. JSON : `{"cmd":"...", ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Pose une valeur de paramètre (adresse stable).
    ParamSet {
        addr: String,
        value: ParamValue,
        source: Source,
    },
    /// Incrémente un paramètre flottant (nudge encodeur/clavier).
    ParamNudge {
        addr: String,
        delta: f32,
        source: Source,
    },
    CueGo,
    CueBack,
    CueGoto {
        cue: CueNumber,
    },
    CueStandby {
        cue: CueNumber,
    },
    /// Arrêt d'urgence de la conduite : fondu au noir + stop des lectures.
    CuePanic {
        fade_s: f32,
    },
    /// Dead blackout (voile noir maître), temps de fondu réglable.
    Dbo {
        fade_s: f32,
    },
    DboRelease,
    TapTempo,
    BpmSet {
        bpm: f32,
    },
    /// Toute mutation du modèle (undo-able en mode édition).
    Edit(EditOp),
    /// Annule la dernière édition (pile de snapshots, mode Edit uniquement).
    Undo,
    /// Rétablit la dernière édition annulée (mode Edit uniquement).
    Redo,
    /// Arme la capture MIDI learn : le prochain message significatif est
    /// capturé et publié à l'UI (pré-remplissage d'un binding).
    MidiLearnStart,
    /// Désarme la capture MIDI learn.
    MidiLearnCancel,
    ShowSave,
    ShowSaveAs {
        name: String,
    },
    ShowLoad {
        name: String,
    },
    ShowNew,
    /// Re-scan du dossier `media/` (vignettes, état OK/manquant).
    MediaRescan,
    /// Génère un zip de diagnostic (logs récents, config, show, versions,
    /// santé — chemins personnels expurgés) en tâche de fond ; l'app publie
    /// [`crate::StateEvent::DiagnosticReady`] à la fin.
    DiagnosticReport,
    /// « Collecter le show » : copie de tous les médias dans un dossier autonome.
    ShowCollect,
    /// Edit (libre) | Show (verrouillé).
    ModeSet {
        mode: AppMode,
    },
    /// Charge le fichier de récupération proposé au démarrage
    /// (`StateEvent::RecoveryAvailable`). Le chemin est validé côté session :
    /// uniquement un `recover-*.json` du dossier `shows/`.
    RecoveryLoad {
        path: String,
    },
    /// Écarte la proposition de récupération.
    RecoveryDismiss,
    /// Arrêt propre : flush des journaux, sauvegarde si le show est
    /// modifié, sortie code 0.
    Quit,
}

/// Mutation du modèle de show — undo-able, refusée en mode Show.
/// JSON : `{"op":"...", ...}` (imbriqué dans `{"cmd":"edit", ...}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    SliceAdd { slice: Slice },
    SliceRemove { id: SliceId },
    SliceUpdate { slice: Slice },
    /// Déplace un coin d'un slice. `index` ∈ 0..=3 (TL, TR, BR, BL),
    /// coordonnées normalisées 0..1 dans l'espace de sortie.
    CornerSet { slice: SliceId, index: u8, x: f32, y: f32 },
    OutputAdd { output: OutputCfg },
    OutputRemove { id: OutputId },
    OutputUpdate { output: OutputCfg },
    CueAdd { cue: Cue },
    CueRemove { number: CueNumber },
    /// Remplace la cue entière (même numéro).
    CueUpdate { cue: Cue },
    /// Remplace l'état d'un seul slice dans une cue.
    CueUpdateState { number: CueNumber, state: SliceState },
    MediaAdd { media: MediaRef },
    MediaRemove { id: MediaId },
    MediaUpdate { media: MediaRef },
    MaterialAdd { material: MaterialRef },
    MaterialRemove { id: MaterialId },
    MaterialUpdate { material: MaterialRef },
    ModulatorAdd { modulator: ModulatorCfg },
    ModulatorRemove { id: ModId },
    ModulatorUpdate { modulator: ModulatorCfg },
    RouteAdd { route: ModRoute },
    RouteRemove { id: u32 },
    RouteUpdate { route: ModRoute },
    PatchArtnetAdd { entry: PatchEntry },
    PatchArtnetRemove { index: usize },
    PatchArtnetUpdate { index: usize, entry: PatchEntry },
    PatchMidiAdd { binding: MidiBinding },
    PatchMidiRemove { index: usize },
    PatchMidiUpdate { index: usize, binding: MidiBinding },
    PatchOscOutSet { cfg: Option<OscOutCfg> },
    KeyBindingAdd { binding: KeyBinding },
    KeyBindingRemove { index: usize },
    ShowRename { name: String },
    SettingsUpdate { settings: ShowSettings },
}

impl EditOp {
    /// Applique l'opération au show. Idempotent quand la cible n'existe pas
    /// (remove sur id inconnu = no-op) : jamais de panic en régie.
    pub fn apply(&self, show: &mut Show) {
        match self {
            EditOp::SliceAdd { slice } => show.slices.push(slice.clone()),
            EditOp::SliceRemove { id } => show.slices.retain(|s| s.id != *id),
            EditOp::SliceUpdate { slice } => {
                if let Some(s) = show.slices.iter_mut().find(|s| s.id == slice.id) {
                    *s = slice.clone();
                }
            }
            EditOp::CornerSet { slice, index, x, y } => {
                if let Some(s) = show.slices.iter_mut().find(|s| s.id == *slice) {
                    if let Some(c) = s.corners.get_mut(*index as usize) {
                        *c = [*x, *y];
                    }
                }
            }
            EditOp::OutputAdd { output } => show.outputs.push(output.clone()),
            EditOp::OutputRemove { id } => show.outputs.retain(|o| o.id != *id),
            EditOp::OutputUpdate { output } => {
                if let Some(o) = show.outputs.iter_mut().find(|o| o.id == output.id) {
                    *o = output.clone();
                }
            }
            EditOp::CueAdd { cue } => {
                // Insertion triée par numéro (remplace si numéro identique).
                match show.cues.binary_search_by(|c| c.number.cmp(&cue.number)) {
                    Ok(i) => show.cues[i] = cue.clone(),
                    Err(i) => show.cues.insert(i, cue.clone()),
                }
            }
            EditOp::CueRemove { number } => show.cues.retain(|c| c.number != *number),
            EditOp::CueUpdate { cue } => {
                if let Some(c) = show.cues.iter_mut().find(|c| c.number == cue.number) {
                    *c = cue.clone();
                }
            }
            EditOp::CueUpdateState { number, state } => {
                if let Some(c) = show.cues.iter_mut().find(|c| c.number == *number) {
                    match c.states.iter_mut().find(|s| s.slice == state.slice) {
                        Some(s) => *s = state.clone(),
                        None => c.states.push(state.clone()),
                    }
                }
            }
            EditOp::MediaAdd { media } => show.media.push(media.clone()),
            EditOp::MediaRemove { id } => show.media.retain(|m| m.id != *id),
            EditOp::MediaUpdate { media } => {
                if let Some(m) = show.media.iter_mut().find(|m| m.id == media.id) {
                    *m = media.clone();
                }
            }
            EditOp::MaterialAdd { material } => show.materials.push(material.clone()),
            EditOp::MaterialRemove { id } => show.materials.retain(|m| m.id != *id),
            EditOp::MaterialUpdate { material } => {
                if let Some(m) = show.materials.iter_mut().find(|m| m.id == material.id) {
                    *m = material.clone();
                }
            }
            EditOp::ModulatorAdd { modulator } => show.modulators.push(modulator.clone()),
            EditOp::ModulatorRemove { id } => show.modulators.retain(|m| m.id != *id),
            EditOp::ModulatorUpdate { modulator } => {
                if let Some(m) = show.modulators.iter_mut().find(|m| m.id == modulator.id) {
                    *m = modulator.clone();
                }
            }
            EditOp::RouteAdd { route } => show.routes.push(route.clone()),
            EditOp::RouteRemove { id } => show.routes.retain(|r| r.id != *id),
            EditOp::RouteUpdate { route } => {
                if let Some(r) = show.routes.iter_mut().find(|r| r.id == route.id) {
                    *r = route.clone();
                }
            }
            EditOp::PatchArtnetAdd { entry } => show.patch.artnet.push(entry.clone()),
            EditOp::PatchArtnetRemove { index } => {
                if *index < show.patch.artnet.len() {
                    show.patch.artnet.remove(*index);
                }
            }
            EditOp::PatchArtnetUpdate { index, entry } => {
                if let Some(e) = show.patch.artnet.get_mut(*index) {
                    *e = entry.clone();
                }
            }
            EditOp::PatchMidiAdd { binding } => show.patch.midi.push(binding.clone()),
            EditOp::PatchMidiRemove { index } => {
                if *index < show.patch.midi.len() {
                    show.patch.midi.remove(*index);
                }
            }
            EditOp::PatchMidiUpdate { index, binding } => {
                if let Some(b) = show.patch.midi.get_mut(*index) {
                    *b = binding.clone();
                }
            }
            EditOp::PatchOscOutSet { cfg } => show.patch.osc_out = cfg.clone(),
            EditOp::KeyBindingAdd { binding } => show.patch.keys.push(binding.clone()),
            EditOp::KeyBindingRemove { index } => {
                if *index < show.patch.keys.len() {
                    show.patch.keys.remove(*index);
                }
            }
            EditOp::ShowRename { name } => show.name = name.clone(),
            EditOp::SettingsUpdate { settings } => show.settings = settings.clone(),
        }
    }
}

/// Sous-ensemble sérialisable de [`Command`] sans valeurs runtime — utilisé
/// par les bindings MIDI note→commande et les déclencheurs configurés.
/// JSON : tag `cmd`, snake_case (même contrat que `Command`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum CommandTemplate {
    Go,
    Back,
    Goto { cue: CueNumber },
    Standby { cue: CueNumber },
    Panic { fade_s: f32 },
    Dbo { fade_s: f32 },
    DboRelease,
    TapTempo,
    BpmSet { bpm: f32 },
    /// Pose une valeur fixe de paramètre.
    ParamSet { addr: String, value: ParamValue },
    ModeSet { mode: AppMode },
}

impl CommandTemplate {
    /// Instancie la commande runtime correspondante, en marquant l'origine.
    pub fn to_command(&self, source: Source) -> Command {
        match self {
            CommandTemplate::Go => Command::CueGo,
            CommandTemplate::Back => Command::CueBack,
            CommandTemplate::Goto { cue } => Command::CueGoto { cue: *cue },
            CommandTemplate::Standby { cue } => Command::CueStandby { cue: *cue },
            CommandTemplate::Panic { fade_s } => Command::CuePanic { fade_s: *fade_s },
            CommandTemplate::Dbo { fade_s } => Command::Dbo { fade_s: *fade_s },
            CommandTemplate::DboRelease => Command::DboRelease,
            CommandTemplate::TapTempo => Command::TapTempo,
            CommandTemplate::BpmSet { bpm } => Command::BpmSet { bpm: *bpm },
            CommandTemplate::ParamSet { addr, value } => Command::ParamSet {
                addr: addr.clone(),
                value: value.clone(),
                source,
            },
            CommandTemplate::ModeSet { mode } => Command::ModeSet { mode: *mode },
        }
    }
}
