//! La session : cœur de l'application sur le thread principal.
//!
//! Possède le [`Show`], le registre de paramètres, le moteur de cues, la
//! modulation, les lecteurs média, l'undo et le mode Edit/Show. Un tick :
//! drain des commandes → cue.tick → modulation.tick → params (blend +
//! modulation + lissage) → horloges média + poll → uploads → rendu des
//! sorties → préview → santé (ordre normatif, INTERFACES §app).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use bytes::Bytes;
use conduite_compositor::{BlendMode, SliceDraw};
use conduite_control_osc::FeedbackEvent;
use conduite_core::{
    AppMode, Command, Content, Cue, CueNumber, EditOp, MaterialId, OutputCfg, OutputId,
    ParamValue, PatternKind, RuntimeStatus, Show, SliceId, Source, StateEvent,
};
use conduite_cue::{CueEngine, CueEvent, CueFrame, EngineTick, SceneTarget};
use conduite_isf::{IsfInputKind, IsfSources};
use conduite_media_library::ProbeInfo;
use conduite_modulation::{FftFrame, ModEngine};
use conduite_params::{ParamKind, ParamSpec, Registry};
use conduite_system::{FpsCounter, HealthSampler};
use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch};
use tracing::{debug, error, info, warn};

use crate::config::AppConfig;
use crate::dirs::{safe_show_name, Dirs};
use crate::gfx::Gfx;
use crate::logsetup;
use crate::players::{Deck, Players};
use crate::preview::{placeholder_jpeg, PreviewJob, PreviewWorker};
use crate::protocols::Protocols;
use crate::undo::UndoStack;

/// Cadence des trames d'état vers l'UI et le feedback OSC.
const STATE_PERIOD: Duration = Duration::from_millis(100);
/// Cadence du bandeau santé.
const HEALTH_PERIOD: Duration = Duration::from_secs(1);

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

    start: Instant,
    last_tick: Instant,
    frame_index: i64,

    dbo_level: f32,
    dbo_target: f32,
    dbo_fade_s: f32,

    health: HealthSampler,
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

    fft: FftFrame,
    placeholder: Bytes,
    outputs_dirty: bool,
    last_active: Option<CueNumber>,
    last_standby: Option<CueNumber>,
    gl_failed_flagged: bool,
}

impl Session {
    /// Construit la session : charge le show, (re)construit tous les moteurs
    /// et démarre les surfaces de contrôle.
    pub fn new(dirs: Dirs, config: AppConfig, show_name: String, ch: SessionChannels) -> Session {
        let (show, warnings) =
            match conduite_core::load_show_with_media(&dirs.show_dir(&show_name), &dirs.media) {
                Ok(pair) => pair,
                Err(e) => {
                    error!(target: "app::session", show = %show_name, error = %e,
                        "chargement du show impossible : show de démo en mémoire");
                    (conduite_core::demo_show(), Vec::new())
                }
            };
        for w in &warnings {
            warn!(target: "app::session", "avertissement de chargement : {w}");
        }
        if config.audio_input.is_some() {
            warn!(target: "app::session",
                "entrée audio configurée mais non intégrée (stub v1) : \
                 FFT vide, les LFO fonctionnent — TODO cpal+rustfft");
        }
        let protocols = Protocols::spawn(ch.cmd_tx.clone(), &show.settings, &show.patch);
        let preview = PreviewWorker::spawn(ch.preview_tx.clone(), ch.preview_b_tx.clone());
        let placeholder = placeholder_jpeg(show.settings.mjpeg_width, show.settings.mjpeg_height);
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
            start: now,
            last_tick: now,
            frame_index: 0,
            dbo_level: 0.0,
            dbo_target: 0.0,
            dbo_fade_s: 0.0,
            health: HealthSampler::new(),
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
            fft: FftFrame::empty(),
            placeholder,
            outputs_dirty: true,
            last_active: None,
            last_standby: None,
            gl_failed_flagged: false,
        };
        session.players = Players::new(session.dirs.media.clone());
        session.rebuild_all();
        session.spawn_thumbs();
        session.update_recover_snapshot();
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
            let _ = self
                .cmd_tx
                .send((Source::Ui, Command::Edit(EditOp::OutputUpdate { output: cfg })));
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

