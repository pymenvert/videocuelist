//! Serveur HTTP/WS de la web UI : webui embarquée, WebSocket d'état et de
//! commandes, préviews MJPEG, vignettes.
//!
//! Le serveur tourne sur son propre thread avec un runtime tokio dédié :
//! `app` reste maître de sa boucle de rendu, la régie web ne peut pas la
//! bloquer. Toutes les erreurs runtime sont tracées et dégradées proprement.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use crossbeam_channel::Sender;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Notify};
use tokio::time::MissedTickBehavior;
use tokio_stream::wrappers::BroadcastStream;

use conduite_core::{Command, Source};

use crate::assets;

/// Boundary des flux MJPEG (`multipart/x-mixed-replace`).
const MJPEG_BOUNDARY: &str = "frame";
/// Cadence maximale des trames `dyn` vers chaque client WS (10 Hz).
const DYN_PERIOD: Duration = Duration::from_millis(100);

/// Dépendances injectées par `app` au démarrage du serveur.
pub struct HttpDeps {
    /// Commandes sortantes vers le moteur (la web UI émet `Source::Ui`).
    pub cmd_tx: Sender<(Source, Command)>,
    /// État UI complet sérialisé par `app` : `{ "show": …, "runtime": … }`.
    pub state_rx: watch::Receiver<Value>,
    /// `StateEvent` sérialisés + lignes de journal.
    pub events_rx: broadcast::Receiver<Value>,
    /// Frames JPEG de la préview program (~8 fps côté producteur).
    pub preview_rx: broadcast::Receiver<Bytes>,
    /// Frames JPEG de la préview de la cue standby.
    pub preview_b_rx: broadcast::Receiver<Bytes>,
    /// Dossier du cache de vignettes (`<id>.jpg`).
    pub thumb_dir: PathBuf,
}

/// État partagé du routeur (cloné par connexion, tout est `Arc`).
#[derive(Clone)]
struct AppState {
    inner: Arc<HttpDeps>,
}

/// Serveur HTTP de contrôle — voir [`HttpServer::spawn`].
pub struct HttpServer;

/// Poignée du serveur : adresse réelle + arrêt. `Drop` arrête le serveur.
pub struct HttpServerHandle {
    addr: SocketAddr,
    shutdown: Arc<Notify>,
    thread: Option<thread::JoinHandle<()>>,
}

impl HttpServer {
    /// Démarre le serveur sur `addr` (port 0 accepté : port éphémère) dans
    /// un thread dédié avec son propre runtime tokio. Retourne dès que le
    /// port est lié — `local_addr()` est immédiatement fiable.
    pub fn spawn(addr: SocketAddr, deps: HttpDeps) -> io::Result<HttpServerHandle> {
        let listener = std::net::TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let local = listener.local_addr()?;
        let shutdown = Arc::new(Notify::new());
        let sd = shutdown.clone();

        let thread = thread::Builder::new()
            .name("conduite-http".into())
            .spawn(move || run_server(listener, deps, sd, local))?;

        Ok(HttpServerHandle {
            addr: local,
            shutdown,
            thread: Some(thread),
        })
    }
}

impl HttpServerHandle {
    /// Adresse réellement liée (utile avec le port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Arrête le serveur et joint le thread.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.shutdown.notify_one();
        if let Some(t) = self.thread.take() {
            if t.join().is_err() {
                tracing::error!(target: "control_http", "le thread HTTP a paniqué");
            }
        }
    }
}

impl Drop for HttpServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Corps du thread serveur : runtime dédié, arrêt net sur notification
/// (les connexions ouvertes — WS, MJPEG — sont coupées, pas attendues).
fn run_server(
    listener: std::net::TcpListener,
    deps: HttpDeps,
    shutdown: Arc<Notify>,
    local: SocketAddr,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(target: "control_http", error = %e, "runtime tokio impossible");
            return;
        }
    };
    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(target: "control_http", error = %e, "écoute TCP impossible");
                return;
            }
        };
        let app = router(AppState {
            inner: Arc::new(deps),
        });
        tracing::info!(target: "control_http", addr = %local, "serveur web démarré");
        tokio::select! {
            _ = shutdown.notified() => {
                tracing::info!(target: "control_http", "arrêt du serveur web");
            }
            res = axum::serve(listener, app) => {
                if let Err(e) = res {
                    tracing::error!(target: "control_http", error = %e, "serveur web arrêté sur erreur");
                }
            }
        }
    });
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset_route))
        .route("/ws", get(ws_route))
        .route("/preview.mjpeg", get(preview_program))
        .route("/preview-b.mjpeg", get(preview_standby))
        .route("/thumb/{file}", get(thumb))
        .with_state(state)
}

