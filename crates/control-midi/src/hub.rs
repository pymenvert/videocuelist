// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Connexion matérielle midir : le SEUL module avec de l'IO.
//!
//! [`MidiHub::spawn`] lance un thread superviseur qui :
//! - ouvre le port d'entrée (filtré par nom si demandé) et branche le
//!   callback midir sur le [`MidiEngine`] partagé (parse → resolve →
//!   `tx.try_send`, jamais bloquant) ;
//! - ouvre le port de sortie correspondant pour le feedback (LED, faders
//!   motorisés) ;
//! - re-scanne périodiquement : port disparu → déconnexion signalée, retry
//!   périodique journalisé (un contrôleur rebranché à chaud revit tout seul) ;
//! - tick le moteur (timeouts d'appariement 14 bits et learn).
//!
//! Une erreur MIDI ne fait JAMAIS tomber la régie : tout est journalisé et
//! réessayé.

use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, unbounded, Receiver, RecvTimeoutError, Sender};
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use tracing::{debug, error, info, warn};

use conduite_core::{Command, MidiBinding};

use crate::engine::{EngineEvent, MidiEngine};

/// Période du tick superviseur (flush moteur, drain contrôle).
const TICK: Duration = Duration::from_millis(100);
/// Période de re-scan des ports (détection débranchement / retry connexion).
const SCAN: Duration = Duration::from_millis(1000);
/// Taille du canal d'événements vers l'UI (droppés si plein).
const EVENTS_CAP: usize = 256;

/// Événement du hub vers l'app/UI (learn, pickup, état de connexion).
#[derive(Debug, Clone, PartialEq)]
pub enum HubEvent {
    /// Port d'entrée ouvert.
    Connected { port: String },
    /// Port disparu (contrôleur débranché) — retry en cours.
    Disconnected,
    /// Learn : binding capturé, pré-rempli pour l'UI.
    Learned(MidiBinding),
    /// Soft-takeover : fader pas encore à niveau (indicateur UI).
    PickupBlocked {
        addr: String,
        current: f32,
        incoming: f32,
    },
    /// Soft-takeover : le fader a repris la main.
    PickupEngaged { addr: String },
}

/// Messages de contrôle vers le thread superviseur.
enum Ctl {
    SetBindings(Vec<MidiBinding>),
    UpdateLogical(String, f32),
    LearnArm,
    LearnDisarm,
    /// Feedback note (LED) : canal, note, vélocité (0 = éteint).
    SendNote(u8, u8, u8),
    /// Feedback CC (fader motorisé, anneau d'encodeur).
    SendCc(u8, u8, u8),
    Shutdown,
}

/// Poignée du hub MIDI. La lâcher arrête proprement le thread superviseur.
pub struct MidiHub {
    ctl: Sender<Ctl>,
    events: Receiver<HubEvent>,
    handle: Option<JoinHandle<()>>,
}

impl MidiHub {
    /// Lance le hub. `port_name` : sous-chaîne du port à ouvrir (sinon premier
    /// port non virtuel). `tx` : bus de commandes de l'app (try_send, jamais
    /// bloquant). Ne échoue jamais : sans port au démarrage, le superviseur
    /// réessaie périodiquement.
    pub fn spawn(port_name: Option<String>, tx: Sender<Command>) -> MidiHub {
        let (ctl_tx, ctl_rx) = unbounded::<Ctl>();
        let (events_tx, events_rx) = bounded::<HubEvent>(EVENTS_CAP);
        let engine = Arc::new(Mutex::new(MidiEngine::default()));

        let handle = std::thread::Builder::new()
            .name("conduite-midi-hub".into())
            .spawn(move || {
                supervisor(port_name, engine, tx, ctl_rx, events_tx);
            });
        let handle = match handle {
            Ok(h) => Some(h),
            Err(e) => {
                error!(error = %e, "impossible de lancer le thread MIDI : hub inactif");
                None
            }
        };
        MidiHub {
            ctl: ctl_tx,
            events: events_rx,
            handle,
        }
    }

