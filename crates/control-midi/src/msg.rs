// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Décodage des messages MIDI bruts en [`MidiMsg`] — logique pure, testée.

/// Message MIDI décodé (sous-ensemble utile à la régie).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiMsg {
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8 },
    /// Control Change 7 bits (0..127).
    ControlChange { channel: u8, cc: u8, value: u8 },
    /// Control Change 14 bits (0..16383) — produit par [`crate::Cc14Assembler`],
    /// jamais par [`parse_midi`]. `cc` est le numéro MSB (0..=31).
    ControlChange14 { channel: u8, cc: u8, value: u16 },
    /// Pitch bend 14 bits (0..16383, centre 8192).
    PitchBend { channel: u8, value: u16 },
    /// Trame SysEx complète, `F0 … F7` inclus.
    SysEx(Vec<u8>),
}

/// Décode un message MIDI brut. Retourne `None` pour tout ce qui ne nous
/// concerne pas (aftertouch, program change, temps réel, trame tronquée…).
///
/// Un note-on à vélocité 0 est normalisé en [`MidiMsg::NoteOff`] (convention
/// MIDI très répandue chez les contrôleurs).
pub fn parse_midi(bytes: &[u8]) -> Option<MidiMsg> {
    let status = *bytes.first()?;
    if status == 0xF0 {
        // SysEx : midir livre des trames complètes ; on exige F0 … F7.
        if bytes.len() >= 2 && *bytes.last()? == 0xF7 {
            return Some(MidiMsg::SysEx(bytes.to_vec()));
        }
        return None;
    }
    let channel = status & 0x0F;
    match status & 0xF0 {
        0x80 => Some(MidiMsg::NoteOff {
            channel,
            note: *bytes.get(1)? & 0x7F,
        }),
        0x90 => {
            let note = *bytes.get(1)? & 0x7F;
            let velocity = *bytes.get(2)? & 0x7F;
            if velocity == 0 {
                // Note-on vélocité 0 = note-off déguisé.
                Some(MidiMsg::NoteOff { channel, note })
            } else {
                Some(MidiMsg::NoteOn {
                    channel,
                    note,
                    velocity,
                })
            }
        }
        0xB0 => Some(MidiMsg::ControlChange {
            channel,
            cc: *bytes.get(1)? & 0x7F,
            value: *bytes.get(2)? & 0x7F,
        }),
        0xE0 => {
            let lsb = u16::from(*bytes.get(1)? & 0x7F);
            let msb = u16::from(*bytes.get(2)? & 0x7F);
            Some(MidiMsg::PitchBend {
                channel,
                value: (msb << 7) | lsb,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_note_on_off() {
        assert_eq!(
            parse_midi(&[0x90, 60, 100]),
            Some(MidiMsg::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100
            })
        );
        assert_eq!(
            parse_midi(&[0x9A, 61, 1]),
            Some(MidiMsg::NoteOn {
                channel: 10,
                note: 61,
                velocity: 1
            })
        );
        // Note-off explicite et note-on vélocité 0 → NoteOff.
        assert_eq!(
            parse_midi(&[0x80, 60, 64]),
            Some(MidiMsg::NoteOff {
                channel: 0,
                note: 60
            })
        );
        assert_eq!(
            parse_midi(&[0x93, 60, 0]),
            Some(MidiMsg::NoteOff {
                channel: 3,
                note: 60
            })
        );
    }

    #[test]
    fn parse_cc_and_pitch_bend() {
        assert_eq!(
            parse_midi(&[0xB3, 7, 127]),
            Some(MidiMsg::ControlChange {
                channel: 3,
                cc: 7,
                value: 127
            })
        );
        // Pitch bend : LSB puis MSB. Centre = 0x00 0x40 → 8192.
        assert_eq!(
            parse_midi(&[0xE0, 0x00, 0x40]),
            Some(MidiMsg::PitchBend {
                channel: 0,
                value: 8192
            })
        );
        assert_eq!(
            parse_midi(&[0xE5, 0x7F, 0x7F]),
            Some(MidiMsg::PitchBend {
                channel: 5,
                value: 16383
            })
        );
    }

    #[test]
    fn parse_sysex_frames() {
        let frame = [0xF0, 0x7F, 0x01, 0x02, 0x7F, 0x01, 0xF7];
        assert_eq!(parse_midi(&frame), Some(MidiMsg::SysEx(frame.to_vec())));
        // SysEx tronquée (pas de F7) : ignorée.
        assert_eq!(parse_midi(&[0xF0, 0x7F, 0x01]), None);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse_midi(&[]), None);
        assert_eq!(parse_midi(&[0x90, 60]), None); // tronqué
        assert_eq!(parse_midi(&[0xB0]), None); // tronqué
        assert_eq!(parse_midi(&[0xC0, 5]), None); // program change : ignoré
        assert_eq!(parse_midi(&[0xF8]), None); // horloge temps réel
    }
}
