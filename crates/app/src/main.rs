//! `conduite` — le binaire : assemble tous les moteurs (cues, params,
//! modulation, lecteurs, compositor) avec les surfaces de contrôle (web,
//! OSC, MIDI, Art-Net) autour d'une boucle de session sur le thread
//! principal (winit) ou d'une boucle simple en `--headless`.

mod audio;
mod config;
mod crash;
mod diagnostic;
mod dirs;
mod gfx;
mod logsetup;
mod platform;
mod players;
mod preview;
mod protocols;
mod saver;
mod session;
mod shaderwatch;
mod undo;
mod update;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// Allocateur global : mimalloc (P2 endurance — RSS qui redescend après un
/// pic, fragmentation maîtrisée sur 8 h de show, support x86_64/ARM/RPi).
#[global_allocator]
static GLOBAL_ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

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

/// Codes de sortie (contrat supervision — le watchdog s'y fie) :
/// 0 = arrêt normal, 1 = erreur générique, 2 = usage CLI,
/// 10 = port web pris / instance déjà lancée (NE PAS relancer),
/// 11 = perte GPU (relance par watchdog conseillée, < 5 s).
const EXIT_OK: i32 = 0;
const EXIT_GENERIC: i32 = 1;
const EXIT_PORT_BUSY: i32 = 10;
const EXIT_GPU_LOST: i32 = 11;

/// Options de ligne de commande.
#[derive(Debug, Default)]
struct Cli {
    show: Option<String>,
    port: Option<u16>,
    /// Dossier de travail (`--home`) : prioritaire sur `CONDUITE_HOME`.
    home: Option<std::path::PathBuf>,
    headless: bool,
    version: bool,
    help: bool,
    /// Flag caché de recette (absent de `--help`) : crash volontaire juste
    /// après l'installation de la capture — vérifie qu'un dump apparaît
    /// bien dans `logs/crash/`. Actif en build DEBUG uniquement.
    crash_test: bool,
}

/// Texte de `--help` (aligné sur README.md et docs/MANUEL.md).
const HELP: &str = "\
Conduite — régie vidéo de spectacle

Usage : conduite [OPTIONS]

Options :
  --show <nom>    Show à charger (dossier dans shows/) — défaut : dernier show ouvert
  --port <port>   Port de l'interface web — défaut : 9820 (clé http_port de config.toml)
  --home <dir>    Dossier de travail (config.toml, media/, shows/, shaders/, logs/)
                  — défaut : le dossier de l'exécutable. Aussi : CONDUITE_HOME
  --headless      Sans fenêtres de sortie (moteur + interface web seulement)
  -V, --version   Affiche la version et quitte
  -h, --help      Affiche cette aide et quitte

Une fois lancé, l'interface de régie est sur http://localhost:9820";

fn parse_cli() -> Cli {
    let mut cli = Cli::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--show" => cli.show = args.next(),
            "--port" => cli.port = args.next().and_then(|p| p.parse().ok()),
            "--home" => cli.home = args.next().map(std::path::PathBuf::from),
            "--headless" => cli.headless = true,
            // Recette de la capture de crash (interne, debug uniquement —
            // volontairement absent de --help, comme --crash-server).
            #[cfg(debug_assertions)]
            "--crash-test" => cli.crash_test = true,
            "--version" | "-V" => cli.version = true,
            "--help" | "-h" => cli.help = true,
            other => {
                // Option inconnue = refus de démarrer : une faute de frappe
                // ne doit jamais lancer un show avec de mauvais réglages.
                eprintln!("option inconnue : {other}\n\n{HELP}");
                std::process::exit(2);
            }
        }
    }
    cli
}

/// Version affichable : `0.1.0 (abc1234)` — hash git court si disponible
/// (embarqué au build par crates/app/build.rs).
fn version_string() -> String {
    let git = env!("CONDUITE_GIT_HASH");
    if git.is_empty() {
        env!("CARGO_PKG_VERSION").to_string()
    } else {
        format!("{} ({git})", env!("CARGO_PKG_VERSION"))
    }
}

