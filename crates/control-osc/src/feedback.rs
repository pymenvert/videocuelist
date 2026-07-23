// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Retour d'état OSC : émet `/conduite/status/*` vers un hôte cible
//! (TouchOSC, Open Stage Control, Companion, Chataigne…).
//!
//! Anti-spam : une adresse n'est réémise que si sa valeur change (casse
//! aussi les boucles de feedback entre deux machines qui se « mirrorent »),
//! et `progress`/`remaining` sont plafonnés à 10 Hz.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use conduite_core::{CueNumber, RuntimeStatus, StateEvent};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use rosc::{OscMessage, OscPacket, OscType};
use tracing::{debug, info, warn};

/// Période de réveil du thread pour vérifier le drapeau d'arrêt.
const POLL_TIMEOUT: Duration = Duration::from_millis(100);

/// Intervalle minimal entre deux émissions de progress/remaining (10 Hz).
const THROTTLE_INTERVAL: Duration = Duration::from_millis(100);

const ACTIVE_ADDR: &str = "/conduite/status/active";
const STANDBY_ADDR: &str = "/conduite/status/standby";
const PROGRESS_ADDR: &str = "/conduite/status/progress";
const REMAINING_ADDR: &str = "/conduite/status/remaining";

/// Événement consommé par le feedback : événement d'état ponctuel, ou
/// instantané périodique de conduite (déjà throttlé ~10 Hz par l'app).
#[derive(Debug, Clone, PartialEq)]
pub enum FeedbackEvent {
    State(StateEvent),
    Status(RuntimeStatus),
}

/// Émetteur de feedback OSC (constructeur uniquement — l'état vit dans le thread).
pub struct OscFeedback;

impl OscFeedback {
    /// Démarre le thread d'émission vers `target`. `Err` si le bind local
    /// échoue : à l'appelant de tracer et de continuer sans feedback.
    pub fn spawn(target: SocketAddr, rx: Receiver<FeedbackEvent>) -> io::Result<OscFeedbackHandle> {
        // Socket d'émission de la même famille d'adresses que la cible.
        let bind = match target {
            SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
        };
        let socket = UdpSocket::bind(bind)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("conduite-osc-out".into())
            .spawn(move || feedback_loop(&socket, target, &rx, &stop_thread))?;
        info!(%target, "retour d'état OSC actif");
        Ok(OscFeedbackHandle {
            stop,
            thread: Some(thread),
        })
    }
}

/// Poignée du feedback : arrêt propre via drapeau + join (aussi au drop).
pub struct OscFeedbackHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OscFeedbackHandle {
    /// Demande l'arrêt et attend la fin du thread (≤ ~100 ms).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                warn!("le thread de feedback OSC s'est terminé en panique");
            }
        }
    }
}

