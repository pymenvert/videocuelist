//! Pacing PUR : choix de la frame à afficher selon l'horloge média.
//!
//! Aucune IO ici. Le [`Pacer`] convertit l'horloge média fournie par l'app
//! en « temps de flux » monotone (les pts des frames produites par le
//! process ffmpeg sont monotones, même à travers les boucles), puis décide :
//! - **dup** : toutes les frames disponibles sont dans le futur → `Hold`
//!   (l'app garde la frame précédente) ;
//! - **skip** : l'horloge est en avance → on saute les frames en retard et
//!   on prend la plus récente admissible.
//!
//! La vitesse de lecture est entièrement gérée ainsi : une horloge média qui
//! avance à 2× consomme deux fois plus de frames (skip), à 0,5× elle duplique.

/// Décision de pacing pour un instant donné.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Rien à afficher de neuf : garder la frame précédente (dup).
    Hold,
    /// Prendre la frame `index` (les `skipped` précédentes sont jetées).
    Take { index: usize, skipped: usize },
}

/// Convertisseur horloge média → temps de flux + sélecteur de frame.
#[derive(Debug, Clone)]
pub struct Pacer {
    frame_dur_s: f64,
    in_s: f64,
    /// Longueur du segment lu (in..out ou in..durée). `<= 0` = inconnue.
    seg_len_s: f64,
    /// Décalage accumulé quand l'horloge média reboucle en arrière.
    loop_offset_s: f64,
    /// Dernier pts de flux servi (pour détecter les rebouclages).
    last_pts_s: Option<f64>,
}

impl Pacer {
    /// `fps` invalide (0, NaN…) → repli 30 fps avec warn.
    pub fn new(fps: f64, in_s: f64, seg_len_s: f64) -> Self {
        let fps = if fps.is_finite() && fps > 0.0 {
            fps
        } else {
            tracing::warn!(target: "engine::pacing", fps, "fps invalide, repli sur 30");
            30.0
        };
        Pacer {
            frame_dur_s: 1.0 / fps,
            in_s,
            seg_len_s,
            loop_offset_s: 0.0,
            last_pts_s: None,
        }
    }

    /// Durée d'une frame (s).
    pub fn frame_dur_s(&self) -> f64 {
        self.frame_dur_s
    }

    /// Horloge média → temps de flux monotone.
    ///
    /// Si l'app fait reboucler son horloge (retour brutal vers `in`), on
    /// ajoute autant de longueurs de segment que nécessaire pour rester
    /// aligné sur les pts monotones du flux.
    pub fn stream_time(&mut self, media_time_s: f64) -> f64 {
        let mut t = media_time_s - self.in_s + self.loop_offset_s;
        if let Some(last) = self.last_pts_s {
            if self.seg_len_s > 0.0 && t < last - 2.0 * self.frame_dur_s {
                let wraps = ((last - t) / self.seg_len_s).ceil().max(1.0);
                self.loop_offset_s += wraps * self.seg_len_s;
                t += wraps * self.seg_len_s;
            }
        }
        t
    }

    /// Choisit parmi `available_pts` (pts de flux, ordre croissant) la frame
    /// pour `media_time_s`. Prend la plus récente dont le pts est échu
    /// (tolérance d'une demi-frame) ; sinon `Hold`.
    pub fn choose(&mut self, available_pts: &[f64], media_time_s: f64) -> Decision {
        let st = self.stream_time(media_time_s);
        let deadline = st + self.frame_dur_s * 0.5;
        let mut best: Option<usize> = None;
        for (i, &pts) in available_pts.iter().enumerate() {
            if pts <= deadline {
                best = Some(i);
            } else {
                break;
            }
        }
        match best {
            None => Decision::Hold,
            Some(i) => {
                self.last_pts_s = Some(available_pts[i]);
                Decision::Take { index: i, skipped: i }
            }
        }
    }

    /// Réinitialise après un seek : le prochain poll doit correspondre à la
    /// position de flux `stream_pos_s` (= position_seek - in).
    pub fn reset_to(&mut self, stream_pos_s: f64) {
        self.loop_offset_s = 0.0;
        self.last_pts_s = None;
        // On mémorise la position pour que stream_time reparte proprement :
        // l'app est censée re-caler son horloge sur la position de seek.
        let _ = stream_pos_s;
    }

