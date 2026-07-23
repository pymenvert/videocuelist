// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Traduction message MIDI → [`Command`] selon les bindings du show —
//! logique pure, testée.
//!
//! Note : [`resolve`] applique la plage `min..max` (inversion permise via
//! `min > max`) avec une courbe linéaire — `core::MidiBinding` ne porte pas
//! de champ courbe en v1. [`scale`] accepte néanmoins toute [`Curve`] pour
//! les appelants qui en ont besoin.

use conduite_core::{Command, Curve, MidiBinding, ParamValue, Source};

use crate::msg::MidiMsg;

/// Champs utiles d'un binding CC apparié.
pub(crate) struct CcMatch<'a> {
    pub addr: &'a str,
    pub min: f32,
    pub max: f32,
    pub pickup: bool,
}

/// Cherche le premier binding CC correspondant (canal, numéro, largeur).
pub(crate) fn find_cc(
    bindings: &[MidiBinding],
    channel: u8,
    cc: u8,
    fourteen: bool,
) -> Option<CcMatch<'_>> {
    bindings.iter().find_map(|b| match b {
        MidiBinding::Cc {
            channel: bc,
            cc: bn,
            fourteen_bits,
            addr,
            min,
            max,
            pickup,
        } if *bc == channel && *bn == cc && *fourteen_bits == fourteen => Some(CcMatch {
            addr,
            min: *min,
            max: *max,
            pickup: *pickup,
        }),
        _ => None,
    })
}

/// Met `t` (0..1) à l'échelle `min..max` après application de la courbe.
/// `min > max` inverse la réponse du fader.
pub fn scale(t: f32, min: f32, max: f32, curve: Curve) -> f32 {
    min + curve.apply(t) * (max - min)
}

/// Cherche le premier binding qui correspond au message et produit la
/// commande associée (source [`Source::Midi`]).
///
/// Ne gère PAS le soft-takeover : c'est [`crate::MidiEngine`] qui insère le
/// filtre [`crate::Pickup`] entre l'appariement et l'émission.
pub fn resolve(bindings: &[MidiBinding], msg: &MidiMsg) -> Option<(Source, Command)> {
    match *msg {
        MidiMsg::NoteOn { channel, note, .. } => bindings.iter().find_map(|b| match b {
            MidiBinding::Note {
                channel: bc,
                note: bn,
                command,
            } if *bc == channel && *bn == note => {
                Some((Source::Midi, command.to_command(Source::Midi)))
            }
            _ => None,
        }),
        MidiMsg::ControlChange { channel, cc, value } => {
            cc_command(bindings, channel, cc, false, f32::from(value) / 127.0)
        }
        MidiMsg::ControlChange14 { channel, cc, value } => {
            cc_command(bindings, channel, cc, true, f32::from(value) / 16383.0)
        }
        // Pitch bend / note-off / SysEx : pas de binding possible en v1
        // (le MSC SysEx est traité à part par `parse_msc`).
        _ => None,
    }
}

fn cc_command(
    bindings: &[MidiBinding],
    channel: u8,
    cc: u8,
    fourteen: bool,
    t: f32,
) -> Option<(Source, Command)> {
    let m = find_cc(bindings, channel, cc, fourteen)?;
    Some((
        Source::Midi,
        Command::ParamSet {
            addr: m.addr.to_string(),
            value: ParamValue::F(scale(t, m.min, m.max, Curve::Linear)),
            source: Source::Midi,
        },
    ))
}

#[cfg(test)]
mod tests {
    use conduite_core::CommandTemplate;

    use super::*;

    fn note_binding(channel: u8, note: u8, command: CommandTemplate) -> MidiBinding {
        MidiBinding::Note {
            channel,
            note,
            command,
        }
    }

    fn cc_binding(cc: u8, fourteen: bool, min: f32, max: f32) -> MidiBinding {
        MidiBinding::Cc {
            channel: 0,
            cc,
            fourteen_bits: fourteen,
            addr: "slice/1/opacity".into(),
            min,
            max,
            pickup: false,
        }
    }

    fn param_f(cmd: &Command) -> f32 {
        match cmd {
            Command::ParamSet {
                value: ParamValue::F(v),
                ..
            } => *v,
            other => panic!("attendu ParamSet(F), obtenu {other:?}"),
        }
    }