impl Drop for OscFeedbackHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Boucle d'émission : consomme les événements, traduit, envoie en UDP.
fn feedback_loop(
    socket: &UdpSocket,
    target: SocketAddr,
    rx: &Receiver<FeedbackEvent>,
    stop: &AtomicBool,
) {
    let mut state = FeedbackState::default();
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(POLL_TIMEOUT) {
            Ok(event) => {
                for message in state.translate(&event, Instant::now()) {
                    send(socket, target, message);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    info!("retour d'état OSC arrêté");
}

/// Encode et envoie un message ; un hôte éteint n'est jamais fatal.
fn send(socket: &UdpSocket, target: SocketAddr, message: OscMessage) {
    match rosc::encoder::encode(&OscPacket::Message(message)) {
        Ok(bytes) => {
            if let Err(e) = socket.send_to(&bytes, target) {
                debug!(error = %e, "feedback OSC non délivré (hôte éteint ?)");
            }
        }
        Err(e) => warn!(error = ?e, "feedback OSC non encodé"),
    }
}

/// Traducteur PUR événement → messages `/conduite/status/*`, avec anti-spam.
/// L'horloge est passée en paramètre : testable sans attente réelle.
#[derive(Default)]
pub struct FeedbackState {
    /// Derniers arguments émis par adresse (n'émettre que les changements).
    last_sent: HashMap<String, Vec<OscType>>,
    /// Dernière émission de progress / remaining (plafond 10 Hz).
    last_progress: Option<Instant>,
    last_remaining: Option<Instant>,
}

impl FeedbackState {
    /// Traduit un événement en zéro ou plusieurs messages à émettre.
    pub fn translate(&mut self, event: &FeedbackEvent, now: Instant) -> Vec<OscMessage> {
        let mut out = Vec::new();
        match event {
            FeedbackEvent::State(StateEvent::CueChanged { active }) => {
                self.push_if_changed(&mut out, ACTIVE_ADDR, cue_args(*active));
            }
            FeedbackEvent::State(StateEvent::StandbyChanged { standby }) => {
                self.push_if_changed(&mut out, STANDBY_ADDR, cue_args(*standby));
            }
            FeedbackEvent::State(StateEvent::TransitionProgress { progress }) => {
                self.push_progress(&mut out, PROGRESS_ADDR, *progress, now);
            }
            // Les autres événements d'état n'ont pas de retour OSC défini.
            FeedbackEvent::State(_) => {}
            FeedbackEvent::Status(status) => {
                self.push_if_changed(&mut out, ACTIVE_ADDR, cue_args(status.active));
                self.push_if_changed(&mut out, STANDBY_ADDR, cue_args(status.standby));
                self.push_progress(&mut out, PROGRESS_ADDR, status.progress, now);
                self.push_progress(&mut out, REMAINING_ADDR, status.remaining_s, now);
            }
        }
        out
    }

    /// N'émet que si la valeur diffère de la dernière émise pour l'adresse.
    fn push_if_changed(&mut self, out: &mut Vec<OscMessage>, addr: &str, args: Vec<OscType>) {
        if self.last_sent.get(addr) == Some(&args) {
            return; // valeur inchangée : anti-spam / anti-boucle
        }
        self.last_sent.insert(addr.to_string(), args.clone());
        out.push(OscMessage {
            addr: addr.to_string(),
            args,
        });
    }

    /// Comme `push_if_changed`, plafonné à 10 Hz (progress/remaining).
    fn push_progress(&mut self, out: &mut Vec<OscMessage>, addr: &str, value: f32, now: Instant) {
        let last = if addr == PROGRESS_ADDR {
            &mut self.last_progress
        } else {
            &mut self.last_remaining
        };
        if let Some(t) = *last {
            if now.saturating_duration_since(t) < THROTTLE_INTERVAL {
                return; // plafond 10 Hz — la prochaine trame passera
            }
        }
        let args = vec![OscType::Float(value)];
        if self.last_sent.get(addr) == Some(&args) {
            return; // valeur inchangée : ne compte pas comme une émission
        }
        *last = Some(now);
        self.last_sent.insert(addr.to_string(), args.clone());
        out.push(OscMessage {
            addr: addr.to_string(),
            args,
        });
    }
}

/// Argument OSC d'un numéro de cue : "12.5", ou chaîne vide si aucune cue.
fn cue_args(n: Option<CueNumber>) -> Vec<OscType> {
    vec![OscType::String(
        n.map(|c| c.to_string()).unwrap_or_default(),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(active: u32, progress: f32, remaining: f32) -> FeedbackEvent {
        FeedbackEvent::Status(RuntimeStatus {
            active: Some(CueNumber(active)),
            standby: Some(CueNumber(active + 1000)),
            progress,
            remaining_s: remaining,
            ..RuntimeStatus::default()
        })
    }

    #[test]
    fn state_events_translate_to_status_addresses() {
        let mut state = FeedbackState::default();
        let now = Instant::now();

        let out = state.translate(
            &FeedbackEvent::State(StateEvent::CueChanged {
                active: Some(CueNumber(12500)),
            }),
            now,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].addr, "/conduite/status/active");
        assert_eq!(out[0].args, vec![OscType::String("12.5".into())]);

        let out = state.translate(
            &FeedbackEvent::State(StateEvent::StandbyChanged {
                standby: Some(CueNumber(13000)),
            }),
            now,
        );
        assert_eq!(out[0].addr, "/conduite/status/standby");
        assert_eq!(out[0].args, vec![OscType::String("13".into())]);

        // Plateau vide : chaîne vide (TouchOSC affiche un label vide).
        let out = state.translate(
            &FeedbackEvent::State(StateEvent::CueChanged { active: None }),
            now,
        );
        assert_eq!(out[0].args, vec![OscType::String(String::new())]);

        // Les événements sans retour OSC ne produisent rien.
        let out = state.translate(
            &FeedbackEvent::State(StateEvent::BpmChanged { bpm: 120.0 }),
            now,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn unchanged_values_are_not_reemitted() {
        let mut state = FeedbackState::default();
        let now = Instant::now();
        let event = FeedbackEvent::State(StateEvent::CueChanged {
            active: Some(CueNumber(1000)),
        });
        assert_eq!(state.translate(&event, now).len(), 1);
        // Même valeur : silence (anti-spam / anti-boucle de feedback).
        assert!(state.translate(&event, now).is_empty());
        // Valeur différente : réémise.
        let other = FeedbackEvent::State(StateEvent::CueChanged {
            active: Some(CueNumber(2000)),
        });
        assert_eq!(state.translate(&other, now).len(), 1);
    }

    #[test]
    fn progress_is_throttled_to_ten_hz() {
        let mut state = FeedbackState::default();
        let t0 = Instant::now();

        // Première trame : tout sort (active, standby, progress, remaining).
        let out = state.translate(&status(1000, 0.10, 9.0), t0);
        assert_eq!(out.len(), 4);

        // 50 ms plus tard : progress/remaining ont changé mais sont
        // plafonnés à 10 Hz ; active/standby inchangés → silence total.
        let out = state.translate(&status(1000, 0.15, 8.9), t0 + Duration::from_millis(50));
        assert!(out.is_empty(), "attendu silence, obtenu {out:?}");

        // 120 ms plus tard : la fenêtre est passée, progress/remaining sortent.
        let out = state.translate(&status(1000, 0.20, 8.8), t0 + Duration::from_millis(120));
        let addrs: Vec<&str> = out.iter().map(|m| m.addr.as_str()).collect();
        assert_eq!(
            addrs,
            ["/conduite/status/progress", "/conduite/status/remaining"]
        );

        // Fenêtre passée mais valeurs identiques : silence (que les changements).
        let out = state.translate(&status(1000, 0.20, 8.8), t0 + Duration::from_millis(400));
        assert!(out.is_empty());
    }

    #[test]
    fn transition_progress_shares_the_progress_throttle() {
        let mut state = FeedbackState::default();
        let t0 = Instant::now();
        let out = state.translate(
            &FeedbackEvent::State(StateEvent::TransitionProgress { progress: 0.3 }),
            t0,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].addr, "/conduite/status/progress");
        // Une trame de statut 20 ms après : son progress est throttlé.
        let out = state.translate(&status(1000, 0.35, 5.0), t0 + Duration::from_millis(20));
        let addrs: Vec<&str> = out.iter().map(|m| m.addr.as_str()).collect();
        assert!(
            !addrs.contains(&"/conduite/status/progress"),
            "progress aurait dû être throttlé : {addrs:?}"
        );
    }

    /// Round-trip réel : événement → datagramme UDP reçu sur localhost.
    #[test]
    fn feedback_delivers_datagrams_on_localhost() {
        let receiver = UdpSocket::bind("127.0.0.1:0").expect("bind récepteur");
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        let target = receiver.local_addr().expect("addr");

        let (tx, rx) = crossbeam_channel::unbounded();
        let handle = OscFeedback::spawn(target, rx).expect("spawn");

        tx.send(FeedbackEvent::State(StateEvent::CueChanged {
            active: Some(CueNumber(12000)),
        }))
        .expect("send événement");

        let mut buf = [0u8; 1024];
        let (len, _) = receiver.recv_from(&mut buf).expect("datagramme attendu");
        let (_, packet) = rosc::decoder::decode_udp(&buf[..len]).expect("décodage");
        let OscPacket::Message(message) = packet else {
            panic!("message attendu");
        };
        assert_eq!(message.addr, "/conduite/status/active");
        assert_eq!(message.args, vec![OscType::String("12".into())]);
        handle.stop();
    }

    /// La fermeture du canal amont arrête le thread proprement.
    #[test]
    fn feedback_stops_when_channel_closes() {
        let receiver = UdpSocket::bind("127.0.0.1:0").expect("bind récepteur");
        let target = receiver.local_addr().expect("addr");
        let (tx, rx) = crossbeam_channel::unbounded::<FeedbackEvent>();
        let handle = OscFeedback::spawn(target, rx).expect("spawn");
        drop(tx);
        // stop() doit rendre la main sans bloquer même si le thread s'est
        // déjà terminé de lui-même.
        let started = Instant::now();
        handle.stop();
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
