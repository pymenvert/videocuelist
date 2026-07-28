//! La session : cœur de l'application sur le thread principal.
//!
//! Possède le [`Show`], le registre de paramètres, le moteur de cues, la
//! modulation, les lecteurs média, l'undo et le mode Edit/Show. Un tick :
//! drain des commandes → cue.tick → modulation.tick → params (blend +
//! modulation + lissage) → horloges média + poll → uploads → rendu des
//! sorties → préview → santé (ordre normatif, INTERFACES §app).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use conduite_compositor::{BlendMode, SliceDraw};
use conduite_control_osc::FeedbackEvent;
use conduite_core::{
    AppMode, Command, Content, CoreError, Cue, CueNumber, EditOp, LoadWarning, MaterialId,
    MaterialRef, MediaRef, OutputCfg, OutputId, ParamValue, PatternKind, RuntimeStatus, Show,
    ShowSettings, SliceId, Source, StateEvent,
};
use conduite_cue::{CueEngine, CueEvent, CueFrame, EngineTick, SceneTarget};
use conduite_isf::{IsfInputKind, IsfSources};
use conduite_media_library::ProbeInfo;
use conduite_modulation::{
    spectrum_bins, ModEngine, SPECTRUM_BINS_DEFAULT, SPECTRUM_HIGH_HZ_DEFAULT,
    SPECTRUM_LOW_HZ_DEFAULT,
};
use conduite_params::{ParamKind, ParamSpec, Registry};
use conduite_system::{FpsCounter, SamplerThread};
use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch};
use tracing::{debug, error, info, warn};

use crate::audio::{AudioInput, SpectrumSmoother};
use crate::config::AppConfig;
use crate::dirs::{safe_show_name, Dirs};
use crate::gfx::Gfx;
use crate::players::{Deck, Players};
use crate::preview::{placeholder_jpeg, PreviewJob, PreviewWorker};
use crate::protocols::Protocols;
use crate::saver::{SaveJob, Saver};
use crate::undo::UndoStack;

/// Cadence des trames d'état vers l'UI et le feedback OSC.
const STATE_PERIOD: Duration = Duration::from_millis(100);
/// Cadence du bandeau santé.
const HEALTH_PERIOD: Duration = Duration::from_secs(1);
/// Budget de commandes drainées par tick : un flood réseau (OSC/Art-Net/WS,
/// bus non authentifié) ne doit JAMAIS monopoliser la boucle de rendu — le
/// reste attend le tick suivant, la saturation est journalisée (throttlée).
const CMD_BUDGET_PER_TICK: usize = 256;
/// Débounce du snapshot de récupération (clone + sérialisation sur worker).
const RECOVER_PUSH_PERIOD: Duration = Duration::from_secs(1);
/// Nombre de fichiers `recover-*.json` conservés au démarrage.
const RECOVER_KEEP: usize = 5;

/// Résultat d'un re-scan des médias/matériaux (worker `conduite-rescan`).
struct RescanResult {
    media: Vec<MediaRef>,
    materials: Vec<MaterialRef>,
}

/// Canaux injectés par `main` (partagés avec le serveur HTTP).
pub struct SessionChannels {
    pub cmd_tx: Sender<(Source, Command)>,
    pub cmd_rx: Receiver<(Source, Command)>,
    pub state_tx: watch::Sender<Value>,
    pub events_tx: broadcast::Sender<Value>,
    pub preview_tx: broadcast::Sender<Bytes>,
    pub preview_b_tx: broadcast::Sender<Bytes>,
}

/// Matériau ISF prêt : sources GLSL + inputs → adresses de paramètres.
struct MaterialData {
    sources: IsfSources,
    /// (nom d'uniform, adresse registre).
    inputs: Vec<(String, String)>,
    /// Specs typées des inputs, construites au parse : `rebuild_registry`
    /// ne relit JAMAIS les `.fs` du disque (un drag de coin déclenchait une
    /// relecture de tous les shaders sur le thread de tick).
    typed_specs: Vec<ParamSpec>,
}

/// Adresses pré-formatées d'un slice (zéro allocation par frame).
struct SliceAddrs {
    opacity: String,
    gain_r: String,
    gain_g: String,
    gain_b: String,
    gamma: String,
    speed: String,
    blendmode: String,
}

impl SliceAddrs {
    fn new(id: SliceId) -> Self {
        SliceAddrs {
            opacity: format!("slice/{id}/opacity"),
            gain_r: format!("slice/{id}/gain/r"),
            gain_g: format!("slice/{id}/gain/g"),
            gain_b: format!("slice/{id}/gain/b"),
            gamma: format!("slice/{id}/gamma"),
            speed: format!("slice/{id}/media/speed"),
            blendmode: format!("slice/{id}/blendmode"),
        }
    }
}

/// L'orchestrateur. Voir la doc du module.
pub struct Session {
    dirs: Dirs,
    config: AppConfig,
    show: Show,
    show_name: String,
    mode: AppMode,
    registry: Registry,
    cue: CueEngine,
    modul: ModEngine,
    players: Players,
    undo: UndoStack,
    protocols: Protocols,
    preview: PreviewWorker,

    cmd_tx: Sender<(Source, Command)>,
    cmd_rx: Receiver<(Source, Command)>,
    state_tx: watch::Sender<Value>,
    events_tx: broadcast::Sender<Value>,
    preview_tx: broadcast::Sender<Bytes>,
    preview_b_tx: broadcast::Sender<Bytes>,

    dirty: bool,
    last_edit: Option<Instant>,
    last_save: Instant,
    /// Écritures disque hors tick (show.json, backups, snapshot post-panic).
    saver: Saver,
    /// Une sauvegarde est en vol sur le worker (une seule à la fois).
    save_in_flight: bool,
    /// Génération d'édition : `dirty` ne retombe que si la sauvegarde
    /// terminée correspond au dernier état édité.
    edit_gen: u64,
    /// Le snapshot de récupération doit être re-poussé (débounce 1 s).
    recover_dirty: bool,
    last_recover_push: Instant,

    start: Instant,
    last_tick: Instant,
    frame_index: i64,

    dbo_level: f32,
    dbo_target: f32,
    dbo_fade_s: f32,

    /// Échantillonnage sysinfo sur thread dédié (None = thread refusé par
    /// l'OS : santé machine absente, jamais de sysinfo sur le tick).
    health: Option<SamplerThread>,
    fps: HashMap<OutputId, FpsCounter>,
    last_health: Instant,
    last_state: Instant,
    last_preview: Instant,
    last_preview_b: Instant,

    materials: HashMap<MaterialId, MaterialData>,
    materials_failed: HashSet<MaterialId>,
    /// Matériau actuellement posé sur chaque (slice, deck) du compositor.
    material_bound: HashMap<(SliceId, u8), MaterialId>,
    addr_cache: HashMap<SliceId, SliceAddrs>,
    scratch_uniforms: Vec<(String, ParamValue)>,

    /// Entrée audio réelle (cpal + rustfft) — trame FFT pour la modulation.
    audio: AudioInput,
    /// Lissage d'affichage du spectre publié à l'UI (attack/release).
    fft_smoother: SpectrumSmoother,
    placeholder: Bytes,
    outputs_dirty: bool,
    last_active: Option<CueNumber>,
    last_standby: Option<CueNumber>,
    gl_failed_flagged: bool,

    /// Le Show a muté depuis la dernière sérialisation vers l'UI : la trame
    /// d'état 10 Hz ne re-sérialise le Show QUE dans ce cas (le runtime
    /// léger seul est sérialisé à 10 Hz).
    state_show_dirty: bool,
    /// Re-scan des médias en tâche de fond (résultat par canal).
    rescan_tx: Sender<RescanResult>,
    rescan_rx: Receiver<RescanResult>,
    rescan_in_flight: bool,
    /// Maintenance compositor différée au prochain tick avec GL courant :
    /// détacher tous les matériaux + purger programmes/slices disparus,
    /// puis préchauffer les programmes ISF du show (jamais de compilation
    /// shader au GO).
    comp_sync: bool,
    /// Matériaux à recompiler à chaud (MaterialUpdate).
    comp_reload: Vec<MaterialId>,
    /// Buffers réutilisés des lectures préview asynchrones (program/standby).
    preview_scratch: Vec<u8>,
    preview_scratch_b: Vec<u8>,
    /// Throttle du warn de saturation du bus de commandes.
    last_cmd_warn: Option<Instant>,
    /// Une génération de vignettes est déjà en cours (coalescence).
    thumbs_running: Arc<AtomicBool>,
}

