//! Backend ffmpeg : un process `ffmpeg` par média, sortie rawvideo RGBA
//! sur stdout, thread lecteur → ring buffer borné (backpressure via le pipe).
//!
//! Tout le CYCLE DE VIE du process (spawn, respawn de boucle, seek, relance
//! après crash, kill/wait) vit dans un thread superviseur dédié : le thread
//! appelant (rendu) ne fait que des opérations non bloquantes — `poll_frame`
//! lit le ring, `seek`/`set_playback` postent une commande.
//!
//! - vitesse = cadence de consommation (dup/skip dans [`Pacer`]) ;
//! - pause = on ne consomme plus (le pipe se remplit, ffmpeg bloque) ;
//! - seek = commande au superviseur (kill + respawn hors thread appelant ;
//!   en attendant, `poll_frame` rend `None` et l'app tient la dernière frame) ;
//! - EOF = pipe fermé → selon [`EndMode`] : Loop relance (par le superviseur),
//!   Hold/Black/FollowNext passent `eof()` à vrai une fois le ring vidé ;
//! - PingPong v1 = traité comme Loop (warn) ;
//! - mort prématurée du process : 1 relance automatique, puis `healthy() = false` ;
//! - **zéro zombie** : le child est récolté (`wait`) dès la fin de son flux,
//!   sans attendre le drop — et kill + wait + join au drop ;
//! - buffers de frames recyclés via [`BufferPool`] (pas d'allocation zérotée
//!   par frame : restitution automatique au drop de la frame).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::Context;
use conduite_core::{EndMode, Playback};
use tracing::{debug, error, warn};

use crate::pacing::Pacer;
use crate::pool::BufferPool;
use crate::probe::{no_window, probe_with_codec, resolve_ffmpeg};
use crate::ring::{FrameRing, RING_CAPACITY};
use crate::{FrameRgba, MediaInfo, PixelOrder, Player};

const LOG: &str = "engine::ffmpeg";
/// Tolérance de fin naturelle : si le flux s'arrête à moins de 4 frames de la
/// fin attendue, on considère l'EOF comme normal.
const EOF_TOLERANCE_FRAMES: f64 = 4.0;
/// Buffers gardés en réserve par player : ring + frame affichée + marge.
const POOL_SPARES: usize = RING_CAPACITY + 2;

/// Options de génération de la ligne de commande ffmpeg (pur, testé).
#[derive(Debug, Clone, Copy, Default)]
struct SpawnOpts {
    /// `-stream_loop -1` (boucle du fichier entier depuis 0).
    stream_loop: bool,
    /// Sortie `-pix_fmt bgra` au lieu de `rgba` (upload GL sans swizzle,
    /// activé par le compositor via [`crate::set_decode_bgra`]).
    bgra: bool,
    /// `-hwaccel d3d11va` (décodage matériel Windows, frames redescendues en
    /// mémoire système — la sortie rawvideo sur pipe reste identique).
    hwaccel_d3d11va: bool,
}

/// Arguments ffmpeg pour lire `path` en RGBA/BGRA brut depuis `start_s`.
/// Pur (testé) : `stop_s` borne la lecture (durée de sortie `-t stop_s - start_s`).
fn build_args(
    path: &Path,
    start_s: f64,
    stop_s: Option<f64>,
    opts: SpawnOpts,
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    let push = |args: &mut Vec<std::ffi::OsString>, s: &str| args.push(s.into());
    push(&mut args, "-v");
    push(&mut args, "error");
    push(&mut args, "-nostdin");
    if opts.hwaccel_d3d11va {
        // Option d'ENTRÉE : avant -i. Pas de -hwaccel_output_format : les
        // frames décodées redescendent en mémoire système puis swscale
        // convertit vers rgba/bgra comme au chemin logiciel.
        push(&mut args, "-hwaccel");
        push(&mut args, "d3d11va");
    }
    if opts.stream_loop {
        push(&mut args, "-stream_loop");
        push(&mut args, "-1");
    }
    if start_s > 0.0 {
        push(&mut args, "-ss");
        args.push(format!("{start_s:.6}").into());
    }
    push(&mut args, "-i");
    args.push(path.into());
    if let Some(stop) = stop_s {
        // Après un seek d'entrée (-ss avant -i), l'horloge de sortie repart à
        // zéro : on borne donc par une DURÉE (-t stop-start) et non par -to.
        let dur = (stop - start_s).max(0.0);
        push(&mut args, "-t");
        args.push(format!("{dur:.6}").into());
    }
    push(&mut args, "-an");
    push(&mut args, "-sn");
    push(&mut args, "-f");
    push(&mut args, "rawvideo");
    push(&mut args, "-pix_fmt");
    push(&mut args, if opts.bgra { "bgra" } else { "rgba" });
    push(&mut args, "pipe:1");
    args
}

