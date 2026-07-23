//! Le nœud Art-Net : un thread UDP (port 6454 en production) qui écoute les
//! trames de la console lumière, répond aux ArtPoll et publie des
//! [`Command::ParamSet`] via le canal de commandes du moteur.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use conduite_core::{Command, PatchTable, Source};
use crossbeam_channel::{Receiver, Sender};
use tracing::{debug, error, info, warn};

use crate::mapper::{DmxMapper, SequenceTracker};
use crate::packet::{build_artpoll_reply, parse_artdmx, parse_artpoll};

/// Taille de réception : ArtDMX max = 18 + 512 octets, marge comprise.
const RECV_BUF: usize = 1024;
/// Timeout de lecture : borne la latence d'arrêt et de mise à jour du patch.
const READ_TIMEOUT: Duration = Duration::from_millis(100);
/// Au plus une ArtPollReply par source et par seconde : sans limite, le nœud
/// est un réflecteur/amplificateur UDP (~17×) pour un flood d'ArtPoll à
/// adresse source usurpée.
const POLL_REPLY_WINDOW: Duration = Duration::from_secs(1);
/// Sources suivies au maximum par le limiteur — les adresses source UDP sont
/// usurpables à volonté : la table doit rester bornée en mémoire.
const MAX_POLL_SOURCES: usize = 64;

/// Limiteur de débit des ArtPollReply, par adresse source. Logique pure
/// (horloge injectée), mémoire bornée : table pleine ⇒ purge des entrées
/// expirées, et refus si tout est encore « chaud ».
struct PollReplyLimiter {
    last_reply: HashMap<SocketAddr, Instant>,
}

impl PollReplyLimiter {
    fn new() -> Self {
        PollReplyLimiter {
            last_reply: HashMap::new(),
        }
    }

    /// Peut-on répondre à `from` à l'instant `now` ?
    fn allow(&mut self, from: SocketAddr, now: Instant) -> bool {
        if let Some(last) = self.last_reply.get(&from) {
            if now.duration_since(*last) < POLL_REPLY_WINDOW {
                return false;
            }
        } else if self.last_reply.len() >= MAX_POLL_SOURCES {
            self.last_reply
                .retain(|_, t| now.duration_since(*t) < POLL_REPLY_WINDOW);
            if self.last_reply.len() >= MAX_POLL_SOURCES {
                // Flood de sources distinctes : on cesse de répondre plutôt
                // que de grossir — les outils légitimes re-pollent (~2,5 s).
                return false;
            }
        }
        self.last_reply.insert(from, now);
        true
    }
}

/// Throttle de warn du chemin de réception (état local au thread) : au plus
/// une émission par [`POLL_REPLY_WINDOW`], les messages tus sont comptés.
struct WarnThrottle {
    last: Option<Instant>,
    suppressed: u64,
}

impl WarnThrottle {
    fn new() -> Self {
        WarnThrottle {
            last: None,
            suppressed: 0,
        }
    }

    /// `Some(n)` = émission autorisée (`n` messages tus depuis la dernière),
    /// `None` = se taire.
    fn allow(&mut self, now: Instant) -> Option<u64> {
        match self.last {
            Some(last) if now.duration_since(last) < POLL_REPLY_WINDOW => {
                self.suppressed += 1;
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.suppressed))
            }
        }
    }
}

/// Poignée du thread nœud Art-Net. L'arrêt est propre au [`Drop`] (ou via
/// [`ArtnetNode::shutdown`]) : drapeau + join, ≤ ~100 ms.
pub struct ArtnetNode {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    local_addr: SocketAddr,
}

impl ArtnetNode {
    /// Démarre le nœud : socket UDP liée sur `bind` (production :
    /// `0.0.0.0:6454` ; tests : port éphémère), écoute des `universes`
    /// donnés. Les commandes sortent sur `tx` ; le patch initial et ses
    /// mises à jour arrivent par `patch_rx` (la dernière table reçue gagne).
    pub fn spawn(
        bind: SocketAddr,
        node_name: impl Into<String>,
        universes: Vec<u16>,
        tx: Sender<(Source, Command)>,
        patch_rx: Receiver<PatchTable>,
    ) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(bind)?;
        socket.set_read_timeout(Some(READ_TIMEOUT))?;
        let local_addr = socket.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let name = node_name.into();
        let handle = std::thread::Builder::new()
            .name("artnet-node".into())
            .spawn(move || run(&socket, &name, &universes, &tx, &patch_rx, &thread_stop))?;
        info!(%local_addr, "nœud Art-Net démarré");
        Ok(Self {
            stop,
            handle: Some(handle),
            local_addr,
        })
    }

    /// Adresse effectivement liée (utile avec un port éphémère).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Arrête le thread proprement.
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                error!("le thread du nœud Art-Net a paniqué");
            }
        }
    }
}

