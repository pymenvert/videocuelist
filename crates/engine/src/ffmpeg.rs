//! Backend ffmpeg : un process `ffmpeg` par média, sortie rawvideo RGBA
//! sur stdout, thread lecteur → ring buffer borné (backpressure via le pipe).
//!
//! - vitesse = cadence de consommation (dup/skip dans [`Pacer`]) ;
//! - pause = on ne consomme plus (le pipe se remplit, ffmpeg bloque) ;
//! - seek = kill + respawn à la position ;
//! - EOF = pipe fermé → selon [`EndMode`] : Loop relance, Hold/Black/FollowNext
//!   passent `eof()` à vrai (l'app garde la dernière frame ou affiche noir) ;
//! - PingPong v1 = traité comme Loop (warn) ;
//! - mort prématurée du process : 1 relance automatique, puis `healthy() = false` ;
//! - **zéro zombie** : kill + wait + join dans `Drop`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;

use anyhow::Context;
use conduite_core::{EndMode, Playback};
use tracing::{debug, error, warn};

use crate::pacing::Pacer;
use crate::probe::{no_window, probe, resolve_ffmpeg};
use crate::ring::FrameRing;
use crate::{FrameRgba, MediaInfo, Player};

const LOG: &str = "engine::ffmpeg";
/// Tolérance de fin naturelle : si le flux s'arrête à moins de 4 frames de la
/// fin attendue, on considère l'EOF comme normal.
const EOF_TOLERANCE_FRAMES: f64 = 4.0;

/// Arguments ffmpeg pour lire `path` en RGBA brut depuis `start_s`.
/// Pur (testé) : `stream_loop` active `-stream_loop -1`, `stop_s` borne la
/// lecture (durée de sortie `-t stop_s - start_s`).
fn build_args(path: &Path, start_s: f64, stop_s: Option<f64>, stream_loop: bool) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    let push = |args: &mut Vec<std::ffi::OsString>, s: &str| args.push(s.into());
    push(&mut args, "-v");
    push(&mut args, "error");
    push(&mut args, "-nostdin");
    if stream_loop {
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
    push(&mut args, "rgba");
    push(&mut args, "pipe:1");
    args
}

/// Process ffmpeg vivant + son thread lecteur. `Drop` = kill absolu.
struct Proc {
    child: Child,
    ring: FrameRing,
    reader: Option<JoinHandle<()>>,
}

impl Drop for Proc {
    fn drop(&mut self) {
        // Ordre important : fermer le ring débloque un lecteur en attente,
        // kill ferme le pipe et débloque un lecteur en read, wait évite le
        // zombie, join termine proprement le thread.
        self.ring.close();
        if let Err(e) = self.child.kill() {
            debug!(target: LOG, error = %e, "kill ffmpeg (probablement déjà terminé)");
        }
        if let Err(e) = self.child.wait() {
            warn!(target: LOG, error = %e, "wait ffmpeg");
        }
        if let Some(h) = self.reader.take() {
            if h.join().is_err() {
                warn!(target: LOG, "le thread lecteur a paniqué");
            }
        }
    }
}

/// Lecteur vidéo adossé à un process ffmpeg.
pub struct FfmpegPlayer {
    info: MediaInfo,
    path: PathBuf,
    pb: Playback,
    proc: Option<Proc>,
    pacer: Pacer,
    playing: bool,
    eof: bool,
    healthy: bool,
    /// Relances après mort prématurée depuis le dernier seek/spawn sain.
    retries: u32,
    /// Pts de flux de la première frame du process courant.
    stream_base_s: f64,
    /// Pts de flux attendu en fin de process courant (INFINITY si sans fin).
    spawn_end_stream_s: f64,
    /// Dernier pts de flux réellement servi (reprise après crash).
    last_stream_pts_s: f64,
    warned_pingpong: bool,
}

impl FfmpegPlayer {
    /// Ouvre `path` (sondé via ffprobe) et précharge : process lancé,
    /// premières frames en buffer, lecture en pause.
    pub fn open(path: &Path, pb: &Playback) -> anyhow::Result<Self> {
        let info = probe(path)?;
        let mut player = FfmpegPlayer {
            info,
            path: path.to_path_buf(),
            pb: pb.clone(),
            proc: None,
            pacer: Pacer::new(info.fps, pb.in_s, 0.0),
            playing: false,
            eof: false,
            healthy: true,
            retries: 0,
            stream_base_s: 0.0,
            spawn_end_stream_s: f64::INFINITY,
            last_stream_pts_s: 0.0,
            warned_pingpong: false,
        };
        player.apply_playback(pb.clone());
        player.spawn(player.pb.in_s).context("préchargement ffmpeg")?;
        Ok(player)
    }

