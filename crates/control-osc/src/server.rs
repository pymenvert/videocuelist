// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Serveur OSC entrant : thread std + `UdpSocket` bloquant, arrêt propre via
//! drapeau atomique. Chaque message OSC valide devient un
//! `(Source::Osc, Command)` sur le bus de commandes.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use conduite_core::{Command, Source};
use crossbeam_channel::Sender;
use tracing::{debug, info, warn};

use crate::map::map_message;
use crate::packet::{count_bundles, flatten, MAX_BUNDLES};

/// Période de réveil du thread de réception pour vérifier le drapeau
/// d'arrêt (le recv bloquant est armé de ce timeout).
const POLL_TIMEOUT: Duration = Duration::from_millis(100);

/// Serveur OSC entrant (constructeur uniquement — l'état vit dans le thread).
pub struct OscServer;

impl OscServer {
    /// Bind UDP + démarrage du thread de réception. Retourne une poignée
    /// d'arrêt propre. `Err` si le bind échoue (port occupé…) : à l'appelant
    /// de tracer et de continuer sans OSC — jamais de panic en régie.
    pub fn spawn(bind: SocketAddr, tx: Sender<(Source, Command)>) -> io::Result<OscServerHandle> {
        let socket = UdpSocket::bind(bind)?;
        socket.set_read_timeout(Some(POLL_TIMEOUT))?;
        let local_addr = socket.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("conduite-osc-in".into())
            .spawn(move || receive_loop(&socket, &tx, &stop_thread))?;
        info!(%local_addr, "serveur OSC démarré (UDP)");
        Ok(OscServerHandle {
            stop,
            thread: Some(thread),
            local_addr,
        })
    }
}

/// Poignée du serveur : arrêt propre via drapeau + join (aussi au drop).
pub struct OscServerHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    local_addr: SocketAddr,
}

impl OscServerHandle {
    /// Adresse réellement liée (utile avec un port 0 éphémère).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Demande l'arrêt et attend la fin du thread (≤ ~100 ms).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                warn!("le thread OSC entrant s'est terminé en panique");
            }
        }
    }
}

impl Drop for OscServerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Boucle de réception bloquante (timeout court pour honorer le drapeau).
fn receive_loop(socket: &UdpSocket, tx: &Sender<(Source, Command)>, stop: &AtomicBool) {
    let mut buf = vec![0u8; 64 * 1024];
    while !stop.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                if !handle_datagram(&buf[..len], &from, tx) {
                    break; // bus de commandes fermé : plus personne à servir
                }
            }
            // Timeout de poll (WouldBlock sous Unix, TimedOut sous Windows) :
            // on re-vérifie simplement le drapeau d'arrêt.
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) => {
                // Erreur transitoire (ICMP port unreachable sous Windows…) :
                // on continue d'écouter.
                warn!(error = %e, "erreur de réception OSC");
            }
        }
    }
    info!("serveur OSC arrêté");
}

/// Traite un datagramme. Retourne `false` si le bus de commandes est fermé.
fn handle_datagram(datagram: &[u8], from: &SocketAddr, tx: &Sender<(Source, Command)>) -> bool {
    // Garde anti-débordement de pile : rosc décode les bundles imbriqués par
    // récursion sans limite de profondeur — on borne AVANT le décodage.
    if count_bundles(datagram) > MAX_BUNDLES {
        warn!(%from, "paquet OSC rejeté : bundles trop imbriqués (possible attaque)");
        return true;
    }
    match rosc::decoder::decode_udp(datagram) {
        Ok((_rest, packet)) => {
            let mut messages = Vec::new();
            flatten(packet, &mut messages);
            for message in messages {
                if let Some(command) = map_message(&message.addr, &message.args) {
                    debug!(addr = %message.addr, %from, "OSC → commande");
                    if tx.send((Source::Osc, command)).is_err() {
                        warn!("bus de commandes fermé : arrêt du serveur OSC");
                        return false;
                    }
                }
            }
        }
        Err(e) => warn!(%from, error = ?e, "paquet OSC illisible"),
    }
    true
}

