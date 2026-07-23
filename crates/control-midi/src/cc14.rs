//! Assemblage des paires CC 14 bits — logique pure, testée.
//!
//! Convention MIDI : le MSB arrive sur le CC `n` (0..=31), le LSB sur le CC
//! `n + 32`. Seules les paires **déclarées** (via les bindings 14 bits) sont
//! assemblées ; tout autre CC traverse tel quel.
//!
//! Timeout d'appariement : un MSB sans LSB dans la fenêtre est émis seul
//! (valeur `msb << 7`) au prochain [`Cc14Assembler::feed`] ou
//! [`Cc14Assembler::flush`] — les contrôleurs « gros grain » qui n'envoient
//! jamais de LSB restent donc utilisables.

use std::collections::{HashMap, HashSet};

use crate::msg::MidiMsg;

/// Fenêtre d'appariement MSB→LSB par défaut (ms).
pub const DEFAULT_PAIR_TIMEOUT_MS: u64 = 50;

#[derive(Debug, Clone, Copy)]
struct Pending {
    msb: u8,
    at_ms: u64,
}

/// Assembleur de paires CC 14 bits, piloté par une horloge externe (`now_ms`).
#[derive(Debug, Default)]
pub struct Cc14Assembler {
    /// Paires (canal, cc MSB) déclarées 14 bits.
    pairs: HashSet<(u8, u8)>,
    /// MSB reçus en attente de leur LSB.
    pending: HashMap<(u8, u8), Pending>,
    /// Dernier MSB connu par paire — permet les LSB seuls (réglage fin).
    last_msb: HashMap<(u8, u8), u8>,
    timeout_ms: u64,
}

impl Cc14Assembler {
    pub fn new() -> Self {
        Cc14Assembler {
            pairs: HashSet::new(),
            pending: HashMap::new(),
            last_msb: HashMap::new(),
            timeout_ms: DEFAULT_PAIR_TIMEOUT_MS,
        }
    }

