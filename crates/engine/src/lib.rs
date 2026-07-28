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
//! - le **seek** = commande au thread superviseur du player (kill + respawn
//!   hors du thread appelant : `poll_frame` reste non bloquant) ;
//! - **zéro zombie** : chaque process ffmpeg est récolté dès la fin de son
//!   flux, et kill + wait au drop ;
//! - les buffers de frames sont **recyclés** ([`FrameData`]) : restitution
//!   automatique au pool au drop de la frame, sur n'importe quel thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use conduite_core::Playback;

pub mod pacing;
mod pool;
mod probe;
mod ring;

mod ffmpeg;
mod test_player;

pub use ffmpeg::FfmpegPlayer;
pub use pool::FrameData;
pub use probe::{probe, resolve_ffmpeg, resolve_ffprobe};
pub use ring::{FrameRing, RING_CAPACITY};
pub use test_player::TestPlayer;

/// Ordre des canaux d'une frame décodée (toujours 4 octets par pixel).
///
/// `Bgra` n'est produit que si [`set_decode_bgra`] a été activé (par le
/// compositor, quand le contexte GL accepte `GL_BGRA` en format client —
/// format natif des drivers Windows, upload sans swizzle). Chaque frame
/// PORTE son ordre ([`FrameRgba::pixel_order`]) : un changement de réglage
/// en cours de route ne peut pas produire de frame mal interprétée.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PixelOrder {
    #[default]
    Rgba,
    Bgra,
}

/// Demande à ffmpeg de sortir du BGRA (`-pix_fmt bgra`) au lieu du RGBA.
/// À appeler UNE FOIS à l'initialisation (le compositor le fait quand le
/// contexte GL supporte `GL_BGRA`). Pris en compte à chaque (re)lancement
/// de process ffmpeg ; les frames déjà décodées gardent leur ordre.
static DECODE_BGRA: AtomicBool = AtomicBool::new(false);

pub fn set_decode_bgra(enabled: bool) {
    let previous = DECODE_BGRA.swap(enabled, Ordering::Relaxed);
    if previous != enabled {
        tracing::info!(
            target: "engine",
            bgra = enabled,
            "format de décodage ffmpeg : {}",
            if enabled { "bgra (upload GL direct)" } else { "rgba" }
        );
    }
}

/// Ordre de canaux demandé aux prochains process ffmpeg.
pub fn decode_bgra() -> bool {
    DECODE_BGRA.load(Ordering::Relaxed)
}

/// Métadonnées d'un média vidéo (issues de ffprobe).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaInfo {
    /// Durée en secondes (0.0 si inconnue : image fixe, flux…).
    pub duration_s: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
}

/// Une frame décodée, 4 octets par pixel, lignes de haut en bas. L'ordre des
/// canaux (RGBA par défaut, BGRA si [`set_decode_bgra`] est actif) est porté
/// par le buffer : voir [`FrameRgba::pixel_order`].
#[derive(Debug, Clone, PartialEq)]
pub struct FrameRgba {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` octets. Buffer éventuellement issu d'un pool :
    /// la restitution est automatique au drop ([`FrameData`] déréférence en
    /// `[u8]`, et un `Vec<u8>` s'y convertit via `From`/`.into()`).
    pub data: FrameData,
    /// Position de la frame sur la ligne de temps média (s).
    pub pts_s: f64,
}

impl FrameRgba {
    /// Ordre des canaux du buffer (RGBA sauf décodage BGRA activé).
    pub fn pixel_order(&self) -> PixelOrder {
        self.data.pixel_order()
    }
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