impl Drop for ArtnetNode {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// Boucle du thread : patch → mapper, trames → commandes, polls → replies.
fn run(
    socket: &UdpSocket,
    name: &str,
    universes: &[u16],
    tx: &Sender<(Source, Command)>,
    patch_rx: &Receiver<PatchTable>,
    stop: &AtomicBool,
) {
    let mut mapper = DmxMapper::default();
    let mut sequences = SequenceTracker::default();
    let mut poll_limiter = PollReplyLimiter::new();
    let mut gap_warns = WarnThrottle::new();
    // IP annoncée dans l'ArtPollReply : celle de la socket (0.0.0.0 si bind
    // générique — suffisant pour être listé, l'outil voit l'IP source UDP).
    let ip = match socket.local_addr() {
        Ok(SocketAddr::V4(a)) => a.ip().octets(),
        _ => [0, 0, 0, 0],
    };
    let mut buf = [0u8; RECV_BUF];
    while !stop.load(Ordering::Relaxed) {
        drain_patch(&mut mapper, patch_rx);
        let (len, from) = match socket.recv_from(&mut buf) {
            Ok(received) => received,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(e) => {
                error!(%e, "réception Art-Net");
                continue;
            }
        };
        // Un patch envoyé avant la trame doit s'appliquer à cette trame.
        drain_patch(&mut mapper, patch_rx);
        let packet = &buf[..len];
        if let Some(dmx) = parse_artdmx(packet) {
            if !universes.contains(&dmx.universe) {
                debug!(universe = dmx.universe, "univers non écouté, trame ignorée");
                continue;
            }
            if let Some(gap) = sequences.observe(dmx.universe, dmx.sequence) {
                // Débit limité : une console hostile à séquence sautante ne
                // doit pas coûter un warn (formatage + broadcast) par trame.
                if let Some(suppressed) = gap_warns.allow(Instant::now()) {
                    warn!(
                        universe = dmx.universe,
                        gap, suppressed, "gros saut de séquence ArtDMX (trames perdues ?)"
                    );
                }
            }
            for cmd in mapper.apply(dmx.universe, dmx.data) {
                if tx.send((Source::ArtNet, cmd)).is_err() {
                    info!("canal de commandes fermé — arrêt du nœud Art-Net");
                    return;
                }
            }
        } else if parse_artpoll(packet).is_some() {
            // Réponse unicast à l'émetteur du poll (les outils type
            // DMX-Workshop l'acceptent ; pas de broadcast nécessaire).
            // Débit limité par source : sans quoi le nœud sert de
            // réflecteur/amplificateur à un flood d'ArtPoll usurpés.
            if poll_limiter.allow(from, Instant::now()) {
                let reply = build_artpoll_reply(name, ip, universes);
                match socket.send_to(&reply, from) {
                    Ok(_) => debug!(%from, "ArtPollReply envoyée"),
                    Err(e) => warn!(%e, %from, "ArtPollReply non envoyée"),
                }
            } else {
                debug!(%from, "ArtPoll ignoré (limite de débit par source)");
            }
        } else {
            debug!(len, %from, "trame Art-Net ignorée (opcode non géré)");
        }
    }
    info!("nœud Art-Net arrêté");
}

/// Draine le canal de patch : seule la dernière table reçue compte.
fn drain_patch(mapper: &mut DmxMapper, patch_rx: &Receiver<PatchTable>) {
    let mut latest = None;
    while let Ok(patch) = patch_rx.try_recv() {
        latest = Some(patch);
    }
    if let Some(patch) = latest {
        info!(entries = patch.artnet.len(), "patch Art-Net mis à jour");
        mapper.set_entries(patch.artnet);
    }
}

#[cfg(test)]
mod tests {
    use conduite_core::{DmxBits, ParamValue, PatchEntry};
    use crossbeam_channel::unbounded;

    use super::*;
    use crate::packet::{build_artdmx, build_artpoll, OP_POLL_REPLY};

    fn localhost_ephemeral() -> SocketAddr {
        "127.0.0.1:0".parse().expect("addr")
    }

