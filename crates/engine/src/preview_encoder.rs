//! Encodeur H.264 de préview (contrat Préview H.264, docs/INTERFACES.md) :
//! un process `ffmpeg` nourri en rawvideo (RGBA/BGRA) sur stdin, encodage
//! `h264_mf` (MediaFoundation, présent dans le build LGPL embarqué), sortie
//! Annex-B brute sur stdout, découpée en access units pour le WebSocket
//! `/preview.h264` (WebCodecs côté client).
//!
//! Principes :
//! - **probe unique** : la disponibilité de `h264_mf` est testée UNE fois par
//!   process (`h264_mf_available`) ; indisponible ⇒ `Err` propre (le serveur
//!   répond 503 et le client reste en MJPEG) ;
//! - **`push_frame` non bloquant** : canal borné à 1 frame, encodeur en
//!   retard ⇒ frame sautée (compteur, jamais de blocage du thread appelant) ;
//!   buffers recyclés par un canal retour (zéro allocation en régime établi) ;
//! - **zéro zombie** : drop ⇒ canal fermé (stdin fermé), kill + wait + join ;
//! - la découpe Annex-B ([`AnnexBSplitter`]) est **pure et testée** (start
//!   codes 3/4 octets, access units par AUD ou repli heuristique VCL,
//!   drapeau keyframe = présence d'un NAL IDR).
//!
//! Options `h264_mf` vérifiées sur le build embarqué (`-h encoder=h264_mf`) :
//! `rate_control`, `scenario`, `quality`, `hw_encoding` — il n'existe PAS
//! d'option `-realtime` ; la basse latence passe par `-scenario
//! live_streaming` + `-rate_control cbr` + `-g 16 -bf 0`. Le profil se passe
//! en NUMÉRIQUE (`-profile:v 66` = baseline ; le nom « baseline » n'est pas
//! reconnu par cet encodeur). Entrée convertie en yuv420p (h264_mf n'accepte
//! que nv12/yuv420p/d3d11).

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::probe::{no_window, resolve_ffmpeg};
use crate::PixelOrder;

const LOG: &str = "engine::preview_encoder";

/// Chaîne codec annoncée au client WebCodecs (handshake JSON du WS) :
/// H.264 Constrained Baseline, niveau générique 3.0 — en format `annexb`,
/// le décodeur lit les vrais SPS/PPS dans le flux.
pub const PREVIEW_CODEC_STRING: &str = "avc1.42E01E";

/// Intervalle de keyframes demandé à l'encodeur (`-g`).
pub const PREVIEW_GOP: u32 = 16;

/// Access units en attente côté consommateur avant d'en jeter (≈ 4 s à
/// 15 fps : un consommateur bloqué ne fait jamais gonfler la mémoire).
const AU_CHANNEL_CAPACITY: usize = 64;

/// Garde-fou du tampon de découpe : au-delà (flux corrompu, jamais de start
/// code), on jette tout et on repart.
const SPLITTER_MAX_BUF: usize = 8 * 1024 * 1024;

/// Erreurs de l'encodeur de préview.
#[derive(Debug, thiserror::Error)]
pub enum PreviewEncoderError {
    /// `h264_mf` absent du ffmpeg résolu : le serveur répond 503, le client
    /// reste en MJPEG (contrat).
    #[error("encodeur h264_mf indisponible dans ce ffmpeg")]
    Unavailable,
    #[error("dimensions ou cadence invalides ({w}×{h} @ {fps})")]
    BadParams { w: u32, h: u32, fps: u32 },
    #[error("lancement de ffmpeg impossible : {0}")]
    Spawn(String),
}

/// Un access unit H.264 Annex-B complet (start codes inclus), prêt à partir
/// tel quel en message binaire WebSocket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessUnit {
    pub data: Vec<u8>,
    /// Contient un NAL IDR (type 5) : point d'accès décodeur (`type: "key"`
    /// côté WebCodecs).
    pub keyframe: bool,
}

// ---------------------------------------------------------------- découpe

