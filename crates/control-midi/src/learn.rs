//! Mode LEARN — logique pure, testée.
//!
//! Une fois armé, capture le **premier** message significatif (note-on ou CC)
//! et retourne un [`MidiBinding`] pré-rempli que l'UI complète (commande ou
//! adresse de paramètre). Se désarme automatiquement après capture.
//!
//! Détection 14 bits : si le premier CC capturé est un MSB potentiel
//! (cc ≤ 31) et que son LSB (`cc + 32`) suit dans la fenêtre d'appariement,
//! le binding est pré-rempli en 14 bits. Sinon il sort en 7 bits (au prochain
//! message ou au [`Learn::flush`] périodique).

use conduite_core::{CommandTemplate, MidiBinding};

use crate::cc14::DEFAULT_PAIR_TIMEOUT_MS;
use crate::msg::MidiMsg;

#[derive(Debug, Clone, Copy)]
struct PendingCc {
    channel: u8,
    cc: u8,
    at_ms: u64,
}

/// État du mode learn (armé / en attente d'un éventuel LSB).
#[derive(Debug, Default)]
pub struct Learn {
    armed: bool,
    pending: Option<PendingCc>,
    timeout_ms: u64,
}

impl Learn {
    pub fn new() -> Self {
        Learn {
            armed: false,
            pending: None,
            timeout_ms: DEFAULT_PAIR_TIMEOUT_MS,
        }
    }

    /// Arme la capture du prochain message significatif.
    pub fn arm(&mut self) {
        self.armed = true;
        self.pending = None;
        tracing::info!("MIDI learn armé");
    }

