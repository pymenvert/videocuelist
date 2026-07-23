//! Journalisation : console + fichier journalier `logs/conduite.log` +
//! diffusion des lignes (niveau ≥ INFO) vers l'UI web via le canal
//! d'événements du serveur HTTP (`StateEvent::LogLine` sérialisé).
//!
//! Fournit aussi le hook de panic : log + tentative de sauvegarde du show
//! courant en `shows/recover-<horodatage>.json`.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer};

/// Garde à conserver vivant pendant toute la vie du process (flush fichier).
pub struct LogGuard {
    _file: tracing_appender::non_blocking::WorkerGuard,
}

/// Initialise tracing : console (ansi) + fichier journalier + broadcast UI.
/// `RUST_LOG` respecté, défaut `info`.
pub fn init(logs_dir: &std::path::Path, events_tx: broadcast::Sender<Value>) -> Option<LogGuard> {
    let appender = tracing_appender::rolling::daily(logs_dir, "conduite.log");
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let console = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_ansi(true);
    let file = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_ansi(false)
        .with_writer(file_writer);
    let ui = UiLogLayer { tx: events_tx };

    match tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(file)
        .with(ui)
        .try_init()
    {
        Ok(()) => Some(LogGuard { _file: guard }),
        Err(e) => {
            eprintln!("initialisation du journal impossible : {e}");
            None
        }
    }
}

/// Layer qui pousse chaque ligne (niveau ≥ INFO) vers l'UI, au format
/// `StateEvent::LogLine` : `{"type":"log_line","level","target","message"}`.
struct UiLogLayer {
    tx: broadcast::Sender<Value>,
}

impl<S> Layer<S> for UiLogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Level : ERROR < WARN < INFO — on garde INFO et plus grave.
        if *meta.level() > Level::INFO {
            return;
        }
        let mut message = String::new();
        event.record(&mut MessageVisitor(&mut message));
        // Broadcast sans abonné = Err : normal avant la première connexion UI.
        let _ = self.tx.send(json!({
            "type": "log_line",
            "level": meta.level().to_string(),
            "target": meta.target(),
            "message": message,
        }));
    }
}

/// Extrait le champ `message` d'un événement tracing.
struct MessageVisitor<'a>(&'a mut String);

impl Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        }
    }
}

// ---------------------------------------------------------------- recovery

/// Instantané de récupération : dossier `shows/` + JSON du show courant.
static RECOVER: OnceLock<Mutex<Option<(PathBuf, String)>>> = OnceLock::new();

fn recover_cell() -> &'static Mutex<Option<(PathBuf, String)>> {
    RECOVER.get_or_init(|| Mutex::new(None))
}

/// Met à jour l'instantané utilisé par le hook de panic (appelé par la
/// session après chaque édition / sauvegarde).
pub fn set_recover_snapshot(shows_dir: PathBuf, show_json: String) {
    match recover_cell().lock() {
        Ok(mut guard) => *guard = Some((shows_dir, show_json)),
        Err(poisoned) => *poisoned.into_inner() = Some((shows_dir, show_json)),
    }
}

/// Installe le hook de panic : log + sauvegarde `shows/recover-<ts>.json`.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(target: "app", "PANIC : {info}");
        let snapshot = match recover_cell().lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        if let Some((shows_dir, json)) = snapshot {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let path = shows_dir.join(format!("recover-{stamp}.json"));
            match std::fs::write(&path, json) {
                Ok(()) => tracing::error!(target: "app",
                    path = %path.display(), "show sauvegardé pour récupération"),
                Err(e) => tracing::error!(target: "app", error = %e,
                    "sauvegarde de récupération impossible"),
            }
        }
        default(info);
    }));
}