/// Découpe un flux Annex-B en access units (pur, testé).
///
/// Règles : un NAL AUD (type 9) ouvre toujours un nouvel access unit ; sans
/// AUD dans le flux, repli heuristique — un NAL (VCL ou SPS/PPS/SEI) qui
/// suit un NAL VCL (types 1..5) du même access unit ouvre le suivant. Les
/// octets d'un NAL potentiellement incomplet restent en tampon jusqu'au
/// start code suivant ([`AnnexBSplitter::finish`] vide le reliquat).
#[derive(Debug, Default)]
pub struct AnnexBSplitter {
    buf: Vec<u8>,
    /// Le flux emploie des AUD : la découpe heuristique est désactivée.
    saw_aud: bool,
}

/// Type de NAL H.264 (5 bits de poids faible du premier octet après le
/// start code). VCL = 1..5.
fn nal_type(header: u8) -> u8 {
    header & 0x1F
}

fn is_vcl(t: u8) -> bool {
    (1..=5).contains(&t)
}

/// Positions des start codes (`00 00 01` ou `00 00 00 01`) dans `buf` :
/// `(position, longueur du start code)`.
fn find_start_codes(buf: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 {
            if buf[i + 2] == 1 {
                out.push((i, 3));
                i += 3;
                continue;
            }
            if i + 4 <= buf.len() && buf[i + 2] == 0 && buf[i + 3] == 1 {
                out.push((i, 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    out
}

impl AnnexBSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pousse des octets du flux et récolte les access units complets.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<AccessUnit>) {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() > SPLITTER_MAX_BUF {
            tracing::warn!(target: LOG, "flux h264 sans start code, tampon purgé");
            self.buf.clear();
            return;
        }

        let scs = find_start_codes(&self.buf);
        let Some(&(first, _)) = scs.first() else {
            return;
        };

        // Marche sur les NALs : la décision de frontière ne dépend que du
        // TYPE (octet d'en-tête), disponible même pour le dernier NAL encore
        // ouvert — la découpe est donc identique quel que soit le découpage
        // des `push` (testé octet par octet).
        let mut cut = first; // début de l'access unit en cours d'assemblage
        let mut have_vcl = false; // l'AU en cours contient un NAL VCL
        for &(start, sc_len) in &scs {
            let Some(&header) = self.buf.get(start + sc_len) else {
                break; // en-tête pas encore arrivé : décision au prochain push
            };
            let t = nal_type(header);
            if t == 9 {
                self.saw_aud = true;
            }
            let boundary = t == 9 || (!self.saw_aud && have_vcl);
            if boundary && start > cut {
                out.push(make_au(&self.buf[cut..start]));
                cut = start;
                have_vcl = false;
            }
            if is_vcl(t) {
                have_vcl = true;
            }
        }

        // On garde l'AU en cours (bruit d'avant-flux et AUs émis : drainés).
        if cut > 0 {
            self.buf.drain(..cut);
        }
    }

    /// Fin de flux : émet le reliquat comme dernier access unit (s'il
    /// contient au moins un NAL).
    pub fn finish(&mut self, out: &mut Vec<AccessUnit>) {
        let scs = find_start_codes(&self.buf);
        if let Some(&(first, _)) = scs.first() {
            if self.buf.len() > first {
                out.push(make_au(&self.buf[first..]));
            }
        }
        self.buf.clear();
    }
}

/// Construit un [`AccessUnit`] : keyframe = présence d'un NAL IDR (type 5).
fn make_au(bytes: &[u8]) -> AccessUnit {
    let keyframe = find_start_codes(bytes)
        .iter()
        .any(|&(pos, len)| bytes.get(pos + len).is_some_and(|&h| nal_type(h) == 5));
    AccessUnit { data: bytes.to_vec(), keyframe }
}

// ---------------------------------------------------------- ligne ffmpeg

/// Débit cible : ~0,12 bit/pixel/frame, borné 400 kb/s .. 8 Mb/s
/// (≈ 500 kb/s en 640×360 @ 15, largement suffisant pour une préview).
fn preview_bitrate(w: u32, h: u32, fps: u32) -> u32 {
    let raw = (w as f64) * (h as f64) * (fps as f64) * 0.12;
    raw.clamp(400_000.0, 8_000_000.0) as u32
}

/// Arguments ffmpeg de l'encodeur de préview (pur, testé). Voir l'en-tête du
/// module pour la justification de chaque option `h264_mf`.
fn build_encoder_args(w: u32, h: u32, fps: u32, order: PixelOrder) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    let push = |args: &mut Vec<std::ffi::OsString>, s: &str| args.push(s.into());
    push(&mut args, "-v");
    push(&mut args, "error");
    // Entrée : rawvideo sur stdin (PAS de -nostdin ici, c'est notre pipe).
    push(&mut args, "-f");
    push(&mut args, "rawvideo");
    push(&mut args, "-pix_fmt");
    push(
        &mut args,
        match order {
            PixelOrder::Rgba => "rgba",
            PixelOrder::Bgra => "bgra",
        },
    );
    push(&mut args, "-s");
    args.push(format!("{w}x{h}").into());
    push(&mut args, "-r");
    args.push(fps.to_string().into());
    push(&mut args, "-i");
    push(&mut args, "pipe:0");
    push(&mut args, "-an");
    // Sortie : h264_mf baseline (profil NUMÉRIQUE : « baseline » n'est pas
    // reconnu par cet encodeur), GOP court, zéro B-frame, basse latence.
    push(&mut args, "-c:v");
    push(&mut args, "h264_mf");
    push(&mut args, "-profile:v");
    push(&mut args, "66");
    push(&mut args, "-g");
    args.push(PREVIEW_GOP.to_string().into());
    push(&mut args, "-bf");
    push(&mut args, "0");
    push(&mut args, "-rate_control");
    push(&mut args, "cbr");
    push(&mut args, "-b:v");
    args.push(preview_bitrate(w, h, fps).to_string().into());
    push(&mut args, "-scenario");
    push(&mut args, "live_streaming");
    push(&mut args, "-pix_fmt");
    push(&mut args, "yuv420p");
    push(&mut args, "-flush_packets");
    push(&mut args, "1");
    push(&mut args, "-f");
    push(&mut args, "h264");
    push(&mut args, "pipe:1");
    args
}

// ---------------------------------------------------------------- encodeur

/// `h264_mf` est-il disponible dans le ffmpeg résolu ? Sondé UNE fois par
/// process (`ffmpeg -encoders`), mémorisé.
pub fn h264_mf_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let ffmpeg = resolve_ffmpeg();
        let mut cmd = Command::new(&ffmpeg);
        cmd.args(["-hide_banner", "-encoders"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        no_window(&mut cmd);
        let found = match cmd.output() {
            Ok(out) if out.status.success() => encoders_list_has_h264_mf(&out.stdout),
            _ => false,
        };
        tracing::info!(
            target: LOG,
            disponible = found,
            ffmpeg = %ffmpeg.display(),
            "sonde de l'encodeur h264_mf"
        );
        found
    })
}

