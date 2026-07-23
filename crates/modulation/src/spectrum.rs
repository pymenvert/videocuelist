//! Réduction d'une trame FFT en bins log-échelonnés pour l'affichage
//! (analyseur de spectre de l'UI, champ `fft.bins` de la trame WS `dyn`).

use crate::FftFrame;

/// Nombre de bins par défaut de l'analyseur de spectre UI.
pub const SPECTRUM_BINS_DEFAULT: usize = 64;
/// Borne basse par défaut de l'analyseur UI (Hz).
pub const SPECTRUM_LOW_HZ_DEFAULT: f32 = 20.0;
/// Borne haute par défaut de l'analyseur UI (Hz).
pub const SPECTRUM_HIGH_HZ_DEFAULT: f32 = 16_000.0;

/// Réduit `fft` en `n` bins **log-échelonnés** entre `low_hz` et `high_hz`
/// (défaut UI : 64 bins, 20 Hz → 16 kHz).
///
/// Le bin de sortie `k` couvre `[f_k, f_{k+1})` avec
/// `f_k = low_hz × (high_hz/low_hz)^(k/n)` et prend le **maximum** des
/// magnitudes des bins FFT dont la fréquence centrale tombe dans
/// l'intervalle — un pic étroit reste visible, contrairement à une moyenne.
/// Un intervalle plus étroit qu'un bin FFT (bas du spectre) échantillonne le
/// bin le plus proche de son centre géométrique.
///
/// L'énergie est compressée en douceur (racine carrée) puis clampée 0..1.
/// Contrat d'entrée : magnitudes ≈ 0..1 (sinusoïde pleine échelle ≈ 1,
/// normalisation Hann à la charge du producteur, cf. `FftFrame`).
///
/// Pure (aucun état, aucun panic) ; retourne `vec![0.0; n]` si la trame est
/// invalide/vide ou si les bornes sont incohérentes (`low_hz ≤ 0`,
/// `high_hz ≤ low_hz`).
pub fn spectrum_bins(fft: &FftFrame, n: usize, low_hz: f32, high_hz: f32) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    if fft.bins_hz <= 0.0 || fft.magnitudes.is_empty() || low_hz <= 0.0 || high_hz <= low_hz {
        return vec![0.0; n];
    }
    let low = f64::from(low_hz);
    let ratio = f64::from(high_hz) / low;
    let bins_hz = f64::from(fft.bins_hz);
    let mags = &fft.magnitudes;
    // Bord k de la grille log : f_k = low × ratio^(k/n).
    let edge = |k: usize| low * ratio.powf(k as f64 / n as f64);

    (0..n)
        .map(|k| {
            let lo = edge(k);
            let hi = edge(k + 1);
            // Bins FFT dont le centre `i × bins_hz` tombe dans [lo, hi).
            let i_lo = (lo / bins_hz).ceil() as usize;
            let i_hi = ((hi / bins_hz).ceil() as usize).min(mags.len());
            let raw = if i_lo < i_hi {
                mags[i_lo..i_hi].iter().copied().fold(0.0f32, f32::max)
            } else {
                // Intervalle plus étroit qu'un bin FFT : bin le plus proche
                // du centre géométrique de l'intervalle.
                let i = ((lo * hi).sqrt() / bins_hz).round() as usize;
                mags.get(i).copied().unwrap_or(0.0)
            };
            raw.max(0.0).sqrt().clamp(0.0, 1.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trame 10 Hz/bin couvrant 0..20 kHz, silencieuse sauf `peaks`.
    fn frame_with_peaks(peaks: &[(usize, f32)]) -> FftFrame {
        let mut magnitudes = vec![0.0f32; 2001];
        for &(i, m) in peaks {
            magnitudes[i] = m;
        }
        FftFrame {
            bins_hz: 10.0,
            magnitudes,
        }
    }

    /// Bin log attendu pour `hz` sur la grille par défaut (64, 20 Hz→16 kHz).
    fn expected_bin(hz: f32) -> usize {
        let pos = 64.0 * (f64::from(hz) / 20.0).ln() / (16_000.0f64 / 20.0).ln();
        pos.floor() as usize
    }

    #[test]
    fn peak_at_440_hz_lands_in_the_right_bin() {
        let fft = frame_with_peaks(&[(44, 1.0)]); // 440 Hz, pleine échelle
        let out = spectrum_bins(&fft, 64, 20.0, 16_000.0);
        assert_eq!(out.len(), 64);
        let k = expected_bin(440.0);
        assert_eq!(k, 29, "grille de référence 64 bins 20 Hz→16 kHz");
        assert!((out[k] - 1.0).abs() < 1e-6, "pic pleine échelle → 1.0");
        for (i, v) in out.iter().enumerate() {
            if i != k {
                assert_eq!(*v, 0.0, "bin {i} devrait être silencieux");
            }
        }
    }

    #[test]
    fn peaks_low_and_high_land_in_their_bins() {
        // 100 Hz → bin 15, 8 kHz → bin 57 sur la grille par défaut.
        let fft = frame_with_peaks(&[(10, 1.0), (800, 1.0)]);
        let out = spectrum_bins(&fft, 64, 20.0, 16_000.0);
        assert!((out[expected_bin(100.0)] - 1.0).abs() < 1e-6);
        assert!((out[expected_bin(8_000.0)] - 1.0).abs() < 1e-6);
        assert_eq!(out.iter().filter(|v| **v > 0.0).count(), 2);
    }

    #[test]
    fn soft_compression_is_sqrt_and_output_is_clamped() {
        let fft = frame_with_peaks(&[(44, 0.25)]);
        let out = spectrum_bins(&fft, 64, 20.0, 16_000.0);
        assert!((out[29] - 0.5).abs() < 1e-6, "sqrt(0.25) = 0.5");
        // Magnitude hors contrat (> 1) : clamp, jamais > 1.
        let fft = frame_with_peaks(&[(44, 9.0)]);
        let out = spectrum_bins(&fft, 64, 20.0, 16_000.0);
        assert!(out.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!((out[29] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dc_is_excluded_from_the_default_range() {
        // Toute l'énergie en 0 Hz : rien à afficher au-dessus de 20 Hz.
        let fft = frame_with_peaks(&[(0, 1.0)]);
        let out = spectrum_bins(&fft, 64, 20.0, 16_000.0);
        assert!(out.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn narrow_low_bins_sample_the_nearest_fft_bin() {
        // 25 Hz avec des bins FFT de 10 Hz : les intervalles log du bas du
        // spectre sont plus étroits qu'un bin FFT mais doivent voir le pic.
        let fft = frame_with_peaks(&[(2, 1.0), (3, 1.0)]); // 20 et 30 Hz
        let out = spectrum_bins(&fft, 64, 20.0, 16_000.0);
        assert!(out[0] > 0.0, "le premier bin voit l'énergie à ~20 Hz");
    }

    #[test]
    fn invalid_inputs_yield_silence_of_the_requested_size() {
        assert!(spectrum_bins(&FftFrame::empty(), 64, 20.0, 16_000.0)
            .iter()
            .all(|v| *v == 0.0));
        assert_eq!(spectrum_bins(&FftFrame::empty(), 64, 20.0, 16_000.0).len(), 64);
        let fft = frame_with_peaks(&[(44, 1.0)]);
        // Bornes incohérentes.
        assert!(spectrum_bins(&fft, 8, 0.0, 16_000.0).iter().all(|v| *v == 0.0));
        assert!(spectrum_bins(&fft, 8, 200.0, 100.0).iter().all(|v| *v == 0.0));
        // n = 0 : vide, pas de panic.
        assert!(spectrum_bins(&fft, 0, 20.0, 16_000.0).is_empty());
    }

    #[test]
    fn every_fft_bin_in_range_reaches_exactly_one_or_more_output_bins() {
        // Balayage : aucune énergie perdue entre 2 bins de sortie adjacents.
        for i in 3..1600 {
            let fft = frame_with_peaks(&[(i, 1.0)]);
            let out = spectrum_bins(&fft, 64, 20.0, 16_000.0);
            assert!(
                out.iter().any(|v| *v > 0.0),
                "bin FFT {i} ({} Hz) invisible dans la sortie",
                i * 10
            );
        }
    }
}