    /// Fin de segment sur la ligne de temps média (out, sinon durée, sinon rien).
    fn segment_end_s(&self) -> Option<f64> {
        match self.pb.out_s {
            Some(out) => Some(out.min(self.effective_duration().unwrap_or(out))),
            None => self.effective_duration(),
        }
    }

    fn effective_duration(&self) -> Option<f64> {
        (self.info.duration_s > 0.0).then_some(self.info.duration_s)
    }

    /// Longueur du segment lu (0 si inconnue).
    fn segment_len_s(&self) -> f64 {
        self.segment_end_s().map(|e| (e - self.pb.in_s).max(0.0)).unwrap_or(0.0)
    }

    fn loops(&self) -> bool {
        matches!(self.pb.end, EndMode::Loop | EndMode::PingPong)
    }

    /// Mémorise la playback et recale le pacer (fps/in/segment).
    fn apply_playback(&mut self, pb: Playback) {
        if matches!(pb.end, EndMode::PingPong) && !self.warned_pingpong {
            warn!(target: LOG, "EndMode::PingPong non supporté en v1, traité comme Loop");
            self.warned_pingpong = true;
        }
        self.pb = pb;
        self.pacer = Pacer::new(self.info.fps, self.pb.in_s, self.segment_len_s());
    }

    /// (Re)lance le process ffmpeg à la position média `start_s`.
    /// Le pts de flux de la première frame est `start_s - in_s + loop_offset`
    /// déjà contenu dans `self.stream_base_s` (à poser AVANT l'appel).
    fn spawn(&mut self, start_s: f64) -> anyhow::Result<()> {
        self.proc = None; // Drop : kill de l'ancien process d'abord.

        // stream_loop seulement pour une boucle du fichier entier depuis 0 :
        // au-delà (in/out), les pts deviendraient faux → on reboucle par respawn.
        let whole_file = self.pb.in_s <= 0.0 && self.pb.out_s.is_none();
        let stream_loop = self.loops() && whole_file && start_s <= 0.0;
        let stop_s = if stream_loop { None } else { self.pb.out_s };

        let seg_end = self.segment_end_s();
        self.spawn_end_stream_s = if stream_loop {
            f64::INFINITY
        } else {
            match seg_end {
                Some(end) => self.stream_base_s + (end - start_s).max(0.0),
                None => f64::INFINITY, // durée inconnue : EOF = fin naturelle
            }
        };

        let ffmpeg = resolve_ffmpeg();
        let args = build_args(&self.path, start_s, stop_s, stream_loop);
        let mut cmd = Command::new(&ffmpeg);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        no_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("lancement de ffmpeg impossible ({})", ffmpeg.display()))?;
        let stdout = child.stdout.take().context("stdout ffmpeg absent")?;

        let ring = FrameRing::new();
        let ring_prod = ring.clone();
        let (w, h) = (self.info.width, self.info.height);
        let frame_dur = self.pacer.frame_dur_s();
        let base = self.stream_base_s;
        let reader = std::thread::Builder::new()
            .name("ffmpeg-reader".into())
            .spawn(move || read_frames(stdout, ring_prod, w, h, frame_dur, base))
            .context("thread lecteur")?;