impl Session {
    /// Construit la session : charge le show (avec repli sur les backups en
    /// cas de fichier illisible — le show d'origine n'est JAMAIS écrasé par
    /// la démo), (re)construit tous les moteurs et démarre les surfaces.
    pub fn new(dirs: Dirs, config: AppConfig, show_name: String, ch: SessionChannels) -> Session {
        let loaded = load_show_or_recover(&dirs, &show_name);
        let (show, show_name, warnings) = (loaded.show, loaded.name, loaded.warnings);
        for w in &warnings {
            warn!(target: "app::session", "avertissement de chargement : {w}");
        }
        notice_recover_files(&dirs.shows, &dirs.show_dir(&show_name));
        let audio = AudioInput::new(effective_audio_input(&config, &show.settings));
        let protocols = Protocols::spawn(ch.cmd_tx.clone(), &show.settings, &show.patch);
        let preview = PreviewWorker::spawn(ch.preview_tx.clone(), ch.preview_b_tx.clone());
        let placeholder = placeholder_jpeg(show.settings.mjpeg_width, show.settings.mjpeg_height);
        let health = match SamplerThread::spawn() {
            Ok(t) => Some(t),
            Err(e) => {
                warn!(target: "app::session", error = %e,
                    "thread santé impossible : santé machine indisponible");
                None
            }
        };
        let (rescan_tx, rescan_rx) = crossbeam_channel::bounded::<RescanResult>(1);
        let now = Instant::now();
        let mut session = Session {
            dirs,
            config,
            show,
            show_name,
            mode: AppMode::Edit,
            registry: Registry::new(),
            cue: CueEngine::new(),
            modul: ModEngine::new(),
            players: Players::new(std::path::PathBuf::new()),
            undo: UndoStack::new(),
            protocols,
            preview,
            cmd_tx: ch.cmd_tx,
            cmd_rx: ch.cmd_rx,
            state_tx: ch.state_tx,
            events_tx: ch.events_tx,
            preview_tx: ch.preview_tx,
            preview_b_tx: ch.preview_b_tx,
            dirty: false,
            last_edit: None,
            last_save: now,
            saver: Saver::spawn(),
            save_in_flight: false,
            edit_gen: 0,
            recover_dirty: false,
            last_recover_push: now,
            start: now,
            last_tick: now,
            frame_index: 0,
            dbo_level: 0.0,
            dbo_target: 0.0,
            dbo_fade_s: 0.0,
            health,
            fps: HashMap::new(),
            last_health: now,
            last_state: now,
            last_preview: now,
            last_preview_b: now,
            materials: HashMap::new(),
            materials_failed: HashSet::new(),
            material_bound: HashMap::new(),
            addr_cache: HashMap::new(),
            scratch_uniforms: Vec::with_capacity(32),
            audio,
            fft_smoother: SpectrumSmoother::new(SPECTRUM_BINS_DEFAULT),
            placeholder,
            outputs_dirty: true,
            last_active: None,
            last_standby: None,
            gl_failed_flagged: false,
            state_show_dirty: true,
            rescan_tx,
            rescan_rx,
            rescan_in_flight: false,
            comp_sync: true,
            comp_reload: Vec::new(),
            preview_scratch: Vec::new(),
            preview_scratch_b: Vec::new(),
            last_cmd_warn: None,
            thumbs_running: Arc::new(AtomicBool::new(false)),
        };
        session.players = Players::new(session.dirs.media.clone());
        session.rebuild_all();
        session.spawn_thumbs();
        session.push_recover_snapshot();
        info!(target: "app::session", show = %session.show.name,
            cues = session.show.cues.len(), "session prête");
        session
    }

    // ------------------------------------------------------------ accès app

    pub fn outputs(&self) -> &[OutputCfg] {
        &self.show.outputs
    }

    pub fn target_fps(&self) -> u32 {
        self.config.target_fps
    }

    /// Les sorties ont changé depuis le dernier appel (l'app recrée les
    /// fenêtres).
    pub fn take_outputs_dirty(&mut self) -> bool {
        std::mem::take(&mut self.outputs_dirty)
    }

    /// Fermeture d'une fenêtre de sortie : ignorée en mode Show, désactive
    /// la sortie en mode Edit.
    pub fn on_output_close_requested(&mut self, output: OutputId) {
        if self.mode == AppMode::Show {
            warn!(target: "app::session", output,
                "fermeture de fenêtre ignorée (mode Show verrouillé)");
            return;
        }
        if let Some(cfg) = self.show.outputs.iter().find(|o| o.id == output) {
            let mut cfg = cfg.clone();
            cfg.enabled = false;
            // try_send : le bus est borné, le thread de session ne doit
            // JAMAIS bloquer dessus (il est le seul à le drainer).
            if self
                .cmd_tx
                .try_send((Source::Ui, Command::Edit(EditOp::OutputUpdate { output: cfg })))
                .is_err()
            {
                warn!(target: "app::session", output, "bus saturé : fermeture de sortie perdue");
            }
        }
    }

    // ------------------------------------------------------------------ tick

    /// Un tick complet de la session. `gfx` : sous-système graphique
    /// (headless accepté : uploads et rendus sautés, préview placeholder).
    pub fn tick(&mut self, gfx: &mut Gfx) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().clamp(0.0, 0.25);
        self.last_tick = now;
        let now_s = (now - self.start).as_secs_f64();
        self.frame_index += 1;

        if gfx.failed && !self.gl_failed_flagged {
            self.gl_failed_flagged = true;
            error!(target: "app::session",
                "GL indisponible : mode dégradé headless (UI/OSC/cues actifs)");
        }

        // 1. Drain des commandes — BORNÉ : un flood réseau ne monopolise
        // jamais la boucle de rendu, le reste attend le tick suivant.
        let mut drained = 0usize;
        while drained < CMD_BUDGET_PER_TICK {
            match self.cmd_rx.try_recv() {
                Ok((source, cmd)) => {
                    drained += 1;
                    self.handle_command(source, cmd, now_s);
                }
                Err(_) => break,
            }
        }
        if drained == CMD_BUDGET_PER_TICK && !self.cmd_rx.is_empty() {
            let throttled = self
                .last_cmd_warn
                .map(|t| now.duration_since(t) < Duration::from_secs(1))
                .unwrap_or(false);
            if !throttled {
                self.last_cmd_warn = Some(now);
                warn!(target: "app::session", backlog = self.cmd_rx.len(),
                    budget = CMD_BUDGET_PER_TICK,
                    "bus de commandes saturé : drain plafonné (flood réseau ?)");
            }
        }

        // 1 bis. Résultat d'un re-scan média en tâche de fond.
        if let Ok(res) = self.rescan_rx.try_recv() {
            self.apply_rescan(res);
        }

        // 2. Moteur de cues (l'oracle EOF interroge les players).
        let frame = {
            let players = &self.players;
            let eof = |sid: SliceId| players.media_eof(sid);
            self.cue.tick(EngineTick {
                now_s,
                media_eof: &eof,
            })
        };
        self.process_cue_events(&frame.events);

        // 3. Modulation (trame FFT réelle : cpal + rustfft, vide sans
        // entrée audio active — lecture lock-free de l'ArcSwap).
        let bpm = self.registry.value_f32("bpm").max(1.0);
        let fft = self.audio.latest();
        let offsets = self.modul.tick(now_s, bpm, &fft);

        // 4. Paramètres : blend de cue, offsets de modulation, lissage.
        if let Some((target, alpha)) = &frame.params_target {
            self.registry.blend_toward(target, *alpha);
        }
        self.registry.apply_modulation(&offsets);
        self.registry.tick(dt);

        // 5. DBO (fondu maître d'urgence, indépendant de la conduite).
        self.step_dbo(dt);

        // 5 bis. Multiplicateurs de vitesse live → moteur de cues (compte à
        // rebours AfterMedia). Poussé chaque tick : la vitesse peut aussi
        // changer par blend de cue, pas seulement par ParamSet.
        {
            let cue = &mut self.cue;
            let registry = &self.registry;
            for (sid, addrs) in &self.addr_cache {
                let mult = registry.value_f32(&addrs.speed);
                // Vitesse nulle (média en pause) : mult invalide pour le
                // moteur (warning) — on garde le dernier multiplicateur.
                if mult.is_finite() && mult > 0.0 {
                    cue.set_speed_mult(*sid, mult);
                }
            }
        }

        // 6. Lecteurs : synchro sur les decks, horloges, préchargement.
        let status = self.cue.status();
        self.players.sync(&frame, &self.show, status.transition_active);
        {
            let registry = &self.registry;
            let cache = &self.addr_cache;
            self.players.advance(f64::from(dt), |sid| {
                cache
                    .get(&sid)
                    .map(|a| registry.value_f32(&a.speed))
                    .unwrap_or(1.0)
            });
        }

        // 7. Uploads + rendu des sorties (mode fenêtré uniquement).
        let master = self.registry.value_f32("master/intensity").clamp(0.0, 1.0);
        let master_eff = master * (1.0 - frame.black.clamp(0.0, 1.0));
        if gfx.ready() && gfx.make_root_current() {
            self.apply_compositor_maintenance(gfx);
            if let Some(gl) = gfx.gl.as_mut() {
                let comp = &mut gl.compositor;
                self.players.poll_uploads(&mut |slice, deck, f| {
                    comp.upload_frame(slice, deck_gl(deck), f);
                });
            }
            self.apply_materials(gfx, &frame, now_s, dt);
            let plans = self.build_draws(&frame, None);
            let fps = &mut self.fps;
            let target_fps = self.config.target_fps;
            gfx.render_outputs(&plans, master_eff, self.dbo_level, |output| {
                fps.entry(output)
                    .or_insert_with(|| FpsCounter::new(target_fps as f32))
                    .tick(now_s);
            });
            self.render_previews(gfx, &frame, master_eff, now, &plans);
        } else {
            // Headless : les décodages restent cadencés (frames jetées),
            // l'endpoint MJPEG reste vivant (placeholder).
            self.players.poll_uploads(&mut |_, _, _| {});
            self.headless_previews(now);
        }

        // 8. Santé.
        if now - self.last_health >= HEALTH_PERIOD {
            self.last_health = now;
            self.publish_health();
        }