/// `h264_mf` figure-t-il dans une sortie `ffmpeg -encoders` ? (pur, testé)
fn encoders_list_has_h264_mf(stdout: &[u8]) -> bool {
    String::from_utf8_lossy(stdout)
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some("h264_mf"))
}

/// Encodeur H.264 de préview : frames brutes en entrée (non bloquant),
/// access units Annex-B en sortie (canal borné).
#[derive(Debug)]
pub struct PreviewEncoder {
    child: Option<Child>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    /// Canal des frames vers le thread écrivain ; `None` après drop partiel.
    frame_tx: Option<SyncSender<Vec<u8>>>,
    /// Buffers rendus par le thread écrivain (recyclage, zéro alloc).
    recycle_rx: Receiver<Vec<u8>>,
    /// Buffer gardé localement quand un envoi a échoué (frame sautée).
    spare: Option<Vec<u8>>,
    au_rx: Receiver<AccessUnit>,
    alive: Arc<AtomicBool>,
    frame_size: usize,
    dropped: u64,
}

impl PreviewEncoder {
    /// Lance ffmpeg + threads écrivain/lecteur. Frames attendues en RGBA
    /// (l'ordre du chemin préview GL : `glReadPixels` sort toujours du RGBA) ;
    /// voir [`PreviewEncoder::new_with_order`] pour du BGRA.
    pub fn new(w: u32, h: u32, fps: u32) -> Result<Self, PreviewEncoderError> {
        Self::new_with_order(w, h, fps, PixelOrder::Rgba)
    }