    #[test]
    fn note_binding_fires_command() {
        let bindings = vec![
            note_binding(0, 60, CommandTemplate::Go),
            note_binding(0, 62, CommandTemplate::Back),
        ];
        let go = MidiMsg::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        };
        assert_eq!(
            resolve(&bindings, &go),
            Some((Source::Midi, Command::CueGo))
        );
        // Mauvais canal : rien.
        let autre_canal = MidiMsg::NoteOn {
            channel: 1,
            note: 60,
            velocity: 100,
        };
        assert_eq!(resolve(&bindings, &autre_canal), None);
        // Note non bindée : rien.
        let inconnue = MidiMsg::NoteOn {
            channel: 0,
            note: 61,
            velocity: 100,
        };
        assert_eq!(resolve(&bindings, &inconnue), None);
    }

    #[test]
    fn cc_maps_range_min_max() {
        let bindings = vec![cc_binding(7, false, 0.0, 2.0)];
        let at = |value| MidiMsg::ControlChange {
            channel: 0,
            cc: 7,
            value,
        };
        let (src, cmd) = resolve(&bindings, &at(127)).expect("binding");
        assert_eq!(src, Source::Midi);
        assert!((param_f(&cmd) - 2.0).abs() < 1e-6);
        let (_, cmd) = resolve(&bindings, &at(0)).expect("binding");
        assert!(param_f(&cmd).abs() < 1e-6);
        // Milieu ≈ 1.0 (63.5/127 exactement au centre impossible en 7 bits).
        let (_, cmd) = resolve(&bindings, &at(64)).expect("binding");
        assert!((param_f(&cmd) - 2.0 * 64.0 / 127.0).abs() < 1e-6);
    }

    #[test]
    fn cc_inverted_range() {
        // min > max : fader inversé.
        let bindings = vec![cc_binding(7, false, 1.0, 0.0)];
        let (_, cmd) = resolve(
            &bindings,
            &MidiMsg::ControlChange {
                channel: 0,
                cc: 7,
                value: 127,
            },
        )
        .expect("binding");
        assert!(param_f(&cmd).abs() < 1e-6, "à fond = min inversé (0.0)");
    }

    #[test]
    fn cc_width_must_match_binding() {
        let bindings = vec![cc_binding(7, true, 0.0, 1.0)];
        // Binding 14 bits : un CC 7 bits brut ne matche pas…
        assert_eq!(
            resolve(
                &bindings,
                &MidiMsg::ControlChange {
                    channel: 0,
                    cc: 7,
                    value: 127
                }
            ),
            None
        );
        // …mais le message assemblé, oui, avec la précision 14 bits.
        let (_, cmd) = resolve(
            &bindings,
            &MidiMsg::ControlChange14 {
                channel: 0,
                cc: 7,
                value: 16383,
            },
        )
        .expect("binding");
        assert!((param_f(&cmd) - 1.0).abs() < 1e-6);
        let (_, cmd) = resolve(
            &bindings,
            &MidiMsg::ControlChange14 {
                channel: 0,
                cc: 7,
                value: 8192,
            },
        )
        .expect("binding");
        assert!((param_f(&cmd) - 8192.0 / 16383.0).abs() < 1e-6);
    }

    #[test]
    fn scale_applies_curves_and_clamps() {
        assert!((scale(0.5, 0.0, 2.0, Curve::Linear) - 1.0).abs() < 1e-6);
        assert!((scale(0.5, 0.0, 2.0, Curve::EaseIn) - 0.5).abs() < 1e-6); // 0.25 * 2
        assert!((scale(0.5, 0.0, 2.0, Curve::EaseOut) - 1.5).abs() < 1e-6); // 0.75 * 2
        // Clamp hors bornes (Curve::apply clampe t).
        assert!((scale(1.5, 0.0, 2.0, Curve::Linear) - 2.0).abs() < 1e-6);
        assert!(scale(-1.0, 0.0, 2.0, Curve::Linear).abs() < 1e-6);
        // Plage inversée + courbe.
        assert!((scale(1.0, 1.0, 0.0, Curve::SCurve)).abs() < 1e-6);
    }
}
