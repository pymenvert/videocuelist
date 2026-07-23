//! `conduite` — le binaire : assemble tous les moteurs (cues, params,
//! modulation, lecteurs, compositor) avec les surfaces de contrôle (web,
//! OSC, MIDI, Art-Net) autour d'une boucle de session sur le thread
//! principal (winit) ou d'une boucle simple en `--headless`.

mod config;
mod dirs;
mod gfx;
mod logsetup;
mod players;
mod preview;
mod protocols;
mod session;
mod undo;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use conduite_control_http::{HttpDeps, HttpServer, HttpServerHandle};
use serde_json::json;
use tokio::sync::{broadcast, watch};
use tracing::{error, info, warn};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

use crate::config::AppConfig;
use crate::dirs::Dirs;
use crate::gfx::Gfx;
use crate::session::{Session, SessionChannels};

/// Options de ligne de commande.
#[derive(Debug, Default)]
struct Cli {
    show: Option<String>,
    port: Option<u16>,
    headless: bool,
    version: bool,
}

fn parse_cli() -> Cli {
    let mut cli = Cli::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--show" => cli.show = args.next(),
            "--port" => cli.port = args.next().and_then(|p| p.parse().ok()),
            "--headless" => cli.headless = true,
            "--version" | "-V" => cli.version = true,
            other => eprintln!("option inconnue ignorée : {other}"),
        }
    }
    cli
}

