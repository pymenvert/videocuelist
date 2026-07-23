//! Surfaces de contrôle : OSC entrant/sortant, MIDI (learn, MSC, feedback),
//! Art-Net. Toutes alimentent le même bus `(Source, Command)` ; re-spawn
//! propre quand les réglages du show changent.
//!
//! Le serveur HTTP vit dans `main` (port machine, pas un réglage du show).

use std::net::{SocketAddr, ToSocketAddrs as _};

use conduite_control_artnet::{smoothing_overrides, ArtnetNode};
use conduite_control_midi::{HubEvent, MidiHub};
use conduite_control_osc::{FeedbackEvent, OscFeedback, OscFeedbackHandle, OscServer, OscServerHandle};
use conduite_core::{Command, PatchTable, ShowSettings, Source};
use crossbeam_channel::Sender;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

/// Port Art-Net standard.
const ARTNET_PORT: u16 = 6454;

/// L'ensemble des surfaces actives. Drop = arrêt propre de tous les threads.
pub struct Protocols {
    cmd_tx: Sender<(Source, Command)>,
    osc_in: Option<OscServerHandle>,
    feedback: Option<(OscFeedbackHandle, Sender<FeedbackEvent>)>,
    midi: Option<MidiHub>,
    artnet: Option<(ArtnetNode, Sender<PatchTable>)>,
}

impl Protocols {
    /// Démarre toutes les surfaces d'après les réglages du show.
    pub fn spawn(
        cmd_tx: Sender<(Source, Command)>,
        settings: &ShowSettings,
        patch: &PatchTable,
    ) -> Protocols {
        let mut p = Protocols {
            cmd_tx,
            osc_in: None,
            feedback: None,
            midi: None,
            artnet: None,
        };
        p.respawn(settings, patch);
        p
    }

    /// Arrête puis redémarre les surfaces (changement de réglages / de show).
    pub fn respawn(&mut self, settings: &ShowSettings, patch: &PatchTable) {
        // Drop des anciennes poignées = arrêt propre (drapeaux + join).
        self.osc_in = None;
        self.feedback = None;
        self.midi = None;
        self.artnet = None;

        // --- OSC entrant.
        let bind: SocketAddr = SocketAddr::from(([0, 0, 0, 0], settings.osc_in_port));
        match OscServer::spawn(bind, self.cmd_tx.clone()) {
            Ok(handle) => self.osc_in = Some(handle),
            Err(e) => warn!(target: "app::protocols", port = settings.osc_in_port, error = %e,
                "serveur OSC impossible (port occupé ?) — OSC entrant inactif"),
        }

        // --- Feedback OSC sortant (si une cible est configurée).
        if let Some(cfg) = &patch.osc_out {
            match resolve_host(&cfg.host, cfg.port) {
                Some(target) => {
                    let (fb_tx, fb_rx) = crossbeam_channel::unbounded::<FeedbackEvent>();
                    match OscFeedback::spawn(target, fb_rx) {
                        Ok(handle) => self.feedback = Some((handle, fb_tx)),
                        Err(e) => warn!(target: "app::protocols", %target, error = %e,
                            "feedback OSC impossible"),
                    }
                }
                None => warn!(target: "app::protocols", host = %cfg.host, port = cfg.port,
                    "hôte de feedback OSC irrésoluble"),
            }
        }

        // --- MIDI : hub + pont (Source::Midi) vers le bus de commandes.
        let (midi_tx, midi_rx) = crossbeam_channel::unbounded::<Command>();
        let bus = self.cmd_tx.clone();
        let bridge = std::thread::Builder::new()
            .name("conduite-midi-bridge".into())
            .spawn(move || {
                while let Ok(cmd) = midi_rx.recv() {
                    if bus.send((Source::Midi, cmd)).is_err() {
                        break;
                    }
                }
                debug!(target: "app::protocols", "pont MIDI arrêté");
            });
        if let Err(e) = bridge {
            warn!(target: "app::protocols", error = %e, "pont MIDI impossible");
        }
        let hub = MidiHub::spawn(None, midi_tx);
        hub.set_bindings(patch.midi.clone());
        self.midi = Some(hub);

        // --- Art-Net (si activé).
        if settings.artnet_enabled {
            let (patch_tx, patch_rx) = crossbeam_channel::unbounded::<PatchTable>();
            let bind = SocketAddr::from(([0, 0, 0, 0], ARTNET_PORT));
            match ArtnetNode::spawn(
                bind,
                "Conduite",
                settings.artnet_universes.clone(),
                self.cmd_tx.clone(),
                patch_rx,
            ) {
                Ok(node) => {
                    let _ = patch_tx.send(patch.clone());
                    self.artnet = Some((node, patch_tx));
                }
                Err(e) => warn!(target: "app::protocols", error = %e,
                    "nœud Art-Net impossible (port 6454 occupé ?)"),
            }
        }

        info!(target: "app::protocols",
            osc_in = self.osc_in.is_some(),
            osc_out = self.feedback.is_some(),
            artnet = self.artnet.is_some(),
            "surfaces de contrôle (re)démarrées");
    }