        // 9. État UI (10 Hz) + feedback OSC + événements de conduite.
        // Seul le runtime (léger) est sérialisé à 10 Hz ; le Show complet
        // n'est re-sérialisé QUE s'il a muté (édition, chargement, rescan) —
        // jamais d'arbre JSON du show entier par période en régime établi.
        self.publish_cue_changes(&status);
        if now - self.last_state >= STATE_PERIOD {
            let state_dt = (now - self.last_state).as_secs_f32().min(1.0);
            self.last_state = now;
            let rt = self.runtime_status(&status);
            self.protocols.osc_feedback(FeedbackEvent::Status(rt.clone()));
            let mut rt_value = serde_json::to_value(&rt).unwrap_or(Value::Null);
            // Devices d'entrée audio (onglet Réglages/Modulation) : liste
            // énumérée par le worker + device réellement ouvert.
            if let Value::Object(map) = &mut rt_value {
                map.insert(
                    "audio_devices".to_string(),
                    json!({
                        "available": self.audio.devices(),
                        "active": self.audio.active_device(),
                    }),
                );
            }
            let fft_value = self.fft_state_value(&fft, state_dt);
            let show_value = if self.state_show_dirty {
                self.state_show_dirty = false;
                Some(serde_json::to_value(&self.show).unwrap_or(Value::Null))
            } else {
                None
            };
            self.state_tx.send_modify(|v| {
                if let Some(sv) = show_value {
                    v["show"] = sv;
                }
                v["runtime"] = rt_value;
                // CONTRAT WS : `fft` absent de la trame dyn quand aucune
                // entrée audio n'est active.
                match fft_value {
                    Some(f) => v["fft"] = f,
                    None => {
                        if let Some(obj) = v.as_object_mut() {
                            obj.remove("fft");
                        }
                    }
                }
            });
        }
        self.protocols.drain_midi_events(&self.events_tx);

