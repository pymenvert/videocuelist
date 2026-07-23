//! # conduite-engine
//!
//! Lecture vidéo pour Conduite : trait [`Player`], backend ffmpeg en
//! sous-process ([`FfmpegPlayer`]), sondage [`probe`] via ffprobe, et
//! [`TestPlayer`] procédural (dev/CI sans ffmpeg).
//!
//! Contrat normatif : `docs/INTERFACES.md`, section *engine*.
//!
//! Principes :
//! - la **vitesse** de lecture est réalisée par l'horloge média passée à
//!   [`Player::poll_frame`] (dup/skip, module [`pacing`]) — jamais par ffmpeg ;
//! - la **pause** = ne plus consommer (backpressure du pipe) ;
//! - le **seek** = kill + respawn du process à la position ;
//! - **zéro zombie** : tout process ffmpeg est tué et attendu au drop.

use std::path::{Path, PathBuf};

use conduite_core::Playback;

pub mod pacing;
mod probe;
mod ring;

mod ffmpeg;
mod test_player;

pub use ffmpeg::FfmpegPlayer;
pub use probe::{probe, resolve_ffmpeg, resolve_ffprobe};
pub use ring::{FrameRing, RING_CAPACITY};
pub use test_player::TestPlayer;

/// Métadonnées d'un média vidéo (issues de ffprobe).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaInfo {
    /// Durée en secondes (0.0 si inconnue : image fixe, flux…).
    pub duration_s: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
}

/// Une frame décodée, RGBA 8 bits, lignes de haut en bas.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameRgba {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` octets.
    pub data: Vec<u8>,
    /// Position de la frame sur la ligne de temps média (s).
    pub pts_s: f64,
}

/// Lecteur vidéo abstrait (ffmpeg, mire de test…).
pub trait Player: Send {
    fn info(&self) -> &MediaInfo;
    /// Change in/out/vitesse/mode de fin. La vitesse est réalisée par
    /// l'horloge média de l'app ; in/out/fin peuvent relancer le décodage.
    fn set_playback(&mut self, pb: &Playback);
    fn play(&mut self);
    fn pause(&mut self);
    fn seek(&mut self, s: f64);
    /// Frame à afficher pour l'horloge média donnée (`None` = garder la précédente).
    fn poll_frame(&mut self, media_time_s: f64) -> Option<FrameRgba>;
    /// Fin de média atteinte (Hold : garder la dernière frame affichée ;
    /// Black : afficher noir ; FollowNext : le moteur de cues enchaîne).
    fn eof(&self) -> bool;
    /// `false` après deux morts consécutives du process de décodage.
    fn healthy(&self) -> bool;
}

/// Ouvre un média avec le backend ffmpeg (préchargé, en pause).
pub fn open_ffmpeg(path: &Path, pb: &Playback) -> anyhow::Result<Box<dyn Player>> {
    Ok(Box::new(FfmpegPlayer::open(path, pb)?))
}

/// Vérifie que ffmpeg ET ffprobe sont lançables (utile aux tests et au démarrage).
pub fn ffmpeg_available() -> bool {
    fn runs(tool: PathBuf) -> bool {
        let mut cmd = std::process::Command::new(tool);
        cmd.arg("-version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        probe::no_window(&mut cmd);
        matches!(cmd.status(), Ok(s) if s.success())
    }
    runs(resolve_ffmpeg()) && runs(resolve_ffprobe())
}