#[cfg(test)]
mod tests {
    use conduite_core::CueNumber;
    use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};

    use super::*;

    const RECV_TIMEOUT: Duration = Duration::from_secs(2);

    fn localhost() -> SocketAddr {
        "127.0.0.1:0".parse().expect("adresse localhost")
    }

    fn encode_message(addr: &str, args: Vec<OscType>) -> Vec<u8> {
        rosc::encoder::encode(&OscPacket::Message(OscMessage {
            addr: addr.into(),
            args,
        }))
        .expect("encode")
    }

    /// Round-trip réel : datagramme UDP localhost → commande sur le canal.
    #[test]
    fn server_roundtrip_on_localhost() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let handle = OscServer::spawn(localhost(), tx).expect("spawn");
        let server_addr = handle.local_addr();

        let sender = UdpSocket::bind(localhost()).expect("bind émetteur");
        sender
            .send_to(
                &encode_message("/conduite/cue/goto", vec![OscType::Float(12.5)]),
                server_addr,
            )
            .expect("send");

        let (source, command) = rx.recv_timeout(RECV_TIMEOUT).expect("commande attendue");
        assert_eq!(source, Source::Osc);
        assert_eq!(
            command,
            Command::CueGoto {
                cue: CueNumber(12500)
            }
        );
        handle.stop();
    }

    /// Un bundle (même imbriqué) livre tous ses messages, dans l'ordre.
    #[test]
    fn server_flattens_bundles_from_the_wire() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let handle = OscServer::spawn(localhost(), tx).expect("spawn");

        let time = OscTime {
            seconds: 0,
            fractional: 1,
        };
        let inner = OscPacket::Bundle(OscBundle {
            timetag: time,
            content: vec![OscPacket::Message(OscMessage {
                addr: "/conduite/cue/go".into(),
                args: vec![],
            })],
        });
        let outer = rosc::encoder::encode(&OscPacket::Bundle(OscBundle {
            timetag: time,
            content: vec![
                inner,
                OscPacket::Message(OscMessage {
                    addr: "/conduite/master".into(),
                    args: vec![OscType::Float(0.5)],
                }),
            ],
        }))
        .expect("encode bundle");

        let sender = UdpSocket::bind(localhost()).expect("bind émetteur");
        sender.send_to(&outer, handle.local_addr()).expect("send");

        let (_, first) = rx.recv_timeout(RECV_TIMEOUT).expect("1er message");
        let (_, second) = rx.recv_timeout(RECV_TIMEOUT).expect("2e message");
        assert_eq!(first, Command::CueGo);
        assert!(matches!(second, Command::ParamSet { ref addr, .. } if addr == "master/intensity"));
        handle.stop();
    }

    /// Datagramme illisible, adresse inconnue ou bundle forgé : ignorés sans
    /// casser le serveur — le message valide suivant passe toujours.
    #[test]
    fn server_survives_garbage_and_forged_bundles() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let handle = OscServer::spawn(localhost(), tx).expect("spawn");
        let server_addr = handle.local_addr();
        let sender = UdpSocket::bind(localhost()).expect("bind émetteur");

        // 1. Bruit binaire.
        sender.send_to(b"pas de l'OSC", server_addr).expect("send");
        // 2. Adresse inconnue.
        sender
            .send_to(&encode_message("/self/destruct", vec![]), server_addr)
            .expect("send");
        // 3. Bundle forgé trop imbriqué (garde anti-récursion).
        let mut forged = Vec::new();
        for _ in 0..64 {
            forged.extend_from_slice(b"#bundle\0");
            forged.extend_from_slice(&[0u8; 8]);
        }
        sender.send_to(&forged, server_addr).expect("send");
        // 4. Message valide : doit toujours passer.
        sender
            .send_to(&encode_message("/conduite/bpm/tap", vec![]), server_addr)
            .expect("send");

        let (_, command) = rx.recv_timeout(RECV_TIMEOUT).expect("commande attendue");
        assert_eq!(command, Command::TapTempo);
        assert!(rx.is_empty(), "les messages invalides n'ont rien produit");
        handle.stop();
    }

    /// stop() rend la main vite et libère le port.
    #[test]
    fn server_stops_cleanly_and_releases_the_port() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let handle = OscServer::spawn(localhost(), tx).expect("spawn");
        let addr = handle.local_addr();
        let started = std::time::Instant::now();
        handle.stop();
        assert!(started.elapsed() < Duration::from_secs(1), "arrêt trop lent");
        // Le port est libéré : on peut re-binder dessus.
        let (tx2, _rx2) = crossbeam_channel::unbounded();
        let handle2 = OscServer::spawn(addr, tx2).expect("re-bind après arrêt");
        handle2.stop();
    }
}