        // 10. Autosave (débounce après édition + périodique si dirty).
        self.autosave(now);
    }

    // ------------------------------------------------------------- commandes

    fn handle_command(&mut self, source: Source, cmd: Command, now_s: f64) {
        match cmd {
            // P2-2 : génération du zip de diagnostic (implémentation à venir
            // dans ce lot — le refus est propre en attendant).
            Command::DiagnosticReport => {
                warn!(target: "app::session", "rapport de diagnostic : pas encore disponible");
            }
            Command::ParamSet { addr, value, source } => self.param_set(&addr, value, source),
            Command::ParamNudge { addr, delta, source } => {
                // Base du nudge : la CIBLE posée, hors modulation et hors
                // lissage — nudger la valeur lue cuirait l'offset LFO dans
                // la base et avalerait les crans rapides d'encodeur.
                let next = match self.registry.target(&addr) {
                    Some(ParamValue::F(x)) => Some(ParamValue::F(x + delta)),
                    Some(ParamValue::I(i)) => Some(ParamValue::I(i + delta.round() as i64)),
                    Some(_) => {
                        warn!(target: "app::session", %addr, "nudge sur un paramètre non scalaire");
                        None
                    }
                    None => None,
                };
                if let Some(v) = next {
                    self.param_set(&addr, v, source);
                }
            }
            Command::CueGo => self.cue.go(),
            Command::CueBack => self.cue.back(),
            Command::CueGoto { cue } => self.cue.goto(cue),
            Command::CueStandby { cue } => self.cue.standby(cue),
            Command::CuePanic { fade_s } => self.cue.panic(fade_s.max(0.0)),
            Command::Dbo { fade_s } => {
                self.dbo_target = 1.0;
                self.dbo_fade_s = fade_s.max(0.0);
                self.publish_event(&StateEvent::DboChanged { active: true });
                warn!(target: "app::session", fade_s, "DBO engagé");
            }
            Command::DboRelease => {
                self.dbo_target = 0.0;
                self.publish_event(&StateEvent::DboChanged { active: false });
                info!(target: "app::session", "DBO relâché");
            }
            Command::TapTempo => {
                if let Some(bpm) = self.modul.tap(now_s) {
                    self.registry.set("bpm", ParamValue::F(bpm), source);
                    self.publish_event(&StateEvent::BpmChanged { bpm });
                }
            }
            Command::BpmSet { bpm } => {
                self.registry.set("bpm", ParamValue::F(bpm), source);
                self.publish_event(&StateEvent::BpmChanged {
                    bpm: self.registry.value_f32("bpm"),
                });
            }
            Command::Edit(op) => self.apply_edit(op),
            Command::Undo => self.do_undo(),
            Command::Redo => self.do_redo(),
            Command::MidiLearnStart => self.protocols.midi_learn(true),
            Command::MidiLearnCancel => self.protocols.midi_learn(false),
            Command::ShowSave => self.save_show(),
            Command::ShowSaveAs { name } => {
                let name = safe_show_name(&name);
                self.show_name = name.clone();
                self.save_show();
                self.config.last_show = name;
                self.config.save(&self.dirs.base);
            }
            Command::ShowLoad { name } => self.load_show(&safe_show_name(&name)),
            Command::ShowNew => {
                if self.mode == AppMode::Show {
                    warn!(target: "app::session", "ShowNew refusé en mode Show");
                    return;
                }
                let show = Show::new("Nouveau show");
                self.install_show(show, "nouveau".to_string());
            }
            Command::MediaRescan => self.media_rescan(),
            Command::ShowCollect => self.show_collect(),
            Command::ModeSet { mode } => {
                self.mode = mode;
                if mode == AppMode::Show {
                    // Filet de sécurité : tous les programmes ISF du show
                    // sont préchauffés avant la représentation (jamais de
                    // glCompileShader au GO).
                    self.comp_sync = true;
                }
                info!(target: "app::session", ?mode, "mode changé");
                self.publish_event(&StateEvent::ModeChanged { mode });
            }
        }
    }

    fn param_set(&mut self, addr: &str, value: ParamValue, source: Source) {
        // Adresse inconnue du registre (OSC/WS forgé ou périmé) : sortie
        // IMMÉDIATE — ne jamais alimenter le soft-takeover MIDI ni le
        // feedback avec des adresses arbitraires (épuisement mémoire de
        // `Pickup.logical` sous flood réseau). Warn throttlé côté surfaces.
        if !self.registry.contains(addr) {
            debug!(target: "app::session", %addr, ?source, "param_set sur une adresse inconnue");
            return;
        }
        self.registry.set(addr, value, source);
        let live = self.registry.value_f32(addr);
        self.protocols.midi_update_logical(addr, live);
        match addr {
            "bpm" => self.publish_event(&StateEvent::BpmChanged { bpm: live }),
            "master/intensity" => self.publish_event(&StateEvent::MasterChanged { value: live }),
            _ => {}
        }
        self.apply_mod_param(addr, live);
    }

    /// `mod/{id}/freq` et `mod/{id}/depth` pilotent le moteur de modulation
    /// à chaud (la phase des LFO est préservée par `ModEngine::load`).
    fn apply_mod_param(&mut self, addr: &str, value: f32) {
        let Some(rest) = addr.strip_prefix("mod/") else { return };
        let Some((id_str, which)) = rest.split_once('/') else { return };
        let Ok(id) = id_str.parse::<u32>() else { return };
        match which {
            "freq" => {
                let mut changed = false;
                for m in &mut self.show.modulators {
                    if m.id == id {
                        if let conduite_core::ModKind::Lfo { freq, .. } = &mut m.kind {
                            *freq = conduite_core::Freq::Hz(value.max(0.0));
                            changed = true;
                        }
                    }
                }
                if changed {
                    self.modul.load(&self.show.modulators, &self.show.routes);
                    self.state_show_dirty = true;
                }
            }
            "depth" => {
                let routes: Vec<u32> = self
                    .show
                    .routes
                    .iter()
                    .filter(|r| r.source == id)
                    .map(|r| r.id)
                    .collect();
                for rid in routes {
                    self.modul.set_route_depth(rid, value);
                }
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------- édition

    fn apply_edit(&mut self, op: EditOp) {
        if self.mode == AppMode::Show {
            warn!(target: "app::session", "édition refusée : mode Show verrouillé");
            return;
        }
        // Les réglages d'avant l'op : `after_model_change` ne respawne les
        // surfaces réseau que si la configuration réseau a réellement changé.
        let prev_settings = self.show.settings.clone();
        self.undo.push(self.show.clone());
        op.apply(&mut self.show);
        self.after_model_change(&op, &prev_settings);
        self.mark_dirty();
        self.publish_event(&StateEvent::EditApplied { op });
    }

    fn do_undo(&mut self) {
        if self.mode == AppMode::Show {
            warn!(target: "app::session", "undo refusé : mode Show verrouillé");
            return;
        }
        match self.undo.undo(&self.show) {
            Some(prev) => {
                self.show = prev;
                self.rebuild_all();
                self.mark_dirty();
                self.publish_event(&StateEvent::ShowLoaded {
                    name: self.show.name.clone(),
                });
                info!(target: "app::session", "undo appliqué");
            }
            None => debug!(target: "app::session", "undo : pile vide"),
        }
    }

    fn do_redo(&mut self) {
        if self.mode == AppMode::Show {
            warn!(target: "app::session", "redo refusé : mode Show verrouillé");
            return;
        }
        match self.undo.redo(&self.show) {
            Some(next) => {
                self.show = next;
                self.rebuild_all();
                self.mark_dirty();
                self.publish_event(&StateEvent::ShowLoaded {
                    name: self.show.name.clone(),
                });
                info!(target: "app::session", "redo appliqué");
            }
            None => debug!(target: "app::session", "redo : pile vide"),
        }
    }

    /// Reconstructions ciblées après une mutation du modèle.
    fn after_model_change(&mut self, op: &EditOp, prev_settings: &ShowSettings) {
        use EditOp::*;
        match op {
            CueAdd { .. } | CueRemove { .. } | CueUpdate { .. } | CueUpdateState { .. } => {
                self.reload_cues_preserving_position();
            }
            SliceAdd { .. } | SliceUpdate { .. } | CornerSet { .. } => {
                self.rebuild_registry();
            }
            SliceRemove { .. } => {
                self.rebuild_registry();
                // Textures/FBO du slice disparu à libérer côté GPU.
                self.comp_sync = true;
            }
            OutputAdd { .. } | OutputRemove { .. } | OutputUpdate { .. } => {
                self.outputs_dirty = true;
            }
            MediaAdd { .. } | MediaRemove { .. } | MediaUpdate { .. } => {
                self.players.clear();
            }
            MaterialUpdate { material } => {
                self.load_materials();
                self.rebuild_registry();
                // Recompilation à chaud : sans elle, le ProgramCache ressert
                // l'ancien programme et le shader corrigé ne prend jamais
                // effet avant redémarrage.
                self.materials_failed.remove(&material.id);
                if !self.comp_reload.contains(&material.id) {
                    self.comp_reload.push(material.id);
                }
            }
            MaterialAdd { .. } | MaterialRemove { .. } => {
                self.load_materials();
                self.rebuild_registry();
                // Remove : purge du programme GL orphelin + détachement des
                // decks qui le portaient ; Add : préchauffage du nouveau.
                self.comp_sync = true;
            }
            ModulatorAdd { .. } | ModulatorRemove { .. } | ModulatorUpdate { .. }
            | RouteAdd { .. } | RouteRemove { .. } | RouteUpdate { .. } => {
                self.modul.load(&self.show.modulators, &self.show.routes);
                self.rebuild_registry();
            }
            PatchArtnetAdd { .. } | PatchArtnetRemove { .. } | PatchArtnetUpdate { .. }
            | PatchMidiAdd { .. } | PatchMidiRemove { .. } | PatchMidiUpdate { .. }
            | PatchOscOutSet { .. } => {
                self.push_patch();
            }
            // Raccourcis clavier : exécutés côté UI, la persistance suffit.
            KeyBindingAdd { .. } | KeyBindingRemove { .. } => {}
            SettingsUpdate { .. } => {
                // Respawn (join de 4 threads, ~gel) uniquement si la
                // configuration RÉSEAU a changé ; le reste se pousse à chaud.
                let _ = prev_settings; // la signature réseau fait foi
                self.protocols
                    .respawn_if_changed(&self.show.settings, &self.show.patch);
                self.push_patch();
                // Entrée audio à chaud : re-spawn du thread de capture
                // uniquement si le device effectif a changé.
                self.sync_audio_input();
            }
            ShowRename { .. } => {}
        }
    }

    /// Recharge la conduite après édition des cues en préservant la position
    /// À CHAUD : la cue active reste au programme (le deck A ne se vide pas,
    /// les players survivent, la sortie ne gèle pas) et le prochain GO
    /// avance au lieu de rejouer la cue courante.
    fn reload_cues_preserving_position(&mut self) {
        self.cue.load_hot(&self.show.cues);
    }

    fn push_patch(&mut self) {
        let overrides = self.protocols.update_patch(&self.show.patch);
        for (addr, ms) in overrides {
            self.registry.set_smoothing_override(&addr, Some(ms));
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_edit = Some(Instant::now());
        self.edit_gen = self.edit_gen.wrapping_add(1);
        self.recover_dirty = true;
        self.state_show_dirty = true;
    }

    // -------------------------------------------------- show load/save/scan

    /// Demande une sauvegarde au thread d'écriture (clone du Show — la
    /// sérialisation et les `fsync` vivent sur le worker, jamais sur le
    /// tick). `dirty` ne retombe qu'au résultat, si rien n'a été édité
    /// depuis (voir [`Session::autosave`]).
    fn save_show(&mut self) {
        if self.save_in_flight {
            debug!(target: "app::session", "sauvegarde déjà en vol");
            return;
        }
        let dir = self.dirs.show_dir(&self.show_name);
        let job = SaveJob::Save {
            dir,
            shows_dir: self.dirs.shows.clone(),
            show: Box::new(self.show.clone()),
            gen: self.edit_gen,
        };
        if self.saver.submit(job) {
            self.save_in_flight = true;
            self.last_save = Instant::now();
            self.recover_dirty = false; // le worker met aussi le snapshot à jour
        }
    }

    fn load_show(&mut self, name: &str) {
        if self.mode == AppMode::Show {
            warn!(target: "app::session", "chargement refusé : mode Show verrouillé");
            return;
        }
        match conduite_core::load_show_with_media(&self.dirs.show_dir(name), &self.dirs.media) {
            Ok((show, warnings)) => {
                for w in &warnings {
                    warn!(target: "app::session", "avertissement de chargement : {w}");
                }
                self.install_show(show, name.to_string());
                self.config.last_show = name.to_string();
                self.config.save(&self.dirs.base);
            }
            Err(e) => error!(target: "app::session", show = name, error = %e,
                "chargement impossible — show courant conservé"),
        }
    }

    /// Installe un show (chargé ou neuf) : reconstruction complète.
    fn install_show(&mut self, show: Show, name: String) {
        self.show = show;
        self.show_name = name;
        self.undo.clear();
        self.dirty = false;
        self.rebuild_all();
        self.spawn_thumbs();
        self.push_recover_snapshot();
        self.publish_event(&StateEvent::ShowLoaded {
            name: self.show.name.clone(),
        });
        info!(target: "app::session", show = %self.show.name, "show installé");
    }

    /// Reconstruction complète : registre, conduite, modulation, matériaux,
    /// lecteurs, surfaces (respawn réseau seulement si la config a changé),
    /// et maintenance GPU différée (détachement des matériaux fantômes,
    /// purge des programmes/slices disparus, préchauffage ISF).
    fn rebuild_all(&mut self) {
        self.load_materials();
        self.rebuild_registry();
        self.cue.load(&self.show.cues);
        self.modul.load(&self.show.modulators, &self.show.routes);
        self.players.clear();
        self.material_bound.clear();
        // L'état GPU (Compositor) survit à material_bound.clear() : sans ce
        // flag, l'ancien shader resterait affiché à la place de la vidéo
        // (matériau fantôme) et ses FBO fuiraient.
        self.comp_sync = true;
        self.protocols
            .respawn_if_changed(&self.show.settings, &self.show.patch);
        self.push_patch();
        self.sync_audio_input();
        self.outputs_dirty = true;
        self.last_active = None;
        self.last_standby = None;
        self.state_show_dirty = true;
    }

    /// Re-scan des médias et matériaux — REFUSÉ en mode Show, exécuté en
    /// tâche de fond (scan disque + un ffprobe par média : jamais sur le
    /// tick). Le résultat revient par canal et s'applique dans `tick`.
    fn media_rescan(&mut self) {
        if self.mode == AppMode::Show {
            warn!(target: "app::session", "re-scan refusé : mode Show verrouillé");
            return;
        }
        if self.rescan_in_flight {
            info!(target: "app::session", "re-scan déjà en cours");
            return;
        }
        info!(target: "app::session", "re-scan des médias et matériaux (tâche de fond)");
        let media_dir = self.dirs.media.clone();
        let shaders_dir = self.dirs.shaders.clone();
        let existing_media = self.show.media.clone();
        let existing_mats = self.show.materials.clone();
        let tx = self.rescan_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("conduite-rescan".into())
            .spawn(move || {
                let scanned = conduite_media_library::scan(&media_dir);
                let mut media = conduite_media_library::reconcile(&existing_media, scanned);
                for m in &mut media {
                    m.missing = conduite_core::validate_relative_path(&m.path).is_err()
                        || !media_dir.join(&m.path).is_file();
                }
                conduite_media_library::probe_all(&mut media, &media_dir, |p| {
                    conduite_engine::probe(p).map(|i| ProbeInfo {
                        duration_s: i.duration_s,
                        fps: i.fps,
                        width: i.width,
                        height: i.height,
                    })
                });
                let scanned_mats = conduite_media_library::scan_materials(&shaders_dir);
                let materials =
                    conduite_media_library::reconcile_materials(&existing_mats, scanned_mats);
                let _ = tx.send(RescanResult { media, materials });
            });
        match spawned {
            Ok(_) => self.rescan_in_flight = true,
            Err(e) => warn!(target: "app::session", error = %e, "thread de re-scan impossible"),
        }
    }

    /// Applique le résultat d'un re-scan (sur le tick, données déjà
    /// sondées : aucune I/O média). Les players ne sont détruits que si le
    /// pool a réellement changé — un re-scan sans changement ne coupe pas
    /// la lecture en cours et ne salit pas le show.
    fn apply_rescan(&mut self, res: RescanResult) {
        self.rescan_in_flight = false;
        if self.mode == AppMode::Show {
            // Passé en mode Show entre-temps : aucune mutation pendant la
            // représentation, l'opérateur relancera un rescan en Edit.
            warn!(target: "app::session", "résultat de re-scan ignoré (mode Show)");
            return;
        }
        let media_changed = self.show.media != res.media;
        let mats_changed = self.show.materials != res.materials;
        if !media_changed && !mats_changed {
            info!(target: "app::session", "re-scan : aucun changement");
            return;
        }
        self.show.media = res.media;
        self.show.materials = res.materials;
        if mats_changed {
            self.load_materials();
            self.comp_sync = true;
        }
        self.rebuild_registry();
        if media_changed {
            self.players.clear();
            self.spawn_thumbs();
        }
        self.mark_dirty();
        self.publish_event(&StateEvent::ShowLoaded {
            name: self.show.name.clone(),
        });
        info!(target: "app::session", media = media_changed, materials = mats_changed,
            "re-scan appliqué");
    }

    fn show_collect(&mut self) {
        let show = self.show.clone();
        let media_dir = self.dirs.media.clone();
        let shaders_dir = self.dirs.shaders.clone();
        let dest = self.dirs.shows.join(format!("{}-collecte", self.show_name));
        let spawned = std::thread::Builder::new()
            .name("conduite-collect".into())
            .spawn(move || {
                match conduite_media_library::collect_show(&show, &media_dir, &shaders_dir, &dest) {
                    Ok(report) => info!(target: "app::session", dest = %dest.display(),
                        ?report, "show collecté"),
                    Err(e) => error!(target: "app::session", error = %e, "collecte impossible"),
                }
            });
        if let Err(e) = spawned {
            warn!(target: "app::session", error = %e, "thread de collecte impossible");
        }
    }

    /// Vignettes en tâche de fond (jamais sur le tick). Une génération déjà
    /// en cours ⇒ coalescence : la génération suivante (rescan, nouveau
    /// show) rattrapera ce qui n'est pas frais — pas de threads empilés.
    fn spawn_thumbs(&self) {
        let media = self.show.media.clone();
        if media.is_empty() {
            return;
        }
        if self
            .thumbs_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            debug!(target: "app::session", "génération de vignettes déjà en cours");
            return;
        }
        let media_dir = self.dirs.media.clone();
        let thumbs_dir = self.dirs.thumbs.clone();
        let running = Arc::clone(&self.thumbs_running);
        let spawned = std::thread::Builder::new()
            .name("conduite-thumbs".into())
            .spawn(move || {
                let report = conduite_media_library::generate_thumbs(&media, &media_dir, &thumbs_dir);
                running.store(false, Ordering::SeqCst);
                info!(target: "app::session", ?report, "vignettes générées");
            });
        if let Err(e) = spawned {
            self.thumbs_running.store(false, Ordering::SeqCst);
            warn!(target: "app::session", error = %e, "thread vignettes impossible");
        }
    }

    /// Pousse un snapshot de récupération au worker (clone seul sur le tick,
    /// sérialisation sur le worker).
    fn push_recover_snapshot(&mut self) {
        let job = SaveJob::Snapshot {
            shows_dir: self.dirs.shows.clone(),
            show: Box::new(self.show.clone()),
        };
        if self.saver.submit(job) {
            self.recover_dirty = false;
            self.last_recover_push = Instant::now();
        }
    }

    /// Autosave + drain des résultats de sauvegarde. Tout le coût disque vit
    /// sur le worker ; le tick clone au plus un Show par débounce.
    fn autosave(&mut self, now: Instant) {
        // Résultats du worker : `dirty` ne retombe que si la sauvegarde
        // correspond au dernier état édité (sinon on resauvera).
        while let Some(outcome) = self.saver.try_result() {
            self.save_in_flight = false;
            if outcome.ok && outcome.gen == self.edit_gen {
                self.dirty = false;
            }
        }

        // Snapshot de récupération débouncé (1 s) après édition.
        if self.recover_dirty && now.duration_since(self.last_recover_push) >= RECOVER_PUSH_PERIOD
        {
            self.push_recover_snapshot();
        }

        if !self.dirty || self.save_in_flight {
            return;
        }
        let debounce = Duration::from_secs_f32(self.show.settings.autosave_debounce_s.max(0.1));
        let interval = Duration::from_secs_f32(self.show.settings.autosave_interval_s.max(5.0));
        let debounced = self
            .last_edit
            .map(|t| now.duration_since(t) >= debounce)
            .unwrap_or(false);
        let periodic = now.duration_since(self.last_save) >= interval;
        if debounced || periodic {
            self.save_show();
        }
    }

    // ------------------------------------------------------------- registre

    /// (Re)déclare toutes les adresses stables. `params` préserve la valeur
    /// quand le kind est identique (re-register sans à-coup).
    fn rebuild_registry(&mut self) {
        let float = |min: f32, max: f32| ParamKind::Float { min, max };
        let spec = |addr: String, label: &str, kind: ParamKind, default: ParamValue,
                    smoothing_ms: f32, scriptable: bool| ParamSpec {
            addr,
            label: label.to_string(),
            kind,
            default,
            smoothing_ms,
            scriptable,
        };

        self.registry.register(spec(
            "master/intensity".into(), "Master", float(0.0, 1.0),
            ParamValue::F(1.0), 80.0, true,
        ));
        self.registry.register(spec(
            "master/dbo".into(), "DBO", float(0.0, 1.0),
            ParamValue::F(0.0), 0.0, false,
        ));
        self.registry.register(spec(
            "bpm".into(), "BPM", float(20.0, 300.0),
            ParamValue::F(120.0), 0.0, false,
        ));

        self.addr_cache.clear();
        for s in &self.show.slices {
            let a = SliceAddrs::new(s.id);
            self.registry.register(spec(a.opacity.clone(), "Opacité", float(0.0, 1.0),
                ParamValue::F(1.0), 50.0, true));
            self.registry.register(spec(a.gain_r.clone(), "Gain R", float(0.0, 2.0),
                ParamValue::F(1.0), 50.0, true));
            self.registry.register(spec(a.gain_g.clone(), "Gain V", float(0.0, 2.0),
                ParamValue::F(1.0), 50.0, true));
            self.registry.register(spec(a.gain_b.clone(), "Gain B", float(0.0, 2.0),
                ParamValue::F(1.0), 50.0, true));
            self.registry.register(spec(a.gamma.clone(), "Gamma", float(0.2, 4.0),
                ParamValue::F(1.0), 50.0, true));
            self.registry.register(spec(a.speed.clone(), "Vitesse", float(0.0, 4.0),
                ParamValue::F(1.0), 0.0, true));
            self.registry.register(spec(
                a.blendmode.clone(), "Fusion",
                ParamKind::Enum(vec!["normal".into(), "add".into(), "screen".into(), "multiply".into()]),
                ParamValue::I(0), 0.0, true,
            ));
            self.addr_cache.insert(s.id, a);
        }

        // Inputs ISF des matériaux : specs typées mises en cache par
        // `load_materials` — aucune lecture disque ici.
        {
            let registry = &mut self.registry;
            for mat in self.materials.values() {
                for sp in &mat.typed_specs {
                    registry.register(sp.clone());
                }
            }
        }

        for m in &self.show.modulators {
            let hz = match &m.kind {
                conduite_core::ModKind::Lfo { freq: conduite_core::Freq::Hz(hz), .. } => *hz,
                _ => 1.0,
            };
            self.registry.register(spec(format!("mod/{}/freq", m.id), "Fréquence",
                float(0.0, 30.0), ParamValue::F(hz), 0.0, false));
            self.registry.register(spec(format!("mod/{}/depth", m.id), "Profondeur",
                float(0.0, 1.0), ParamValue::F(1.0), 0.0, false));
        }

        self.purge_stale_prefixes();
    }

    /// Retire les adresses des slices/matériaux/modulateurs disparus.
    fn purge_stale_prefixes(&mut self) {
        let slice_ids: HashSet<u32> = self.show.slices.iter().map(|s| s.id).collect();
        let material_ids: HashSet<u32> = self.show.materials.iter().map(|m| m.id).collect();
        let mod_ids: HashSet<u32> = self.show.modulators.iter().map(|m| m.id).collect();
        let mut stale: Vec<String> = Vec::new();
        for sp in self.registry.specs() {
            let addr = sp.addr.as_str();
            let keep = if let Some(rest) = addr.strip_prefix("slice/") {
                id_of(rest).map(|i| slice_ids.contains(&i)).unwrap_or(true)
            } else if let Some(rest) = addr.strip_prefix("material/") {
                id_of(rest).map(|i| material_ids.contains(&i)).unwrap_or(true)
            } else if let Some(rest) = addr.strip_prefix("mod/") {
                id_of(rest).map(|i| mod_ids.contains(&i)).unwrap_or(true)
            } else {
                true
            };
            if !keep {
                if let Some(rest) = addr.split('/').nth(1) {
                    let prefix = format!("{}/{}/", addr.split('/').next().unwrap_or(""), rest);
                    if !stale.contains(&prefix) {
                        stale.push(prefix);
                    }
                }
            }
        }
        for prefix in stale {
            self.registry.unregister_prefix(&prefix);
        }
    }

    /// Parse et génère le GLSL de tous les matériaux du show (IO — appelé
    /// uniquement au chargement et sur EditOp matériau, jamais par frame).
    fn load_materials(&mut self) {
        self.materials.clear();
        self.materials_failed.clear();
        for m in &self.show.materials {
            let path = self.dirs.shaders.join(&m.path);
            let src = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    warn!(target: "app::session", material = m.id, path = %path.display(),
                        error = %e, "matériau illisible");
                    continue;
                }
            };
            let doc = match conduite_isf::parse(&src) {
                Ok(d) => d,
                Err(e) => {
                    warn!(target: "app::session", material = m.id, error = %e,
                        "ISF invalide");
                    continue;
                }
            };
            let sources = match conduite_isf::generate_glsl(&doc) {
                Ok(s) => s,
                Err(e) => {
                    warn!(target: "app::session", material = m.id, error = %e,
                        "génération GLSL impossible");
                    continue;
                }
            };
            let inputs = doc
                .inputs
                .iter()
                .filter(|i| {
                    !matches!(
                        i.kind,
                        IsfInputKind::Image
                            | IsfInputKind::Audio
                            | IsfInputKind::AudioFft
                            | IsfInputKind::Event
                    )
                })
                .map(|i| (i.name.clone(), format!("material/{}/{}", m.id, i.name)))
                .collect();
            let typed_specs = typed_specs_of(&doc, m.id);
            self.materials.insert(m.id, MaterialData { sources, inputs, typed_specs });
        }
        info!(target: "app::session", count = self.materials.len(), "matériaux ISF chargés");
    }

    // ---------------------------------------------------------------- rendu

    /// Maintenance GPU différée, exécutée en tête de tick avec le contexte
    /// GL courant :
    /// 1) `comp_sync` : détache TOUS les matériaux (miroir GPU de
    ///    `material_bound.clear()` — sans quoi l'ancien shader reste affiché
    ///    à la place de la vidéo après un chargement de show/undo/redo),
    ///    purge les programmes ISF et les slices disparus (VRAM), puis
    ///    préchauffe les programmes de tous les matériaux du show — jamais
    ///    de `glCompileShader` au GO pendant la représentation ;
    /// 2) `comp_reload` : recompilation à chaud des matériaux édités (sans
    ///    elle le ProgramCache ressert l'ancien programme à vie).
    fn apply_compositor_maintenance(&mut self, gfx: &mut Gfx) {
        if !self.comp_sync && self.comp_reload.is_empty() {
            return;
        }
        let Some(gl) = gfx.gl.as_mut() else { return };
        let comp = &mut gl.compositor;
        if self.comp_sync {
            self.comp_sync = false;
            comp.detach_all_materials();
            self.material_bound.clear();
            let keep_mats: Vec<MaterialId> = self.show.materials.iter().map(|m| m.id).collect();
            comp.retain_materials(&keep_mats);
            let keep_slices: Vec<SliceId> = self.show.slices.iter().map(|s| s.id).collect();
            comp.prune_slices(&keep_slices);
            let list: Vec<(MaterialId, &IsfSources)> = self
                .materials
                .iter()
                .map(|(id, m)| (*id, &m.sources))
                .collect();
            for (id, e) in comp.prewarm(&list) {
                error!(target: "app::session", material = id,
                    "compilation du matériau (préchauffage) :\n{e}");
                self.materials_failed.insert(id);
            }
        }
        for id in std::mem::take(&mut self.comp_reload) {
            let Some(mat) = self.materials.get(&id) else { continue };
            match comp.reload_material(id, &mat.sources) {
                Ok(()) => {
                    self.materials_failed.remove(&id);
                    info!(target: "app::session", material = id, "matériau recompilé à chaud");
                }
                Err(e) => {
                    error!(target: "app::session", material = id,
                        "recompilation du matériau :\n{e}");
                    self.materials_failed.insert(id);
                }
            }
        }
    }

    /// Pose les matériaux ISF des decks et pousse leurs uniforms (TIME,
    /// TIMEDELTA, FRAMEINDEX, RENDERSIZE + inputs pilotés par le registre).
    fn apply_materials(&mut self, gfx: &mut Gfx, frame: &CueFrame, now_s: f64, dt: f32) {
        let Some(gl) = gfx.gl.as_mut() else { return };
        let comp = &mut gl.compositor;
        for (deck_idx, scene) in [(0u8, frame.deck_a.as_ref()), (1u8, frame.deck_b.as_ref())] {
            let deck = if deck_idx == 0 {
                conduite_compositor::DeckSlot::A
            } else {
                conduite_compositor::DeckSlot::B
            };
            let Some(scene) = scene else { continue };
            for t in &scene.per_slice {
                let bound = self.material_bound.get(&(t.slice, deck_idx)).copied();
                match &t.content {
                    Content::Material(id) => {
                        if self.materials_failed.contains(id) {
                            continue;
                        }
                        let Some(mat) = self.materials.get(id) else { continue };
                        if let Err(e) = comp.set_material(t.slice, deck, Some(&mat.sources), *id) {
                            error!(target: "app::session", material = id, slice = t.slice,
                                "compilation du matériau :\n{e}");
                            self.materials_failed.insert(*id);
                            continue;
                        }
                        self.material_bound.insert((t.slice, deck_idx), *id);
                        // Uniforms standard + inputs.
                        self.scratch_uniforms.clear();
                        self.scratch_uniforms
                            .push(("TIME".into(), ParamValue::F(now_s as f32)));
                        self.scratch_uniforms
                            .push(("TIMEDELTA".into(), ParamValue::F(dt)));
                        self.scratch_uniforms
                            .push(("FRAMEINDEX".into(), ParamValue::I(self.frame_index)));
                        let size = self.output_size_of_slice(t.slice);
                        self.scratch_uniforms.push((
                            "RENDERSIZE".into(),
                            ParamValue::P2([size.0 as f32, size.1 as f32]),
                        ));
                        for (name, addr) in &mat.inputs {
                            if let Some(v) = self.registry.value(addr) {
                                self.scratch_uniforms.push((name.clone(), v));
                            }
                        }
                        comp.set_material_uniforms(t.slice, deck, &self.scratch_uniforms);
                    }
                    _ => {
                        if let Some(prev) = bound {
                            // Le deck ne porte plus de matériau : retrait.
                            if let Err(e) = comp.set_material(t.slice, deck, None, prev) {
                                warn!(target: "app::session", slice = t.slice, error = %e,
                                    "retrait de matériau");
                            }
                            self.material_bound.remove(&(t.slice, deck_idx));
                        }
                    }
                }
            }
        }
    }

    /// Taille de la sortie qui porte un slice (RENDERSIZE des matériaux).
    fn output_size_of_slice(&self, slice: SliceId) -> (u32, u32) {
        self.show
            .slices
            .iter()
            .find(|s| s.id == slice)
            .and_then(|s| self.show.outputs.iter().find(|o| o.id == s.output))
            .map(|o| (o.width.max(1), o.height.max(1)))
            .unwrap_or((1280, 720))
    }

    /// Construit les listes de dessin par sortie. `mix_override` : `Some(1.0)`
    /// pour la préview standby (deck B plein).
    fn build_draws(
        &self,
        frame: &CueFrame,
        mix_override: Option<f32>,
    ) -> HashMap<OutputId, Vec<SliceDraw>> {
        let mut plans: HashMap<OutputId, Vec<SliceDraw>> = HashMap::new();
        let mix = mix_override.unwrap_or(frame.blend).clamp(0.0, 1.0);
        for s in &self.show.slices {
            if !s.enabled {
                continue;
            }
            let ca = content_of(frame.deck_a.as_ref(), s.id);
            let cb = content_of(frame.deck_b.as_ref(), s.id);
            let dominant = if mix < 0.5 { ca.or(cb) } else { cb.or(ca) };
            let Some(dominant) = dominant else { continue };
            if matches!(dominant, Content::None) && mix_override.is_none() {
                // Slice éteint dans la cue courante : rien à dessiner.
                let other = if mix < 0.5 { cb } else { ca };
                if !matches!(other, Some(c) if !matches!(c, Content::None)) {
                    continue;
                }
            }
            let Some(addrs) = self.addr_cache.get(&s.id) else { continue };
            let mut sd = SliceDraw::new(s.id);
            sd.corners = s.corners;
            sd.src_rect = s.src;
            sd.z = s.z;
            sd.opacity = self.registry.value_f32(&addrs.opacity);
            sd.gains = [
                self.registry.value_f32(&addrs.gain_r),
                self.registry.value_f32(&addrs.gain_g),
                self.registry.value_f32(&addrs.gain_b),
            ];
            sd.gamma = self.registry.value_f32(&addrs.gamma).max(0.05);
            sd.blend_mode = blend_of(self.registry.value(&addrs.blendmode));
            sd.mix = mix;
            sd.pattern = match dominant {
                Content::Pattern(k) => Some(*k),
                Content::Media(id) if self.media_missing(*id) => Some(PatternKind::Checker),
                _ => None,
            };
            plans.entry(s.output).or_default().push(sd);
        }
        plans
    }

    fn media_missing(&self, id: u32) -> bool {
        self.show
            .media
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.missing)
            .unwrap_or(true)
    }

    /// Préviews MJPEG (program + standby), cadencées, jamais bloquantes :
    /// lecture asynchrone double-PBO (un canal par flux — la frame livrée
    /// est celle du tick préview précédent, `false` = rien à envoyer), et
    /// composition en réutilisant les FBO matériaux déjà rendus ce tick
    /// (pas de re-passe ISF pleine résolution pour une cible 640×360). Le
    /// plan program du tick est réutilisé (pas de `build_draws` en double).
    fn render_previews(
        &mut self,
        gfx: &mut Gfx,
        frame: &CueFrame,
        master: f32,
        now: Instant,
        plans: &HashMap<OutputId, Vec<SliceDraw>>,
    ) {
        let fps = self.show.settings.mjpeg_fps.max(1) as f32;
        let period = Duration::from_secs_f32(1.0 / fps);
        let (w, h) = (self.show.settings.mjpeg_width, self.show.settings.mjpeg_height);
        if now.duration_since(self.last_preview) >= period {
            self.last_preview = now;
            if let Some(slices) = first_output_plan(&self.show.outputs, plans) {
                let mut buf = std::mem::take(&mut self.preview_scratch);
                let got =
                    gfx.render_preview_into(0, w, h, slices, master, self.dbo_level, true, &mut buf);
                if got {
                    self.preview.submit(PreviewJob {
                        rgba: buf.clone(),
                        width: w,
                        height: h,
                        standby: false,
                        flip: true,
                    });
                }
                self.preview_scratch = buf;
            } else {
                let _ = self.preview_tx.send(self.placeholder.clone());
            }
        }
        // Standby : même chemin à cadence moitié (deck B plein, sans master).
        if now.duration_since(self.last_preview_b) >= period * 2 {
            self.last_preview_b = now;
            let plans_b = self.build_draws(frame, Some(1.0));
            if let Some(slices) = first_output_plan(&self.show.outputs, &plans_b) {
                let mut buf = std::mem::take(&mut self.preview_scratch_b);
                let got = gfx.render_preview_into(1, w, h, slices, 1.0, 0.0, true, &mut buf);
                if got {
                    self.preview.submit(PreviewJob {
                        rgba: buf.clone(),
                        width: w,
                        height: h,
                        standby: true,
                        flip: true,
                    });
                }
                self.preview_scratch_b = buf;
            } else {
                let _ = self.preview_b_tx.send(self.placeholder.clone());
            }
        }
    }

    /// Headless : l'endpoint MJPEG reste vivant avec un placeholder.
    fn headless_previews(&mut self, now: Instant) {
        let fps = self.show.settings.mjpeg_fps.max(1) as f32;
        let period = Duration::from_secs_f32(1.0 / fps);
        if now.duration_since(self.last_preview) >= period {
            self.last_preview = now;
            let _ = self.preview_tx.send(self.placeholder.clone());
        }
        if now.duration_since(self.last_preview_b) >= period * 2 {
            self.last_preview_b = now;
            let _ = self.preview_b_tx.send(self.placeholder.clone());
        }
    }

    // ------------------------------------------------------------ événements

    fn process_cue_events(&mut self, events: &[CueEvent]) {
        for ev in events {
            match ev {
                CueEvent::CueStarted { cue } => {
                    self.modul.retrigger();
                    let mut media_slices: Vec<SliceId> = Vec::new();
                    if let Some(c) = self.find_cue(*cue) {
                        let states = c.mod_routes.clone();
                        media_slices = c
                            .states
                            .iter()
                            .filter(|st| matches!(st.content, Content::Media(_)))
                            .map(|st| st.slice)
                            .collect();
                        self.modul.apply_route_states(&states);
                    }
                    // Ré-activation d'une cue au même contenu (goto_after
                    // vers soi-même, média répété) : le player déjà avancé
                    // ou en EOF doit repartir de son point d'entrée.
                    for s in media_slices {
                        self.players.request_restart(s);
                    }
                    info!(target: "app::session", cue = %cue, "GO");
                }
                CueEvent::TransitionFinished { cue } => {
                    debug!(target: "app::session", cue = %cue, "cue au programme");
                }
                CueEvent::FollowArmed { cue, target } => {
                    debug!(target: "app::session", cue = %cue, target_cue = %target, "follow armé");
                }
                CueEvent::FollowFired { cue, target } => {
                    info!(target: "app::session", cue = %cue, target_cue = %target, "follow déclenché");
                }
                CueEvent::PanicStarted { fade_s } => {
                    self.publish_event(&StateEvent::DboChanged { active: true });
                    warn!(target: "app::session", fade_s, "panic (fondu au noir de conduite)");
                }
                CueEvent::Warning { message } => {
                    // Déjà loggué par le moteur ; on relaie à l'UI.
                    let _ = self.events_tx.send(json!({
                        "type": "log_line", "level": "WARN",
                        "target": "cue", "message": message,
                    }));
                }
            }
        }
    }

    fn publish_cue_changes(&mut self, status: &conduite_cue::CueStatus) {
        if status.active != self.last_active {
            self.last_active = status.active;
            self.publish_event(&StateEvent::CueChanged {
                active: status.active,
            });
        }
        if status.standby != self.last_standby {
            self.last_standby = status.standby;
            self.publish_event(&StateEvent::StandbyChanged {
                standby: status.standby,
            });
        }
    }

    fn find_cue(&self, n: CueNumber) -> Option<&Cue> {
        self.show.cues.iter().find(|c| c.number == n)
    }

    fn publish_event(&self, ev: &StateEvent) {
        match serde_json::to_value(ev) {
            Ok(v) => {
                let _ = self.events_tx.send(v);
                self.protocols.osc_feedback(FeedbackEvent::State(ev.clone()));
            }
            Err(e) => warn!(target: "app::session", error = %e, "sérialisation d'événement"),
        }
    }

    fn publish_health(&mut self) {
        // Dernier échantillon du thread santé (verrou court, jamais de
        // sysinfo/WMI sur le tick — à-coup 1 Hz supprimé).
        let sys = self.health.as_ref().map(|h| h.latest()).unwrap_or_default();
        let counters: Vec<(OutputId, &FpsCounter)> =
            self.fps.iter().map(|(id, c)| (*id, c)).collect();
        let snapshot = conduite_system::merge(&counters, sys);
        if self.players.any_unhealthy() {
            warn!(target: "app::session", "au moins un lecteur média en mauvaise santé");
        }
        self.publish_event(&StateEvent::HealthTick { snapshot });
    }

    /// Valeur `fft` de la trame d'état (CONTRAT WS) : `Some({bins, device})`
    /// avec 64 bandes log 20 Hz→16 kHz lissées (attack rapide/release lent)
    /// si une capture est active, `None` sinon (champ absent de la trame).
    fn fft_state_value(
        &mut self,
        fft: &conduite_modulation::FftFrame,
        dt_s: f32,
    ) -> Option<Value> {
        let Some(device) = self.audio.active_device() else {
            self.fft_smoother.reset();
            return None;
        };
        let bins = spectrum_bins(
            fft,
            SPECTRUM_BINS_DEFAULT,
            SPECTRUM_LOW_HZ_DEFAULT,
            SPECTRUM_HIGH_HZ_DEFAULT,
        );
        let smoothed = self.fft_smoother.apply(&bins, dt_s);
        // Arrondi à 3 décimales : la trame dyn part à 10 Hz vers chaque
        // client WS, inutile d'y embarquer des f32 pleine précision.
        let rounded: Vec<f32> = smoothed
            .iter()
            .map(|v| (v * 1000.0).round() / 1000.0)
            .collect();
        Some(json!({ "bins": rounded, "device": device }))
    }

    /// (Re)démarre la capture audio si le device effectif a changé
    /// (SettingsUpdate, chargement de show, undo/redo). No-op sinon.
    fn sync_audio_input(&mut self) {
        self.audio
            .set_device(effective_audio_input(&self.config, &self.show.settings));
    }

    fn runtime_status(&self, st: &conduite_cue::CueStatus) -> RuntimeStatus {
        RuntimeStatus {
            mode: self.mode,
            active: st.active,
            standby: st.standby,
            progress: st.progress,
            remaining_s: st.remaining_s.unwrap_or(0.0),
            transition_active: st.transition_active,
            bpm: self.registry.value_f32("bpm"),
            master: self.registry.value_f32("master/intensity"),
            dbo: self.dbo_level > 0.001,
            mod_levels: self.modul.levels().collect(),
            // Renseigné par la vérification de mise à jour opt-in (P2-3).
            update: None,
        }
    }

    fn step_dbo(&mut self, dt: f32) {
        let rate = if self.dbo_fade_s > 0.0 {
            dt / self.dbo_fade_s
        } else {
            1.0
        };
        if self.dbo_level < self.dbo_target {
            self.dbo_level = (self.dbo_level + rate).min(self.dbo_target);
        } else if self.dbo_level > self.dbo_target {
            self.dbo_level = (self.dbo_level - rate).max(self.dbo_target);
        }
    }
}