// ---- Décodage matériel D3D11VA (Windows uniquement) ----

/// Codecs pour lesquels le décodage matériel D3D11VA est tenté (pur, testé).
fn codec_supports_d3d11va(codec: Option<&str>) -> bool {
    matches!(codec, Some("h264") | Some("hevc"))
}

/// Surface minimale (pixels) pour que le D3D11VA vaille son coût : la
/// création du device D3D11 ajoute ~0,1-0,3 s à CHAQUE lancement de process
/// (donc à chaque seek et à chaque cycle de boucle in/out). Sous ~720p le
/// décodage logiciel est déjà trivial — on ne paie l'init que là où le gain
/// CPU (-60-80 % en 4K) est réel.
const HWACCEL_MIN_PIXELS: u32 = 1280 * 720;

/// Le média justifie-t-il le décodage matériel ? (pur, testé)
fn hwaccel_worthwhile(width: u32, height: u32) -> bool {
    width.saturating_mul(height) >= HWACCEL_MIN_PIXELS
}

/// D3D11VA en échec pour cette session : plus aucune tentative jusqu'au
/// prochain lancement de l'application.
#[cfg(windows)]
static HWACCEL_BROKEN: AtomicBool = AtomicBool::new(false);

/// Le décodage matériel est-il encore candidat pour cette session ?
fn hwaccel_session_available() -> bool {
    #[cfg(windows)]
    {
        !HWACCEL_BROKEN.load(Ordering::Relaxed)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Mémorise l'échec D3D11VA pour toute la session (log une seule fois).
fn disable_hwaccel_for_session() {
    #[cfg(windows)]
    if !HWACCEL_BROKEN.swap(true, Ordering::Relaxed) {
        warn!(
            target: LOG,
            "décodage matériel d3d11va en échec : repli sur le décodage \
             logiciel pour toute la session"
        );
    }
}

/// Fin de segment sur la ligne de temps média (out, sinon durée, sinon rien).
fn segment_end_s(info: &MediaInfo, pb: &Playback) -> Option<f64> {
    let dur = (info.duration_s > 0.0).then_some(info.duration_s);
    match pb.out_s {
        Some(out) => Some(out.min(dur.unwrap_or(out))),
        None => dur,
    }
}

/// Longueur du segment lu (0 si inconnue).
fn segment_len_s(info: &MediaInfo, pb: &Playback) -> f64 {
    segment_end_s(info, pb).map(|e| (e - pb.in_s).max(0.0)).unwrap_or(0.0)
}

fn loops(pb: &Playback) -> bool {
    matches!(pb.end, EndMode::Loop | EndMode::PingPong)
}

/// Pts média (dans le segment) d'un pts de flux monotone.
fn media_pts_of(info: &MediaInfo, pb: &Playback, stream_pts: f64) -> f64 {
    let seg = segment_len_s(info, pb);
    if seg > 0.0 {
        pb.in_s + (stream_pts % seg)
    } else {
        pb.in_s + stream_pts
    }
}

/// Fin naturelle du flux (pur, testé) : le dernier pts poussé arrive à moins
/// de [`EOF_TOLERANCE_FRAMES`] de la fin attendue, ou — fin attendue inconnue
/// (INFINITY) — le process s'est terminé avec un code succès.
fn is_natural_end(
    last_pts_s: f64,
    spawn_end_stream_s: f64,
    frame_dur_s: f64,
    clean_exit: bool,
) -> bool {
    last_pts_s >= spawn_end_stream_s - EOF_TOLERANCE_FRAMES * frame_dur_s
        || (spawn_end_stream_s.is_infinite() && clean_exit)
}

/// Messages reçus par le thread superviseur.
enum Msg {
    /// Seek/respawn à la position média `pos_s`, avec la playback courante.
    Seek { pb: Playback, pos_s: f64 },
    /// Le thread lecteur de la génération donnée a terminé (dernier pts poussé).
    ReaderDone { generation: u64, last_pts_s: Option<f64> },
    /// Arrêt du player (drop).
    Shutdown,
}

/// État partagé entre le player (thread de rendu) et le superviseur.
struct Shared {
    ring: FrameRing,
    /// Flux terminé (plus aucune frame à venir) — hors boucle.
    ended: AtomicBool,
    healthy: AtomicBool,
    /// Seeks postés non encore réalisés : `poll_frame` rend `None` en
    /// attendant (les frames du process précédent n'ont plus cours).
    pending_seeks: AtomicU32,
}

/// Lecteur vidéo adossé à un process ffmpeg piloté par un thread superviseur.
pub struct FfmpegPlayer {
    info: MediaInfo,
    pb: Playback,
    pacer: Pacer,
    playing: bool,
    /// Cache local : vrai une fois le flux terminé ET le ring vidé.
    eof: bool,
    warned_pingpong: bool,
    shared: Arc<Shared>,
    tx: Sender<Msg>,
    supervisor: Option<JoinHandle<()>>,
}

impl FfmpegPlayer {
    /// Ouvre `path` (sondé via ffprobe) et précharge : process lancé,
    /// premières frames en buffer, lecture en pause. Bloquant : à appeler
    /// hors du thread de rendu (chargement).
    pub fn open(path: &Path, pb: &Playback) -> anyhow::Result<Self> {
        let (info, codec) = probe_with_codec(path)?;
        let shared = Arc::new(Shared {
            ring: FrameRing::new(),
            ended: AtomicBool::new(false),
            healthy: AtomicBool::new(true),
            pending_seeks: AtomicU32::new(0),
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let mut supervisor = Supervisor::new(
            path.to_path_buf(),
            info,
            codec,
            pb.clone(),
            Arc::clone(&shared),
            tx.clone(),
            rx,
        );
        // Préchargement synchrone : un échec de lancement est rendu à l'appelant.
        supervisor.spawn_at(pb.in_s).context("préchargement ffmpeg")?;
        let handle = std::thread::Builder::new()
            .name("ffmpeg-supervisor".into())
            .spawn(move || supervisor.run())
            .context("thread superviseur")?;

        let mut player = FfmpegPlayer {
            info,
            pb: pb.clone(),
            pacer: Pacer::new(info.fps, pb.in_s, 0.0),
            playing: false,
            eof: false,
            warned_pingpong: false,
            shared,
            tx,
            supervisor: Some(handle),
        };
        player.apply_playback(pb.clone());
        Ok(player)
    }

    /// Mémorise la playback et recale le pacer (fps/in/segment).
    fn apply_playback(&mut self, pb: Playback) {
        if matches!(pb.end, EndMode::PingPong) && !self.warned_pingpong {
            warn!(target: LOG, "EndMode::PingPong non supporté en v1, traité comme Loop");
            self.warned_pingpong = true;
        }
        self.pb = pb;
        self.pacer = Pacer::new(self.info.fps, self.pb.in_s, segment_len_s(&self.info, &self.pb));
    }
}

impl Player for FfmpegPlayer {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn set_playback(&mut self, pb: &Playback) {
        let needs_respawn =
            pb.in_s != self.pb.in_s || pb.out_s != self.pb.out_s || pb.end != self.pb.end;
        self.apply_playback(pb.clone());
        // La vitesse ne concerne pas le process : c'est l'horloge média de
        // l'app (dup/skip) qui la réalise.
        if needs_respawn {
            self.seek(self.pb.in_s);
        }
    }

    fn play(&mut self) {
        self.playing = true;
    }

    fn pause(&mut self) {
        // Ne plus consommer suffit : ring plein → pipe plein → ffmpeg bloqué.
        self.playing = false;
    }

    fn seek(&mut self, s: f64) {
        let end = segment_end_s(&self.info, &self.pb).unwrap_or(f64::INFINITY);
        let s = s.clamp(self.pb.in_s, end);
        self.eof = false;
        self.pacer = Pacer::new(self.info.fps, self.pb.in_s, segment_len_s(&self.info, &self.pb));
        self.pacer.reset_to(s - self.pb.in_s);
        // Kill + respawn sont réalisés par le superviseur ; en attendant,
        // poll_frame rend None (l'app tient la dernière frame).
        self.shared.pending_seeks.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(Msg::Seek { pb: self.pb.clone(), pos_s: s }).is_err() {
            self.shared.pending_seeks.fetch_sub(1, Ordering::SeqCst);
            self.shared.healthy.store(false, Ordering::SeqCst);
            error!(target: LOG, seek_s = s, "superviseur arrêté : seek impossible");
        }
    }

    fn poll_frame(&mut self, media_time_s: f64) -> Option<FrameRgba> {
        if !self.playing || self.eof {
            return None;
        }
        if self.shared.pending_seeks.load(Ordering::SeqCst) > 0 {
            return None; // seek en cours : frames de l'ancien process sans valeur
        }
        match self.shared.ring.poll(&mut self.pacer, media_time_s) {
            Some(mut frame) => {
                frame.pts_s = media_pts_of(&self.info, &self.pb, frame.pts_s);
                Some(frame)
            }
            None => {
                if self.shared.ended.load(Ordering::SeqCst) && self.shared.ring.is_drained() {
                    self.eof = true;
                }
                None
            }
        }
    }

    fn eof(&self) -> bool {
        self.eof
    }

    fn healthy(&self) -> bool {
        self.shared.healthy.load(Ordering::SeqCst)
    }
}

impl Drop for FfmpegPlayer {
    fn drop(&mut self) {
        // Fermer le ring débloque un lecteur en push, puis le superviseur
        // fait kill + wait + join (Drop de Supervisor).
        self.shared.ring.close();
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(h) = self.supervisor.take() {
            if h.join().is_err() {
                warn!(target: LOG, "le thread superviseur a paniqué");
            }
        }
    }
}

/// Thread superviseur : possède le process ffmpeg et son thread lecteur.
/// Réalise spawn/respawn de boucle/seek/relance/récolte hors du thread de rendu.
struct Supervisor {
    path: PathBuf,
    info: MediaInfo,
    /// Nom du codec vidéo (minuscules, ffprobe) — décide du D3D11VA.
    codec: Option<String>,
    pb: Playback,
    shared: Arc<Shared>,
    /// Sender partagé avec les threads lecteurs (`ReaderDone`).
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pool: BufferPool,
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    /// Génération de process : les `ReaderDone` d'anciennes générations sont ignorés.
    generation: u64,
    /// Relances après mort prématurée depuis le dernier seek/spawn sain.
    retries: u32,
    frame_dur_s: f64,
    /// Pts de flux de la première frame du process courant.
    stream_base_s: f64,
    /// Pts de flux attendu en fin de process courant (INFINITY si sans fin).
    spawn_end_stream_s: f64,
    /// Le process courant a-t-il été lancé avec `-hwaccel d3d11va` ?
    spawn_hwaccel: bool,
}

impl Supervisor {
    fn new(
        path: PathBuf,
        info: MediaInfo,
        codec: Option<String>,
        pb: Playback,
        shared: Arc<Shared>,
        tx: Sender<Msg>,
        rx: Receiver<Msg>,
    ) -> Self {
        let fps = if info.fps.is_finite() && info.fps > 0.0 { info.fps } else { 30.0 };
        let frame_size = (info.width as usize) * (info.height as usize) * 4;
        Supervisor {
            path,
            info,
            codec,
            pb,
            shared,
            tx,
            rx,
            pool: BufferPool::new(frame_size, POOL_SPARES),
            child: None,
            reader: None,
            generation: 0,
            retries: 0,
            frame_dur_s: 1.0 / fps,
            stream_base_s: 0.0,
            spawn_end_stream_s: f64::INFINITY,
            spawn_hwaccel: false,
        }
    }

    fn run(mut self) {
        loop {
            match self.rx.recv() {
                Ok(Msg::Seek { pb, pos_s }) => self.handle_seek(pb, pos_s),
                Ok(Msg::ReaderDone { generation, last_pts_s }) => {
                    self.handle_reader_done(generation, last_pts_s)
                }
                Ok(Msg::Shutdown) | Err(_) => break,
            }
        }
        // Drop de self : kill + wait + join, ring fermé.
    }

    /// (Re)lance le process ffmpeg à la position média `start_s`.
    /// `self.stream_base_s` (pts de flux de la première frame) est à poser
    /// AVANT l'appel.
    fn spawn_at(&mut self, start_s: f64) -> anyhow::Result<()> {
        self.stop_child(); // défensif : l'ancien process d'abord

        // stream_loop seulement pour une boucle du fichier entier depuis 0 :
        // au-delà (in/out), les pts deviendraient faux → on reboucle par respawn.
        let whole_file = self.pb.in_s <= 0.0 && self.pb.out_s.is_none();
        let stream_loop = loops(&self.pb) && whole_file && start_s <= 0.0;
        let stop_s = if stream_loop { None } else { self.pb.out_s };

        self.spawn_end_stream_s = if stream_loop {
            f64::INFINITY
        } else {
            match segment_end_s(&self.info, &self.pb) {
                Some(end) => self.stream_base_s + (end - start_s).max(0.0),
                None => f64::INFINITY, // durée inconnue : EOF = fin naturelle
            }
        };

        let bgra = crate::decode_bgra();
        let hwaccel = hwaccel_session_available()
            && codec_supports_d3d11va(self.codec.as_deref())
            && hwaccel_worthwhile(self.info.width, self.info.height);
        self.spawn_hwaccel = hwaccel;
        let opts = SpawnOpts { stream_loop, bgra, hwaccel_d3d11va: hwaccel };

        let ffmpeg = resolve_ffmpeg();
        let args = build_args(&self.path, start_s, stop_s, opts);
        let mut cmd = Command::new(&ffmpeg);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        no_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("lancement de ffmpeg impossible ({})", ffmpeg.display()))?;
        let stdout = match child.stdout.take() {
            Some(out) => out,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("stdout ffmpeg absent");
            }
        };

        self.generation += 1;
        let generation = self.generation;
        let ring = self.shared.ring.clone();
        ring.reopen(); // le lecteur précédent (terminé) l'avait fermé
        let pool = self.pool.clone();
        let (w, h) = (self.info.width, self.info.height);
        let (frame_dur, base) = (self.frame_dur_s, self.stream_base_s);
        let order = if bgra { PixelOrder::Bgra } else { PixelOrder::Rgba };
        let done = self.tx.clone();
        let reader = std::thread::Builder::new()
            .name("ffmpeg-reader".into())
            .spawn(move || {
                let last_pts_s = read_frames(stdout, ring, &pool, w, h, frame_dur, base, order);
                let _ = done.send(Msg::ReaderDone { generation, last_pts_s });
            });
        let reader = match reader {
            Ok(h) => h,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e).context("thread lecteur");
            }
        };

        debug!(
            target: LOG,
            start_s, stream_loop, bgra, hwaccel,
            path = %self.path.display(),
            "process ffmpeg lancé"
        );
        self.child = Some(child);
        self.reader = Some(reader);
        Ok(())
    }

    /// Seek demandé par le player : kill + respawn à la position, frames de
    /// l'ancienne génération jetées.
    fn handle_seek(&mut self, pb: Playback, pos_s: f64) {
        self.stop_child();
        self.shared.ring.clear(); // les buffers retournent au pool
        self.pb = pb;
        self.retries = 0;
        self.stream_base_s = pos_s - self.pb.in_s;
        self.shared.ended.store(false, Ordering::SeqCst);
        match self.spawn_at(pos_s) {
            Ok(()) => self.shared.healthy.store(true, Ordering::SeqCst),
            Err(e) => {
                error!(target: LOG, error = %e, seek_s = pos_s, "seek : relance impossible");
                self.shared.healthy.store(false, Ordering::SeqCst);
            }
        }
        self.shared.pending_seeks.fetch_sub(1, Ordering::SeqCst);
    }

    /// Le thread lecteur a terminé : le process est récolté IMMÉDIATEMENT
    /// (zéro zombie, même en Hold/Black), puis fin naturelle → boucle/fin de
    /// flux ; mort prématurée → 1 relance, puis `healthy = false`.
    fn handle_reader_done(&mut self, generation: u64, last_pts_s: Option<f64>) {
        if generation != self.generation {
            return; // lecteur d'une génération déjà remplacée (seek)
        }
        let clean_exit = self.reap_child();
        let last = last_pts_s.unwrap_or(self.stream_base_s);

        if is_natural_end(last, self.spawn_end_stream_s, self.frame_dur_s, clean_exit) {
            self.retries = 0;
            if loops(&self.pb) && segment_len_s(&self.info, &self.pb) > 0.0 {
                // Cycle suivant : la base de flux avance d'une longueur de
                // segment ; les frames du cycle fini restent consommables.
                self.stream_base_s = if self.spawn_end_stream_s.is_finite() {
                    self.spawn_end_stream_s
                } else {
                    last + self.frame_dur_s
                };
                if let Err(e) = self.spawn_at(self.pb.in_s) {
                    error!(target: LOG, error = %e, "relance de boucle impossible");
                    self.shared.healthy.store(false, Ordering::SeqCst);
                    self.shared.ended.store(true, Ordering::SeqCst);
                }
            } else {
                // Hold : l'app garde la dernière frame ; Black : l'app affiche
                // noir sur eof() ; FollowNext : le moteur de cues suit sur
                // eof() — posé par le player une fois le ring vidé.
                self.shared.ended.store(true, Ordering::SeqCst);
            }
            return;
        }

        // Mort prématurée avec décodage matériel : le suspect n°1 est le
        // hwaccel lui-même (driver, VRAM, codec refusé). On le coupe pour la
        // session et on relance en logiciel SANS consommer la relance
        // « santé » — un vrai crash logiciel garde son propre filet.
        if self.spawn_hwaccel {
            disable_hwaccel_for_session();
            if last_pts_s.is_some() {
                self.stream_base_s = last + self.frame_dur_s;
            }
            let resume = self.resume_position_s(self.stream_base_s);
            warn!(target: LOG, resume_s = resume, path = %self.path.display(),
                "process ffmpeg (d3d11va) mort en lecture, relance en décodage logiciel");
            if let Err(e) = self.spawn_at(resume) {
                error!(target: LOG, error = %e, "relance impossible");
                self.shared.healthy.store(false, Ordering::SeqCst);
            }
            return;
        }

        // Mort prématurée.
        if self.retries == 0 {
            self.retries = 1;
            let resume = self.resume_position_s(last);
            error!(target: LOG, resume_s = resume, path = %self.path.display(),
                "process ffmpeg mort en lecture, relance");
            self.stream_base_s = last + self.frame_dur_s;
            if let Err(e) = self.spawn_at(resume) {
                error!(target: LOG, error = %e, "relance impossible");
                self.shared.healthy.store(false, Ordering::SeqCst);
            }
        } else {
            error!(target: LOG, path = %self.path.display(),
                "process ffmpeg mort une seconde fois, lecteur déclaré malade");
            self.shared.healthy.store(false, Ordering::SeqCst);
        }
    }

    /// Position média où reprendre après un crash.
    fn resume_position_s(&self, last_stream_pts_s: f64) -> f64 {
        let seg = segment_len_s(&self.info, &self.pb);
        let in_stream = if seg > 0.0 { last_stream_pts_s % seg } else { last_stream_pts_s };
        self.pb.in_s + in_stream
    }

    /// Récolte le process (`wait`) et joint le thread lecteur.
    /// `true` si le process s'est terminé avec un code succès.
    fn reap_child(&mut self) -> bool {
        let clean = match self.child.take() {
            Some(mut child) => match child.wait() {
                Ok(status) => status.success(),
                Err(e) => {
                    warn!(target: LOG, error = %e, "wait ffmpeg");
                    false
                }
            },
            None => false,
        };
        if let Some(h) = self.reader.take() {
            if h.join().is_err() {
                warn!(target: LOG, "le thread lecteur a paniqué");
            }
        }
        clean
    }

    /// Arrêt forcé du process courant. Ordre important : fermer le ring
    /// débloque un lecteur en push, kill ferme le pipe et débloque un lecteur
    /// en read, wait évite le zombie, join termine proprement le thread.
    fn stop_child(&mut self) {
        if self.child.is_none() && self.reader.is_none() {
            return;
        }
        self.shared.ring.close();
        if let Some(child) = self.child.as_mut() {
            if let Err(e) = child.kill() {
                debug!(target: LOG, error = %e, "kill ffmpeg (probablement déjà terminé)");
            }
        }
        let _ = self.reap_child();
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop_child();
        self.shared.ring.close(); // reste fermé pour le consommateur
    }
}

