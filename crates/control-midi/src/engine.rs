//! Moteur MIDI pur : parse → assemblage 14 bits → learn / MSC / resolve
//! (+ soft-takeover). Aucune IO : le hub matériel lui passe les octets bruts
//! et une horloge en millisecondes — entièrement testable.

use conduite_core::{Command, Curve, MidiBinding, ParamValue, Source};
use tracing::trace;

use crate::cc14::Cc14Assembler;
use crate::learn::Learn;
use crate::msc::{parse_msc, MSC_ALL_CALL};
use crate::msg::{parse_midi, MidiMsg};
use crate::pickup::{Pickup, PickupDecision};
use crate::resolve::{find_cc, resolve, scale};

/// Sortie du moteur pour un message entrant.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// Commande à pousser sur le bus (source [`Source::Midi`]).
    Command(Command),
    /// Learn : binding capturé, pré-rempli pour l'UI.
    Learned(MidiBinding),
    /// Soft-takeover : le fader n'a pas encore rejoint la valeur logique.
    PickupBlocked {
        addr: String,
        current: f32,
        incoming: f32,
    },
    /// Soft-takeover : le fader vient de reprendre la main.
    PickupEngaged { addr: String },
}

/// Moteur de traduction MIDI (pur). Le hub le partage entre le callback midir
/// et son thread superviseur.
#[derive(Debug)]
pub struct MidiEngine {
    bindings: Vec<MidiBinding>,
    cc14: Cc14Assembler,
    pickup: Pickup,
    learn: Learn,
    /// Device id MSC de CE récepteur (0x7F = accepte tout).
    msc_device_id: u8,
}

impl Default for MidiEngine {
    fn default() -> Self {
        MidiEngine::new(MSC_ALL_CALL)
    }
}

impl MidiEngine {
    pub fn new(msc_device_id: u8) -> Self {
        MidiEngine {
            bindings: Vec::new(),
            cc14: Cc14Assembler::new(),
            pickup: Pickup::new(),
            learn: Learn::new(),
            msc_device_id,
        }
    }

    /// Remplace les bindings (chargement de show, édition du patch) et met à
    /// jour les paires 14 bits de l'assembleur.
    pub fn set_bindings(&mut self, bindings: Vec<MidiBinding>) {
        let pairs: Vec<(u8, u8)> = bindings
            .iter()
            .filter_map(|b| match b {
                MidiBinding::Cc {
                    channel,
                    cc,
                    fourteen_bits: true,
                    ..
                } => Some((*channel, *cc)),
                _ => None,
            })
            .collect();
        self.cc14.set_pairs(pairs);
        self.bindings = bindings;
    }

    /// Cache soft-takeover : l'app pousse la valeur logique courante quand un
    /// paramètre change ailleurs (cue, UI, OSC…).
    pub fn update_logical(&mut self, addr: &str, value: f32) {
        self.pickup.update_logical(addr, value);
    }

    pub fn learn_arm(&mut self) {
        self.learn.arm();
    }

    pub fn learn_disarm(&mut self) {
        self.learn.disarm();
    }

    pub fn learn_armed(&self) -> bool {
        self.learn.is_armed()
    }