    /// Énumère les ports MIDI d'entrée présents (page Patch de l'UI).
    pub fn list_ports() -> Vec<String> {
        match MidiInput::new("conduite-scan") {
            Ok(input) => input
                .ports()
                .iter()
                .map(|p| input.port_name(p).unwrap_or_default())
                .collect(),
            Err(e) => {
                warn!(error = %e, "énumération des ports MIDI impossible");
                Vec::new()
            }
        }
    }

    /// Flux d'événements (learn, pickup, connexion). Cloneable.
    pub fn events(&self) -> Receiver<HubEvent> {
        self.events.clone()
    }

    /// Remplace les bindings (chargement de show, édition du patch).
    pub fn set_bindings(&self, bindings: Vec<MidiBinding>) {
        self.send_ctl(Ctl::SetBindings(bindings));
    }

    /// Pousse la valeur logique d'un paramètre (cache soft-takeover).
    pub fn update_logical(&self, addr: impl Into<String>, value: f32) {
        self.send_ctl(Ctl::UpdateLogical(addr.into(), value));
    }

    /// Arme la capture learn du prochain message significatif.
    pub fn learn_arm(&self) {
        self.send_ctl(Ctl::LearnArm);
    }

    pub fn learn_disarm(&self) {
        self.send_ctl(Ctl::LearnDisarm);
    }

    /// Feedback note-on vers la surface (LED). Vélocité 0 = note-off.
    pub fn send_note(&self, channel: u8, note: u8, velocity: u8) {
        self.send_ctl(Ctl::SendNote(channel, note, velocity));
    }

    /// Feedback CC vers la surface (fader motorisé, anneau d'encodeur).
    pub fn send_cc(&self, channel: u8, cc: u8, value: u8) {
        self.send_ctl(Ctl::SendCc(channel, cc, value));
    }

    fn send_ctl(&self, msg: Ctl) {
        if self.ctl.send(msg).is_err() {
            debug!("thread MIDI arrêté : message de contrôle ignoré");
        }
    }
}

impl Drop for MidiHub {
    fn drop(&mut self) {
        let _ = self.ctl.send(Ctl::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Verrouille le moteur en récupérant un éventuel empoisonnement (jamais de
/// panic en régie).
fn lock_engine<'a>(engine: &'a Arc<Mutex<MidiEngine>>) -> MutexGuard<'a, MidiEngine> {
    match engine.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            warn!("mutex du moteur MIDI empoisonné : récupéré");
            poisoned.into_inner()
        }
    }
}

/// Route les sorties du moteur : commandes vers le bus, le reste vers l'UI.
fn dispatch(events: Vec<EngineEvent>, tx: &Sender<Command>, events_tx: &Sender<HubEvent>) {
    for ev in events {
        match ev {
            EngineEvent::Command(cmd) => {
                if tx.try_send(cmd).is_err() {
                    warn!("bus saturé ou arrêté : commande MIDI perdue");
                }
            }
            other => {
                let hub_ev = match other {
                    EngineEvent::Learned(b) => HubEvent::Learned(b),
                    EngineEvent::PickupBlocked {
                        addr,
                        current,
                        incoming,
                    } => HubEvent::PickupBlocked {
                        addr,
                        current,
                        incoming,
                    },
                    EngineEvent::PickupEngaged { addr } => HubEvent::PickupEngaged { addr },
                    EngineEvent::Command(_) => continue, // déjà traité
                };
                if events_tx.try_send(hub_ev).is_err() {
                    debug!("canal d'événements MIDI plein : événement UI droppé");
                }
            }
        }
    }
}

/// Vrai si le nom désigne un port virtuel de bouclage (le « Midi Through »
/// d'ALSA, présent sur tout Linux/Pi). À éviter comme choix par défaut : il
/// est énuméré AVANT les contrôleurs USB mais ne reçoit jamais rien d'un
/// périphérique physique.
fn is_virtual_port(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("midi through") || n.contains("through port")
}

