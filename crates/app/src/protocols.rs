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

/// Signature de la configuration réseau des surfaces : un respawn (drop +
/// join de 4 threads, jusqu'à ~100 ms chacun) n'est justifié que si elle
/// change. Tout le reste (bindings MIDI, patch DMX) se pousse à chaud.
#[derive(Debug, Clone, PartialEq)]
struct ProtoSig {
    osc_in_port: u16,
    artnet_enabled: bool,
    artnet_universes: Vec<u16>,
    osc_out: Option<(String, u16)>,
}

fn proto_sig(settings: &ShowSettings, patch: &PatchTable) -> ProtoSig {
    ProtoSig {
        osc_in_port: settings.osc_in_port,
        artnet_enabled: settings.artnet_enabled,
        artnet_universes: settings.artnet_universes.clone(),
        osc_out: patch.osc_out.as_ref().map(|o| (o.host.clone(), o.port)),
    }
}

/// Statut réel de chaque protocole, publié dans `runtime.protocols`
/// (contrat : `"ok" | "inactif" | "erreur: <msg>"`). Le Patch de la webui
/// affiche l'état RÉEL, plus jamais le port configuré d'un bind raté.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolStatus {
    pub osc_in: String,
    pub osc_out: String,
    pub artnet: String,
    pub midi: String,
}

impl Default for ProtocolStatus {
    fn default() -> Self {
        ProtocolStatus {
            osc_in: "inactif".to_string(),
            osc_out: "inactif".to_string(),
            artnet: "inactif".to_string(),
            midi: "inactif".to_string(),
        }
    }
}