fn main() {
    // Mode serveur de crash hors-process (`--crash-server <nom> <dossier>`,
    // usage interne) : traité AVANT TOUT — pas de verrou mono-instance, pas
    // de ports, pas de fichier de log.
    if let Some(code) = crash::maybe_run_server() {
        std::process::exit(code);
    }
    // Tout vit dans `run` : les gardes (verrou, logs, timer) sont relâchées
    // AVANT `process::exit` (qui n'exécute aucun destructeur).
    let code = run();
    std::process::exit(code);
}

fn run() -> i32 {
    let cli = parse_cli();
    if cli.help {
        println!("{HELP}");
        return EXIT_OK;
    }
    if cli.version {
        println!("conduite {}", version_string());
        return EXIT_OK;
    }

    // Canaux partagés (session ↔ serveur web ↔ journal). Bus de commandes
    // BORNÉ : un flood réseau (OSC/Art-Net/WS non authentifiés) fait
    // backpressure sur les threads des surfaces au lieu de gonfler la
    // mémoire sans limite — le drain du tick est de son côté plafonné
    // (budget par frame, session.rs).
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(8192);
    let (state_tx, state_rx) = watch::channel(json!({ "show": null, "runtime": null }));
    let (events_tx, _events_keep) = broadcast::channel(512);
    let (preview_tx, _preview_keep) = broadcast::channel(8);
    let (preview_b_tx, _preview_b_keep) = broadcast::channel(8);
    // Préview H.264 (WS /preview.h264) : config + access units produits par
    // la session ; le compteur de clients pilote le cycle de vie de
    // l'encodeur ffmpeg (0 client = aucun process).
    let (h264_tx, _h264_keep) = broadcast::channel(64);
    let h264_clients = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let dirs = Dirs::detect(cli.home.clone());
    let log_handles = logsetup::init(&dirs.logs, events_tx.clone());
    let _log_guard = log_handles.guard;
    logsetup::install_panic_hook();
    info!(target: "app", version = env!("CARGO_PKG_VERSION"),
        base = %dirs.base.display(), "démarrage de Conduite");

    // Arrêt propre sur Ctrl-C / fermeture console (drapeau consulté par les
    // boucles), résolution timer 1 ms (relâchée au drop) et promotion MMCSS
    // du thread de rendu — dégradation silencieuse partout.
    platform::install_quit_handler();
    let _timer_res = platform::TimerResolution::new();
    platform::promote_render_thread();

    // Verrou mono-instance (verrou de fichier OS : libéré même après crash).
    // Deux instances qui sauvent le même show se corrompent mutuellement.
    let _instance_lock = match conduite_core::acquire_instance_lock(&dirs.base) {
        Ok(lock) => Some(lock),
        Err(conduite_core::CoreError::InstanceLocked { path }) => {
            error!(target: "app", %path,
                "Conduite est DÉJÀ lancé (verrou tenu par une autre instance) : \
                 fermez l'autre instance puis relancez — démarrage refusé");
            eprintln!(
                "Conduite est déjà lancé (verrou : {path}).\n\
                 Fermez l'autre instance puis relancez."
            );
            // Même sémantique que le port pris : NE PAS relancer en boucle.
            return EXIT_PORT_BUSY;
        }
        Err(e) => {
            warn!(target: "app", error = %e,
                "verrou mono-instance indisponible : on continue sans (prudence)");
            None
        }
    };

    // Capture de crash hors-process : notre propre exe relancé en serveur
    // de minidump (logs/crash/, rétention 5, aucun envoi réseau). L'app
    // démarre normalement si la capture est indisponible.
    let _crash_guard = crash::spawn(&dirs.logs);
    if cli.crash_test {
        // Recette : accès mémoire invalide RÉEL (pas un panic Rust — le
        // hook de panic ne passe pas par le handler de crash). Le serveur
        // hors-process doit écrire logs/crash/crash-<ts>.dmp.
        warn!(target: "app::crash",
            "--crash-test : crash volontaire dans 500 ms (recette de la capture)");
        std::thread::sleep(std::time::Duration::from_millis(500));
        unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u32>(), 0xDEAD) };
    }

    let mut config = AppConfig::load(&dirs.base);
    if let Some(p) = cli.port {
        config.http_port = p;
    }
    let show_name = dirs::safe_show_name(
        cli.show.as_deref().unwrap_or(config.last_show.as_str()),
    );
    ensure_show_exists(&dirs, &show_name);

    // Serveur web (port machine). Échec de bind = SORTIE IMMÉDIATE code 10 :
    // plus jamais de moteur zombie sans UI qui ouvre les ports MIDI et
    // dispute l'écriture du show à l'instance visible.
    let tick_ms = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
        conduite_control_http::epoch_ms(),
    ));
    let http = match spawn_http(&config, HttpDeps {
        cmd_tx: cmd_tx.clone(),
        state_rx: state_rx.clone(),
        events_rx: events_tx.subscribe(),
        preview_rx: preview_tx.subscribe(),
        preview_b_rx: preview_b_tx.subscribe(),
        thumb_dir: dirs.thumbs.clone(),
        about: about_info(),
        tick_ms: tick_ms.clone(),
        version: version_string(),
        early_log: log_handles.early_log,
        h264_rx: h264_tx.subscribe(),
        h264_clients: h264_clients.clone(),
        // Sonde paresseuse (un `ffmpeg -encoders` mémorisé au 1er client).
        h264_available: std::sync::Arc::new(conduite_engine::h264_mf_available),
    }) {
        Ok(handle) => Some(handle),
        Err(code) => return code,
    };

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
            h264_tx,
            h264_clients,
            tick_ms,
        },
    );

    let code = if cli.headless {
        info!(target: "app", "mode headless (--headless)");
        run_headless(session, http)
    } else {
        run_windowed(session, http)
    };
    info!(target: "app", code, "arrêt de Conduite");
    code
}