/// Boucle du thread lecteur : lit des frames complètes (4 octets/pixel,
/// ordre `order`) sur le pipe et les pousse dans le ring (bloquant =
/// backpressure). Sort sur EOF, erreur de lecture ou fermeture du ring.
/// `pool` fournit des buffers recyclés de `width * height * 4` octets.
/// Rend le dernier pts poussé.
#[allow(clippy::too_many_arguments)]
fn read_frames(
    mut stdout: impl Read,
    ring: FrameRing,
    pool: &BufferPool,
    width: u32,
    height: u32,
    frame_dur_s: f64,
    stream_base_s: f64,
    order: PixelOrder,
) -> Option<f64> {
    let frame_size = (width as usize) * (height as usize) * 4;
    if frame_size == 0 {
        ring.close();
        return None;
    }
    let mut index: u64 = 0;
    let mut last_pts_s = None;
    loop {
        let mut data = pool.take();
        data.set_pixel_order(order);
        match read_exact_or_eof(&mut stdout, &mut data) {
            Ok(true) => {}
            Ok(false) => break, // pipe fermé proprement (EOF)
            Err(e) => {
                debug!(target: LOG, error = %e, "lecture pipe ffmpeg interrompue");
                break;
            }
        }
        let pts_s = stream_base_s + index as f64 * frame_dur_s;
        index += 1;
        if !ring.push(FrameRgba { width, height, data, pts_s }) {
            break; // ring fermé (drop/seek) : on s'arrête sans bruit
        }
        last_pts_s = Some(pts_s);
    }
    ring.close();
    last_pts_s
}