/// L'ensemble des surfaces actives. Drop = arrêt propre de tous les threads.
pub struct Protocols {
    cmd_tx: Sender<(Source, Command)>,
    osc_in: Option<OscServerHandle>,
    feedback: Option<(OscFeedbackHandle, Sender<FeedbackEvent>)>,
    midi: Option<MidiHub>,
    artnet: Option<(ArtnetNode, Sender<PatchTable>)>,
    /// Configuration réseau des surfaces vivantes (None avant le 1er spawn).
    sig: Option<ProtoSig>,
    /// Statut réel par protocole (mis à jour au respawn + événements MIDI).
    status: ProtocolStatus,
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
            sig: None,
            status: ProtocolStatus::default(),
        };
        p.respawn(settings, patch);
        p
    }

    /// Statut réel courant des protocoles (contrat `runtime.protocols`).
    pub fn status(&self) -> &ProtocolStatus {
        &self.status
    }

    /// Respawn uniquement si la configuration réseau a changé — évite de
    /// joindre les 4 threads (gel cumulé jusqu'à ~400 ms sur le thread de
    /// session) pour une édition sans rapport, un undo/redo ou le second
    /// appel du boot. Retourne vrai si un respawn a eu lieu ; sinon, à
    /// l'appelant de pousser le patch à chaud (`update_patch`).
    pub fn respawn_if_changed(&mut self, settings: &ShowSettings, patch: &PatchTable) -> bool {
        let sig = proto_sig(settings, patch);
        if self.sig.as_ref() == Some(&sig) {
            debug!(target: "app::protocols", "configuration réseau inchangée : pas de respawn");
            return false;
        }
        self.respawn(settings, patch);
        true
    }

    /// Arrête puis redémarre les surfaces (changement de réglages / de show).
    pub fn respawn(&mut self, settings: &ShowSettings, patch: &PatchTable) {
        self.sig = Some(proto_sig(settings, patch));
        // Drop des anciennes poignées = arrêt propre (drapeaux + join).
        self.osc_in = None;
        self.feedback = None;
        self.midi = None;
        self.artnet = None;

        // --- OSC entrant.
        let bind: SocketAddr = SocketAddr::from(([0, 0, 0, 0], settings.osc_in_port));
        match OscServer::spawn(bind, self.cmd_tx.clone()) {
            Ok(handle) => {
                self.osc_in = Some(handle);
                self.status.osc_in = "ok".to_string();
            }
            Err(e) => {
                warn!(target: "app::protocols", port = settings.osc_in_port, error = %e,
                    "serveur OSC impossible (port occupé ?) — OSC entrant inactif");
                self.status.osc_in =
                    format!("erreur: bind du port {} impossible ({e})", settings.osc_in_port);
            }
        }

        // --- Feedback OSC sortant (si une cible est configurée).
        self.status.osc_out = "inactif".to_string();
        if let Some(cfg) = &patch.osc_out {
            match resolve_host(&cfg.host, cfg.port) {
                Some(target) => {
                    let (fb_tx, fb_rx) = crossbeam_channel::unbounded::<FeedbackEvent>();
                    match OscFeedback::spawn(target, fb_rx) {
                        Ok(handle) => {
                            self.feedback = Some((handle, fb_tx));
                            self.status.osc_out = "ok".to_string();
                        }
                        Err(e) => {
                            warn!(target: "app::protocols", %target, error = %e,
                                "feedback OSC impossible");
                            self.status.osc_out = format!("erreur: {e}");
                        }
                    }
                }
                None => {
                    warn!(target: "app::protocols", host = %cfg.host, port = cfg.port,
                        "hôte de feedback OSC irrésoluble");
                    self.status.osc_out =
                        format!("erreur: hôte {} irrésoluble", cfg.host);
                }
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
        // MIDI : « inactif » tant qu'aucun périphérique n'est connecté ; le
        // statut passe à « ok » / « erreur » au fil des HubEvent (drain).
        self.status.midi = "inactif".to_string();

        // --- Art-Net (si activé).
        self.status.artnet = "inactif".to_string();
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
                    self.status.artnet = "ok".to_string();
                }
                Err(e) => {
                    warn!(target: "app::protocols", error = %e,
                        "nœud Art-Net impossible (port 6454 occupé ?)");
                    self.status.artnet =
                        format!("erreur: bind du port {ARTNET_PORT} impossible ({e})");
                }
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

    /// Draine les événements du hub MIDI vers le canal d'événements UI
    /// (et met à jour le statut réel du protocole MIDI).
    pub fn drain_midi_events(&mut self, events_tx: &broadcast::Sender<Value>) {
        let Some(hub) = &self.midi else { return };
        let rx = hub.events();
        while let Ok(ev) = rx.try_recv() {
            let payload = match ev {
                HubEvent::Connected { port } => {
                    info!(target: "app::protocols", %port, "MIDI connecté");
                    self.status.midi = "ok".to_string();
                    json!({"type": "midi_connected", "port": port})
                }
                HubEvent::Disconnected => {
                    warn!(target: "app::protocols", "MIDI déconnecté (retry en cours)");
                    self.status.midi =
                        "erreur: périphérique déconnecté (reconnexion en cours)".to_string();
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

#[cfg(test)]
mod tests {
    use super::*;
    use conduite_core::OscOutCfg;

    /// Seuls les réglages RÉSEAU justifient un respawn ; un réglage sans
    /// rapport (autosave, mjpeg…) laisse la signature inchangée.
    #[test]
    fn proto_sig_detects_only_network_changes() {
        let s = ShowSettings::default();
        let p = PatchTable::default();
        assert_eq!(proto_sig(&s, &p), proto_sig(&s, &p));

        let mut s2 = s.clone();
        s2.autosave_interval_s = 120.0;
        s2.mjpeg_fps = 4;
        assert_eq!(proto_sig(&s, &p), proto_sig(&s2, &p), "réglage non réseau");

        let mut s3 = s.clone();
        s3.osc_in_port = 9100;
        assert_ne!(proto_sig(&s, &p), proto_sig(&s3, &p), "port OSC");

        let mut s4 = s.clone();
        s4.artnet_enabled = true;
        assert_ne!(proto_sig(&s, &p), proto_sig(&s4, &p), "Art-Net");

        let mut p2 = p.clone();
        p2.osc_out = Some(OscOutCfg { host: "10.0.0.2".into(), port: 9001 });
        assert_ne!(proto_sig(&s, &p), proto_sig(&s, &p2), "cible de feedback");
    }
}