    fn test_patch() -> PatchTable {
        PatchTable {
            artnet: vec![
                PatchEntry {
                    universe: 0,
                    channel: 1,
                    bits: DmxBits::Eight,
                    addr: "master/intensity".into(),
                    min: 0.0,
                    max: 1.0,
                    smoothing_ms: 80.0,
                },
                PatchEntry {
                    universe: 0,
                    channel: 2,
                    bits: DmxBits::Sixteen,
                    addr: "slice/1/media/position".into(),
                    min: 0.0,
                    max: 10.0,
                    smoothing_ms: 0.0,
                },
            ],
            ..PatchTable::default()
        }
    }

    /// Bout en bout sur socket réelle (localhost, ports éphémères — jamais
    /// 6454 en test) : patch appliqué, ParamSet émis, anti-spam, univers non
    /// écouté ignoré, mise à jour de patch prise en compte.
    #[test]
    fn node_maps_real_udp_frames_to_commands() {
        let (tx, rx) = unbounded();
        let (patch_tx, patch_rx) = unbounded();
        let node = ArtnetNode::spawn(localhost_ephemeral(), "Conduite test", vec![0], tx, patch_rx)
            .expect("spawn");
        let node_addr = node.local_addr();
        let console = UdpSocket::bind(localhost_ephemeral()).expect("console");

        // Le patch part AVANT la première trame : il doit s'y appliquer.
        patch_tx.send(test_patch()).expect("patch");
        // Canal 1 = 255 ; canaux 2-3 = 0x8000 (position 16 bits mi-course).
        console
            .send_to(&build_artdmx(0, 1, &[255, 0x80, 0x00]), node_addr)
            .expect("send dmx");

        let mut got = Vec::new();
        for _ in 0..2 {
            let (source, cmd) = rx
                .recv_timeout(Duration::from_secs(3))
                .expect("commande attendue");
            assert_eq!(source, Source::ArtNet);
            got.push(cmd);
        }
        match &got[0] {
            Command::ParamSet {
                addr,
                value: ParamValue::F(v),
                source: Source::ArtNet,
            } => {
                assert_eq!(addr, "master/intensity");
                assert!((v - 1.0).abs() < 1e-6);
            }
            other => panic!("attendu ParamSet, obtenu {other:?}"),
        }
        match &got[1] {
            Command::ParamSet {
                addr,
                value: ParamValue::F(v),
                ..
            } => {
                assert_eq!(addr, "slice/1/media/position");
                let want = f32::from(0x8000u16) / 65535.0 * 10.0;
                assert!((v - want).abs() < 1e-4, "{v} != {want}");
            }
            other => panic!("attendu ParamSet, obtenu {other:?}"),
        }

        // Anti-spam : la même trame rejouée (comme une console à 44 Hz)
        // ne produit aucune commande.
        console
            .send_to(&build_artdmx(0, 2, &[255, 0x80, 0x00]), node_addr)
            .expect("send dmx");
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "trame identique : aucune commande"
        );

        // Univers non écouté : ignoré même si un canal change.
        console
            .send_to(&build_artdmx(7, 3, &[0, 0, 0]), node_addr)
            .expect("send dmx");
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "univers 7 non écouté"
        );

        // Un canal change : une seule commande repart.
        console
            .send_to(&build_artdmx(0, 4, &[128, 0x80, 0x00]), node_addr)
            .expect("send dmx");
        let (_, cmd) = rx.recv_timeout(Duration::from_secs(3)).expect("commande");
        match cmd {
            Command::ParamSet { addr, .. } => assert_eq!(addr, "master/intensity"),
            other => panic!("attendu ParamSet, obtenu {other:?}"),
        }

        // Mise à jour du patch en cours de route : nouvelle adresse ciblée.
        let mut updated = test_patch();
        updated.artnet.truncate(1);
        updated.artnet[0].addr = "slice/2/opacity".into();
        patch_tx.send(updated).expect("patch 2");
        console
            .send_to(&build_artdmx(0, 5, &[128]), node_addr)
            .expect("send dmx");
        let (_, cmd) = rx.recv_timeout(Duration::from_secs(3)).expect("commande");
        match cmd {
            Command::ParamSet { addr, .. } => assert_eq!(addr, "slice/2/opacity"),
            other => panic!("attendu ParamSet, obtenu {other:?}"),
        }