/* ------------------------------------------------------------ assets */

async fn index() -> Response {
    if let Some((ct, body)) = assets::asset_dev("index.html") {
        return ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-cache")], body)
            .into_response();
    }
    Html(assets::INDEX_HTML).into_response()
}

async fn asset_route(Path(path): Path<String>) -> Response {
    if let Some((content_type, body)) = assets::asset_dev(&path) {
        return (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            body,
        )
            .into_response();
    }
    match assets::asset(&path) {
        Some((content_type, body)) => (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/* -------------------------------------------------------------- thumbs */

/// `GET /thumb/{id}.jpg` — fichier du cache de vignettes. L'id est un u32 :
/// aucun chemin arbitraire ne peut être lu.
async fn thumb(Path(file): Path<String>, State(st): State<AppState>) -> Response {
    let id = file
        .strip_suffix(".jpg")
        .and_then(|s| s.parse::<u32>().ok());
    let Some(id) = id else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = st.inner.thumb_dir.join(format!("{id}.jpg"));
    match tokio::fs::read(&path).await {
        Ok(data) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "max-age=10"),
            ],
            data,
        )
            .into_response(),
        Err(e) => {
            tracing::debug!(target: "control_http", id, error = %e, "vignette absente");
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

/* --------------------------------------------------------------- MJPEG */

/// `?deck=preview` (compat INTERFACES.md) renvoie le flux de la cue standby,
/// comme `/preview-b.mjpeg`.
async fn preview_program(
    Query(params): Query<std::collections::HashMap<String, String>>,
    State(st): State<AppState>,
) -> Response {
    if params.get("deck").map(String::as_str) == Some("preview") {
        mjpeg_response(st.inner.preview_b_rx.resubscribe())
    } else {
        mjpeg_response(st.inner.preview_rx.resubscribe())
    }
}

async fn preview_standby(State(st): State<AppState>) -> Response {
    mjpeg_response(st.inner.preview_b_rx.resubscribe())
}

/// Flux `multipart/x-mixed-replace` alimenté par le broadcast de frames
/// JPEG. Un client en retard saute des frames (jamais de backpressure sur
/// le producteur).
fn mjpeg_response(rx: broadcast::Receiver<Bytes>) -> Response {
    let stream =
        tokio_stream::StreamExt::filter_map(BroadcastStream::new(rx), |item| match item {
            Ok(jpeg) => Some(Ok::<Bytes, std::convert::Infallible>(mjpeg_part(&jpeg))),
            Err(_) => None, // client en retard : frames sautées
        });
    match Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/x-mixed-replace; boundary={MJPEG_BOUNDARY}"),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(target: "control_http", error = %e, "réponse MJPEG impossible");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Encadre une frame JPEG pour le flux multipart.
fn mjpeg_part(jpeg: &Bytes) -> Bytes {
    let header = format!(
        "--{MJPEG_BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        jpeg.len()
    );
    let mut out = Vec::with_capacity(header.len() + jpeg.len() + 2);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(jpeg);
    out.extend_from_slice(b"\r\n");
    Bytes::from(out)
}

/* ------------------------------------------------------------ WebSocket */

async fn ws_route(ws: WebSocketUpgrade, State(st): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, st))
}

type WsSink = SplitSink<WebSocket, Message>;

async fn send_json(tx: &mut WsSink, value: &Value) -> Result<(), axum::Error> {
    let text = match serde_json::to_string(value) {
        Ok(t) => t,
        Err(e) => {
            // Value → String n'échoue pas en pratique ; on trace par principe.
            tracing::error!(target: "control_http", error = %e, "sérialisation WS impossible");
            return Ok(());
        }
    };
    tx.send(Message::Text(text.into())).await
}

/// Protocole par connexion :
/// 1. à l'ouverture : `{"type":"hello","state":<watch courant>}` ;
/// 2. relaye chaque événement : `{"type":"event","event":…}` ;
/// 3. sur changement du watch, throttlé à 10 Hz : `{"type":"dyn","runtime":…}` ;
/// 4. entrant `{"type":"cmd","cmd":{…Command}}` → `cmd_tx` (Source::Ui) ;
///    `{"type":"ping"}` → `{"type":"pong"}`.
async fn handle_ws(socket: WebSocket, st: AppState) {
    let (mut tx, mut rx) = socket.split();
    let mut state_rx = st.inner.state_rx.clone();
    let mut events_rx = st.inner.events_rx.resubscribe();

    let hello = json!({ "type": "hello", "state": state_rx.borrow_and_update().clone() });
    if send_json(&mut tx, &hello).await.is_err() {
        return;
    }
    tracing::debug!(target: "control_http", "client WS connecté");

    let mut tick = tokio::time::interval(DYN_PERIOD);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut state_alive = true;
    let mut events_alive = true;

    loop {
        tokio::select! {
            // Trames dynamiques : au plus une par tick, seulement si l'état a changé.
            _ = tick.tick() => {
                if !state_alive {
                    continue;
                }
                match state_rx.has_changed() {
                    Ok(true) => {
                        let runtime = state_rx
                            .borrow_and_update()
                            .get("runtime")
                            .cloned()
                            .unwrap_or(Value::Null);
                        let msg = json!({ "type": "dyn", "runtime": runtime });
                        if send_json(&mut tx, &msg).await.is_err() {
                            break;
                        }
                    }
                    Ok(false) => {}
                    Err(_) => {
                        tracing::debug!(target: "control_http", "canal d'état fermé — plus de trames dyn");
                        state_alive = false;
                    }
                }
            }

            // Événements moteur (StateEvent + journal), si le canal vit encore.
            ev = events_rx.recv(), if events_alive => match ev {
                Ok(v) => {
                    let msg = json!({ "type": "event", "event": v });
                    if send_json(&mut tx, &msg).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(target: "control_http", skipped = n, "client WS en retard, événements sautés");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::debug!(target: "control_http", "canal d'événements fermé");
                    events_alive = false;
                }
            },

            // Messages du client.
            msg = rx.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if handle_client_text(&st, text.as_str(), &mut tx).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // binaire / ping-pong protocole : ignorés
                Some(Err(e)) => {
                    tracing::debug!(target: "control_http", error = %e, "erreur WS côté client");
                    break;
                }
            },
        }
    }
    tracing::debug!(target: "control_http", "client WS déconnecté");
}

/// Traite un message texte entrant. Un message invalide est tracé et ignoré
/// (le client ne peut pas faire tomber la régie) ; seul un échec d'envoi
/// sortant coupe la connexion.
async fn handle_client_text(
    st: &AppState,
    text: &str,
    tx: &mut WsSink,
) -> Result<(), axum::Error> {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "control_http", error = %e, "message WS illisible");
            return Ok(());
        }
    };
    match value.get("type").and_then(Value::as_str) {
        Some("cmd") => {
            let cmd_value = value.get("cmd").cloned().unwrap_or(Value::Null);
            match serde_json::from_value::<Command>(cmd_value) {
                Ok(cmd) => {
                    if let Err(e) = st.inner.cmd_tx.send((Source::Ui, cmd)) {
                        tracing::error!(target: "control_http", error = %e, "canal de commandes fermé");
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "control_http", error = %e, "commande WS invalide, ignorée");
                }
            }
            Ok(())
        }
        Some("ping") => send_json(tx, &json!({ "type": "pong" })).await,
        other => {
            tracing::debug!(target: "control_http", kind = ?other, "message WS inconnu, ignoré");
            Ok(())
        }
    }
}
