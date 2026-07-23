//! Compteur FPS et frames perdues par sortie — **pur** : l'horloge est
//! injectée par l'appelant (`tick(now_s)`), aucune lecture d'horloge ici.

/// Constante de temps du lissage exponentiel du FPS (~fenêtre 0,5 s).
const SMOOTHING_TAU_S: f64 = 0.5;

/// Un intervalle > 1,5 × l'intervalle cible compte des frames perdues.
const DROP_THRESHOLD: f64 = 1.5;

/// Cadence cible de repli si l'appelant fournit une valeur invalide.
const DEFAULT_TARGET_FPS: f32 = 60.0;

/// Compteur FPS lissé + cumul de frames perdues pour une sortie.
///
/// Appeler [`FpsCounter::tick`] à chaque frame présentée, avec une horloge
/// monotone en secondes. Une frame « en retard » (intervalle > 1,5 × la cible)
/// incrémente le compteur de drops du nombre de frames manquées estimé.
#[derive(Debug, Clone)]
pub struct FpsCounter {
    /// Intervalle cible entre deux frames, en secondes.
    target_dt_s: f64,
    /// Horodatage du dernier tick accepté.
    last_tick_s: Option<f64>,
    /// FPS lissé (EMA à constante de temps) — `None` tant qu'aucun intervalle mesuré.
    fps_smoothed: Option<f64>,
    /// Cumul de frames perdues depuis la création.
    drops: u64,
}

impl FpsCounter {
    /// Crée un compteur pour la cadence cible donnée (ex. 60.0).
    /// Une cible invalide (≤ 0, NaN, ∞) retombe sur 60 fps avec un warn.
    pub fn new(target_fps: f32) -> Self {
        let target = if target_fps.is_finite() && target_fps > 0.0 {
            target_fps
        } else {
            tracing::warn!(
                target: "system::fps",
                target_fps,
                "cadence cible invalide, repli sur {DEFAULT_TARGET_FPS} fps"
            );
            DEFAULT_TARGET_FPS
        };
        FpsCounter {
            target_dt_s: 1.0 / f64::from(target),
            last_tick_s: None,
            fps_smoothed: None,
            drops: 0,
        }
    }

    /// Signale une frame présentée à l'instant `now_s` (horloge monotone, secondes).
    ///
    /// Un intervalle non fini ou ≤ 0 (horloge non monotone) est ignoré :
    /// on resynchronise sur `now_s` sans toucher au FPS ni aux drops.
    pub fn tick(&mut self, now_s: f64) {
        if !now_s.is_finite() {
            tracing::warn!(target: "system::fps", now_s, "horodatage non fini ignoré");
            return;
        }
        let Some(last) = self.last_tick_s else {
            // Premier tick : on établit la référence, pas encore d'intervalle.
            self.last_tick_s = Some(now_s);
            return;
        };
        let dt = now_s - last;
        if dt <= 0.0 {
            // Horloge non monotone : resynchronisation silencieuse.
            self.last_tick_s = Some(now_s);
            return;
        }
        self.last_tick_s = Some(now_s);

        // FPS instantané, lissé par EMA dont l'alpha dépend de dt
        // (comportement identique quelle que soit la cadence d'appel).
        let inst = 1.0 / dt;
        self.fps_smoothed = Some(match self.fps_smoothed {
            None => inst,
            Some(prev) => {
                let alpha = 1.0 - (-dt / SMOOTHING_TAU_S).exp();
                prev + (inst - prev) * alpha
            }
        });

        // Frame en retard : on estime le nombre de frames manquées.
        // Petite tolérance pour que le bruit flottant au seuil exact ne compte pas.
        if dt > DROP_THRESHOLD * self.target_dt_s * (1.0 + 1e-6) {
            let missed = ((dt / self.target_dt_s).round() as u64)
                .saturating_sub(1)
                .max(1);
            self.drops = self.drops.saturating_add(missed);
        }
    }

    /// FPS lissé courant (0.0 tant que moins de deux ticks).
    pub fn fps(&self) -> f32 {
        self.fps_smoothed.unwrap_or(0.0) as f32
    }

    /// Cumul de frames perdues depuis la création du compteur.
    pub fn drops(&self) -> u64 {
        self.drops
    }