    /// Comme [`PreviewEncoder::new`] avec l'ordre de canaux explicite.
    pub fn new_with_order(
        w: u32,
        h: u32,
        fps: u32,
        order: PixelOrder,
    ) -> Result<Self, PreviewEncoderError> {
        if w == 0 || h == 0 || fps == 0 || w > 8192 || h > 8192 {
            return Err(PreviewEncoderError::BadParams { w, h, fps });
        }
        if !h264_mf_available() {
            return Err(PreviewEncoderError::Unavailable);
        }

        let ffmpeg = resolve_ffmpeg();
        let args = build_encoder_args(w, h, fps, order);
        let mut cmd = Command::new(&ffmpeg);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        no_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| PreviewEncoderError::Spawn(format!("{} : {e}", ffmpeg.display())))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PreviewEncoderError::Spawn("pipes ffmpeg absents".into()));
        };

        let alive = Arc::new(AtomicBool::new(true));
        // 1 frame en file au plus : l'encodeur en retard fait sauter les
        // suivantes (push_frame non bloquant, contrat).
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        let (recycle_tx, recycle_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (au_tx, au_rx) = std::sync::mpsc::sync_channel::<AccessUnit>(AU_CHANNEL_CAPACITY);

        let writer = std::thread::Builder::new()
            .name("preview-enc-writer".into())
            .spawn(move || write_frames(stdin, frame_rx, recycle_tx))
            .map_err(|e| {
                let _ = child.kill();
                let _ = child.wait();
                PreviewEncoderError::Spawn(format!("thread écrivain : {e}"))
            })?;

        let alive_r = Arc::clone(&alive);
        let reader = std::thread::Builder::new()
            .name("preview-enc-reader".into())
            .spawn(move || {
                read_access_units(stdout, au_tx);
                alive_r.store(false, Ordering::SeqCst);
            })
            .map_err(|e| {
                // L'écrivain se termine à la fermeture du canal des frames.
                let _ = child.kill();
                let _ = child.wait();
                PreviewEncoderError::Spawn(format!("thread lecteur : {e}"))
            })?;

        tracing::info!(target: LOG, w, h, fps, ?order, "encodeur h264_mf lancé");
        Ok(PreviewEncoder {
            child: Some(child),
            writer: Some(writer),
            reader: Some(reader),
            frame_tx: Some(frame_tx),
            recycle_rx,
            spare: None,
            au_rx,
            alive,
            frame_size: (w as usize) * (h as usize) * 4,
            dropped: 0,
        })
    }

    /// Pousse une frame brute (`w*h*4` octets). **Non bloquant** : rend
    /// `false` si la frame est sautée (encodeur en retard ou mort). Les
    /// octets excédentaires sont ignorés, une frame trop courte est refusée.
    pub fn push_frame(&mut self, frame: &[u8]) -> bool {
        if frame.len() < self.frame_size {
            tracing::warn!(
                target: LOG,
                attendu = self.frame_size,
                recu = frame.len(),
                "frame préview trop courte, ignorée"
            );
            return false;
        }
        let Some(tx) = &self.frame_tx else { return false };
        let mut buf = self
            .spare
            .take()
            .or_else(|| self.recycle_rx.try_recv().ok())
            .unwrap_or_default();
        buf.clear();
        buf.extend_from_slice(&frame[..self.frame_size]);
        match tx.try_send(buf) {
            Ok(()) => true,
            Err(TrySendError::Full(b)) => {
                self.spare = Some(b);
                self.dropped += 1;
                if self.dropped.is_power_of_two() {
                    tracing::debug!(target: LOG, total = self.dropped, "frame préview sautée (encodeur en retard)");
                }
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.alive.store(false, Ordering::SeqCst);
                false
            }
        }
    }

    /// Prochain access unit disponible, sans bloquer.
    pub fn poll_access_unit(&self) -> Option<AccessUnit> {
        self.au_rx.try_recv().ok()
    }

    /// Prochain access unit, en attendant au plus `timeout` (pour la boucle
    /// d'envoi WebSocket).
    pub fn recv_access_unit(&self, timeout: Duration) -> Option<AccessUnit> {
        self.au_rx.recv_timeout(timeout).ok()
    }

    /// `false` une fois le process ffmpeg terminé (flux de sortie clos) : le
    /// serveur ferme le flux et le client retombe en MJPEG.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Frames sautées depuis le lancement (encodeur en retard).
    pub fn dropped_frames(&self) -> u64 {
        self.dropped
    }
}