/// `read_exact` tolérant : `Ok(false)` si EOF avant le premier octet,
/// erreur si EOF au milieu d'une frame.
fn read_exact_or_eof(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "frame tronquée",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_str(path: &str, start: f64, stop: Option<f64>, sl: bool) -> Vec<String> {
        args_opts(path, start, stop, SpawnOpts { stream_loop: sl, ..Default::default() })
    }

    fn args_opts(path: &str, start: f64, stop: Option<f64>, opts: SpawnOpts) -> Vec<String> {
        build_args(Path::new(path), start, stop, opts)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn args_nominal_sans_in_out() {
        let a = args_str("clip.mp4", 0.0, None, false);
        assert!(!a.contains(&"-ss".to_string()), "pas de -ss à 0 : {a:?}");
        assert!(!a.contains(&"-stream_loop".to_string()));
        assert!(!a.contains(&"-t".to_string()));
        let tail: Vec<_> = a.iter().rev().take(5).rev().cloned().collect();
        assert_eq!(tail, ["-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"]);
    }

    #[test]
    fn args_in_out_utilisent_ss_et_duree() {
        let a = args_str("clip.mp4", 2.0, Some(5.0), false);
        let ss = a.iter().position(|s| s == "-ss").expect("-ss attendu");
        assert_eq!(a[ss + 1], "2.000000");
        let i = a.iter().position(|s| s == "-i").expect("-i attendu");
        assert!(ss < i, "-ss doit précéder -i (seek d'entrée rapide)");
        let t = a.iter().position(|s| s == "-t").expect("-t attendu");
        assert_eq!(a[t + 1], "3.000000", "durée = out - in");
        assert!(i < t, "-t est une option de sortie");
    }

    #[test]
    fn args_stream_loop_avant_i() {
        let a = args_str("clip.mp4", 0.0, None, true);
        let sl = a.iter().position(|s| s == "-stream_loop").expect("-stream_loop");
        assert_eq!(a[sl + 1], "-1");
        let i = a.iter().position(|s| s == "-i").expect("-i");
        assert!(sl < i);
    }

    #[test]
    fn args_pas_d_audio_ni_soustitres() {
        let a = args_str("clip.mp4", 0.0, None, false);
        assert!(a.contains(&"-an".to_string()));
        assert!(a.contains(&"-sn".to_string()));
        assert!(a.contains(&"-nostdin".to_string()));
    }

    #[test]
    fn args_bgra_change_le_pix_fmt() {
        let a = args_opts("clip.mp4", 0.0, None, SpawnOpts { bgra: true, ..Default::default() });
        let p = a.iter().position(|s| s == "-pix_fmt").expect("-pix_fmt attendu");
        assert_eq!(a[p + 1], "bgra");
        assert!(!a.contains(&"rgba".to_string()));
    }

    #[test]
    fn args_hwaccel_d3d11va_avant_l_entree() {
        let a = args_opts(
            "clip.mp4",
            2.0,
            None,
            SpawnOpts { hwaccel_d3d11va: true, ..Default::default() },
        );
        let hw = a.iter().position(|s| s == "-hwaccel").expect("-hwaccel attendu");
        assert_eq!(a[hw + 1], "d3d11va");
        let i = a.iter().position(|s| s == "-i").expect("-i");
        assert!(hw < i, "-hwaccel est une option d'entrée : avant -i");
        // Sans l'option : aucune trace.
        let a = args_str("clip.mp4", 0.0, None, false);
        assert!(!a.contains(&"-hwaccel".to_string()));
    }

    #[test]
    fn codecs_candidats_au_d3d11va() {
        assert!(codec_supports_d3d11va(Some("h264")));
        assert!(codec_supports_d3d11va(Some("hevc")));
        assert!(!codec_supports_d3d11va(Some("hap")));
        assert!(!codec_supports_d3d11va(Some("prores")));
        assert!(!codec_supports_d3d11va(None));
    }

    #[test]
    fn d3d11va_reserve_aux_grandes_resolutions() {
        assert!(hwaccel_worthwhile(1280, 720));
        assert!(hwaccel_worthwhile(3840, 2160));
        assert!(hwaccel_worthwhile(1920, 480), "la surface compte, pas le ratio");
        assert!(!hwaccel_worthwhile(640, 360));
        assert!(!hwaccel_worthwhile(64, 64));
        assert!(!hwaccel_worthwhile(0, 0));
    }

    #[test]
    fn read_exact_or_eof_distingue_eof_propre_et_tronque() {
        let mut buf = [0u8; 4];
        // EOF immédiat → Ok(false).
        let mut empty: &[u8] = &[];
        assert!(!read_exact_or_eof(&mut empty, &mut buf).unwrap());
        // Frame complète → Ok(true).
        let mut full: &[u8] = &[1, 2, 3, 4];
        assert!(read_exact_or_eof(&mut full, &mut buf).unwrap());
        assert_eq!(buf, [1, 2, 3, 4]);
        // Frame tronquée → Err.
        let mut partial: &[u8] = &[1, 2];
        assert!(read_exact_or_eof(&mut partial, &mut buf).is_err());
    }

    #[test]
    fn read_frames_pousse_puis_ferme() {
        // 2 frames 2x2 RGBA (16 octets chacune) + 1 octet orphelin ignoré.
        let mut bytes = [7u8; 33];
        bytes[32] = 9;
        let ring = FrameRing::new();
        let pool = BufferPool::new(16, 4);
        let last = read_frames(&bytes[..], ring.clone(), &pool, 2, 2, 1.0 / 30.0, 0.0, PixelOrder::Rgba);
        assert_eq!(ring.len(), 2);
        assert!(!ring.is_drained());
        assert!((last.unwrap() - 1.0 / 30.0).abs() < 1e-9, "dernier pts poussé");
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        let f0 = ring.poll(&mut pacer, 0.0).unwrap();
        assert_eq!(f0.pts_s, 0.0);
        let f1 = ring.poll(&mut pacer, 1.0 / 30.0).unwrap();
        assert!((f1.pts_s - 1.0 / 30.0).abs() < 1e-9);
        assert!(ring.is_drained());
    }

    #[test]
    fn read_frames_estampille_l_ordre_des_canaux() {
        let bytes = [0u8; 16];
        let ring = FrameRing::new();
        let pool = BufferPool::new(16, 4);
        read_frames(&bytes[..], ring.clone(), &pool, 2, 2, 1.0 / 30.0, 0.0, PixelOrder::Bgra);
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        let f = ring.poll(&mut pacer, 0.0).expect("une frame");
        assert_eq!(f.pixel_order(), PixelOrder::Bgra);
        // Un buffer recyclé repart en RGBA par défaut jusqu'au prochain stamp.
        drop(f);
        assert_eq!(pool.take().pixel_order(), PixelOrder::Rgba);
    }

    #[test]
    fn read_frames_respecte_stream_base() {
        let bytes = [0u8; 16];
        let ring = FrameRing::new();
        let pool = BufferPool::new(16, 4);
        let last = read_frames(&bytes[..], ring.clone(), &pool, 2, 2, 1.0 / 30.0, 5.0, PixelOrder::Rgba);
        assert!((last.unwrap() - 5.0).abs() < 1e-9);
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        // stream_time(5.0) = 5.0 (in=0) → la frame pts 5.0 est due.
        let f = ring.poll(&mut pacer, 5.0).unwrap();
        assert!((f.pts_s - 5.0).abs() < 1e-9);
    }

    #[test]
    fn read_frames_sans_frame_rend_none() {
        let bytes: &[u8] = &[];
        let ring = FrameRing::new();
        let pool = BufferPool::new(16, 4);
        assert!(read_frames(bytes, ring.clone(), &pool, 2, 2, 1.0 / 30.0, 0.0, PixelOrder::Rgba).is_none());
        assert!(ring.is_drained());
    }

    #[test]
    fn read_frames_recycle_les_buffers_via_le_pool() {
        // 3 frames lues et consommées une à une : au plus 2 buffers vivants
        // à la fois → la 3e frame doit réutiliser une allocation rendue.
        let bytes = [1u8; 48]; // 3 frames de 16 octets
        let ring = FrameRing::new();
        let pool = BufferPool::new(16, 4);
        read_frames(&bytes[..], ring.clone(), &pool, 2, 2, 1.0 / 30.0, 0.0, PixelOrder::Rgba);
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        let mut ptrs = Vec::new();
        for i in 0..3 {
            let f = ring.poll(&mut pacer, i as f64 / 30.0).unwrap();
            ptrs.push(f.data.as_ptr());
            // f droppée ici → buffer rendu au pool.
        }
        // Après consommation, les 3 buffers sont en réserve (≤ POOL_SPARES).
        let reused = pool.take();
        assert!(ptrs.contains(&reused.as_ptr()), "take doit recycler un buffer rendu");
    }

    #[test]
    fn fin_naturelle_dans_la_tolerance() {
        let dur = 1.0 / 30.0;
        // Flux attendu jusqu'à 1.0 s : s'arrête 2 frames avant → naturel.
        assert!(is_natural_end(1.0 - 2.0 * dur, 1.0, dur, false));
        // Pile à la fin → naturel.
        assert!(is_natural_end(1.0, 1.0, dur, false));
    }

    #[test]
    fn fin_prematuree_hors_tolerance() {
        let dur = 1.0 / 30.0;
        // S'arrête à 0.5 s sur 1.0 s attendue → mort prématurée.
        assert!(!is_natural_end(0.5, 1.0, dur, false));
        // Même avec un code succès : la fin attendue est CONNUE et non atteinte.
        assert!(!is_natural_end(0.5, 1.0, dur, true));
    }

    #[test]
    fn fin_inconnue_selon_le_code_de_sortie() {
        let dur = 1.0 / 30.0;
        // Durée inconnue (INFINITY) : seul un exit propre fait foi.
        assert!(is_natural_end(3.0, f64::INFINITY, dur, true));
        assert!(!is_natural_end(3.0, f64::INFINITY, dur, false));
    }

    #[test]
    fn media_pts_reboucle_dans_le_segment() {
        let info = MediaInfo { duration_s: 10.0, fps: 30.0, width: 2, height: 2 };
        let pb = Playback { in_s: 2.0, out_s: Some(4.0), speed: 1.0, end: EndMode::Loop };
        // Segment de 2 s : pts de flux 3.5 (2e cycle) → média 2.0 + 1.5.
        assert!((media_pts_of(&info, &pb, 3.5) - 3.5).abs() < 1e-9);
        assert!((media_pts_of(&info, &pb, 2.5) - 2.5).abs() < 1e-9);
        // 5.0 = 2 cycles + 1.0 → média 3.0.
        assert!((media_pts_of(&info, &pb, 5.0) - 3.0).abs() < 1e-9);
    }
}
