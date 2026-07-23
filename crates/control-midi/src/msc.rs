//! MIDI Show Control (MSC) — logique pure, testée.
//!
//! Trame : `F0 7F <device_id> 02 <command_format> <command> [data] F7`.
//! Sous-ensemble reconnu (console lumière / QLab → Conduite) :
//!
//! | commande | octet | traduction |
//! |---|---|---|
//! | GO     | 0x01 | [`Command::CueGoto`] si numéro présent, sinon [`Command::CueGo`] |
//! | STOP   | 0x02 | [`Command::CuePanic`] (fade 0) |
//! | RESUME | 0x03 | [`Command::CueGo`] |
//! | LOAD   | 0x04 | [`Command::CueStandby`] (numéro requis) |
//!
//! Le numéro de cue est de l'ASCII (`"12.5"`), terminé par `0x00` s'il est
//! suivi d'un numéro de liste (ignoré en v1). Un numéro illisible ne déclenche
//! **rien** : pendant un show, mieux vaut un no-op journalisé qu'une mauvaise
//! cue.

use std::str::FromStr;

use conduite_core::{Command, CueNumber};
use tracing::{debug, warn};

/// Device id « all-call » : accepté par tout le monde.
pub const MSC_ALL_CALL: u8 = 0x7F;

/// Universal Real Time SysEx.
const SYSEX_REAL_TIME: u8 = 0x7F;
/// Sub-ID #1 de MIDI Show Control.
const MSC_SUB_ID: u8 = 0x02;

/// Champ « numéro de cue » d'une trame MSC.
enum CueField {
    /// Pas de données de cue.
    Absent,
    Valid(CueNumber),
    /// Présent mais illisible (on ne devine pas).
    Invalid,
}

/// Analyse une trame SysEx complète (`F0 … F7`). Retourne la commande de
/// conduite correspondante, ou `None` si la trame n'est pas du MSC qui nous
/// concerne (mauvais device, commande inconnue, numéro illisible…).
///
/// `device_id` : identifiant de CE récepteur (0..=111, ou [`MSC_ALL_CALL`]
/// pour accepter tous les messages). Une trame adressée à `0x7F` est toujours
/// acceptée.
pub fn parse_msc(frame: &[u8], device_id: u8) -> Option<Command> {
    // En-tête minimal : F0 7F dev 02 fmt cmd F7 = 7 octets.
    if frame.len() < 7 {
        return None;
    }
    if frame[0] != 0xF0
        || frame[1] != SYSEX_REAL_TIME
        || frame[3] != MSC_SUB_ID
        || frame[frame.len() - 1] != 0xF7
    {
        return None;
    }
    let dev = frame[2];
    if dev != MSC_ALL_CALL && device_id != MSC_ALL_CALL && dev != device_id {
        debug!(dev, device_id, "trame MSC pour un autre device : ignorée");
        return None;
    }
    // frame[4] = command format (lumière, son, tous…) : accepté tel quel.
    let command = frame[5];
    let data = &frame[6..frame.len() - 1];
    let cue = parse_cue_field(data);

    match command {
        // GO
        0x01 => match cue {
            CueField::Absent => Some(Command::CueGo),
            CueField::Valid(cue) => Some(Command::CueGoto { cue }),
            CueField::Invalid => {
                warn!("MSC GO avec numéro de cue illisible : ignoré");
                None
            }
        },
        // STOP → panic immédiat (arrêt de conduite).
        0x02 => Some(Command::CuePanic { fade_s: 0.0 }),
        // RESUME → repart (GO).
        0x03 => Some(Command::CueGo),
        // LOAD → standby (numéro requis).
        0x04 => match cue {
            CueField::Valid(cue) => Some(Command::CueStandby { cue }),
            _ => {
                warn!("MSC LOAD sans numéro de cue valide : ignoré");
                None
            }
        },
        other => {
            debug!(command = other, "commande MSC non gérée");
            None
        }
    }
}