        node.shutdown();
    }

    /// Limiteur pur : 1 réponse/s par source, sources indépendantes, purge.
    #[test]
    fn poll_reply_limiter_is_one_per_second_per_source() {
        let mut l = PollReplyLimiter::new();
        let a: SocketAddr = "10.0.0.1:6454".parse().expect("addr");
        let b: SocketAddr = "10.0.0.2:6454".parse().expect("addr");
        let t0 = Instant::now();

        assert!(l.allow(a, t0), "première réponse à A");
        assert!(!l.allow(a, t0 + Duration::from_millis(10)), "A refloodé : silence");
        assert!(!l.allow(a, t0 + Duration::from_millis(900)), "toujours dans la fenêtre");
        assert!(l.allow(b, t0 + Duration::from_millis(20)), "B indépendant de A");
        assert!(l.allow(a, t0 + Duration::from_millis(1_001)), "fenêtre écoulée : A resservi");
    }

    /// Table pleine + sources usurpées toutes distinctes : mémoire bornée et
    /// refus de répondre, puis reprise quand les entrées expirent.
    #[test]
    fn poll_reply_limiter_memory_is_bounded_under_spoofed_flood() {
        let mut l = PollReplyLimiter::new();
        let t0 = Instant::now();
        for i in 0..(MAX_POLL_SOURCES * 4) {
            let from: SocketAddr = format!("10.1.{}.{}:6454", i / 256, i % 256)
                .parse()
                .expect("addr");
            l.allow(from, t0);
        }
        assert!(
            l.last_reply.len() <= MAX_POLL_SOURCES,
            "table non bornée : {} sources",
            l.last_reply.len()
        );
        // Table saturée d'entrées « chaudes » : nouvelle source refusée.
        let fresh: SocketAddr = "10.9.9.9:6454".parse().expect("addr");
        assert!(!l.allow(fresh, t0 + Duration::from_millis(500)));
        // Après expiration de la fenêtre : purge et reprise du service.
        assert!(l.allow(fresh, t0 + Duration::from_millis(1_100)));
        assert!(l.last_reply.len() <= MAX_POLL_SOURCES);
    }

    /// Throttle de warn : premier passe, flood tu et compté, fenêtre rouverte.
    #[test]
    fn warn_throttle_counts_suppressed() {
        let mut w = WarnThrottle::new();
        let t0 = Instant::now();
        assert_eq!(w.allow(t0), Some(0));
        for i in 1..100u64 {
            assert_eq!(w.allow(t0 + Duration::from_millis(i)), None);
        }
        assert_eq!(w.allow(t0 + Duration::from_millis(1_500)), Some(99));
        assert_eq!(w.allow(t0 + Duration::from_millis(3_000)), Some(0));
    }

    /// Le nœud répond à un ArtPoll par une ArtPollReply adressée à l'émetteur.
    #[test]
    fn node_replies_to_artpoll() {
        let (tx, _rx) = unbounded();
        let (_patch_tx, patch_rx) = unbounded();
        let node = ArtnetNode::spawn(
            localhost_ephemeral(),
            "Conduite poll",
            vec![0, 1],
            tx,
            patch_rx,
        )
        .expect("spawn");
        let console = UdpSocket::bind(localhost_ephemeral()).expect("console");
        console
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("timeout");

        console
            .send_to(&build_artpoll(), node.local_addr())
            .expect("send poll");
        let mut buf = [0u8; 512];
        let (len, from) = console.recv_from(&mut buf).expect("reply attendue");
        assert_eq!(from, node.local_addr());
        let reply = &buf[..len];
        assert_eq!(len, crate::packet::POLL_REPLY_LEN);
        assert_eq!(&reply[0..8], b"Art-Net\0");
        assert_eq!(u16::from_le_bytes([reply[8], reply[9]]), OP_POLL_REPLY);
        assert_eq!(&reply[26..39], b"Conduite poll");
        assert_eq!(reply[173], 2, "deux univers écoutés = deux ports");

        // Anti-réflecteur : un second poll de la MÊME source dans la seconde
        // ne reçoit aucune réponse…
        console
            .send_to(&build_artpoll(), node.local_addr())
            .expect("send poll 2");
        console
            .set_read_timeout(Some(Duration::from_millis(300)))
            .expect("timeout court");
        assert!(
            console.recv_from(&mut buf).is_err(),
            "poll refloodé : le nœud ne doit pas répondre dans la fenêtre"
        );

        // … mais une AUTRE source (autre port) est servie immédiatement.
        let console2 = UdpSocket::bind(localhost_ephemeral()).expect("console 2");
        console2
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("timeout");
        console2
            .send_to(&build_artpoll(), node.local_addr())
            .expect("send poll 3");
        let (len2, _) = console2.recv_from(&mut buf).expect("reply pour l'autre source");
        assert_eq!(len2, crate::packet::POLL_REPLY_LEN);

        node.shutdown();
    }
}