/// Données « À propos » servies sur `GET /about` (l'affichage est fait par
/// la webui, onglet Réglages) : version, licence, crédits, liens.
fn about_info() -> serde_json::Value {
    json!({
        "name": "Conduite",
        "description": conduite_core::ui_text::about::DESCRIPTION,
        "version": env!("CARGO_PKG_VERSION"),
        "git": env!("CONDUITE_GIT_HASH"),
        "license": "MIT",
        "copyright": "© 2026 Pym",
        "website": "https://github.com/pymenvert/videocuelist",
        "credits": [
            {
                "name": "FFmpeg",
                "role": conduite_core::ui_text::about::ROLE_FFMPEG,
                "license": "LGPL v3",
                "url": "https://ffmpeg.org",
                "notice": "licenses/FFMPEG.txt"
            },
            {
                "name": conduite_core::ui_text::about::NAME_RUST_DEPS,
                "role": conduite_core::ui_text::about::ROLE_RUST_DEPS,
                "license": "MIT / Apache-2.0 / BSD / ISC / Zlib",
                "notice": "licenses/THIRD-PARTY-NOTICES.html"
            },
            {
                "name": conduite_core::ui_text::about::NAME_SHADERS,
                "role": conduite_core::ui_text::about::ROLE_SHADERS,
                "license": "© Pym — Pack Sources Dome-Native",
                "notice": "shaders/CREDITS.txt"
            }
        ]
    })
}