// ------------------------------------------------------------------ helpers

/// Device d'entrée audio effectif : le réglage du show prime, sinon le
/// réglage machine (`config.toml`), sinon capture coupée.
fn effective_audio_input(config: &AppConfig, settings: &ShowSettings) -> Option<String> {
    settings
        .audio_input
        .clone()
        .or_else(|| config.audio_input.clone())
}

/// Contenu visé par une scène pour un slice donné.
fn content_of(scene: Option<&SceneTarget>, slice: SliceId) -> Option<&Content> {
    scene?.per_slice.iter().find(|t| t.slice == slice).map(|t| &t.content)
}

/// Plan de dessin de la première sortie activée (préview).
fn first_output_plan<'a>(
    outputs: &[OutputCfg],
    plans: &'a HashMap<OutputId, Vec<SliceDraw>>,
) -> Option<&'a [SliceDraw]> {
    let first = outputs.iter().find(|o| o.enabled)?;
    plans.get(&first.id).map(|v| v.as_slice())
}

/// `ParamValue` d'enum blendmode → mode de fusion du compositor.
fn blend_of(v: Option<ParamValue>) -> BlendMode {
    match v {
        Some(ParamValue::I(1)) => BlendMode::Add,
        Some(ParamValue::I(2)) => BlendMode::Screen,
        Some(ParamValue::I(3)) => BlendMode::Multiply,
        _ => BlendMode::Normal,
    }
}