/// Extrait le premier champ de données (numéro de cue ASCII, terminé par
/// `0x00` ou la fin des données). Les champs suivants (liste, chemin) sont
/// ignorés en v1.
fn parse_cue_field(data: &[u8]) -> CueField {
    let field: &[u8] = match data.iter().position(|&b| b == 0x00) {
        Some(i) => &data[..i],
        None => data,
    };
    if field.is_empty() {
        return CueField::Absent;
    }
    match std::str::from_utf8(field)
        .ok()
        .and_then(|s| CueNumber::from_str(s.trim()).ok())
    {
        Some(n) => CueField::Valid(n),
        None => CueField::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trame réelle : GO cue "12.5", device all-call, format all-types.
    /// "12.5" = 0x31 0x32 0x2E 0x35.
    const GO_12_5: [u8; 11] = [
        0xF0, 0x7F, 0x7F, 0x02, 0x7F, 0x01, 0x31, 0x32, 0x2E, 0x35, 0xF7,
    ];

    #[test]
    fn go_with_cue_number() {
        assert_eq!(
            parse_msc(&GO_12_5, MSC_ALL_CALL),
            Some(Command::CueGoto {
                cue: CueNumber(12500)
            })
        );
    }

    #[test]
    fn go_with_cue_and_list() {
        // GO cue "12.5", liste "1" (0x31) séparée par 0x00 — liste ignorée.
        let frame = [
            0xF0, 0x7F, 0x01, 0x02, 0x01, 0x01, 0x31, 0x32, 0x2E, 0x35, 0x00, 0x31, 0xF7,
        ];
        assert_eq!(
            parse_msc(&frame, 0x01),
            Some(Command::CueGoto {
                cue: CueNumber(12500)
            })
        );
    }

    #[test]
    fn go_without_cue_is_plain_go() {
        let frame = [0xF0, 0x7F, 0x7F, 0x02, 0x01, 0x01, 0xF7];
        assert_eq!(parse_msc(&frame, 0x00), Some(Command::CueGo));
    }

    #[test]
    fn stop_resume_load() {
        let stop = [0xF0, 0x7F, 0x7F, 0x02, 0x7F, 0x02, 0xF7];
        assert_eq!(
            parse_msc(&stop, 0x00),
            Some(Command::CuePanic { fade_s: 0.0 })
        );
        let resume = [0xF0, 0x7F, 0x7F, 0x02, 0x7F, 0x03, 0xF7];
        assert_eq!(parse_msc(&resume, 0x00), Some(Command::CueGo));
        // LOAD "3" → standby cue 3.
        let load = [0xF0, 0x7F, 0x7F, 0x02, 0x7F, 0x04, 0x33, 0xF7];
        assert_eq!(
            parse_msc(&load, 0x00),
            Some(Command::CueStandby {
                cue: CueNumber(3000)
            })
        );
        // LOAD sans numéro : refusé.
        let load_vide = [0xF0, 0x7F, 0x7F, 0x02, 0x7F, 0x04, 0xF7];
        assert_eq!(parse_msc(&load_vide, 0x00), None);
    }

    #[test]
    fn device_id_filtering() {
        // Trame adressée au device 0x05.
        let frame = [0xF0, 0x7F, 0x05, 0x02, 0x7F, 0x01, 0xF7];
        assert_eq!(parse_msc(&frame, 0x05), Some(Command::CueGo));
        assert_eq!(parse_msc(&frame, 0x03), None, "autre device : ignoré");
        // Notre id all-call accepte tout.
        assert_eq!(parse_msc(&frame, MSC_ALL_CALL), Some(Command::CueGo));
        // Une trame all-call est acceptée par tout le monde.
        assert!(parse_msc(&GO_12_5, 0x03).is_some());
    }

    #[test]
    fn invalid_frames_are_rejected() {
        // Pas du MSC (sub-id 0x06 = MMC).
        let mmc = [0xF0, 0x7F, 0x7F, 0x06, 0x7F, 0x01, 0xF7];
        assert_eq!(parse_msc(&mmc, MSC_ALL_CALL), None);
        // Pas Universal Real Time.
        let autre = [0xF0, 0x43, 0x7F, 0x02, 0x7F, 0x01, 0xF7];
        assert_eq!(parse_msc(&autre, MSC_ALL_CALL), None);
        // Trop court / sans F7 final.
        assert_eq!(parse_msc(&[0xF0, 0x7F, 0x7F, 0x02, 0xF7], MSC_ALL_CALL), None);
        let sans_fin = [0xF0, 0x7F, 0x7F, 0x02, 0x7F, 0x01, 0x31];
        assert_eq!(parse_msc(&sans_fin, MSC_ALL_CALL), None);
        // Numéro de cue illisible sur GO : no-op (on ne devine pas).
        let go_bad = [0xF0, 0x7F, 0x7F, 0x02, 0x7F, 0x01, 0x41, 0x42, 0xF7]; // "AB"
        assert_eq!(parse_msc(&go_bad, MSC_ALL_CALL), None);
        // Commande inconnue.
        let unknown = [0xF0, 0x7F, 0x7F, 0x02, 0x7F, 0x10, 0xF7];
        assert_eq!(parse_msc(&unknown, MSC_ALL_CALL), None);
    }
}