    /// Annule la capture en cours.
    pub fn disarm(&mut self) {
        self.armed = false;
        self.pending = None;
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Injecte un message. Retourne le binding capturé (et se désarme), ou
    /// `None` si la capture continue. Les note-off, pitch bend et SysEx ne
    /// sont pas « significatifs » : ignorés.
    pub fn feed(&mut self, msg: &MidiMsg, now_ms: u64) -> Option<MidiBinding> {
        if !self.armed {
            return None;
        }
        // Un MSB en attente dont la fenêtre a expiré sort en 7 bits d'abord.
        if let Some(b) = self.flush(now_ms) {
            return Some(b);
        }
        match *msg {
            MidiMsg::NoteOn { channel, note, .. } => Some(self.captured(MidiBinding::Note {
                channel,
                note,
                command: CommandTemplate::Go,
            })),
            MidiMsg::ControlChange { channel, cc, .. } => {
                if let Some(p) = self.pending {
                    if p.channel == channel && cc == p.cc + 32 {
                        // LSB apparié : le fader est 14 bits.
                        return Some(self.captured(cc_binding(p.channel, p.cc, true)));
                    }
                    // Autre CC : le premier message capturé gagne, en 7 bits.
                    return Some(self.captured(cc_binding(p.channel, p.cc, false)));
                }
                if cc <= 31 {
                    // MSB potentiel : on attend son LSB (cc + 32).
                    self.pending = Some(PendingCc {
                        channel,
                        cc,
                        at_ms: now_ms,
                    });
                    None
                } else {
                    Some(self.captured(cc_binding(channel, cc, false)))
                }
            }
            // Déjà assemblé (paire connue de l'assembleur) : 14 bits direct.
            MidiMsg::ControlChange14 { channel, cc, .. } => {
                Some(self.captured(cc_binding(channel, cc, true)))
            }
            MidiMsg::NoteOff { .. } | MidiMsg::PitchBend { .. } | MidiMsg::SysEx(_) => None,
        }
    }

    /// Fait sortir en 7 bits un MSB dont la fenêtre d'appariement a expiré.
    /// À appeler périodiquement (tick du hub).
    pub fn flush(&mut self, now_ms: u64) -> Option<MidiBinding> {
        let p = self.pending?;
        if now_ms.saturating_sub(p.at_ms) >= self.timeout_ms {
            return Some(self.captured(cc_binding(p.channel, p.cc, false)));
        }
        None
    }

    fn captured(&mut self, binding: MidiBinding) -> MidiBinding {
        self.armed = false;
        self.pending = None;
        tracing::info!(?binding, "MIDI learn : message capturé");
        binding
    }
}

/// Binding CC pré-rempli : l'UI fournit l'adresse cible et la plage.
fn cc_binding(channel: u8, cc: u8, fourteen_bits: bool) -> MidiBinding {
    MidiBinding::Cc {
        channel,
        cc,
        fourteen_bits,
        addr: String::new(),
        min: 0.0,
        max: 1.0,
        pickup: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(channel: u8, cc: u8, value: u8) -> MidiMsg {
        MidiMsg::ControlChange { channel, cc, value }
    }

    #[test]
    fn note_is_captured_and_disarms() {
        let mut l = Learn::new();
        l.arm();
        let b = l.feed(
            &MidiMsg::NoteOn {
                channel: 2,
                note: 60,
                velocity: 100,
            },
            0,
        );
        assert_eq!(
            b,
            Some(MidiBinding::Note {
                channel: 2,
                note: 60,
                command: CommandTemplate::Go
            })
        );
        assert!(!l.is_armed());
        // Désarmé : plus rien ne sort.
        assert_eq!(
            l.feed(
                &MidiMsg::NoteOn {
                    channel: 2,
                    note: 61,
                    velocity: 100
                },
                10
            ),
            None
        );
    }

    #[test]
    fn high_cc_is_captured_as_7_bits_immediately() {
        let mut l = Learn::new();
        l.arm();
        match l.feed(&cc(0, 40, 64), 0) {
            Some(MidiBinding::Cc {
                channel: 0,
                cc: 40,
                fourteen_bits: false,
                pickup: true,
                ..
            }) => {}
            other => panic!("attendu Cc 7 bits, obtenu {other:?}"),
        }
    }

    #[test]
    fn low_cc_followed_by_lsb_becomes_14_bits() {
        let mut l = Learn::new();
        l.arm();
        assert_eq!(l.feed(&cc(0, 7, 100), 0), None); // MSB : on attend
        match l.feed(&cc(0, 39, 3), 10) {
            Some(MidiBinding::Cc {
                cc: 7,
                fourteen_bits: true,
                ..
            }) => {}
            other => panic!("attendu Cc 14 bits sur 7, obtenu {other:?}"),
        }
        assert!(!l.is_armed());
    }

    #[test]
    fn low_cc_repeated_becomes_7_bits() {
        let mut l = Learn::new();
        l.arm();
        assert_eq!(l.feed(&cc(0, 7, 100), 0), None);
        // Le même CC revient (fader 7 bits qui bouge) : capture en 7 bits.
        match l.feed(&cc(0, 7, 101), 10) {
            Some(MidiBinding::Cc {
                cc: 7,
                fourteen_bits: false,
                ..
            }) => {}
            other => panic!("attendu Cc 7 bits, obtenu {other:?}"),
        }
    }

    #[test]
    fn low_cc_alone_flushes_as_7_bits_after_timeout() {
        let mut l = Learn::new();
        l.arm();
        assert_eq!(l.feed(&cc(0, 7, 100), 0), None);
        assert_eq!(l.flush(10), None); // fenêtre pas expirée
        match l.flush(DEFAULT_PAIR_TIMEOUT_MS) {
            Some(MidiBinding::Cc {
                cc: 7,
                fourteen_bits: false,
                ..
            }) => {}
            other => panic!("attendu Cc 7 bits, obtenu {other:?}"),
        }
        assert!(!l.is_armed());
    }

    #[test]
    fn assembled_cc14_is_captured_as_14_bits() {
        let mut l = Learn::new();
        l.arm();
        match l.feed(
            &MidiMsg::ControlChange14 {
                channel: 1,
                cc: 3,
                value: 1000,
            },
            0,
        ) {
            Some(MidiBinding::Cc {
                channel: 1,
                cc: 3,
                fourteen_bits: true,
                ..
            }) => {}
            other => panic!("attendu Cc 14 bits, obtenu {other:?}"),
        }
    }

    #[test]
    fn insignificant_messages_are_ignored() {
        let mut l = Learn::new();
        l.arm();
        assert_eq!(
            l.feed(
                &MidiMsg::NoteOff {
                    channel: 0,
                    note: 60
                },
                0
            ),
            None
        );
        assert_eq!(
            l.feed(
                &MidiMsg::PitchBend {
                    channel: 0,
                    value: 8192
                },
                0
            ),
            None
        );
        assert_eq!(l.feed(&MidiMsg::SysEx(vec![0xF0, 0xF7]), 0), None);
        assert!(l.is_armed(), "toujours armé après messages ignorés");
        // Non armé : tout est ignoré.
        l.disarm();
        assert_eq!(l.feed(&cc(0, 40, 1), 0), None);
    }
}
