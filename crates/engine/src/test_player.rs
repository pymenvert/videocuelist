//! Player procédural sans ffmpeg : dégradé animé + barres qui défilent.
//!
//! Précieux pour le dev et la CI : il implémente [`Player`] à l'identique
//! (in/out, modes de fin, pause, seek) mais génère ses frames par calcul.

use conduite_core::{EndMode, Playback};
use tracing::warn;

use crate::{FrameRgba, MediaInfo, Player};

const LOG: &str = "engine::test_player";

/// Générateur de mire animée conforme au trait [`Player`].
pub struct TestPlayer {
    info: MediaInfo,
    pb: Playback,
    playing: bool,
    eof: bool,
    /// Dernier index de frame généré (dédoublonnage à vitesse lente).
    last_frame_idx: Option<i64>,
    warned_pingpong: bool,
}

impl TestPlayer {
    /// Mire `width`×`height` à `fps`, durée virtuelle `duration_s`.
    pub fn new(width: u32, height: u32, fps: f64, duration_s: f64) -> Self {
        let fps = if fps.is_finite() && fps > 0.0 { fps } else { 30.0 };
        TestPlayer {
            info: MediaInfo {
                duration_s: duration_s.max(0.0),
                fps,
                width: width.max(1),
                height: height.max(1),
            },
            pb: Playback::default(),
            playing: false,
            eof: false,
            last_frame_idx: None,
            warned_pingpong: false,
        }
    }

    /// Fin du segment sur la ligne de temps média.
    fn segment_end_s(&self) -> f64 {
        self.pb.out_s.unwrap_or(self.info.duration_s).min(self.info.duration_s)
    }

    fn segment_len_s(&self) -> f64 {
        (self.segment_end_s() - self.pb.in_s).max(0.0)
    }

    /// Dessine la frame à l'instant `t` (s, depuis le début du média) :
    /// dégradé horizontal/vertical animé + barres verticales défilantes.
    fn render(&self, t: f64) -> Vec<u8> {
        let (w, h) = (self.info.width as usize, self.info.height as usize);
        let mut data = vec![0u8; w * h * 4];
        // Phase du dégradé et des barres (défilement à l'écran en ~2 s).
        let shift = (t * 0.5).fract();
        let bar_w = (w / 8).max(4);
        let bar_offset = (t * w as f64 / 2.0) as usize;
        for y in 0..h {
            let g = (y * 255 / h.max(1)) as u8;
            let row = y * w * 4;
            for x in 0..w {
                let fx = (x as f64 / w as f64 + shift).fract();
                let r = (fx * 255.0) as u8;
                let b = (255.0 * (1.0 - fx)) as u8;
                // Barres claires qui défilent vers la droite.
                let bar = ((x + bar_offset) / bar_w).is_multiple_of(2);
                let boost = if bar { 64u16 } else { 0 };
                let o = row + x * 4;
                data[o] = (r as u16 + boost).min(255) as u8;
                data[o + 1] = (g as u16 + boost).min(255) as u8;
                data[o + 2] = (b as u16 + boost).min(255) as u8;
                data[o + 3] = 255;
            }
        }
        data
    }
}

impl Default for TestPlayer {
    /// 640×360 à 30 fps, 60 s.
    fn default() -> Self {
        TestPlayer::new(640, 360, 30.0, 60.0)
    }
}

impl Player for TestPlayer {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn set_playback(&mut self, pb: &Playback) {
        if matches!(pb.end, EndMode::PingPong) && !self.warned_pingpong {
            warn!(target: LOG, "EndMode::PingPong non supporté en v1, traité comme Loop");
            self.warned_pingpong = true;
        }
        self.pb = pb.clone();
        self.eof = false;
        self.last_frame_idx = None;
    }

    fn play(&mut self) {
        self.playing = true;
    }

    fn pause(&mut self) {
        self.playing = false;
    }

    fn seek(&mut self, _s: f64) {
        // Pas de process : il suffit d'oublier l'état (l'app re-cale son horloge).
        self.eof = false;
        self.last_frame_idx = None;
    }

    fn poll_frame(&mut self, media_time_s: f64) -> Option<FrameRgba> {
        if !self.playing || self.eof {
            return None;
        }
        let t_raw = (media_time_s - self.pb.in_s).max(0.0);
        let mut t_rel = t_raw;
        let seg = self.segment_len_s();
        if seg > 0.0 && t_rel >= seg {
            match self.pb.end {
                EndMode::Loop | EndMode::PingPong => t_rel %= seg,
                EndMode::Hold | EndMode::Black | EndMode::FollowNext => {
                    // Hold : l'app garde la dernière frame ; Black : affiche noir.
                    self.eof = true;
                    return None;
                }
            }
        }
        // Dédoublonnage sur l'horloge NON rebouclée : deux cycles de boucle
        // produisent bien des frames distinctes.
        let idx = (t_raw * self.info.fps).floor() as i64;
        if self.last_frame_idx == Some(idx) {
            return None; // dup : rien de neuf pour cette frame d'horloge
        }
        self.last_frame_idx = Some(idx);
        let t = self.pb.in_s + t_rel;
        Some(FrameRgba {
            width: self.info.width,
            height: self.info.height,
            data: self.render(t).into(),
            pts_s: t,
        })
    }

