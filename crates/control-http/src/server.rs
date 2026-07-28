//! Serveur HTTP/WS de la web UI : webui embarquée, WebSocket d'état et de
//! commandes, préviews MJPEG, vignettes.
//!
//! Le serveur tourne sur son propre thread avec un runtime tokio dédié :
//! `app` reste maître de sa boucle de rendu, la régie web ne peut pas la
//! bloquer. Toutes les erreurs runtime sont tracées et dégradées proprement.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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

/// Message du flux préview H.264 (`GET /preview.h264`, WebSocket). Produit
/// par la session (app), diffusé tel quel à chaque client connecté.
#[derive(Debug, Clone)]
pub enum H264Msg {
    /// Handshake JSON du contrat — `{"codec","format":"annexb","width",
    /// "height","fps"}`. Ré-émis par la session à chaque nouveau client et à
    /// chaque (re)démarrage d'encodeur ; le serveur garantit qu'il précède
    /// toute frame binaire sur chaque connexion.
    Config(Value),
    /// Un access unit H.264 Annex-B complet (message binaire WS).
    Au(Bytes),
}

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
    /// Bloc « À propos » statique servi sur `GET /about` : version, licence,
    /// crédits, liens — construit par `app` au démarrage.
    pub about: Value,
    /// Horodatage (millisecondes UNIX) du dernier tick de rendu, mis à jour
    /// par la session à chaque frame — `GET /health` détecte « vivant mais
    /// figé ».
    pub tick_ms: Arc<AtomicU64>,
    /// Version de l'application (CARGO_PKG_VERSION du binaire).
    pub version: String,
    /// Lignes de journal WARN/ERROR capturées depuis le démarrage (borné) :
    /// rejouées à chaque nouvelle connexion WS pour que les erreurs émises
    /// AVANT la connexion (bind OSC raté…) apparaissent dans le journal web.
    pub early_log: Arc<Mutex<Vec<Value>>>,
    /// Flux préview H.264 (`GET /preview.h264`) : config + access units
    /// produits par la session.
    pub h264_rx: broadcast::Receiver<H264Msg>,
    /// Nombre de clients H.264 connectés — la session démarre/arrête
    /// l'encodeur ffmpeg selon ce compteur (0 client = pas de process).
    pub h264_clients: Arc<AtomicUsize>,
    /// `h264_mf` est-il disponible ? Sondé PARESSEUSEMENT (premier appel =
    /// un `ffmpeg -encoders`, mémorisé côté engine) : `false` ⇒ l'endpoint
    /// répond 503 et le client reste en MJPEG (contrat).
    pub h264_available: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// Âge de tick au-delà duquel `/health` répond `stalled` (moteur figé).
const HEALTH_STALL_MS: u64 = 2_000;

/// Millisecondes UNIX courantes (horloge système).
pub fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
        .route("/preview.h264", get(preview_h264))
        .route("/thumb/{file}", get(thumb))
        .route("/about", get(about))
        .route("/health", get(health))
        .with_state(state)
}

/// `GET /health` — contrat supervision : `{ status, tick_age_ms, version }`.
/// `status` passe à `"stalled"` quand le tick de rendu n'a pas avancé depuis
/// plus de 2 s (le pire mode de panne : « vivant mais figé »).
async fn health(State(st): State<AppState>) -> Response {
    let last = st.inner.tick_ms.load(Ordering::Relaxed);
    let age = epoch_ms().saturating_sub(last);
    let status = if age <= HEALTH_STALL_MS { "ok" } else { "stalled" };
    axum::Json(json!({
        "status": status,
        "tick_age_ms": age,
        "version": st.inner.version,
    }))
    .into_response()
}