        debug!(target: LOG, start_s, stream_loop, path = %self.path.display(), "process ffmpeg lancé");
        self.proc = Some(Proc { child, ring, reader: Some(reader) });
        Ok(())
    }

    /// Le producteur a terminé : fin naturelle → Loop relance / autres → eof ;
    /// mort prématurée → 1 relance, puis `healthy = false`.
    fn handle_stream_end(&mut self) {
        let natural = self.last_stream_pts_s
            >= self.spawn_end_stream_s - EOF_TOLERANCE_FRAMES * self.pacer.frame_dur_s()
            || self.spawn_end_stream_s.is_infinite() && self.stream_ended_cleanly();

        if natural {
            self.retries = 0;
            if self.loops() {
                // Cycle suivant : la base de flux avance d'une longueur de segment.
                let seg = self.segment_len_s();
                if seg > 0.0 {
                    self.stream_base_s = if self.spawn_end_stream_s.is_finite() {
                        self.spawn_end_stream_s
                    } else {
                        self.last_stream_pts_s + self.pacer.frame_dur_s()
                    };
                    if let Err(e) = self.spawn(self.pb.in_s) {
                        error!(target: LOG, error = %e, "relance de boucle impossible");
                        self.healthy = false;
                        self.eof = true;
                    }
                } else {
                    // Segment de longueur nulle/inconnue : rien à reboucler.
                    self.eof = true;
                }
            } else {
                // Hold : l'app garde la dernière frame ; Black : l'app affiche
                // noir sur eof() ; FollowNext : le moteur de cues suit sur eof().
                self.eof = true;
            }
            return;
        }

        // Mort prématurée.
        if self.retries == 0 {
            self.retries = 1;
            let resume = self.resume_position_s();
            error!(target: LOG, resume_s = resume, path = %self.path.display(),
                "process ffmpeg mort en lecture, relance");
            self.stream_base_s = self.last_stream_pts_s + self.pacer.frame_dur_s();
            if let Err(e) = self.spawn(resume) {
                error!(target: LOG, error = %e, "relance impossible");
                self.healthy = false;
            }
        } else {
            error!(target: LOG, path = %self.path.display(),
                "process ffmpeg mort une seconde fois, lecteur déclaré malade");
            self.healthy = false;
            self.proc = None;
        }
    }

    /// `true` si le process s'est terminé sans code d'erreur (fin naturelle
    /// quand la durée attendue est inconnue).
    fn stream_ended_cleanly(&mut self) -> bool {
        match self.proc.as_mut().map(|p| p.child.try_wait()) {
            Some(Ok(Some(status))) => status.success(),
            _ => false,
        }
    }

    /// Position média où reprendre après un crash.
    fn resume_position_s(&self) -> f64 {
        let seg = self.segment_len_s();
        let in_stream = if seg > 0.0 {
            self.last_stream_pts_s % seg
        } else {
            self.last_stream_pts_s
        };
        self.pb.in_s + in_stream
    }

    /// Pts média (dans le segment) d'un pts de flux monotone.
    fn media_pts_of(&self, stream_pts: f64) -> f64 {
        let seg = self.segment_len_s();
        if seg > 0.0 {
            self.pb.in_s + (stream_pts % seg)
        } else {
            self.pb.in_s + stream_pts
        }
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
        let end = self.segment_end_s().unwrap_or(f64::INFINITY);
        let s = s.clamp(self.pb.in_s, end);
        self.eof = false;
        self.retries = 0;
        self.healthy = true;
        self.stream_base_s = s - self.pb.in_s;
        self.last_stream_pts_s = self.stream_base_s;
        self.pacer = Pacer::new(self.info.fps, self.pb.in_s, self.segment_len_s());
        self.pacer.reset_to(self.stream_base_s);
        if let Err(e) = self.spawn(s) {
            error!(target: LOG, error = %e, seek_s = s, "seek : relance impossible");
            self.healthy = false;
        }
    }

    fn poll_frame(&mut self, media_time_s: f64) -> Option<FrameRgba> {
        if !self.playing || self.eof {
            return None;
        }
        let ring = self.proc.as_ref()?.ring.clone();
        match ring.poll(&mut self.pacer, media_time_s) {
            Some(mut frame) => {
                self.last_stream_pts_s = frame.pts_s;
                frame.pts_s = self.media_pts_of(frame.pts_s);
                Some(frame)
            }
            None => {
                if ring.is_drained() {
                    self.handle_stream_end();
                }
                None
            }
        }
    }

    fn eof(&self) -> bool {
        self.eof
    }

    fn healthy(&self) -> bool {
        self.healthy
    }
}

impl Drop for FfmpegPlayer {
    fn drop(&mut self) {
        // Proc::drop fait le kill/wait/join ; on force l'ordre ici pour la clarté.
        self.proc = None;
    }
}

/// Boucle du thread lecteur : lit des frames RGBA complètes sur le pipe et
/// les pousse dans le ring (bloquant = backpressure). Sort sur EOF, erreur
/// de lecture ou fermeture du ring.
fn read_frames(
    mut stdout: impl Read,
    ring: FrameRing,
    width: u32,
    height: u32,
    frame_dur_s: f64,
    stream_base_s: f64,
) {
    let frame_size = (width as usize) * (height as usize) * 4;
    if frame_size == 0 {
        ring.close();
        return;
    }
    let mut index: u64 = 0;
    loop {
        let mut data = vec![0u8; frame_size];
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
    }
    ring.close();
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
        build_args(Path::new(path), start, stop, sl)
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
        read_frames(&bytes[..], ring.clone(), 2, 2, 1.0 / 30.0, 0.0);
        assert_eq!(ring.len(), 2);
        assert!(!ring.is_drained());
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        let f0 = ring.poll(&mut pacer, 0.0).unwrap();
        assert_eq!(f0.pts_s, 0.0);
        let f1 = ring.poll(&mut pacer, 1.0 / 30.0).unwrap();
        assert!((f1.pts_s - 1.0 / 30.0).abs() < 1e-9);
        assert!(ring.is_drained());
    }

    #[test]
    fn read_frames_respecte_stream_base() {
        let bytes = [0u8; 16];
        let ring = FrameRing::new();
        read_frames(&bytes[..], ring.clone(), 2, 2, 1.0 / 30.0, 5.0);
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        // stream_time(5.0) = 5.0 (in=0) → la frame pts 5.0 est due.
        let f = ring.poll(&mut pacer, 5.0).unwrap();
        assert!((f.pts_s - 5.0).abs() < 1e-9);
    }
}
