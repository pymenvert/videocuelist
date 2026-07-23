//! Types de patch (Art-Net, MIDI, OSC sortant) — owned par `core` car
//! sérialisés dans le show. La machinerie (sockets, learn, pickup) vit dans
//! les crates `control-*`.

use serde::{Deserialize, Serialize};

use crate::command::CommandTemplate;

/// Table de patch complète du show.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PatchTable {
    pub artnet: Vec<PatchEntry>,
    pub midi: Vec<MidiBinding>,
    pub osc_out: Option<OscOutCfg>,
}

/// Résolution DMX d'une entrée de patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DmxBits {
    Eight,
    /// 16 bits : canal (MSB) + canal suivant (LSB).
    Sixteen,
}

/// Patch d'un canal DMX (Art-Net) vers un paramètre.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchEntry {
    pub universe: u16,
    /// Canal DMX 1..=512 (en 16 bits, le LSB est sur `channel + 1`).
    pub channel: u16,
    pub bits: DmxBits,
    /// Adresse stable du paramètre cible.
    pub addr: String,
    pub min: f32,
    pub max: f32,
    /// Lissage à la réception (le DMX arrive à ~44 Hz, on interpole).
    pub smoothing_ms: f32,
}

/// Binding MIDI learn-able : note → commande, ou CC → paramètre.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MidiBinding {
    /// Note-on (canal 0..15, note 0..127) → commande sérialisable.
    Note {
        channel: u8,
        note: u8,
        command: CommandTemplate,
    },
    /// Control Change (7 ou 14 bits) → paramètre avec plage.
    Cc {
        channel: u8,
        /// Numéro du CC (en 14 bits : MSB sur `cc`, LSB sur `cc + 32`).
        cc: u8,
        fourteen_bits: bool,
        /// Adresse stable du paramètre cible.
        addr: String,
        min: f32,
        max: f32,
        /// Soft-takeover : pas de saut tant que le fader physique n'a pas
        /// rejoint la valeur mémorisée.
        pickup: bool,
    },
}

/// Destination OSC sortante (feedback régie : TouchOSC, Companion…).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OscOutCfg {
    pub host: String,
    pub port: u16,
}