/// `GET /about` — données « À propos » (JSON statique) : version, licence,
/// crédits tiers, liens. L'affichage est fait par la webui (Réglages).
async fn about(State(st): State<AppState>) -> Response {
    axum::Json(st.inner.about.clone()).into_response()
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

/* -------------------------------------------------------- préview H.264 */

/// Garde de comptage des clients H.264 : incrémente à la connexion,
/// décrémente au drop (déconnexion, erreur, arrêt serveur) — la session
/// observe ce compteur pour démarrer/arrêter l'encodeur ffmpeg.
struct H264ClientGuard(Arc<AtomicUsize>);

impl H264ClientGuard {
    fn new(counter: Arc<AtomicUsize>) -> H264ClientGuard {
        counter.fetch_add(1, Ordering::SeqCst);
        H264ClientGuard(counter)
    }
}

impl Drop for H264ClientGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `GET /preview.h264` — préview H.264 sur WebSocket (contrat
/// docs/INTERFACES.md) : 1er message texte = config JSON
/// `{"codec","format":"annexb","width","height","fps"}`, puis frames
/// binaires Annex-B. Si `h264_mf` est indisponible : **503** (le client
/// reste en MJPEG). Requête non-WebSocket : 426 (l'endpoint existe).
async fn preview_h264(
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
    State(st): State<AppState>,
) -> Response {
    // Sonde bloquante (un `ffmpeg -encoders` mémorisé) : hors du runtime.
    let probe = st.inner.h264_available.clone();
    let available = tokio::task::spawn_blocking(move || probe())
        .await
        .unwrap_or(false);
    if !available {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "encodeur h264_mf indisponible : rester en MJPEG",
        )
            .into_response();
    }
    match ws {
        Ok(ws) => ws.on_upgrade(move |socket| handle_h264(socket, st)),
        Err(_) => (
            StatusCode::UPGRADE_REQUIRED,
            "préview H.264 : WebSocket attendu",
        )
            .into_response(),
    }
}

/// Boucle d'une connexion préview H.264 : garantit que la config précède
/// toute frame binaire ; un client en retard saute des access units (il se
/// resynchronise au keyframe suivant, jamais de backpressure).
async fn handle_h264(socket: WebSocket, st: AppState) {
    // S'abonner AVANT d'annoncer le client : la config que la session
    // ré-émet en voyant le compteur monter ne peut pas être ratée.
    let mut rx = st.inner.h264_rx.resubscribe();
    let _guard = H264ClientGuard::new(st.inner.h264_clients.clone());
    let (mut tx, mut client) = socket.split();
    let mut configured = false;
    tracing::debug!(target: "control_http", "client préview H.264 connecté");
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(H264Msg::Config(v)) => {
                    let Ok(text) = serde_json::to_string(&v) else { continue };
                    if tx.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                    configured = true;
                }
                Ok(H264Msg::Au(bytes)) => {
                    // Le 1er message de chaque connexion DOIT être la config.
                    if !configured {
                        continue;
                    }
                    if tx.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(target: "control_http", skipped = n,
                        "client H.264 en retard : access units sautés");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            m = client.next() => match m {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // le client n'a rien à nous dire
                Some(Err(e)) => {
                    tracing::debug!(target: "control_http", error = %e,
                        "erreur WS préview H.264");
                    break;
                }
            },
        }
    }
    tracing::debug!(target: "control_http", "client préview H.264 déconnecté");
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
/// 3. sur changement du watch, throttlé à 10 Hz :
///    `{"type":"dyn","runtime":…}` + champ `fft` ({bins, device}) quand une
///    entrée audio est active (absent sinon — contrat WS) ;
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
    // Rejoue les WARN/ERROR émis avant cette connexion (bind OSC raté au
    // démarrage…) : sans cela le journal web ne voit jamais les erreurs
    // antérieures à l'ouverture de la page.
    let replay: Vec<Value> = match st.inner.early_log.lock() {
        Ok(lines) => lines.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    for line in replay {
        let msg = json!({ "type": "event", "event": line });
        if send_json(&mut tx, &msg).await.is_err() {
            return;
        }
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
                        let (runtime, fft) = {
                            let state = state_rx.borrow_and_update();
                            (
                                state.get("runtime").cloned().unwrap_or(Value::Null),
                                state.get("fft").cloned(),
                            )
                        };
                        let mut msg = json!({ "type": "dyn", "runtime": runtime });
                        if let Some(fft) = fft {
                            if !fft.is_null() {
                                msg["fft"] = fft;
                            }
                        }
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
                    // try_send : le bus est borné et ce handler tourne sur le
                    // runtime mono-thread du serveur — un send bloquant sur
                    // bus plein gèlerait TOUTE la web UI. Erreur throttlée.
                    if let Err(e) = st.inner.cmd_tx.try_send((Source::Ui, cmd)) {
                        static LAST_WARN_S: AtomicU64 = AtomicU64::new(0);
                        let now_s = epoch_ms() / 1000;
                        if LAST_WARN_S.swap(now_s, Ordering::Relaxed) != now_s {
                            tracing::warn!(target: "control_http", error = %e,
                                "bus de commandes saturé ou fermé : commande WS perdue");
                        }
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