impl Drop for PreviewEncoder {
    fn drop(&mut self) {
        // 1. Fermer le canal des frames : l'écrivain sort et lâche stdin.
        self.frame_tx.take();
        // 2. Kill immédiat (pas d'attente du flush encodeur) + wait : zéro
        //    zombie ; ferme aussi les pipes, ce qui débloque les threads.
        if let Some(mut child) = self.child.take() {
            if let Err(e) = child.kill() {
                tracing::debug!(target: LOG, %e, "kill ffmpeg préview (déjà terminé ?)");
            }
            let _ = child.wait();
        }
        // 3. Join des threads (leurs pipes sont clos, ils se terminent).
        if let Some(h) = self.writer.take() {
            if h.join().is_err() {
                tracing::warn!(target: LOG, "le thread écrivain préview a paniqué");
            }
        }
        if let Some(h) = self.reader.take() {
            if h.join().is_err() {
                tracing::warn!(target: LOG, "le thread lecteur préview a paniqué");
            }
        }
        if self.dropped > 0 {
            tracing::info!(target: LOG, sautees = self.dropped, "encodeur préview arrêté");
        }
    }
}

/// Boucle du thread écrivain : chaque frame reçue est écrite sur stdin de
/// ffmpeg, le buffer est rendu au canal de recyclage. Sort à la fermeture du
/// canal (drop) ou sur erreur d'écriture (process mort).
fn write_frames(
    mut stdin: impl Write,
    frames: Receiver<Vec<u8>>,
    recycle: std::sync::mpsc::Sender<Vec<u8>>,
) {
    while let Ok(frame) = frames.recv() {
        if let Err(e) = stdin.write_all(&frame) {
            tracing::debug!(target: LOG, %e, "écriture vers ffmpeg préview interrompue");
            break;
        }
        let _ = recycle.send(frame); // l'encodeur droppé : sans importance
    }
    // stdin droppé ici → EOF côté ffmpeg (flush final puis sortie propre).
}

