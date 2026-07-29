//! # conduite-control-midi
//!
//! Traduction MIDI ↔ [`Command`](conduite_core::Command) pour Conduite :
//! learn, CC 7/14 bits avec plages, soft-takeover (pickup), MIDI Show
//! Control, feedback vers les surfaces (LED, faders motorisés).
//!
//! Architecture (pattern hérité de Lanterne) : **toute la logique est pure et
//! testée** — [`parse_midi`], [`Cc14Assembler`], [`Pickup`], [`parse_msc`],
//! [`Learn`], [`resolve`], composés par [`MidiEngine`]. Seul [`MidiHub`]
//! touche le matériel (midir), avec reconnexion périodique si le port
//! disparaît. Une erreur MIDI ne fait JAMAIS tomber la régie.
//!
//! Chaîne d'un message entrant :
//!
//! ```text
//! octets bruts → parse_midi → Cc14Assembler → Learn (si armé)
//!                    │                      └→ resolve + Pickup → Command
//!                    └ SysEx → parse_msc ────────────────────────→ Command
//! ```

pub mod cc14;
pub mod engine;
pub mod hub;
pub mod learn;
pub mod msc;
pub mod msg;
pub mod mtc;
pub mod pickup;
pub mod resolve;

pub use cc14::{Cc14Assembler, DEFAULT_PAIR_TIMEOUT_MS};
pub use engine::{EngineEvent, MidiEngine};
pub use hub::{choose_port, HubEvent, MidiHub};
pub use learn::Learn;
pub use msc::{parse_msc, MSC_ALL_CALL};
pub use msg::{parse_midi, MidiMsg};
pub use mtc::{MtcAssembler, MtcClock, MtcEvent, FREEWHEEL_S};
pub use pickup::{Pickup, PickupDecision};
pub use resolve::{resolve, scale};