    /// Cadence cible effective, en fps.
    pub fn target_fps(&self) -> f32 {
        (1.0 / self.target_dt_s) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Horloge simulée : avance manuelle, injectée dans `tick`.
    struct FakeClock {
        now_s: f64,
    }

    impl FakeClock {
        fn new() -> Self {
            FakeClock { now_s: 100.0 }
        }
        fn advance(&mut self, dt_s: f64) -> f64 {
            self.now_s += dt_s;
            self.now_s
        }
    }

    #[test]
    fn steady_60fps_measures_60_and_no_drops() {
        let mut clock = FakeClock::new();
        let mut c = FpsCounter::new(60.0);
        for _ in 0..240 {
            c.tick(clock.advance(1.0 / 60.0));
        }
        assert!((c.fps() - 60.0).abs() < 0.01, "fps = {}", c.fps());
        assert_eq!(c.drops(), 0);
    }

    #[test]
    fn no_measurement_before_two_ticks() {
        let mut c = FpsCounter::new(60.0);
        assert_eq!(c.fps(), 0.0);
        assert_eq!(c.drops(), 0);
        c.tick(1.0);
        assert_eq!(c.fps(), 0.0, "un seul tick : pas d'intervalle mesurable");
    }

    #[test]
    fn late_frame_counts_missed_frames() {
        let target_dt = 1.0 / 60.0;
        let mut clock = FakeClock::new();
        let mut c = FpsCounter::new(60.0);
        c.tick(clock.advance(target_dt));
        c.tick(clock.advance(target_dt));
        assert_eq!(c.drops(), 0);

        // 3 × l'intervalle cible : 2 frames manquées.
        c.tick(clock.advance(3.0 * target_dt));
        assert_eq!(c.drops(), 2);

        // 1,6 × : au-dessus du seuil 1,5 → 1 frame manquée.
        c.tick(clock.advance(1.6 * target_dt));
        assert_eq!(c.drops(), 3);

        // 1,4 × : sous le seuil → rien.
        c.tick(clock.advance(1.4 * target_dt));
        assert_eq!(c.drops(), 3);

        // Exactement 1,5 × : seuil strict (>), pas de drop.
        c.tick(clock.advance(1.5 * target_dt));
        assert_eq!(c.drops(), 3);
    }

    #[test]
    fn one_second_stall_at_60fps_counts_59_drops() {
        let mut clock = FakeClock::new();
        let mut c = FpsCounter::new(60.0);
        c.tick(clock.advance(1.0 / 60.0));
        c.tick(clock.advance(1.0 / 60.0));
        c.tick(clock.advance(1.0)); // gel d'une seconde
        assert_eq!(c.drops(), 59);
    }

    #[test]
    fn smoothed_fps_converges_after_rate_change() {
        let mut clock = FakeClock::new();
        let mut c = FpsCounter::new(60.0);
        // 2 s à 60 fps…
        for _ in 0..120 {
            c.tick(clock.advance(1.0 / 60.0));
        }
        assert!((c.fps() - 60.0).abs() < 0.01);
        // …puis 5 s à 30 fps : le lissage (tau 0,5 s) doit avoir convergé.
        for _ in 0..150 {
            c.tick(clock.advance(1.0 / 30.0));
        }
        assert!((c.fps() - 30.0).abs() < 0.1, "fps = {}", c.fps());
        // Chaque frame à 30 fps est en retard (2 × la cible 60) → 1 drop chacune.
        assert_eq!(c.drops(), 150);
    }

    #[test]
    fn smoothing_damps_a_single_hiccup() {
        let mut clock = FakeClock::new();
        let mut c = FpsCounter::new(60.0);
        for _ in 0..120 {
            c.tick(clock.advance(1.0 / 60.0));
        }
        // Une seule frame très en retard ne doit pas effondrer le FPS lissé.
        c.tick(clock.advance(4.0 / 60.0));
        assert!(c.fps() > 45.0, "fps = {} (lissage trop nerveux)", c.fps());
        assert_eq!(c.drops(), 3);
    }

    #[test]
    fn non_monotonic_clock_is_ignored() {
        let mut c = FpsCounter::new(60.0);
        c.tick(100.0);
        c.tick(100.0 + 1.0 / 60.0);
        let fps_before = c.fps();
        let drops_before = c.drops();

        c.tick(50.0); // saut en arrière : ignoré, resynchronisé
        c.tick(50.0); // dt == 0 : ignoré
        assert_eq!(c.fps(), fps_before);
        assert_eq!(c.drops(), drops_before);

        // La reprise après resynchronisation mesure un intervalle normal.
        c.tick(50.0 + 1.0 / 60.0);
        assert!((c.fps() - 60.0).abs() < 1.0);
        assert_eq!(c.drops(), drops_before);
    }

    #[test]
    fn non_finite_timestamp_is_ignored() {
        let mut c = FpsCounter::new(60.0);
        c.tick(1.0);
        c.tick(f64::NAN);
        c.tick(f64::INFINITY);
        c.tick(1.0 + 1.0 / 60.0);
        assert!((c.fps() - 60.0).abs() < 0.01);
        assert_eq!(c.drops(), 0);
        assert!(c.fps().is_finite());
    }

    #[test]
    fn invalid_target_falls_back_to_60fps() {
        for bad in [0.0, -5.0, f32::NAN, f32::INFINITY] {
            let c = FpsCounter::new(bad);
            assert!((c.target_fps() - 60.0).abs() < 1e-3, "cible {bad}");
        }
        // À 60 fps effectifs, aucune frame n'est comptée perdue.
        let mut clock = FakeClock::new();
        let mut c = FpsCounter::new(0.0);
        for _ in 0..60 {
            c.tick(clock.advance(1.0 / 60.0));
        }
        assert_eq!(c.drops(), 0);
    }

    #[test]
    fn works_at_other_target_rates() {
        // 25 fps (sortie PAL / Pi) : cadence tenue, pas de drops.
        let mut clock = FakeClock::new();
        let mut c = FpsCounter::new(25.0);
        for _ in 0..100 {
            c.tick(clock.advance(1.0 / 25.0));
        }
        assert!((c.fps() - 25.0).abs() < 0.01);
        assert_eq!(c.drops(), 0);
        // Une frame à 2 × l'intervalle → 1 drop.
        c.tick(clock.advance(2.0 / 25.0));
        assert_eq!(c.drops(), 1);
    }
}