/// Deck app → deck compositor.
fn deck_gl(d: Deck) -> conduite_compositor::DeckSlot {
    match d {
        Deck::A => conduite_compositor::DeckSlot::A,
        Deck::B => conduite_compositor::DeckSlot::B,
    }
}

/// Id numérique en tête d'un reste d'adresse (`{id}/...`).
fn id_of(rest: &str) -> Option<u32> {
    rest.split('/').next()?.parse().ok()
}

/// Specs typées des inputs ISF d'un doc parsé (Float/Bool/Long/Color/
/// Point2D) — construites une fois au `load_materials`, jamais relues du
/// disque par `rebuild_registry`.
fn typed_specs_of(doc: &conduite_isf::IsfDoc, material: u32) -> Vec<ParamSpec> {
    let mut out = Vec::new();
    for input in &doc.inputs {
        let addr = format!("material/{material}/{}", input.name);
        let (kind, default) = match &input.kind {
            IsfInputKind::Float { min, max, default } => (
                ParamKind::Float { min: *min, max: *max },
                ParamValue::F(*default),
            ),
            IsfInputKind::Bool { default } => (ParamKind::Bool, ParamValue::B(*default)),
            IsfInputKind::Long { min, max, default, values, labels } => {
                if !labels.is_empty() && labels.len() == values.len() {
                    let idx = values.iter().position(|v| v == default).unwrap_or(0);
                    (ParamKind::Enum(labels.clone()), ParamValue::I(idx as i64))
                } else {
                    (ParamKind::Int { min: *min, max: *max }, ParamValue::I(*default))
                }
            }
            IsfInputKind::Color { default } => (ParamKind::Color, ParamValue::Color(*default)),
            IsfInputKind::Point2D { default, .. } => (ParamKind::Point2, ParamValue::P2(*default)),
            IsfInputKind::Image
            | IsfInputKind::Event
            | IsfInputKind::Audio
            | IsfInputKind::AudioFft => continue,
        };
        out.push(ParamSpec {
            addr,
            label: input.label.clone(),
            kind,
            default,
            smoothing_ms: 50.0,
            scriptable: true,
        });
    }
    out
}