    /// Change la fenêtre d'appariement (tests, contrôleurs lents).
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms.max(1);
        self
    }

    /// Déclare les paires 14 bits (canal, cc MSB). Un cc > 31 ne peut pas
    /// porter de MSB : ignoré avec un avertissement.
    pub fn set_pairs(&mut self, pairs: impl IntoIterator<Item = (u8, u8)>) {
        self.pairs.clear();
        for (channel, cc) in pairs {
            if cc > 31 {
                tracing::warn!(channel, cc, "binding 14 bits invalide : le CC MSB doit être ≤ 31");
                continue;
            }
            self.pairs.insert((channel, cc));
        }
        // Les états en attente de paires disparues n'ont plus de sens.
        self.pending.retain(|k, _| self.pairs.contains(k));
        self.last_msb.retain(|k, _| self.pairs.contains(k));
    }

    /// Injecte un message. Retourne 0..n messages prêts : les CC appariés
    /// deviennent des [`MidiMsg::ControlChange14`], le reste traverse.
    pub fn feed(&mut self, msg: MidiMsg, now_ms: u64) -> Vec<MidiMsg> {
        let mut out = self.flush(now_ms);
        match msg {
            MidiMsg::ControlChange { channel, cc, value } => {
                let key_msb = (channel, cc);
                if self.pairs.contains(&key_msb) {
                    // MSB : deux MSB de suite = le LSB ne viendra pas → on
                    // émet le premier « gros grain » avant de mémoriser.
                    if let Some(p) = self.pending.insert(
                        key_msb,
                        Pending {
                            msb: value,
                            at_ms: now_ms,
                        },
                    ) {
                        out.push(MidiMsg::ControlChange14 {
                            channel,
                            cc,
                            value: u16::from(p.msb) << 7,
                        });
                    }
                    self.last_msb.insert(key_msb, value);
                } else if (32..=63).contains(&cc) && self.pairs.contains(&(channel, cc - 32)) {
                    // LSB : combine avec le MSB en attente, sinon avec le
                    // dernier MSB connu (réglage fin), sinon 0.
                    let key = (channel, cc - 32);
                    let msb = self
                        .pending
                        .remove(&key)
                        .map(|p| p.msb)
                        .or_else(|| self.last_msb.get(&key).copied())
                        .unwrap_or(0);
                    out.push(MidiMsg::ControlChange14 {
                        channel,
                        cc: cc - 32,
                        value: (u16::from(msb) << 7) | u16::from(value),
                    });
                } else {
                    out.push(MidiMsg::ControlChange { channel, cc, value });
                }
            }
            other => out.push(other),
        }
        out
    }

    /// Émet les MSB dont la fenêtre d'appariement a expiré (valeur gros grain).
    /// À appeler périodiquement (tick du hub).
    pub fn flush(&mut self, now_ms: u64) -> Vec<MidiMsg> {
        let timeout = self.timeout_ms;
        let mut out = Vec::new();
        self.pending.retain(|&(channel, cc), p| {
            if now_ms.saturating_sub(p.at_ms) >= timeout {
                out.push(MidiMsg::ControlChange14 {
                    channel,
                    cc,
                    value: u16::from(p.msb) << 7,
                });
                false
            } else {
                true
            }
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(channel: u8, cc: u8, value: u8) -> MidiMsg {
        MidiMsg::ControlChange { channel, cc, value }
    }

    fn asm() -> Cc14Assembler {
        let mut a = Cc14Assembler::new();
        a.set_pairs([(0u8, 7u8)]);
        a
    }

    #[test]
    fn pairs_msb_then_lsb_within_window() {
        let mut a = asm();
        assert_eq!(a.feed(cc(0, 7, 0x40), 0), vec![]); // MSB en attente
        assert_eq!(
            a.feed(cc(0, 39, 0x25), 10),
            vec![MidiMsg::ControlChange14 {
                channel: 0,
                cc: 7,
                value: (0x40 << 7) | 0x25
            }]
        );
    }

    #[test]
    fn msb_alone_flushes_after_timeout() {
        let mut a = asm();
        assert_eq!(a.feed(cc(0, 7, 100), 0), vec![]);
        assert_eq!(a.flush(10), vec![]); // fenêtre pas expirée
        assert_eq!(
            a.flush(DEFAULT_PAIR_TIMEOUT_MS),
            vec![MidiMsg::ControlChange14 {
                channel: 0,
                cc: 7,
                value: 100 << 7
            }]
        );
        assert_eq!(a.flush(1000), vec![]); // une seule fois
    }

    #[test]
    fn lsb_alone_reuses_last_msb() {
        let mut a = asm();
        let _ = a.feed(cc(0, 7, 0x10), 0);
        let _ = a.feed(cc(0, 39, 0x00), 5);
        // Réglage fin : le contrôleur n'envoie plus que le LSB.
        assert_eq!(
            a.feed(cc(0, 39, 0x33), 20),
            vec![MidiMsg::ControlChange14 {
                channel: 0,
                cc: 7,
                value: (0x10 << 7) | 0x33
            }]
        );
    }

    #[test]
    fn two_msb_in_a_row_emit_coarse_value() {
        let mut a = asm();
        assert_eq!(a.feed(cc(0, 7, 10), 0), vec![]);
        // Deuxième MSB avant le LSB : le premier sort en gros grain.
        assert_eq!(
            a.feed(cc(0, 7, 11), 5),
            vec![MidiMsg::ControlChange14 {
                channel: 0,
                cc: 7,
                value: 10 << 7
            }]
        );
    }

    #[test]
    fn undeclared_cc_passes_through() {
        let mut a = asm();
        // CC 20 non déclaré, et CC 52 (= 20 + 32) non plus.
        assert_eq!(a.feed(cc(0, 20, 64), 0), vec![cc(0, 20, 64)]);
        assert_eq!(a.feed(cc(0, 52, 64), 0), vec![cc(0, 52, 64)]);
        // Même CC 7 mais sur un autre canal : traverse.
        assert_eq!(a.feed(cc(5, 7, 64), 0), vec![cc(5, 7, 64)]);
        // Les autres messages traversent toujours.
        let note = MidiMsg::NoteOn {
            channel: 0,
            note: 60,
            velocity: 1,
        };
        assert_eq!(a.feed(note.clone(), 0), vec![note]);
    }

    #[test]
    fn invalid_msb_pair_is_ignored() {
        let mut a = Cc14Assembler::new();
        a.set_pairs([(0u8, 40u8)]); // cc > 31 : refusé
        assert_eq!(a.feed(cc(0, 40, 64), 0), vec![cc(0, 40, 64)]);
    }
}