        // 1. Drain des commandes.
        while let Ok((source, cmd)) = self.cmd_rx.try_recv() {
            self.handle_command(source, cmd, now_s);
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

        // 3. Modulation (FFT vide en v1 — stub audio).
        let bpm = self.registry.value_f32("bpm").max(1.0);
        let offsets = self.modul.tick(now_s, bpm, &self.fft);

        // 4. Paramètres : blend de cue, offsets de modulation, lissage.
        if let Some((target, alpha)) = &frame.params_target {
            self.registry.blend_toward(target, *alpha);
        }
        self.registry.apply_modulation(&offsets);
        self.registry.tick(dt);

        // 5. DBO (fondu maître d'urgence, indépendant de la conduite).
        self.step_dbo(dt);

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
            self.render_previews(gfx, &frame, master_eff, now);
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
        self.publish_cue_changes(&status);
        if now - self.last_state >= STATE_PERIOD {
            self.last_state = now;
            let rt = self.runtime_status(&status);
            self.protocols.osc_feedback(FeedbackEvent::Status(rt.clone()));
            let _ = self.state_tx.send(json!({ "show": self.show, "runtime": rt }));
        }
        self.protocols.drain_midi_events(&self.events_tx);

        // 10. Autosave (débounce après édition + périodique si dirty).
        self.autosave(now);
    }

    // ------------------------------------------------------------- commandes