/// Choisit l'index du port à ouvrir parmi `names` :
/// - avec un `filter`, le premier port dont le nom le contient ;
/// - sans filtre, le premier port NON virtuel, et à défaut le premier port.
pub fn choose_port(names: &[String], filter: Option<&str>) -> Option<usize> {
    match filter {
        Some(f) => names.iter().position(|n| n.contains(f)),
        None => names
            .iter()
            .position(|n| !is_virtual_port(n))
            .or(if names.is_empty() { None } else { Some(0) }),
    }
}

/// Boucle superviseur : contrôle, tick moteur, gestion de connexion.
fn supervisor(
    filter: Option<String>,
    engine: Arc<Mutex<MidiEngine>>,
    tx: Sender<Command>,
    ctl_rx: Receiver<Ctl>,
    events_tx: Sender<HubEvent>,
) {
    let epoch = Instant::now();
    let mut input: Option<MidiInputConnection<()>> = None;
    let mut output: Option<MidiOutputConnection> = None;
    let mut port_name: Option<String> = None;
    let mut next_scan = Instant::now();

    info!(filter = ?filter, "hub MIDI démarré");
    'run: loop {
        // 1. Messages de contrôle (avec le tick comme timeout).
        let first = match ctl_rx.recv_timeout(TICK) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break 'run,
        };
        // Draine le reste sans bloquer.
        let mut pending: Vec<Ctl> = first.into_iter().collect();
        while let Ok(msg) = ctl_rx.try_recv() {
            pending.push(msg);
        }
        for msg in pending {
            match msg {
                Ctl::SetBindings(b) => lock_engine(&engine).set_bindings(b),
                Ctl::UpdateLogical(addr, v) => lock_engine(&engine).update_logical(&addr, v),
                Ctl::LearnArm => lock_engine(&engine).learn_arm(),
                Ctl::LearnDisarm => lock_engine(&engine).learn_disarm(),
                Ctl::SendNote(ch, note, vel) => {
                    send_out(&mut output, &[0x90 | (ch & 0x0F), note & 0x7F, vel & 0x7F]);
                }
                Ctl::SendCc(ch, cc, value) => {
                    send_out(&mut output, &[0xB0 | (ch & 0x0F), cc & 0x7F, value & 0x7F]);
                }
                Ctl::Shutdown => break 'run,
            }
        }

        // 2. Tick moteur : timeouts d'appariement (14 bits, learn).
        let now_ms = epoch.elapsed().as_millis() as u64;
        let evs = lock_engine(&engine).flush(now_ms);
        dispatch(evs, &tx, &events_tx);

        // 3. Gestion de connexion (throttlée).
        if Instant::now() >= next_scan {
            next_scan = Instant::now() + SCAN;
            let names = MidiHub::list_ports();
            // Port ouvert mais disparu de l'énumération : déconnexion.
            if input.is_some() {
                let gone = port_name
                    .as_ref()
                    .map(|p| !names.contains(p))
                    .unwrap_or(true);
                if gone {
                    warn!(port = ?port_name, "port MIDI disparu : retry périodique");
                    input = None;
                    output = None;
                    port_name = None;
                    if events_tx.try_send(HubEvent::Disconnected).is_err() {
                        debug!("canal d'événements MIDI plein : Disconnected droppé");
                    }
                }
            }
            if input.is_none() {
                if let Some((conn, name)) =
                    connect_input(filter.as_deref(), &engine, &tx, &events_tx, epoch)
                {
                    output = connect_output(&name, filter.as_deref());
                    if events_tx
                        .try_send(HubEvent::Connected { port: name.clone() })
                        .is_err()
                    {
                        debug!("canal d'événements MIDI plein : Connected droppé");
                    }
                    port_name = Some(name);
                    input = Some(conn);
                }
            }
        }
    }
    info!("hub MIDI arrêté");
}

/// Écrit une trame de feedback sur le port de sortie (s'il existe).
fn send_out(output: &mut Option<MidiOutputConnection>, bytes: &[u8]) {
    let Some(conn) = output.as_mut() else {
        debug!("pas de port MIDI de sortie : feedback ignoré");
        return;
    };
    if let Err(e) = conn.send(bytes) {
        warn!(error = %e, "envoi du feedback MIDI raté");
    }
}