/// Boucle du thread lecteur : découpe le flux Annex-B en access units et les
/// pousse dans le canal borné (consommateur bloqué ⇒ AU jetés, le client se
/// resynchronise au keyframe suivant). Sort sur EOF/erreur de lecture.
fn read_access_units(mut stdout: impl Read, tx: SyncSender<AccessUnit>) {
    let mut splitter = AnnexBSplitter::new();
    let mut chunk = [0u8; 16 * 1024];
    let mut aus: Vec<AccessUnit> = Vec::new();
    loop {
        let n = match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                tracing::debug!(target: LOG, %e, "lecture du flux h264 interrompue");
                break;
            }
        };
        splitter.push(&chunk[..n], &mut aus);
        for au in aus.drain(..) {
            if tx.try_send(au).is_err() {
                // Canal plein (consommateur bloqué) ou fermé : on jette.
            }
        }
    }
    splitter.finish(&mut aus);
    for au in aus.drain(..) {
        let _ = tx.try_send(au);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construit un NAL Annex-B : start code + en-tête de type `t` + charge.
    fn nal(sc_len: usize, t: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = if sc_len == 3 {
            vec![0, 0, 1]
        } else {
            vec![0, 0, 0, 1]
        };
        v.push(t & 0x1F); // nal_ref_idc = 0, type = t
        v.extend_from_slice(payload);
        v
    }

    fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.iter().flatten().copied().collect()
    }

    #[test]
    fn args_ffmpeg_conformes_au_contrat() {
        let a: Vec<String> = build_encoder_args(640, 360, 15, PixelOrder::Rgba)
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let pos = |flag: &str| a.iter().position(|s| s == flag);

        // Entrée rawvideo avant -i, stdin en source.
        let (f, i) = (pos("-f").unwrap(), a.iter().position(|s| s == "-i").unwrap());
        assert!(f < i);
        assert_eq!(a[f + 1], "rawvideo");
        assert_eq!(a[i + 1], "pipe:0");
        let s = pos("-s").unwrap();
        assert_eq!(a[s + 1], "640x360");
        assert!(s < i);
        let r = pos("-r").unwrap();
        assert_eq!(a[r + 1], "15");
        assert!(r < i);
        // pix_fmt d'ENTRÉE avant -i : rgba.
        let p_in = pos("-pix_fmt").unwrap();
        assert!(p_in < i);
        assert_eq!(a[p_in + 1], "rgba");

        // Encodeur et options vérifiées sur h264_mf (voir en-tête du module).
        let c = pos("-c:v").unwrap();
        assert_eq!(a[c + 1], "h264_mf");
        let prof = pos("-profile:v").unwrap();
        assert_eq!(a[prof + 1], "66", "profil NUMÉRIQUE (baseline non reconnu)");
        let g = pos("-g").unwrap();
        assert_eq!(a[g + 1], PREVIEW_GOP.to_string());
        let bf = pos("-bf").unwrap();
        assert_eq!(a[bf + 1], "0");
        let rc = pos("-rate_control").unwrap();
        assert_eq!(a[rc + 1], "cbr");
        let sc = pos("-scenario").unwrap();
        assert_eq!(a[sc + 1], "live_streaming");
        assert!(!a.contains(&"-realtime".to_string()), "option inexistante sur h264_mf");
        // pix_fmt de SORTIE (après -i) : yuv420p (h264_mf n'accepte pas le RGB).
        let p_out = a.iter().rposition(|s| s == "-pix_fmt").unwrap();
        assert!(p_out > i);
        assert_eq!(a[p_out + 1], "yuv420p");
        // Sortie h264 brute sur stdout, paquets flushés.
        assert_eq!(a[a.len() - 3..], ["-f".to_string(), "h264".into(), "pipe:1".into()]);
        assert!(a.contains(&"-flush_packets".to_string()));
        // Notre stdin est la source : -nostdin serait fatal.
        assert!(!a.contains(&"-nostdin".to_string()));
        // BGRA sur demande.
        let b: Vec<String> = build_encoder_args(64, 64, 30, PixelOrder::Bgra)
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(b.contains(&"bgra".to_string()));
        assert!(!b.contains(&"rgba".to_string()));
    }

    #[test]
    fn bitrate_borne() {
        assert_eq!(preview_bitrate(64, 64, 5), 400_000);
        assert_eq!(preview_bitrate(3840, 2160, 60), 8_000_000);
        let mid = preview_bitrate(640, 360, 15);
        assert!((400_000..8_000_000).contains(&mid));
        assert_eq!(mid, (640.0 * 360.0 * 15.0 * 0.12) as u32);
    }

    #[test]
    fn liste_encodeurs_reconnait_h264_mf() {
        let sample = b" V....D h264_amf             AMD AMF H.264 Encoder\n V....D h264_mf              H264 via MediaFoundation (codec h264)\n";
        assert!(encoders_list_has_h264_mf(sample));
        assert!(!encoders_list_has_h264_mf(b" V....D libx264 ...\n"));
        // Le nom en sous-chaîne d'un autre encodeur ne suffit pas.
        assert!(!encoders_list_has_h264_mf(b" V....D h264_mfx_special x\n"));
        assert!(!encoders_list_has_h264_mf(b""));
    }

    #[test]
    fn start_codes_3_et_4_octets() {
        let data = concat(&[nal(4, 7, &[0xAA]), nal(3, 8, &[0xBB]), nal(4, 5, &[0xCC])]);
        let scs = find_start_codes(&data);
        assert_eq!(scs.len(), 3);
        assert_eq!(scs[0], (0, 4));
        assert_eq!(scs[1], (6, 3));
        assert_eq!(scs[2], (11, 4));
    }

    #[test]
    fn decoupe_avec_aud_par_access_unit() {
        // AUD SPS PPS IDR | AUD P : le premier AU sort quand le 2e AUD arrive.
        let au1 = concat(&[nal(4, 9, &[0x10]), nal(4, 7, &[1]), nal(4, 8, &[2]), nal(4, 5, &[3; 8])]);
        let au2 = concat(&[nal(4, 9, &[0x10]), nal(4, 1, &[4; 8])]);
        let mut s = AnnexBSplitter::new();
        let mut out = Vec::new();
        s.push(&concat(&[au1.clone(), au2.clone()]), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, au1, "AU émis verbatim, AUD et start codes inclus");
        assert!(out[0].keyframe, "contient un IDR");
        // finish() vide le reliquat (le 2e AU, non clos par un 3e AUD).
        s.finish(&mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].data, au2);
        assert!(!out[1].keyframe);
    }

    #[test]
    fn decoupe_sans_aud_par_heuristique_vcl() {
        // SPS PPS IDR | SEI P | P : boundaries après chaque NAL VCL.
        let au1 = concat(&[nal(4, 7, &[1]), nal(4, 8, &[2]), nal(4, 5, &[3; 8])]);
        let au2 = concat(&[nal(4, 6, &[9]), nal(4, 1, &[4; 8])]);
        let au3 = concat(&[nal(3, 1, &[5; 8])]);
        let mut s = AnnexBSplitter::new();
        let mut out = Vec::new();
        s.push(&concat(&[au1.clone(), au2.clone(), au3.clone()]), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].data, au1);
        assert!(out[0].keyframe);
        assert_eq!(out[1].data, au2);
        assert!(!out[1].keyframe);
        s.finish(&mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2].data, au3);
    }

    #[test]
    fn decoupe_identique_octet_par_octet() {
        // Le même flux poussé en fragments de 1 octet produit les mêmes AUs.
        let stream = concat(&[
            nal(4, 9, &[0x10]),
            nal(4, 7, &[1, 2, 3]),
            nal(4, 8, &[4]),
            nal(3, 5, &[5; 16]),
            nal(4, 9, &[0x10]),
            nal(3, 1, &[6; 16]),
            nal(4, 9, &[0x10]),
            nal(4, 1, &[7; 16]),
        ]);
        let mut all_at_once = Vec::new();
        let mut s1 = AnnexBSplitter::new();
        s1.push(&stream, &mut all_at_once);
        s1.finish(&mut all_at_once);

        let mut byte_by_byte = Vec::new();
        let mut s2 = AnnexBSplitter::new();
        for b in &stream {
            s2.push(std::slice::from_ref(b), &mut byte_by_byte);
        }
        s2.finish(&mut byte_by_byte);

        assert_eq!(all_at_once.len(), 3);
        assert_eq!(all_at_once, byte_by_byte);
        assert!(all_at_once[0].keyframe);
        assert!(!all_at_once[1].keyframe);
        assert!(!all_at_once[2].keyframe);
    }

    #[test]
    fn bruit_avant_le_premier_start_code_ignore() {
        let mut s = AnnexBSplitter::new();
        let mut out = Vec::new();
        let mut stream = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00]; // bruit
        stream.extend(concat(&[nal(4, 9, &[0x10]), nal(4, 5, &[1; 4]), nal(4, 9, &[0x10])]));
        s.push(&stream, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data[..4], [0, 0, 0, 1], "l'AU commence au start code");
        assert!(out[0].keyframe);
    }

    #[test]
    fn flux_vide_ou_sans_start_code() {
        let mut s = AnnexBSplitter::new();
        let mut out = Vec::new();
        s.push(&[], &mut out);
        s.push(&[0xFF; 128], &mut out);
        assert!(out.is_empty());
        s.finish(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn keyframe_selon_idr_seulement() {
        // SPS+PPS sans IDR : pas un keyframe (le client attend un NAL 5).
        let au = concat(&[nal(4, 7, &[1]), nal(4, 8, &[2])]);
        assert!(!make_au(&au).keyframe);
        let au = concat(&[nal(4, 7, &[1]), nal(4, 5, &[2; 4])]);
        assert!(make_au(&au).keyframe);
        // NAL 1 (non-IDR) : delta.
        let au = nal(3, 1, &[9; 4]);
        assert!(!make_au(&au).keyframe);
    }

    #[test]
    fn erreurs_propres() {
        let e = PreviewEncoder::new(0, 360, 15).unwrap_err();
        assert!(matches!(e, PreviewEncoderError::BadParams { .. }));
        let e = PreviewEncoder::new(640, 0, 15).unwrap_err();
        assert!(matches!(e, PreviewEncoderError::BadParams { .. }));
        let e = PreviewEncoder::new(640, 360, 0).unwrap_err();
        assert!(matches!(e, PreviewEncoderError::BadParams { .. }));
        // Le message d'Unavailable est stable (503 côté serveur).
        assert_eq!(
            PreviewEncoderError::Unavailable.to_string(),
            "encodeur h264_mf indisponible dans ce ffmpeg"
        );
    }
}