    fn handle_command(&mut self, source: Source, cmd: Command, now_s: f64) {
        match cmd {
            Command::ParamSet { addr, value, source } => self.param_set(&addr, value, source),
            Command::ParamNudge { addr, delta, source } => {
                let next = match self.registry.value(&addr) {
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
                info!(target: "app::session", ?mode, "mode changé");
                self.publish_event(&StateEvent::ModeChanged { mode });
            }
        }
    }

    fn param_set(&mut self, addr: &str, value: ParamValue, source: Source) {
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
        self.undo.push(self.show.clone());
        op.apply(&mut self.show);
        self.after_model_change(&op);
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
    fn after_model_change(&mut self, op: &EditOp) {
        use EditOp::*;
        match op {
            CueAdd { .. } | CueRemove { .. } | CueUpdate { .. } | CueUpdateState { .. } => {
                self.reload_cues_preserving_position();
            }
            SliceAdd { .. } | SliceRemove { .. } | SliceUpdate { .. } | CornerSet { .. } => {
                self.rebuild_registry();
            }
            OutputAdd { .. } | OutputRemove { .. } | OutputUpdate { .. } => {
                self.outputs_dirty = true;
            }
            MediaAdd { .. } | MediaRemove { .. } | MediaUpdate { .. } => {
                self.players.clear();
            }
            MaterialAdd { .. } | MaterialRemove { .. } | MaterialUpdate { .. } => {
                self.load_materials();
                self.rebuild_registry();
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
            SettingsUpdate { .. } => {
                self.protocols.respawn(&self.show.settings, &self.show.patch);
                self.push_patch();
            }
            ShowRename { .. } => {}
        }
    }

    /// Recharge la conduite après édition des cues en préservant au mieux la
    /// position (le moteur repart standby sur la cue active précédente).
    fn reload_cues_preserving_position(&mut self) {
        let st = self.cue.status();
        self.cue.load(&self.show.cues);
        if let Some(n) = st.active.or(st.standby) {
            self.cue.standby(n);
        }
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
        self.update_recover_snapshot();
    }

    // -------------------------------------------------- show load/save/scan

    fn save_show(&mut self) {
        let dir = self.dirs.show_dir(&self.show_name);
        match conduite_core::save_show_atomic(&dir, &self.show) {
            Ok(()) => {
                self.dirty = false;
                self.last_save = Instant::now();
                info!(target: "app::session", dir = %dir.display(), "show sauvegardé");
            }
            Err(e) => error!(target: "app::session", error = %e, "sauvegarde du show impossible"),
        }
        self.update_recover_snapshot();
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
        self.update_recover_snapshot();
        self.publish_event(&StateEvent::ShowLoaded {
            name: self.show.name.clone(),
        });
        info!(target: "app::session", show = %self.show.name, "show installé");
    }

    /// Reconstruction complète : registre, conduite, modulation, matériaux,
    /// lecteurs, surfaces.
    fn rebuild_all(&mut self) {
        self.load_materials();
        self.rebuild_registry();
        self.cue.load(&self.show.cues);
        self.modul.load(&self.show.modulators, &self.show.routes);
        self.players.clear();
        self.material_bound.clear();
        self.protocols.respawn(&self.show.settings, &self.show.patch);
        self.push_patch();
        self.outputs_dirty = true;
        self.last_active = None;
        self.last_standby = None;
    }

    fn media_rescan(&mut self) {
        info!(target: "app::session", "re-scan des médias et matériaux");
        let scanned = conduite_media_library::scan(&self.dirs.media);
        self.show.media = conduite_media_library::reconcile(&self.show.media, scanned);
        for m in &mut self.show.media {
            m.missing = conduite_core::validate_relative_path(&m.path).is_err()
                || !self.dirs.media.join(&m.path).is_file();
        }
        conduite_media_library::probe_all(&mut self.show.media, &self.dirs.media, |p| {
            conduite_engine::probe(p).map(|i| ProbeInfo {
                duration_s: i.duration_s,
                fps: i.fps,
                width: i.width,
                height: i.height,
            })
        });
        let scanned_mats = conduite_media_library::scan_materials(&self.dirs.shaders);
        self.show.materials =
            conduite_media_library::reconcile_materials(&self.show.materials, scanned_mats);
        self.load_materials();
        self.rebuild_registry();
        self.players.clear();
        self.spawn_thumbs();
        self.mark_dirty();
        self.publish_event(&StateEvent::ShowLoaded {
            name: self.show.name.clone(),
        });
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

    /// Vignettes en tâche de fond (jamais sur le tick).
    fn spawn_thumbs(&self) {
        let media = self.show.media.clone();
        if media.is_empty() {
            return;
        }
        let media_dir = self.dirs.media.clone();
        let thumbs_dir = self.dirs.thumbs.clone();
        let spawned = std::thread::Builder::new()
            .name("conduite-thumbs".into())
            .spawn(move || {
                let report = conduite_media_library::generate_thumbs(&media, &media_dir, &thumbs_dir);
                info!(target: "app::session", ?report, "vignettes générées");
            });
        if let Err(e) = spawned {
            warn!(target: "app::session", error = %e, "thread vignettes impossible");
        }
    }

    fn update_recover_snapshot(&self) {
        match serde_json::to_string_pretty(&self.show) {
            Ok(json) => logsetup::set_recover_snapshot(self.dirs.shows.clone(), json),
            Err(e) => warn!(target: "app::session", error = %e, "snapshot de récupération impossible"),
        }
    }

    fn autosave(&mut self, now: Instant) {
        if !self.dirty {
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

        // Inputs ISF des matériaux (specs typées depuis les IsfDoc).
        let typed: Vec<ParamSpec> = self.material_typed_specs();
        for sp in typed {
            self.registry.register(sp);
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

    /// Specs typées des inputs ISF (Float/Bool/Long/Color/Point2D).
    fn material_typed_specs(&self) -> Vec<ParamSpec> {
        let mut out = Vec::new();
        for m in &self.show.materials {
            let path = self.dirs.shaders.join(&m.path);
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            let Ok(doc) = conduite_isf::parse(&src) else { continue };
            for input in &doc.inputs {
                let addr = format!("material/{}/{}", m.id, input.name);
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
                    IsfInputKind::Point2D { default, .. } => {
                        (ParamKind::Point2, ParamValue::P2(*default))
                    }
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
        }
        out
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
            self.materials.insert(m.id, MaterialData { sources, inputs });
        }
        info!(target: "app::session", count = self.materials.len(), "matériaux ISF chargés");
    }

    // ---------------------------------------------------------------- rendu

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

    /// Préviews MJPEG (program + standby), cadencées, jamais bloquantes.
    fn render_previews(&mut self, gfx: &mut Gfx, frame: &CueFrame, master: f32, now: Instant) {
        let fps = self.show.settings.mjpeg_fps.max(1) as f32;
        let period = Duration::from_secs_f32(1.0 / fps);
        let (w, h) = (self.show.settings.mjpeg_width, self.show.settings.mjpeg_height);
        if now.duration_since(self.last_preview) >= period {
            self.last_preview = now;
            let plans = self.build_draws(frame, None);
            if let Some(slices) = first_output_plan(&self.show.outputs, &plans) {
                if let Some(rgba) = gfx.render_preview(w, h, slices, master, self.dbo_level) {
                    self.preview.submit(PreviewJob {
                        rgba,
                        width: w,
                        height: h,
                        standby: false,
                        flip: true,
                    });
                }
            } else {
                let _ = self.preview_tx.send(self.placeholder.clone());
            }
        }
        // Standby : même chemin à cadence moitié (deck B plein, sans master).
        if now.duration_since(self.last_preview_b) >= period * 2 {
            self.last_preview_b = now;
            let plans = self.build_draws(frame, Some(1.0));
            if let Some(slices) = first_output_plan(&self.show.outputs, &plans) {
                if let Some(rgba) = gfx.render_preview(w, h, slices, 1.0, 0.0) {
                    self.preview.submit(PreviewJob {
                        rgba,
                        width: w,
                        height: h,
                        standby: true,
                        flip: true,
                    });
                }
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
                    if let Some(c) = self.find_cue(*cue) {
                        let states = c.mod_routes.clone();
                        self.modul.apply_route_states(&states);
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
        let sys = self.health.sample();
        let counters: Vec<(OutputId, &FpsCounter)> =
            self.fps.iter().map(|(id, c)| (*id, c)).collect();
        let snapshot = conduite_system::merge(&counters, sys);
        if self.players.any_unhealthy() {
            warn!(target: "app::session", "au moins un lecteur média en mauvaise santé");
        }
        self.publish_event(&StateEvent::HealthTick { snapshot });
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
