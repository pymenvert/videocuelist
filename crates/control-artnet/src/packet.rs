// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Trames Art-Net côté récepteur : parsing ArtDMX/ArtPoll, construction de
//! l'ArtPollReply. Le format de trame est celui de l'émetteur de Lanterne
//! (`crates/artnet`, fn `art_dmx`), inversé ici pour le parsing ; les
//! builders servent aux tests et au loopback.

/// Port UDP standard Art-Net.
pub const ARTNET_PORT: u16 = 6454;

/// En-tête magique de toute trame Art-Net (8 octets, nul final inclus).
pub const ARTNET_ID: &[u8; 8] = b"Art-Net\0";

/// OpCode ArtPoll (découverte, émis par consoles/outils).
pub const OP_POLL: u16 = 0x2000;
/// OpCode ArtPollReply (notre carte d'identité de nœud).
pub const OP_POLL_REPLY: u16 = 0x2100;
/// OpCode ArtDMX (les 512 canaux d'un univers).
pub const OP_DMX: u16 = 0x5000;
/// Version protocole minimale acceptée (Art-Net II à 4 annoncent 14).
pub const PROT_VER: u16 = 14;

/// Taille de l'ArtPollReply émise (spec Art-Net 4, champs récents à zéro).
pub const POLL_REPLY_LEN: usize = 239;

// ---------------------------------------------------------------------------
// Parsing (nous sommes le média-serveur piloté par la console)
// ---------------------------------------------------------------------------

/// Trame ArtDMX reçue. `data` emprunte le buffer d'origine (zéro copie).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtDmx<'a> {
    /// Port-Address 15 bits : Net(7) + Sub-Net(4) + Universe(4).
    pub universe: u16,
    /// 0 = séquençage désactivé, sinon 1..=255 croissant (reboucle à 1).
    pub sequence: u8,
    /// Valeurs des canaux (1..=512 octets, canal 1 = `data[0]`).
    pub data: &'a [u8],
}

/// Parse une trame ArtDMX. `None` = pas une ArtDMX valide (magic, opcode
/// 0x5000 little-endian, ProtVer >= 14, longueur annoncée cohérente).
/// Tolérant sur la longueur impaire (la spec exige pair, on accepte).
pub fn parse_artdmx(packet: &[u8]) -> Option<ArtDmx<'_>> {
    if packet.len() < 18 || &packet[0..8] != ARTNET_ID {
        return None;
    }
    if u16::from_le_bytes([packet[8], packet[9]]) != OP_DMX {
        return None;
    }
    if u16::from_be_bytes([packet[10], packet[11]]) < PROT_VER {
        return None;
    }
    let sequence = packet[12];
    // packet[13] = Physical (informel, ignoré).
    let universe = u16::from_le_bytes([packet[14], packet[15]]) & 0x7FFF;
    let length = usize::from(u16::from_be_bytes([packet[16], packet[17]]));
    if length == 0 || length > 512 || packet.len() < 18 + length {
        return None;
    }
    Some(ArtDmx {
        universe,
        sequence,
        data: &packet[18..18 + length],
    })
}

/// Contenu utile d'un ArtPoll (les deux octets après l'en-tête).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArtPoll {
    /// TalkToMe : bit 1 = répondre aux changements, bit 2 = diagnostics…
    pub flags: u8,
    /// Priorité minimale des diagnostics demandés.
    pub priority: u8,
}