    /// Traite un message MIDI brut (callback midir). `now_ms` : horloge
    /// monotone du hub.
    pub fn handle(&mut self, bytes: &[u8], now_ms: u64) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        let Some(msg) = parse_midi(bytes) else {
            trace!(?bytes, "message MIDI ignoré");
            return out;
        };
        // MSC : traité même en mode learn (un GO console reste prioritaire).
        if let MidiMsg::SysEx(frame) = &msg {
            if let Some(cmd) = parse_msc(frame, self.msc_device_id) {
                out.push(EngineEvent::Command(cmd));
            }
            return out;
        }
        for m in self.cc14.feed(msg, now_ms) {
            self.process(&m, now_ms, &mut out);
        }
        out
    }

    /// Tick périodique : fait sortir les MSB 14 bits et les captures learn
    /// dont la fenêtre d'appariement a expiré.
    pub fn flush(&mut self, now_ms: u64) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        for m in self.cc14.flush(now_ms) {
            self.process(&m, now_ms, &mut out);
        }
        if let Some(b) = self.learn.flush(now_ms) {
            out.push(EngineEvent::Learned(b));
        }
        out
    }

    fn process(&mut self, msg: &MidiMsg, now_ms: u64, out: &mut Vec<EngineEvent>) {
        // Learn armé : le message est consommé par la capture.
        if self.learn.is_armed() {
            if let Some(b) = self.learn.feed(msg, now_ms) {
                out.push(EngineEvent::Learned(b));
            }
            return;
        }
        match *msg {
            MidiMsg::ControlChange { channel, cc, value } => {
                self.cc_event(channel, cc, false, f32::from(value) / 127.0, out);
            }
            MidiMsg::ControlChange14 { channel, cc, value } => {
                self.cc_event(channel, cc, true, f32::from(value) / 16383.0, out);
            }
            _ => {
                if let Some((_, cmd)) = resolve(&self.bindings, msg) {
                    out.push(EngineEvent::Command(cmd));
                }
            }
        }
    }

    /// CC apparié : plage, puis soft-takeover si le binding le demande.
    fn cc_event(&mut self, channel: u8, cc: u8, fourteen: bool, t: f32, out: &mut Vec<EngineEvent>) {
        let Some(m) = find_cc(&self.bindings, channel, cc, fourteen) else {
            trace!(channel, cc, "CC sans binding");
            return;
        };
        let value = scale(t, m.min, m.max, Curve::Linear);
        let param_set = |addr: &str, v: f32| {
            EngineEvent::Command(Command::ParamSet {
                addr: addr.to_string(),
                value: ParamValue::F(v),
                source: Source::Midi,
            })
        };
        if !m.pickup {
            // Pas de soft-takeover : on garde quand même le cache cohérent.
            self.pickup.update_logical(m.addr, value);
            out.push(param_set(m.addr, value));
            return;
        }
        let was_engaged = self.pickup.is_engaged(channel, cc);
        let tolerance = (m.max - m.min).abs() / 127.0;
        match self.pickup.filter(channel, cc, m.addr, value, tolerance) {
            PickupDecision::Pass(v) => {
                if !was_engaged {
                    out.push(EngineEvent::PickupEngaged {
                        addr: m.addr.to_string(),
                    });
                }
                out.push(param_set(m.addr, v));
            }
            PickupDecision::Blocked { current, incoming } => {
                out.push(EngineEvent::PickupBlocked {
                    addr: m.addr.to_string(),
                    current,
                    incoming,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use conduite_core::CommandTemplate;

    use super::*;

    fn engine_with(bindings: Vec<MidiBinding>) -> MidiEngine {
        let mut e = MidiEngine::new(MSC_ALL_CALL);
        e.set_bindings(bindings);
        e
    }

    fn opacity_cc(fourteen_bits: bool, pickup: bool) -> MidiBinding {
        MidiBinding::Cc {
            channel: 0,
            cc: 7,
            fourteen_bits,
            addr: "slice/1/opacity".into(),
            min: 0.0,
            max: 1.0,
            pickup,
        }
    }

    fn param_value(ev: &EngineEvent) -> f32 {
        match ev {
            EngineEvent::Command(Command::ParamSet {
                value: ParamValue::F(v),
                ..
            }) => *v,
            other => panic!("attendu ParamSet, obtenu {other:?}"),
        }
    }

    #[test]
    fn note_binding_emits_command_from_raw_bytes() {
        let mut e = engine_with(vec![MidiBinding::Note {
            channel: 0,
            note: 60,
            command: CommandTemplate::Go,
        }]);
        assert_eq!(
            e.handle(&[0x90, 60, 100], 0),
            vec![EngineEvent::Command(Command::CueGo)]
        );
        // Note-off : rien.
        assert_eq!(e.handle(&[0x80, 60, 0], 5), vec![]);
    }

    #[test]
    fn cc_without_pickup_is_direct() {
        let mut e = engine_with(vec![opacity_cc(false, false)]);
        let evs = e.handle(&[0xB0, 7, 127], 0);
        assert_eq!(evs.len(), 1);
        assert!((param_value(&evs[0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pickup_blocks_then_engages_through_engine() {
        let mut e = engine_with(vec![opacity_cc(false, true)]);
        e.update_logical("slice/1/opacity", 0.5);
        // Fader à 25/127 ≈ 0.197 : bloqué.
        let evs = e.handle(&[0xB0, 7, 25], 0);
        assert!(
            matches!(&evs[0], EngineEvent::PickupBlocked { addr, .. } if addr == "slice/1/opacity"),
            "attendu PickupBlocked, obtenu {evs:?}"
        );
        // Monte à 80/127 ≈ 0.63 : croise 0.5 → engagé + valeur émise.
        let evs = e.handle(&[0xB0, 7, 80], 10);
        assert_eq!(evs.len(), 2);
        assert!(
            matches!(&evs[0], EngineEvent::PickupEngaged { addr } if addr == "slice/1/opacity")
        );
        assert!((param_value(&evs[1]) - 80.0 / 127.0).abs() < 1e-6);
        // Mouvement suivant : plus d'événement Engaged, juste la valeur.
        let evs = e.handle(&[0xB0, 7, 90], 20);
        assert_eq!(evs.len(), 1);
        assert!((param_value(&evs[0]) - 90.0 / 127.0).abs() < 1e-6);
    }

    #[test]
    fn fourteen_bit_pipeline_from_raw_bytes() {
        let mut e = engine_with(vec![opacity_cc(true, false)]);
        // MSB seul : en attente d'appariement.
        assert_eq!(e.handle(&[0xB0, 7, 0x40], 0), vec![]);
        // LSB : la paire sort en une seule valeur 14 bits.
        let evs = e.handle(&[0xB0, 39, 0x01], 5);
        assert_eq!(evs.len(), 1);
        let expected = f32::from((0x40u16 << 7) | 0x01) / 16383.0;
        assert!((param_value(&evs[0]) - expected).abs() < 1e-6);
        // MSB orphelin : sort en gros grain au flush.
        assert_eq!(e.handle(&[0xB0, 7, 0x20], 100), vec![]);
        let evs = e.flush(200);
        assert_eq!(evs.len(), 1);
        assert!((param_value(&evs[0]) - f32::from(0x20u16 << 7) / 16383.0).abs() < 1e-6);
    }

    #[test]
    fn msc_frame_goes_through_handle() {
        let mut e = MidiEngine::new(0x01);
        let frame = [0xF0, 0x7F, 0x01, 0x02, 0x7F, 0x01, 0x31, 0x32, 0x2E, 0x35, 0xF7];
        assert_eq!(
            e.handle(&frame, 0),
            vec![EngineEvent::Command(Command::CueGoto {
                cue: conduite_core::CueNumber(12500)
            })]
        );
        // Autre device : ignoré.
        let autre = [0xF0, 0x7F, 0x05, 0x02, 0x7F, 0x01, 0xF7];
        assert_eq!(e.handle(&autre, 0), vec![]);
    }

    #[test]
    fn learn_captures_and_suppresses_resolution() {
        let mut e = engine_with(vec![MidiBinding::Note {
            channel: 0,
            note: 60,
            command: CommandTemplate::Go,
        }]);
        e.learn_arm();
        // La note bindée est capturée par le learn, PAS exécutée.
        let evs = e.handle(&[0x90, 60, 100], 0);
        assert_eq!(
            evs,
            vec![EngineEvent::Learned(MidiBinding::Note {
                channel: 0,
                note: 60,
                command: CommandTemplate::Go
            })]
        );
        assert!(!e.learn_armed());
        // Désarmé : la même note redéclenche sa commande.
        assert_eq!(
            e.handle(&[0x90, 60, 100], 10),
            vec![EngineEvent::Command(Command::CueGo)]
        );
    }

    #[test]
    fn learn_cc_capture_via_flush() {
        let mut e = MidiEngine::new(MSC_ALL_CALL);
        e.learn_arm();
        // CC bas (MSB potentiel) : la capture attend un éventuel LSB.
        assert_eq!(e.handle(&[0xB0, 7, 64], 0), vec![]);
        let evs = e.flush(1000);
        assert_eq!(evs.len(), 1);
        assert!(matches!(
            &evs[0],
            EngineEvent::Learned(MidiBinding::Cc {
                cc: 7,
                fourteen_bits: false,
                ..
            })
        ));
    }

    #[test]
    fn msc_still_works_while_learn_armed() {
        let mut e = MidiEngine::new(MSC_ALL_CALL);
        e.learn_arm();
        let go = [0xF0, 0x7F, 0x7F, 0x02, 0x7F, 0x01, 0xF7];
        assert_eq!(e.handle(&go, 0), vec![EngineEvent::Command(Command::CueGo)]);
        assert!(e.learn_armed(), "le MSC ne consomme pas la capture");
    }
}