    /// Pousse la table de patch à chaud (édition du patch sans respawn) et
    /// retourne les overrides de lissage à appliquer au registre.
    pub fn update_patch(&mut self, patch: &PatchTable) -> Vec<(String, f32)> {
        if let Some(hub) = &self.midi {
            hub.set_bindings(patch.midi.clone());
        }
        if let Some((_, patch_tx)) = &self.artnet {
            let _ = patch_tx.send(patch.clone());
        }
        smoothing_overrides(patch)
    }

    /// Émet un événement de feedback OSC (état ou statut périodique).
    pub fn osc_feedback(&self, event: FeedbackEvent) {
        if let Some((_, tx)) = &self.feedback {
            let _ = tx.send(event);
        }
    }

    /// Arme / désarme la capture MIDI learn.
    pub fn midi_learn(&self, arm: bool) {
        if let Some(hub) = &self.midi {
            if arm {
                hub.learn_arm();
            } else {
                hub.learn_disarm();
            }
        }
    }

    /// Pousse la valeur logique d'un paramètre (soft-takeover MIDI).
    pub fn midi_update_logical(&self, addr: &str, value: f32) {
        if let Some(hub) = &self.midi {
            hub.update_logical(addr, value);
        }
    }

    /// Draine les événements du hub MIDI vers le canal d'événements UI.
    pub fn drain_midi_events(&self, events_tx: &broadcast::Sender<Value>) {
        let Some(hub) = &self.midi else { return };
        let rx = hub.events();
        while let Ok(ev) = rx.try_recv() {
            let payload = match ev {
                HubEvent::Connected { port } => {
                    info!(target: "app::protocols", %port, "MIDI connecté");
                    json!({"type": "midi_connected", "port": port})
                }
                HubEvent::Disconnected => {
                    warn!(target: "app::protocols", "MIDI déconnecté (retry en cours)");
                    json!({"type": "midi_disconnected"})
                }
                HubEvent::Learned(binding) => {
                    info!(target: "app::protocols", "MIDI learn : binding capturé");
                    json!({"type": "midi_learned", "binding": binding})
                }
                HubEvent::PickupBlocked { addr, current, incoming } => {
                    json!({"type": "midi_pickup_blocked", "addr": addr,
                           "current": current, "incoming": incoming})
                }
                HubEvent::PickupEngaged { addr } => {
                    json!({"type": "midi_pickup_engaged", "addr": addr})
                }
            };
            let _ = events_tx.send(payload);
        }
    }
}

/// Résout `host:port` en adresse socket (première trouvée).
fn resolve_host(host: &str, port: u16) -> Option<SocketAddr> {
    (host, port).to_socket_addrs().ok()?.next()
}