    /// Dernier pts de flux servi (None après reset).
    pub fn last_pts_s(&self) -> Option<f64> {
        self.last_pts_s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(n: usize, fps: f64, base: f64) -> Vec<f64> {
        (0..n).map(|i| base + i as f64 / fps).collect()
    }

    #[test]
    fn fps_invalide_replie_sur_30() {
        let p = Pacer::new(0.0, 0.0, 1.0);
        assert!((p.frame_dur_s() - 1.0 / 30.0).abs() < 1e-9);
        let p = Pacer::new(f64::NAN, 0.0, 1.0);
        assert!((p.frame_dur_s() - 1.0 / 30.0).abs() < 1e-9);
    }

    #[test]
    fn prend_la_premiere_frame_a_t0() {
        let mut p = Pacer::new(30.0, 0.0, 1.0);
        let avail = pts(4, 30.0, 0.0);
        assert_eq!(p.choose(&avail, 0.0), Decision::Take { index: 0, skipped: 0 });
    }

    #[test]
    fn hold_quand_tout_est_dans_le_futur() {
        let mut p = Pacer::new(30.0, 0.0, 1.0);
        // Frames à partir de 0.5 s, horloge à 0.1 s → dup.
        let avail = pts(4, 30.0, 0.5);
        assert_eq!(p.choose(&avail, 0.1), Decision::Hold);
    }

    #[test]
    fn hold_quand_vide() {
        let mut p = Pacer::new(30.0, 0.0, 1.0);
        assert_eq!(p.choose(&[], 0.5), Decision::Hold);
    }

    #[test]
    fn dup_a_vitesse_lente() {
        // Horloge à 0,5× : deux polls consécutifs dans la même frame → 1 Take puis Hold.
        let mut p = Pacer::new(30.0, 0.0, 1.0);
        let avail = pts(4, 30.0, 0.0);
        assert_eq!(p.choose(&avail, 0.0), Decision::Take { index: 0, skipped: 0 });
        // La frame 0 a été consommée ; reste 1..4, horloge n'a presque pas bougé.
        let rest = &avail[1..];
        assert_eq!(p.choose(rest, 0.005), Decision::Hold);
    }

    #[test]
    fn skip_quand_en_retard() {
        // Horloge à 2× : à t=0.1 s (frame 3 à 30 fps), on saute 0,1,2.
        let mut p = Pacer::new(30.0, 0.0, 1.0);
        let avail = pts(4, 30.0, 0.0);
        assert_eq!(p.choose(&avail, 0.1), Decision::Take { index: 3, skipped: 3 });
    }

    #[test]
    fn prend_la_plus_recente_si_tres_en_retard() {
        let mut p = Pacer::new(30.0, 0.0, 1.0);
        let avail = pts(4, 30.0, 0.0); // pts 0..0.1
        // Horloge bien au-delà de la dernière frame dispo → on prend la dernière.
        assert_eq!(p.choose(&avail, 5.0), Decision::Take { index: 3, skipped: 3 });
    }

    #[test]
    fn tolerance_demi_frame() {
        let mut p = Pacer::new(30.0, 0.0, 1.0);
        let avail = pts(4, 30.0, 0.0);
        // t = 0.0166… + un chouia sous la demi-frame suivante → frame 1 pas encore due ?
        // deadline = t + dur/2 ; à t = 0.02, deadline = 0.0366 → frame 1 (pts 0.0333) prise.
        assert_eq!(p.choose(&avail, 0.02), Decision::Take { index: 1, skipped: 1 });
    }

    #[test]
    fn in_point_decale_le_temps_de_flux() {
        // in = 2 s : l'horloge média 2.0 correspond au flux 0.0.
        let mut p = Pacer::new(30.0, 2.0, 1.0);
        let avail = pts(4, 30.0, 0.0);
        assert_eq!(p.choose(&avail, 2.0), Decision::Take { index: 0, skipped: 0 });
        assert!((p.stream_time(2.5) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn horloge_qui_reboucle_est_traduite_en_flux_monotone() {
        // Segment d'1 s à 30 fps. L'app fait reboucler son horloge 0.966→0.0.
        let mut p = Pacer::new(30.0, 0.0, 1.0);
        let avail = pts(30, 30.0, 0.0);
        assert_eq!(p.choose(&avail, 29.0 / 30.0), Decision::Take { index: 29, skipped: 29 });
        // Rebouclage : la frame suivante du flux a pts = 1.0 (2e cycle).
        let next = pts(4, 30.0, 1.0);
        assert_eq!(p.choose(&next, 0.0), Decision::Take { index: 0, skipped: 0 });
        // Et le temps de flux continue de croître.
        assert!(p.stream_time(0.5) > 1.0);
    }

    #[test]
    fn rebouclages_multiples() {
        let mut p = Pacer::new(30.0, 0.0, 1.0);
        let avail = pts(30, 30.0, 0.0);
        let _ = p.choose(&avail, 29.0 / 30.0);
        // L'app saute directement 2 cycles en arrière (horloge re-calée) :
        // on doit rattraper avec assez de longueurs de segment pour rester monotone.
        let st = p.stream_time(0.1);
        assert!(st > p.last_pts_s().unwrap() - 2.0 * p.frame_dur_s());
    }

    #[test]
    fn reset_apres_seek_oublie_le_passe() {
        let mut p = Pacer::new(30.0, 0.0, 10.0);
        let avail = pts(4, 30.0, 0.0);
        let _ = p.choose(&avail, 5.0);
        p.reset_to(2.0);
        assert_eq!(p.last_pts_s(), None);
        // Pas de détection de rebouclage juste après un reset.
        let after = pts(4, 30.0, 2.0);
        assert_eq!(p.choose(&after, 2.0), Decision::Take { index: 0, skipped: 0 });
    }

    #[test]
    fn segment_inconnu_ne_reboucle_jamais() {
        let mut p = Pacer::new(30.0, 0.0, 0.0);
        let avail = pts(30, 30.0, 0.0);
        let _ = p.choose(&avail, 0.9);
        // Horloge en arrière sans longueur de segment : pas d'offset ajouté.
        let st = p.stream_time(0.1);
        assert!((st - 0.1).abs() < 1e-9);
    }
}
