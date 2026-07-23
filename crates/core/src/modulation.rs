//! Types de modulation (LFO, bandes audio) — owned par `core` car sérialisés
//! dans le show. La machinerie d'évaluation vit dans la crate `modulation`.

use serde::{Deserialize, Serialize};

use crate::model::ModId;

/// Forme d'onde d'un LFO.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wave {
    Sine,
    Tri,
    /// Carré avec largeur d'impulsion 0..1.
    Square { pw: f32 },
    Saw,
    /// Random sample & hold (seedé côté moteur).
    RandomSh,
    /// Dérive type Perlin (seedée côté moteur).
    Drift,
}

/// Fréquence d'un LFO : Hz fixes ou synchro BPM.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freq {
    Hz(f32),
    /// `mult` en cycles par temps : 0.25 = 1 cycle sur 4 temps (1 mesure).
    BpmSync { mult: f32 },
}

/// Nature d'un modulateur.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModKind {
    Lfo {
        wave: Wave,
        freq: Freq,
        /// Phase initiale 0..1.
        phase: f32,
    },
    /// Bande d'analyse audio (FFT d'entrée, jamais de sortie son).
    AudioBand {
        low_hz: f32,
        high_hz: f32,
        gain: f32,
        /// Plancher soustrait avant gain (réjection du bruit de fond).
        floor: f32,
        attack_ms: f32,
        release_ms: f32,
    },
}

/// Un modulateur configuré (source de signal interne 0..1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModulatorCfg {
    pub id: ModId,
    pub name: String,
    pub kind: ModKind,
}

/// Mode d'application d'une route de modulation sur son paramètre cible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteMode {
    /// Valeur = base + signal × depth.
    Add,
    /// Valeur = base × (1 - depth + signal × depth).
    Mul,
    /// Valeur = signal × depth (la base est ignorée).
    Replace,
}

/// Branchement modulateur → paramètre (profondeur par défaut du show).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModRoute {
    pub id: u32,
    pub source: ModId,
    /// Adresse stable du paramètre cible (ex. `slice/1/opacity`).
    pub target_addr: String,
    pub depth: f32,
    pub mode: RouteMode,
}

/// État d'une route dans une cue : une cue peut activer/désactiver/changer
/// la profondeur d'un branchement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModRouteState {
    pub route_id: u32,
    pub depth: f32,
    pub enabled: bool,
}