/// Démarre le serveur HTTP. Échec = code de sortie (`Err`) : sans UI web,
/// un moteur invisible qui ouvre quand même MIDI/OSC et l'autosave est un
/// ZOMBIE dangereux — on refuse net (P0 double lancement).
fn spawn_http(config: &AppConfig, deps: HttpDeps) -> Result<HttpServerHandle, i32> {
    let addr: SocketAddr = match format!("{}:{}", config.http_bind, config.http_port).parse() {
        Ok(a) => a,
        Err(e) => {
            error!(target: "app", bind = %config.http_bind, port = config.http_port,
                error = %e, "adresse HTTP invalide (config.toml) — démarrage refusé");
            eprintln!(
                "Adresse web invalide dans config.toml : {}:{} ({e}).",
                config.http_bind, config.http_port
            );
            return Err(EXIT_GENERIC);
        }
    };
    match HttpServer::spawn(addr, deps) {
        Ok(handle) => {
            info!(target: "app", addr = %handle.local_addr(), "web UI disponible");
            Ok(handle)
        }
        Err(e) => {
            error!(target: "app", %addr, error = %e,
                "port web indisponible : Conduite est probablement déjà lancé — \
                 démarrage refusé (code 10, ne pas relancer)");
            eprintln!(
                "Conduite est déjà lancé (ou le port {} est pris par un autre \
                 logiciel).\nOuvrez http://localhost:{} — ou fermez l'autre \
                 instance puis relancez.",
                config.http_port, config.http_port
            );
            Err(EXIT_PORT_BUSY)
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
        armed: true,
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
/// Sort proprement (sauvegarde si modifié) sur `Command::Quit` ou Ctrl-C.
fn run_headless(mut session: Session, _http: Option<HttpServerHandle>) -> i32 {
    let period = Duration::from_secs_f64(1.0 / f64::from(session.target_fps().max(1)));
    let mut gfx = Gfx::headless();
    let mut next = Instant::now();
    loop {
        session.tick(&mut gfx);
        let _ = session.take_outputs_dirty(); // pas de fenêtres en headless
        if session.take_quit() || platform::quit_requested() {
            info!(target: "app", "arrêt propre demandé (Quit / Ctrl-C)");
            session.emergency_save();
            return EXIT_OK;
        }
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

/// Période du poll de topologie des moniteurs (reconnexion projecteur).
const MONITOR_POLL_PERIOD: Duration = Duration::from_secs(2);

/// Application winit : fenêtres de sortie GL + tick cadencé.
struct App {
    session: Session,
    gfx: Gfx,
    period: Duration,
    next_frame: Instant,
    last_monitor_poll: Instant,
    /// Code de sortie décidé par la boucle (Quit = 0, perte GPU = 11).
    exit_code: i32,
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
            // Topologie des moniteurs (~0,5 Hz) : ré-application du plein
            // écran sur le moniteur retrouvé, repli fenêtré + warning sinon.
            if now.duration_since(self.last_monitor_poll) >= MONITOR_POLL_PERIOD {
                self.last_monitor_poll = now;
                self.gfx.poll_monitors(el);
            }
            self.session.tick(&mut self.gfx);
            // Anti-veille : maintenu tant qu'au moins une sortie est active.
            platform::keep_awake(self.gfx.ready());

            // Perte GPU (TDR / échecs GL fatals répétés) : log + sauvegarde
            // + sortie code 11 — le watchdog relance en < 5 s.
            if let Some(msg) = self.gfx.take_fatal() {
                error!(target: "app", "PERTE GPU : {msg} — sauvegarde puis sortie code 11");
                self.session.emergency_save();
                self.exit_code = EXIT_GPU_LOST;
                el.exit();
                return;
            }
            // Arrêt propre (Command::Quit / Ctrl-C) : sauvegarde si modifié.
            if self.session.take_quit() || platform::quit_requested() {
                info!(target: "app", "arrêt propre demandé (Quit / Ctrl-C)");
                self.session.emergency_save();
                self.exit_code = EXIT_OK;
                el.exit();
                return;
            }
            self.next_frame += self.period;
            if self.next_frame <= now {
                self.next_frame = now + self.period;
            }
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

/// Mode fenêtré ; si winit est indisponible (session RDP minimale…),
/// bascule headless — l'app tourne quand même. Retourne le code de sortie.
fn run_windowed(session: Session, http: Option<HttpServerHandle>) -> i32 {
    let event_loop = match EventLoop::new() {
        Ok(el) => el,
        Err(e) => {
            error!(target: "app", error = %e,
                "boucle d'événements impossible : bascule headless");
            return run_headless(session, http);
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    let period = Duration::from_secs_f64(1.0 / f64::from(session.target_fps().max(1)));
    let mut app = App {
        session,
        gfx: Gfx::headless(),
        period,
        next_frame: Instant::now(),
        last_monitor_poll: Instant::now(),
        exit_code: EXIT_OK,
        _http: http,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        warn!(target: "app", error = %e, "boucle d'événements terminée sur erreur");
        if app.exit_code == EXIT_OK {
            app.exit_code = EXIT_GENERIC;
        }
    }
    // L'anti-veille est relâché avant la sortie.
    platform::keep_awake(false);
    app.exit_code
}