fn main() {
    let cli = parse_cli();
    if cli.version {
        println!("conduite {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Canaux partagés (session ↔ serveur web ↔ journal).
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (state_tx, state_rx) = watch::channel(json!({ "show": null, "runtime": null }));
    let (events_tx, _events_keep) = broadcast::channel(512);
    let (preview_tx, _preview_keep) = broadcast::channel(8);
    let (preview_b_tx, _preview_b_keep) = broadcast::channel(8);

    let dirs = Dirs::detect();
    let _log_guard = logsetup::init(&dirs.logs, events_tx.clone());
    logsetup::install_panic_hook();
    info!(target: "app", version = env!("CARGO_PKG_VERSION"),
        base = %dirs.base.display(), "démarrage de Conduite");

    let mut config = AppConfig::load(&dirs.base);
    if let Some(p) = cli.port {
        config.http_port = p;
    }
    let show_name = dirs::safe_show_name(
        cli.show.as_deref().unwrap_or(config.last_show.as_str()),
    );
    ensure_show_exists(&dirs, &show_name);

    // Serveur web (port machine — indépendant des réglages du show).
    let http = spawn_http(&config, &dirs, HttpDeps {
        cmd_tx: cmd_tx.clone(),
        state_rx: state_rx.clone(),
        events_rx: events_tx.subscribe(),
        preview_rx: preview_tx.subscribe(),
        preview_b_rx: preview_b_tx.subscribe(),
        thumb_dir: dirs.thumbs.clone(),
    });

    let session = Session::new(
        dirs,
        config,
        show_name,
        SessionChannels {
            cmd_tx,
            cmd_rx,
            state_tx,
            events_tx,
            preview_tx,
            preview_b_tx,
        },
    );

    if cli.headless {
        info!(target: "app", "mode headless (--headless)");
        run_headless(session, http);
    } else {
        run_windowed(session, http);
    }
}

/// Démarre le serveur HTTP ; un échec n'empêche pas l'app de tourner.
fn spawn_http(config: &AppConfig, _dirs: &Dirs, deps: HttpDeps) -> Option<HttpServerHandle> {
    let addr: SocketAddr = match format!("{}:{}", config.http_bind, config.http_port).parse() {
        Ok(a) => a,
        Err(e) => {
            error!(target: "app", bind = %config.http_bind, port = config.http_port,
                error = %e, "adresse HTTP invalide — serveur web inactif");
            return None;
        }
    };
    match HttpServer::spawn(addr, deps) {
        Ok(handle) => {
            info!(target: "app", addr = %handle.local_addr(), "web UI disponible");
            Some(handle)
        }
        Err(e) => {
            error!(target: "app", %addr, error = %e,
                "serveur web impossible (port occupé ?) — UI web inactive");
            None
        }
    }
}

/// Premier lancement : crée `shows/<nom>/show.json` depuis le show de démo,
/// en référençant les médias présents dans `media/` (demo-*.mp4 & co).
fn ensure_show_exists(dirs: &Dirs, name: &str) {
    let dir = dirs.show_dir(name);
    if dir.join(conduite_core::SHOW_FILE).is_file() {
        return;
    }
    info!(target: "app", show = name, "premier lancement : création du show de démo");
    let mut show = conduite_core::demo_show();
    if name != "demo" {
        show.name = name.to_string();
    }

    // Référencer les médias du dossier portable sur les slices de démo.
    let scanned = conduite_media_library::scan(&dirs.media);
    if !scanned.is_empty() {
        show.media = scanned;
        conduite_media_library::probe_all(&mut show.media, &dirs.media, |p| {
            conduite_engine::probe(p).map(|i| conduite_media_library::ProbeInfo {
                duration_s: i.duration_s,
                fps: i.fps,
                width: i.width,
                height: i.height,
            })
        });
        if let Some(first_video) = show
            .media
            .iter()
            .find(|m| {
                conduite_media_library::media_kind(std::path::Path::new(&m.path))
                    == Some(conduite_media_library::MediaKind::Video)
            })
            .map(|m| m.id)
        {
            show.cues.push(demo_video_cue(first_video));
            info!(target: "app", media = first_video, "cue vidéo de démo ajoutée");
        }
    }

    if let Err(e) = conduite_core::save_show_atomic(&dir, &show) {
        error!(target: "app", error = %e, "création du show de démo impossible");
    }
}

/// Cue « 5 — Vidéo démo » : le média en boucle plein cadre sur le slice 1.
fn demo_video_cue(media: conduite_core::MediaId) -> conduite_core::Cue {
    use conduite_core::*;
    use std::collections::BTreeMap;
    let mut params = BTreeMap::new();
    params.insert("slice/1/opacity".to_string(), ParamValue::F(1.0));
    conduite_core::Cue {
        number: CueNumber(5000),
        name: "Vidéo démo".to_string(),
        color: Some("#3ff59a".to_string()),
        notes: "Lecture en boucle du premier média du dossier media/.".to_string(),
        transition: Transition {
            kind: TransitionKind::Crossfade,
            dur_s: 1.5,
            curve: Curve::SCurve,
        },
        follow: FollowMode::Manual,
        goto_after: None,
        states: vec![
            SliceState {
                slice: 1,
                content: Content::Media(media),
                playback: Some(Playback::default()),
                params,
            },
            SliceState {
                slice: 2,
                content: Content::None,
                playback: None,
                params: BTreeMap::new(),
            },
            SliceState {
                slice: 3,
                content: Content::None,
                playback: None,
                params: BTreeMap::new(),
            },
        ],
        mod_routes: Vec::new(),
        triggers: CueTriggers::default(),
    }
}

// ------------------------------------------------------------------ headless

/// Boucle headless : tick simple avec cadence `target_fps`, sans winit.
fn run_headless(mut session: Session, _http: Option<HttpServerHandle>) {
    let period = Duration::from_secs_f64(1.0 / f64::from(session.target_fps().max(1)));
    let mut gfx = Gfx::headless();
    let mut next = Instant::now();
    loop {
        session.tick(&mut gfx);
        let _ = session.take_outputs_dirty(); // pas de fenêtres en headless
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            // En retard : on repart d'ici (pas de rattrapage en rafale).
            next = now;
        }
    }
}

// ------------------------------------------------------------------ fenêtré

/// Application winit : fenêtres de sortie GL + tick cadencé.
struct App {
    session: Session,
    gfx: Gfx,
    period: Duration,
    next_frame: Instant,
    _http: Option<HttpServerHandle>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        self.gfx.ensure_windows(el, self.session.outputs());
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if let Some(output) = self.gfx.output_of(id) {
                    self.session.on_output_close_requested(output);
                }
            }
            WindowEvent::Resized(size) => {
                self.gfx.resized(id, size.width, size.height);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_frame {
            if self.session.take_outputs_dirty() {
                self.gfx.ensure_windows(el, self.session.outputs());
            }
            self.session.tick(&mut self.gfx);
            self.next_frame += self.period;
            if self.next_frame <= now {
                self.next_frame = now + self.period;
            }
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

/// Mode fenêtré ; si winit est indisponible (session RDP minimale…),
/// bascule headless — l'app tourne quand même.
fn run_windowed(session: Session, http: Option<HttpServerHandle>) {
    let event_loop = match EventLoop::new() {
        Ok(el) => el,
        Err(e) => {
            error!(target: "app", error = %e,
                "boucle d'événements impossible : bascule headless");
            run_headless(session, http);
            return;
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    let period = Duration::from_secs_f64(1.0 / f64::from(session.target_fps().max(1)));
    let mut app = App {
        session,
        gfx: Gfx::headless(),
        period,
        next_frame: Instant::now(),
        _http: http,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        warn!(target: "app", error = %e, "boucle d'événements terminée sur erreur");
    }
    info!(target: "app", "arrêt de Conduite");
}
