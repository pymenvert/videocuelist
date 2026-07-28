//! Événements d'état runtime et instantanés sérialisés vers l'UI
//! (WebSocket, feedback OSC/MIDI).

use serde::{Deserialize, Serialize};

use crate::command::EditOp;
use crate::model::{AppMode, CueNumber, OutputId};

/// Événement runtime publié aux abonnés (UI, feedback OSC/MIDI, journal).
/// JSON : `{"type":"...", ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StateEvent {
    /// La cue active a changé (`None` = plateau vide / panic).
    CueChanged { active: Option<CueNumber> },
    /// La cue en standby a changé.
    StandbyChanged { standby: Option<CueNumber> },
    /// Progression de la transition en cours (0..1).
    TransitionProgress { progress: f32 },
    /// Une mutation du modèle a été appliquée (l'UI se resynchronise).
    EditApplied { op: EditOp },
    /// Bandeau santé périodique.
    HealthTick { snapshot: HealthSnapshot },
    /// Ligne de journal (ring buffer publié à l'UI).
    LogLine {
        level: String,
        target: String,
        message: String,
    },
    /// Mode Edit/Show basculé.
    ModeChanged { mode: AppMode },
    /// Un show a été chargé (l'UI recharge tout).
    ShowLoaded { name: String },
    /// BPM maître changé (tap, nudge, commande).
    BpmChanged { bpm: f32 },
    /// Master intensité changé (0..1).
    MasterChanged { value: f32 },
    /// Dead blackout posé/levé.
    DboChanged { active: bool },
    /// Avertissement de conduite non bloquant (GO refusé par l'anti
    /// double-GO, commande impossible…) — affiché par l'UI, throttlé côté
    /// émetteur.
    Warning { message: String },
    /// Un fichier de récupération post-crash plus récent que le show chargé
    /// existe : l'UI propose `Command::RecoveryLoad { path }` ou
    /// `Command::RecoveryDismiss`. Émis au démarrage, et l'information reste
    /// disponible dans `runtime.recovery` tant qu'elle n'est pas tranchée.
    RecoveryAvailable { path: String, timestamp: String },
    /// Le zip de diagnostic est prêt (chemin expurgé, relatif ou `~`).
    DiagnosticReady { path: String },
}

/// Mise à jour disponible (vérification opt-in au démarrage — voir
/// `ShowSettings::update_check`). Jamais de téléchargement automatique.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateInfo {
    /// Version publiée (ex. `"0.2.0"`).
    pub version: String,
    /// Page de téléchargement (releases GitHub).
    pub url: String,
    /// Notes de version courtes.
    pub notes: String,
}

/// Instantané de conduite sérialisé vers l'UI (~10 Hz).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub mode: AppMode,
    pub active: Option<CueNumber>,
    pub standby: Option<CueNumber>,
    /// Progression de la cue active (média / wait / transition), 0..1.
    pub progress: f32,
    /// Temps restant estimé (média ou wait), en secondes.
    pub remaining_s: f32,
    pub transition_active: bool,
    pub bpm: f32,
    /// Master intensité 0..1.
    pub master: f32,
    /// Dead blackout actif ?
    pub dbo: bool,
    /// Niveaux instantanés des modulateurs : (id, niveau 0..1) — vumètres UI.
    pub mod_levels: Vec<(u32, f32)>,
    /// Mise à jour disponible (`None` = pas de vérification ou à jour).
    /// Absent du JSON quand `None` (compat trames existantes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<UpdateInfo>,
}

impl Default for RuntimeStatus {
    fn default() -> Self {
        RuntimeStatus {
            mode: AppMode::Edit,
            active: None,
            standby: None,
            progress: 0.0,
            remaining_s: 0.0,
            transition_active: false,
            bpm: 120.0,
            master: 1.0,
            dbo: false,
            mod_levels: Vec::new(),
            update: None,
        }
    }
}

/// Instantané santé machine (bandeau Live + page Journal).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HealthSnapshot {
    /// FPS mesuré par sortie : (output, fps).
    pub fps: Vec<(OutputId, f32)>,
    /// Frames perdues (cumul) par sortie : (output, drops).
    pub drops: Vec<(OutputId, u64)>,
    pub cpu_pct: f32,
    pub mem_mb: f32,
    /// Température (Pi) — absente sur les machines qui ne l'exposent pas.
    pub temp_c: Option<f32>,
}