    fn eof(&self) -> bool {
        self.eof
    }

    fn healthy(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(end: EndMode) -> TestPlayer {
        let mut p = TestPlayer::new(16, 8, 30.0, 1.0);
        p.set_playback(&Playback { in_s: 0.0, out_s: None, speed: 1.0, end });
        p.play();
        p
    }

    #[test]
    fn genere_une_frame_valide() {
        let mut p = player(EndMode::Hold);
        let f = p.poll_frame(0.0).expect("frame attendue");
        assert_eq!(f.width, 16);
        assert_eq!(f.height, 8);
        assert_eq!(f.data.len(), 16 * 8 * 4);
        assert!(f.data.chunks(4).all(|px| px[3] == 255), "alpha opaque");
    }

    #[test]
    fn pause_ne_produit_rien() {
        let mut p = player(EndMode::Hold);
        p.pause();
        assert!(p.poll_frame(0.1).is_none());
        p.play();
        assert!(p.poll_frame(0.1).is_some());
    }

    #[test]
    fn dedoublonne_dans_la_meme_frame_d_horloge() {
        let mut p = player(EndMode::Hold);
        assert!(p.poll_frame(0.0).is_some());
        assert!(p.poll_frame(0.001).is_none(), "même frame → dup");
        assert!(p.poll_frame(1.0 / 30.0).is_some(), "frame suivante");
    }

    #[test]
    fn contenu_anime_dans_le_temps() {
        let mut p = player(EndMode::Hold);
        let a = p.poll_frame(0.0).unwrap();
        let b = p.poll_frame(0.5).unwrap();
        assert_ne!(a.data, b.data, "la mire doit bouger");
    }

    #[test]
    fn hold_passe_eof_en_fin_de_segment() {
        let mut p = player(EndMode::Hold);
        assert!(p.poll_frame(0.5).is_some());
        assert!(p.poll_frame(1.5).is_none());
        assert!(p.eof());
        // Et reste eof ensuite.
        assert!(p.poll_frame(2.0).is_none());
    }

    #[test]
    fn black_passe_eof_en_fin_de_segment() {
        let mut p = player(EndMode::Black);
        assert!(p.poll_frame(1.2).is_none());
        assert!(p.eof());
    }

    #[test]
    fn loop_reboucle_sans_eof() {
        let mut p = player(EndMode::Loop);
        assert!(p.poll_frame(0.5).is_some());
        let f = p.poll_frame(1.5).expect("rebouclé");
        assert!(!p.eof());
        assert!((f.pts_s - 0.5).abs() < 1e-6, "1.5 s dans une boucle d'1 s = 0.5 s");
    }

    #[test]
    fn pingpong_traite_comme_loop() {
        let mut p = player(EndMode::PingPong);
        assert!(p.poll_frame(1.5).is_some());
        assert!(!p.eof());
    }

    #[test]
    fn respecte_in_et_out() {
        let mut p = TestPlayer::new(16, 8, 30.0, 10.0);
        p.set_playback(&Playback { in_s: 2.0, out_s: Some(3.0), speed: 1.0, end: EndMode::Hold });
        p.play();
        let f = p.poll_frame(2.0).expect("frame au point d'entrée");
        assert!((f.pts_s - 2.0).abs() < 1e-6);
        assert!(p.poll_frame(3.5).is_none(), "après out → eof");
        assert!(p.eof());
    }

    #[test]
    fn seek_reinitialise_eof() {
        let mut p = player(EndMode::Hold);
        let _ = p.poll_frame(1.5);
        assert!(p.eof());
        p.seek(0.2);
        assert!(!p.eof());
        assert!(p.poll_frame(0.2).is_some());
    }

    #[test]
    fn out_borne_a_la_duree() {
        let mut p = TestPlayer::new(8, 8, 30.0, 1.0);
        p.set_playback(&Playback { in_s: 0.0, out_s: Some(99.0), speed: 1.0, end: EndMode::Hold });
        p.play();
        assert!(p.poll_frame(1.01).is_none());
        assert!(p.eof(), "out au-delà de la durée = borne à la durée");
    }
}