/// Parse un ArtPoll. Tolérant : certains émetteurs omettent flags/priority
/// (défaut 0), mais magic + opcode 0x2000 + ProtVer >= 14 sont exigés.
pub fn parse_artpoll(packet: &[u8]) -> Option<ArtPoll> {
    if packet.len() < 12 || &packet[0..8] != ARTNET_ID {
        return None;
    }
    if u16::from_le_bytes([packet[8], packet[9]]) != OP_POLL {
        return None;
    }
    if u16::from_be_bytes([packet[10], packet[11]]) < PROT_VER {
        return None;
    }
    Some(ArtPoll {
        flags: packet.get(12).copied().unwrap_or(0),
        priority: packet.get(13).copied().unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// Construit l'ArtPollReply du nœud : les champs minimaux corrects pour être
/// listé par les consoles et outils type DMX-Workshop. Un seul « bind » de
/// 4 ports max : seuls les univers de la même page Net/Sub-Net que le premier
/// sont annoncés (limitation v1, documentée).
pub fn build_artpoll_reply(node_name: &str, ip: [u8; 4], universes: &[u16]) -> Vec<u8> {
    let mut p = vec![0u8; POLL_REPLY_LEN];
    p[0..8].copy_from_slice(ARTNET_ID);
    p[8..10].copy_from_slice(&OP_POLL_REPLY.to_le_bytes());
    p[10..14].copy_from_slice(&ip);
    // Port : « low byte first » dans la spec = little-endian.
    p[14..16].copy_from_slice(&ARTNET_PORT.to_le_bytes());
    // VersInfo H/L : version firmware affichée (0.1).
    p[16] = 0;
    p[17] = 1;
    // Net/Sub-Net de la page annoncée (celle du premier univers écouté).
    let first = universes.first().copied().unwrap_or(0) & 0x7FFF;
    p[18] = ((first >> 8) & 0x7F) as u8; // NetSwitch
    p[19] = ((first >> 4) & 0x0F) as u8; // SubSwitch
    // Oem Hi/Lo : 0x00FF = OemUnknown.
    p[20] = 0x00;
    p[21] = 0xFF;
    // p[22] = version UBEA (absent). Status1 : indicateurs normaux,
    // Port-Address programmée par le réseau.
    p[23] = 0xE0;
    // EstaMan Lo/Hi : 0x7FF0 = code constructeur « prototype ».
    p[24] = 0xF0;
    p[25] = 0x7F;
    write_name(&mut p[26..44], node_name); // PortName (court, 17 + nul)
    write_name(&mut p[44..108], node_name); // LongName (63 + nul)
    write_name(&mut p[108..172], "#0001 [0000] conduite OK"); // NodeReport
    // Ports : univers de la même page Net/Sub, 4 max. Chaque port est une
    // « sortie » au sens Art-Net : le nœud consomme l'ArtDMX du réseau.
    let mut count = 0usize;
    for u in universes.iter().map(|u| u & 0x7FFF) {
        if (u >> 4) != (first >> 4) || count == 4 {
            continue;
        }
        p[174 + count] = 0x80; // PortTypes : peut sortir des données du réseau
        p[182 + count] = 0x80; // GoodOutput : données transmises
        p[190 + count] = (u & 0x0F) as u8; // SwOut : 4 bits bas de l'univers
        count += 1;
    }
    p[172] = 0; // NumPortsHi
    p[173] = count as u8; // NumPortsLo (<= 4 par construction)
    // p[200] Style : 0x00 = StNode (le plus large support outils).
    // MAC (201..207) inconnue : zéros.
    p[207..211].copy_from_slice(&ip); // BindIp
    p[211] = 1; // BindIndex (premier et seul bind)
    p[212] = 0x08; // Status2 : Port-Address 15 bits (Art-Net 3+)
    p
}

/// Copie `name` tronqué dans un champ à nul final (frontière UTF-8 respectée).
fn write_name(dst: &mut [u8], name: &str) {
    let max = dst.len().saturating_sub(1);
    let mut end = name.len().min(max);
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    dst[..end].copy_from_slice(&name.as_bytes()[..end]);
    // Le reste est déjà à zéro (champ pré-rempli).
}

/// Construit une trame ArtDMX (côté émetteur — tests, loopback).
/// `universe` = Net(7)+SubUni(8) sur 15 bits, `data` = valeurs des canaux
/// (tronqué à 512, complété à une longueur paire comme l'exige la spec).
// Adapté de Lanterne (pymenvert/toolbox), MIT.
pub fn build_artdmx(universe: u16, sequence: u8, data: &[u8]) -> Vec<u8> {
    let mut channels = data[..data.len().min(512)].to_vec();
    if channels.len() % 2 == 1 {
        channels.push(0);
    }
    let mut packet = Vec::with_capacity(18 + channels.len());
    packet.extend_from_slice(ARTNET_ID);
    packet.extend_from_slice(&OP_DMX.to_le_bytes());
    packet.extend_from_slice(&PROT_VER.to_be_bytes());
    packet.push(sequence); // 0 = désactivé, sinon 1..255
    packet.push(0); // Physical (informel)
    packet.extend_from_slice(&universe.to_le_bytes()); // SubUni puis Net
    #[allow(clippy::cast_possible_truncation)] // borné à 512
    packet.extend_from_slice(&(channels.len() as u16).to_be_bytes());
    packet.extend_from_slice(&channels);
    packet
}

/// Construit un ArtPoll minimal (comme une console qui découvre le réseau).
pub fn build_artpoll() -> Vec<u8> {
    let mut packet = Vec::with_capacity(14);
    packet.extend_from_slice(ARTNET_ID);
    packet.extend_from_slice(&OP_POLL.to_le_bytes());
    packet.extend_from_slice(&PROT_VER.to_be_bytes());
    packet.push(0); // Flags (TalkToMe)
    packet.push(0); // Priority
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le builder reproduit octet par octet la trame de Lanterne (référence).
    #[test]
    fn build_artdmx_is_conformant() {
        let packet = build_artdmx(3, 7, &[10, 20, 30]);
        assert_eq!(&packet[0..8], b"Art-Net\0");
        assert_eq!(u16::from_le_bytes([packet[8], packet[9]]), 0x5000);
        assert_eq!(u16::from_be_bytes([packet[10], packet[11]]), 14);
        assert_eq!(packet[12], 7); // séquence
        assert_eq!(u16::from_le_bytes([packet[14], packet[15]]), 3); // univers
        // Longueur paire : 3 canaux → 4 annoncés.
        assert_eq!(u16::from_be_bytes([packet[16], packet[17]]), 4);
        assert_eq!(&packet[18..22], &[10, 20, 30, 0]);
        assert_eq!(packet.len(), 22);
    }

    #[test]
    fn parse_artdmx_roundtrips_builder() {
        let packet = build_artdmx(0x1234 & 0x7FFF, 42, &[1, 2, 3, 4]);
        let dmx = parse_artdmx(&packet).expect("trame valide");
        assert_eq!(dmx.universe, 0x1234);
        assert_eq!(dmx.sequence, 42);
        assert_eq!(dmx.data, &[1, 2, 3, 4]);
    }

    #[test]
    fn parse_artdmx_full_universe() {
        let data = [7u8; 512];
        let packet = build_artdmx(0, 1, &data);
        let dmx = parse_artdmx(&packet).expect("trame valide");
        assert_eq!(dmx.data.len(), 512);
        assert!(dmx.data.iter().all(|&b| b == 7));
    }

    #[test]
    fn parse_artdmx_rejects_malformed() {
        let good = build_artdmx(0, 1, &[1, 2]);

        // Magic faux.
        let mut bad = good.clone();
        bad[0] = b'X';
        assert_eq!(parse_artdmx(&bad), None);

        // Mauvais opcode (un ArtPoll n'est pas une ArtDMX).
        assert_eq!(parse_artdmx(&build_artpoll()), None);

        // ProtVer trop vieille.
        let mut bad = good.clone();
        bad[10] = 0;
        bad[11] = 13;
        assert_eq!(parse_artdmx(&bad), None);

        // Trame tronquée (longueur annoncée > octets présents).
        let mut bad = good.clone();
        bad.truncate(19);
        assert_eq!(parse_artdmx(&bad), None);

        // Longueur nulle.
        let mut bad = good.clone();
        bad[16] = 0;
        bad[17] = 0;
        assert_eq!(parse_artdmx(&bad), None);

        // Longueur > 512.
        let mut bad = good;
        bad[16] = 0x02;
        bad[17] = 0x02; // 514
        assert_eq!(parse_artdmx(&bad), None);

        // Trop court pour l'en-tête.
        assert_eq!(parse_artdmx(b"Art-Net\0"), None);
    }

    /// Un émetteur non conforme (longueur impaire) est toléré à la réception.
    #[test]
    fn parse_artdmx_tolerates_odd_length() {
        let mut packet = build_artdmx(0, 0, &[9, 8, 7, 6]);
        packet[17] = 3; // longueur annoncée impaire, octets présents
        let dmx = parse_artdmx(&packet).expect("toléré");
        assert_eq!(dmx.data, &[9, 8, 7]);
        assert_eq!(dmx.sequence, 0, "séquence 0 = désactivée");
    }

    #[test]
    fn parse_artpoll_accepts_console_poll() {
        let poll = parse_artpoll(&build_artpoll()).expect("poll valide");
        assert_eq!(poll, ArtPoll::default());
        // Une ArtDMX n'est pas un poll ; un poll tronqué sans flags passe.
        assert_eq!(parse_artpoll(&build_artdmx(0, 1, &[1, 2])), None);
        assert!(parse_artpoll(&build_artpoll()[..12]).is_some());
        assert_eq!(parse_artpoll(&build_artpoll()[..11]), None);
    }

    #[test]
    fn artpoll_reply_is_structurally_correct() {
        // Univers 0x104 : Net 1, Sub 0, Uni 4 — plus un voisin de page 0x105.
        let reply = build_artpoll_reply("Conduite", [192, 168, 1, 50], &[0x104, 0x105]);
        assert_eq!(reply.len(), POLL_REPLY_LEN);
        assert_eq!(&reply[0..8], b"Art-Net\0");
        assert_eq!(u16::from_le_bytes([reply[8], reply[9]]), 0x2100);
        assert_eq!(&reply[10..14], &[192, 168, 1, 50]);
        assert_eq!(u16::from_le_bytes([reply[14], reply[15]]), 6454);
        assert_eq!(reply[18], 1, "NetSwitch");
        assert_eq!(reply[19], 0, "SubSwitch");
        // Nom court à l'offset 26, nul final garanti.
        assert_eq!(&reply[26..34], b"Conduite");
        assert_eq!(reply[34], 0);
        assert_eq!(reply[43], 0, "PortName toujours nul-terminé");
        assert_eq!(reply[107], 0, "LongName toujours nul-terminé");
        // Deux ports annoncés, type « sortie réseau », SwOut = univers bas.
        assert_eq!(reply[172], 0);
        assert_eq!(reply[173], 2, "NumPorts");
        assert_eq!(reply[174], 0x80, "PortTypes[0]");
        assert_eq!(reply[175], 0x80, "PortTypes[1]");
        assert_eq!(reply[182], 0x80, "GoodOutput[0]");
        assert_eq!(reply[190], 4, "SwOut[0]");
        assert_eq!(reply[191], 5, "SwOut[1]");
        assert_eq!(reply[200], 0x00, "Style = StNode");
        assert_eq!(&reply[207..211], &[192, 168, 1, 50], "BindIp");
        assert_eq!(reply[211], 1, "BindIndex");
        assert_eq!(reply[212] & 0x08, 0x08, "Status2 : Port-Address 15 bits");
    }

    /// Un univers d'une autre page Net/Sub n'est pas annoncé (v1 : un bind),
    /// et jamais plus de 4 ports.
    #[test]
    fn artpoll_reply_limits_ports_to_first_page() {
        let reply = build_artpoll_reply("n", [10, 0, 0, 1], &[0, 1, 2, 3, 4, 0x10]);
        assert_eq!(reply[173], 4, "4 ports max");
        assert_eq!(&reply[190..194], &[0, 1, 2, 3]);

        // Aucun univers écouté : réponse valide, zéro port.
        let reply = build_artpoll_reply("n", [10, 0, 0, 1], &[]);
        assert_eq!(reply[173], 0);
    }

    /// Un nom accentué plus long que le champ ne panique pas et reste nul-terminé.
    #[test]
    fn artpoll_reply_truncates_names_at_utf8_boundary() {
        let long = "Régie vidéo éééééééééééééééééééé très longue";
        let reply = build_artpoll_reply(long, [0, 0, 0, 0], &[0]);
        assert_eq!(reply[43], 0, "nul final du nom court");
        let short = &reply[26..43];
        let cut = short.iter().position(|&b| b == 0).unwrap_or(short.len());
        assert!(std::str::from_utf8(&short[..cut]).is_ok(), "UTF-8 valide");
    }
}