/// Ouvre le port d'entrée choisi et branche le callback → moteur → bus.
fn connect_input(
    filter: Option<&str>,
    engine: &Arc<Mutex<MidiEngine>>,
    tx: &Sender<Command>,
    events_tx: &Sender<HubEvent>,
    epoch: Instant,
) -> Option<(MidiInputConnection<()>, String)> {
    let mut midi_in = match MidiInput::new("conduite-midi") {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "initialisation MIDI impossible");
            return None;
        }
    };
    // Ne rien ignorer : le MSC arrive en SysEx.
    midi_in.ignore(Ignore::None);
    // Énumération et choix sur la MÊME instance : pas de désalignement
    // d'index si un port apparaît/disparaît entre-temps.
    let ports = midi_in.ports();
    let names: Vec<String> = ports
        .iter()
        .map(|p| midi_in.port_name(p).unwrap_or_default())
        .collect();
    let index = match choose_port(&names, filter) {
        Some(i) if i < ports.len() => i,
        _ => {
            debug!(ports = ?names, filter = ?filter, "aucun port MIDI d'entrée à ouvrir");
            return None;
        }
    };
    let port = ports[index].clone();
    let name = names.get(index).cloned().unwrap_or_default();

    let cb_engine = Arc::clone(engine);
    let cb_tx = tx.clone();
    let cb_events = events_tx.clone();
    // Le callback tourne sur le thread MIDI de l'OS : court et sans blocage.
    match midi_in.connect(
        &port,
        "conduite-in",
        move |_timestamp, bytes, _| {
            let now_ms = epoch.elapsed().as_millis() as u64;
            let evs = lock_engine(&cb_engine).handle(bytes, now_ms);
            dispatch(evs, &cb_tx, &cb_events);
        },
        (),
    ) {
        Ok(conn) => {
            info!(port = %name, "port MIDI d'entrée ouvert");
            Some((conn, name))
        }
        Err(e) => {
            warn!(port = %name, error = %e, "connexion au port MIDI impossible");
            None
        }
    }
}

/// Ouvre le port de sortie pour le feedback : même nom que l'entrée si
/// possible, sinon premier port correspondant au filtre. Sans port de sortie,
/// le feedback est simplement ignoré (journalisé).
fn connect_output(input_name: &str, filter: Option<&str>) -> Option<MidiOutputConnection> {
    let midi_out = match MidiOutput::new("conduite-midi-out") {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "initialisation MIDI sortie impossible");
            return None;
        }
    };
    let ports = midi_out.ports();
    let names: Vec<String> = ports
        .iter()
        .map(|p| midi_out.port_name(p).unwrap_or_default())
        .collect();
    // Même périphérique que l'entrée d'abord, sinon le filtre utilisateur.
    let index = names
        .iter()
        .position(|n| n == input_name)
        .or_else(|| choose_port(&names, filter))?;
    let port = ports.get(index)?.clone();
    match midi_out.connect(&port, "conduite-out") {
        Ok(conn) => {
            info!(port = %names[index], "port MIDI de sortie ouvert (feedback)");
            Some(conn)
        }
        Err(e) => {
            warn!(error = %e, "connexion du port MIDI de sortie impossible : feedback désactivé");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_port_skips_midi_through() {
        // Cas Linux/Pi typique : le Through est énuméré en premier.
        let names = vec![
            "Midi Through:Midi Through Port-0 14:0".to_string(),
            "APC mini mk2:APC mini mk2 MIDI 1 20:0".to_string(),
        ];
        assert_eq!(choose_port(&names, None), Some(1), "on saute le Through");
        // Un filtre explicite prime.
        assert_eq!(choose_port(&names, Some("APC")), Some(1));
        assert_eq!(choose_port(&names, Some("introuvable")), None);
        // Si le Through est le SEUL port, on le prend faute de mieux.
        let only = vec!["Midi Through Port-0".to_string()];
        assert_eq!(choose_port(&only, None), Some(0));
        assert_eq!(choose_port(&[], None), None);
    }

    #[test]
    fn list_ports_never_panics() {
        // Sans matériel : liste possiblement vide, mais jamais de panic.
        let _ = MidiHub::list_ports();
    }
}