// ---------------------------------------------------- chargement tolérant

/// Show chargé (ou récupéré) au démarrage.
struct LoadedShow {
    show: Show,
    /// Nom effectif : `<nom>-secours` si on est retombé sur la démo, pour
    /// que save/autosave n'écrasent JAMAIS le dossier d'origine.
    name: String,
    warnings: Vec<LoadWarning>,
}

/// Charge `shows/<name>/show.json` avec récupération : fichier illisible ou
/// de version future ⇒ l'original est PRÉSERVÉ (renommé `.corrompu-<ts>`),
/// les backups sont tentés du plus récent au plus ancien, et en dernier
/// recours la démo est chargée sous un nom distinct — le show d'origine
/// n'est jamais écrasé et ses backups ne sont jamais élagués par la démo.
fn load_show_or_recover(dirs: &Dirs, name: &str) -> LoadedShow {
    let dir = dirs.show_dir(name);
    match conduite_core::load_show_with_media(&dir, &dirs.media) {
        Ok((show, warnings)) => LoadedShow {
            show,
            name: name.to_string(),
            warnings,
        },
        Err(e) => {
            let unsupported = matches!(e, CoreError::UnsupportedVersion(..));
            error!(target: "app::session", show = name, error = %e,
                "show illisible : tentative de récupération (backups)");
            // Préserver l'original : renommage horodaté (jamais de perte).
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let kept = dir.join(format!("{}.corrompu-{stamp}", conduite_core::SHOW_FILE));
            let src = dir.join(conduite_core::SHOW_FILE);
            match std::fs::rename(&src, &kept) {
                Ok(()) => warn!(target: "app::session", kept = %kept.display(),
                    "fichier d'origine préservé"),
                Err(re) => warn!(target: "app::session", error = %re,
                    "renommage du fichier illisible impossible"),
            }
            if unsupported {
                error!(target: "app::session",
                    "version de format plus récente que ce logiciel : mettez \
                     Conduite à jour ou rouvrez le show sur la machine d'origine");
            }
            if let Some(loaded) = try_backups(dirs, name) {
                return loaded;
            }
            let fallback = format!("{name}-secours");
            error!(target: "app::session", show = name, fallback = %fallback,
                "aucun backup exploitable : show de démo chargé sous un nom \
                 DISTINCT — le dossier d'origine ne sera pas écrasé");
            LoadedShow {
                show: conduite_core::demo_show(),
                name: fallback,
                warnings: Vec::new(),
            }
        }
    }
}

