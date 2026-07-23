// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Décodage sûr des datagrammes OSC : garde anti-récursion appliquée AVANT
//! le décodage rosc, puis aplatissement des bundles en liste de messages.

use rosc::{OscMessage, OscPacket};

/// Imbrication de bundles OSC maximale acceptée dans un seul datagramme.
/// Un contrôleur légitime (Chataigne, TouchOSC…) reste à 1-2 niveaux ;
/// au-delà c'est une entrée forgée qui vise le débordement de pile.
pub const MAX_BUNDLES: usize = 16;

/// Nombre de bundles (imbriqués ou frères) dans un datagramme brut : un
/// marqueur « #bundle\0 » par bundle. La profondeur d'imbrication est
/// forcément ≤ ce compte, ce qui en fait une borne sûre AVANT le décodage
/// récursif de rosc — qui, lui, n'a aucun garde-fou de profondeur : un
/// datagramme forgé de bundles emboîtés (~20 octets par niveau) ferait
/// déborder la pile du thread serveur (OSC n'est pas authentifié).
pub fn count_bundles(datagram: &[u8]) -> usize {
    datagram.windows(8).filter(|w| *w == b"#bundle\0").count()
}

/// Aplatit les bundles (récursifs) en liste de messages. La récursion est
/// bornée par la garde [`count_bundles`] ≤ [`MAX_BUNDLES`] appliquée par
/// l'appelant avant décodage.
pub fn flatten(packet: OscPacket, out: &mut Vec<OscMessage>) {
    match packet {
        OscPacket::Message(message) => out.push(message),
        OscPacket::Bundle(bundle) => {
            for inner in bundle.content {
                flatten(inner, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rosc::{OscBundle, OscTime, OscType};

    use super::*;

    fn message(addr: &str) -> OscPacket {
        OscPacket::Message(OscMessage {
            addr: addr.into(),
            args: vec![],
        })
    }

    fn time() -> OscTime {
        OscTime {
            seconds: 0,
            fractional: 1,
        }
    }

    #[test]
    fn count_bundles_bounds_nesting() {
        // Un message simple : aucun bundle.
        let bytes = rosc::encoder::encode(&OscPacket::Message(OscMessage {
            addr: "/conduite/master".into(),
            args: vec![OscType::Float(0.5)],
        }))
        .expect("encode message");
        assert_eq!(count_bundles(&bytes), 0);

        // Un datagramme forgé de N bundles emboîtés : le compteur les voit
        // tous, donc la garde MAX_BUNDLES le rejette avant tout décodage
        // récursif (celui qui ferait déborder la pile).
        let mut forged = Vec::new();
        for _ in 0..64 {
            forged.extend_from_slice(b"#bundle\0");
            forged.extend_from_slice(&[0u8; 8]); // timetag
        }
        assert!(count_bundles(&forged) > MAX_BUNDLES);

        // Un bundle légitime peu profond passe la garde.
        let bundle = rosc::encoder::encode(&OscPacket::Bundle(OscBundle {
            timetag: time(),
            content: vec![message("/conduite/cue/go")],
        }))
        .expect("encode bundle");
        assert!(count_bundles(&bundle) <= MAX_BUNDLES);
    }

    #[test]
    fn bundles_are_flattened_recursively() {
        let inner = OscPacket::Bundle(OscBundle {
            timetag: time(),
            content: vec![message("/conduite/cue/go")],
        });
        let outer = OscPacket::Bundle(OscBundle {
            timetag: time(),
            content: vec![inner, message("/conduite/cue/back")],
        });
        let mut messages = Vec::new();
        flatten(outer, &mut messages);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].addr, "/conduite/cue/go");
        assert_eq!(messages[1].addr, "/conduite/cue/back");
    }

    #[test]
    fn flatten_keeps_plain_messages() {
        let mut messages = Vec::new();
        flatten(message("/conduite/bpm/tap"), &mut messages);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].addr, "/conduite/bpm/tap");
    }
}
