//! Tap tempo : BPM = 60 / médiane des derniers intervalles.
//!
//! La médiane (plutôt que la moyenne) rejette un tap raté sans fausser la
//! mesure. Un silence de plus de [`RESET_GAP_S`] repart de zéro.

use std::collections::VecDeque;

/// Écart maximal entre deux taps avant remise à zéro de la mesure.
const RESET_GAP_S: f64 = 2.0;
/// Fenêtre glissante d'intervalles retenus.
const MAX_INTERVALS: usize = 7;
/// Nombre minimal d'intervalles pour estimer un tempo (soit 4 taps).
const MIN_INTERVALS: usize = 3;

#[derive(Debug, Default)]
pub struct TapTempo {
    last_tap_s: Option<f64>,
    intervals: VecDeque<f64>,
}

impl TapTempo {
    /// Enregistre un tap à l'horloge monotone `now_s`. Retourne le BPM estimé
    /// dès que 3 intervalles sont mesurés (médiane des 3 à 7 derniers).
    pub fn tap(&mut self, now_s: f64) -> Option<f32> {
        if let Some(last) = self.last_tap_s {
            let dt = now_s - last;
            if dt <= 0.0 || dt > RESET_GAP_S {
                // Trop vieux (ou horloge incohérente) : nouvelle mesure.
                self.intervals.clear();
            } else {
                if self.intervals.len() == MAX_INTERVALS {
                    self.intervals.pop_front();
                }
                self.intervals.push_back(dt);
            }
        }
        self.last_tap_s = Some(now_s);

        if self.intervals.len() < MIN_INTERVALS {
            return None;
        }
        let mut sorted: Vec<f64> = self.intervals.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = sorted.len();
        let median = if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        };
        if median <= f64::EPSILON {
            return None;
        }
        Some((60.0 / median) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_four_taps_before_reporting() {
        let mut t = TapTempo::default();
        assert_eq!(t.tap(0.0), None);
        assert_eq!(t.tap(0.5), None);
        assert_eq!(t.tap(1.0), None);
        let bpm = t.tap(1.5).expect("4 taps = 3 intervalles");
        assert!((bpm - 120.0).abs() < 0.01, "bpm = {bpm}");
    }

    #[test]
    fn median_rejects_one_bad_tap() {
        let mut t = TapTempo::default();
        // Intervalles : 0.5, 0.5, 0.9 (un tap en retard), 0.5.
        for now in [0.0, 0.5, 1.0, 1.9, 2.4] {
            t.tap(now);
        }
        let bpm = t.tap(2.9).expect("assez de taps");
        // Médiane de [0.5, 0.5, 0.9, 0.5, 0.5] = 0.5 → 120 BPM.
        assert!((bpm - 120.0).abs() < 0.01, "bpm = {bpm}");
    }

    #[test]
    fn gap_over_two_seconds_resets() {
        let mut t = TapTempo::default();
        for now in [0.0, 0.5, 1.0] {
            t.tap(now);
        }
        assert!(t.tap(1.5).is_some());
        // Long silence : la mesure repart de zéro.
        assert_eq!(t.tap(10.0), None);
        assert_eq!(t.tap(10.4), None);
        assert_eq!(t.tap(10.8), None);
        let bpm = t.tap(11.2).expect("nouvelle mesure complète");
        assert!((bpm - 150.0).abs() < 0.01, "bpm = {bpm}");
    }

    #[test]
    fn window_keeps_only_last_seven_intervals() {
        let mut t = TapTempo::default();
        // 8 intervalles à 1.0 s (60 BPM)...
        for i in 0..9 {
            t.tap(i as f64);
        }
        // ... puis 7 intervalles à 0.5 s : la fenêtre ne doit contenir que du 0.5.
        let mut now = 8.0;
        let mut bpm = None;
        for _ in 0..7 {
            now += 0.5;
            bpm = t.tap(now);
        }
        let bpm = bpm.expect("mesure disponible");
        assert!((bpm - 120.0).abs() < 0.01, "les vieux intervalles ont expiré : {bpm}");
    }

    #[test]
    fn non_monotonic_clock_resets_measure() {
        let mut t = TapTempo::default();
        for now in [0.0, 0.5, 1.0] {
            t.tap(now);
        }
        // Horloge qui recule : on repart proprement, pas de BPM absurde.
        assert_eq!(t.tap(0.2), None);
    }
}