/// Tente les backups du plus récent au plus ancien : le premier qui se
/// charge est restauré comme `show.json` (écriture atomique) et utilisé.
fn try_backups(dirs: &Dirs, name: &str) -> Option<LoadedShow> {
    let dir = dirs.show_dir(name);
    let backups = dir.join(conduite_core::BACKUP_DIR);
    let mut names: Vec<String> = std::fs::read_dir(&backups)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.starts_with("show-") && n.ends_with(".json"))
        .collect();
    names.sort(); // horodatage à largeur fixe : ordre lexicographique = chrono
    for n in names.into_iter().rev() {
        let path = backups.join(&n);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        if conduite_core::write_atomic(&dir.join(conduite_core::SHOW_FILE), &bytes).is_err() {
            continue;
        }
        match conduite_core::load_show_with_media(&dir, &dirs.media) {
            Ok((show, warnings)) => {
                warn!(target: "app::session", backup = %path.display(),
                    "show restauré depuis un backup — vérifiez la conduite");
                return Some(LoadedShow {
                    show,
                    name: name.to_string(),
                    warnings,
                });
            }
            Err(e) => {
                warn!(target: "app::session", backup = %path.display(), error = %e,
                    "backup inexploitable, suivant");
            }
        }
    }
    None
}

/// Signale au démarrage un fichier de récupération post-panic plus récent
/// que le show courant (log ERROR : visible console + UI web), et élague
/// les `recover-*.json` au-delà de [`RECOVER_KEEP`].
fn notice_recover_files(shows_dir: &std::path::Path, show_dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(shows_dir) else { return };
    let mut recovers: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.starts_with("recover-") && n.ends_with(".json"))
        .collect();
    if recovers.is_empty() {
        return;
    }
    recovers.sort(); // horodaté : ordre chrono
    // Élagage : garder les RECOVER_KEEP plus récents.
    if recovers.len() > RECOVER_KEEP {
        let excess = recovers.len() - RECOVER_KEEP;
        for n in recovers.drain(..excess) {
            let _ = std::fs::remove_file(shows_dir.join(n));
        }
    }
    let newest = shows_dir.join(recovers.last().map(String::as_str).unwrap_or_default());
    let show_mtime = std::fs::metadata(show_dir.join(conduite_core::SHOW_FILE))
        .and_then(|m| m.modified())
        .ok();
    let recover_mtime = std::fs::metadata(&newest).and_then(|m| m.modified()).ok();
    let newer = match (recover_mtime, show_mtime) {
        (Some(r), Some(s)) => r > s,
        (Some(_), None) => true,
        _ => false,
    };
    if newer {
        error!(target: "app::session", path = %newest.display(),
            "un fichier de RÉCUPÉRATION post-crash est plus récent que le \
             show chargé — pour le récupérer : copier ce fichier comme \
             show.json dans le dossier du show puis recharger");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dirs(name: &str) -> Dirs {
        let base = std::env::temp_dir().join(format!(
            "conduite-session-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let dirs = Dirs {
            media: base.join("media"),
            shows: base.join("shows"),
            shaders: base.join("shaders"),
            logs: base.join("logs"),
            thumbs: base.join("thumbs"),
            base,
        };
        for d in [&dirs.media, &dirs.shows, &dirs.shaders] {
            std::fs::create_dir_all(d).expect("mkdir");
        }
        dirs
    }

    /// show.json corrompu + backup sain : le backup le plus récent est
    /// restauré, le nom du show est conservé et l'original est préservé
    /// en `.corrompu-<ts>` (jamais écrasé par la démo).
    #[test]
    fn corrupt_show_recovers_from_backup_and_keeps_original() {
        let dirs = test_dirs("recover");
        let dir = dirs.show_dir("gala");
        let mut show = Show::new("Gala");
        conduite_core::save_show_atomic(&dir, &show).expect("save v1");
        show.name = "Gala v2".to_string();
        conduite_core::save_show_atomic(&dir, &show).expect("save v2");
        // Corruption du fichier principal (coupure de courant simulée).
        std::fs::write(dir.join(conduite_core::SHOW_FILE), b"{ tronqu").expect("corrupt");

        let loaded = load_show_or_recover(&dirs, "gala");
        assert_eq!(loaded.name, "gala", "nom conservé : restauration backup");
        assert_eq!(loaded.show.name, "Gala v2", "backup le plus récent utilisé");
        let corrompu = std::fs::read_dir(&dir)
            .expect("read_dir")
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".corrompu-"));
        assert!(corrompu, "l'original illisible est préservé");
        let _ = std::fs::remove_dir_all(&dirs.base);
    }

    /// show.json corrompu SANS backup : démo chargée sous `<nom>-secours` —
    /// save/autosave n'écraseront jamais le dossier d'origine.
    #[test]
    fn corrupt_show_without_backup_falls_back_to_distinct_name() {
        let dirs = test_dirs("secours");
        let dir = dirs.show_dir("gala");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join(conduite_core::SHOW_FILE), b"pas du json").expect("write");

        let loaded = load_show_or_recover(&dirs, "gala");
        assert_eq!(loaded.name, "gala-secours", "nom DISTINCT du dossier d'origine");
        let corrompu = std::fs::read_dir(&dir)
            .expect("read_dir")
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".corrompu-"));
        assert!(corrompu, "l'original est préservé");
        let _ = std::fs::remove_dir_all(&dirs.base);
    }

    /// Version future du format : refusée, préservée, backups tentés — et
    /// à défaut, démo sous nom distinct.
    #[test]
    fn future_version_never_overwritten_by_demo() {
        let dirs = test_dirs("future");
        let dir = dirs.show_dir("gala");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let future = serde_json::json!({ "format_version": 99, "name": "Futur" });
        std::fs::write(
            dir.join(conduite_core::SHOW_FILE),
            serde_json::to_vec(&future).expect("json"),
        )
        .expect("write");

        let loaded = load_show_or_recover(&dirs, "gala");
        assert_eq!(loaded.name, "gala-secours");
        // Le contenu v99 existe toujours (renommé, pas détruit).
        let kept = std::fs::read_dir(&dir)
            .expect("read_dir")
            .flatten()
            .find(|e| e.file_name().to_string_lossy().contains(".corrompu-"))
            .expect("fichier préservé");
        let bytes = std::fs::read(kept.path()).expect("read");
        assert!(String::from_utf8_lossy(&bytes).contains("\"format_version\":99"));
        let _ = std::fs::remove_dir_all(&dirs.base);
    }

    /// Chargement sain : nom et contenu inchangés.
    #[test]
    fn healthy_show_loads_unchanged() {
        let dirs = test_dirs("sain");
        let dir = dirs.show_dir("gala");
        conduite_core::save_show_atomic(&dir, &Show::new("Gala")).expect("save");
        let loaded = load_show_or_recover(&dirs, "gala");
        assert_eq!(loaded.name, "gala");
        assert_eq!(loaded.show.name, "Gala");
        let _ = std::fs::remove_dir_all(&dirs.base);
    }

    /// Les `recover-*.json` sont élagués au-delà de [`RECOVER_KEEP`].
    #[test]
    fn recover_files_are_pruned() {
        let dirs = test_dirs("prune");
        for i in 0..8 {
            std::fs::write(
                dirs.shows.join(format!("recover-2026010{i}-000000.json")),
                b"{}",
            )
            .expect("write");
        }
        notice_recover_files(&dirs.shows, &dirs.show_dir("gala"));
        let count = std::fs::read_dir(&dirs.shows)
            .expect("read_dir")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("recover-"))
            .count();
        assert_eq!(count, RECOVER_KEEP, "élagage aux {RECOVER_KEEP} plus récents");
        let _ = std::fs::remove_dir_all(&dirs.base);
    }
}
