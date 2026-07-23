//! # conduite-modulation
//!
//! Machinerie d'évaluation des modulateurs (LFO, bandes audio) — voir
//! `docs/INTERFACES.md`. Les types de configuration (`ModulatorCfg`,
//! `ModRoute`…) vivent dans `conduite-core` ; cette crate ne fait qu'évaluer.
//!
//! Garanties de qualité (normatives) :
//! - horloge monotone fournie par l'appelant, jamais lue ici ;
//! - phase **accumulée** (`phase += freq * dt`) : continuité parfaite quand
//!   la fréquence ou le BPM change, jamais `phase = t * freq` ;
//! - `RandomSh`/`Drift` seedés par l'id du modulateur (reproductibles) ;
//! - enveloppes attack/release exponentielles (constantes de temps en ms) ;
//! - aucune IO, aucun panic en runtime.

mod noise;
mod tap;

use std::f64::consts::TAU;

use conduite_core::{Freq, ModId, ModKind, ModRoute, ModRouteState, ModulatorCfg, RouteMode, Wave};
use tracing::warn;

use noise::{cell_noise, seed_from_id, smoothstep};
use tap::TapTempo;

// ------------------------------------------------------------------ FftFrame

/// Trame d'analyse FFT fournie par `app` (cpal + rustfft), magnitudes par bin.
/// Le bin `i` est centré sur `i * bins_hz`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FftFrame {
    /// Largeur d'un bin en Hz (0 ⇒ trame invalide/absente).
    pub bins_hz: f32,
    pub magnitudes: Vec<f32>,
}

impl FftFrame {
    /// Trame vide (aucune entrée audio) : les bandes redescendent à zéro.
    pub fn empty() -> Self {
        Self::default()
    }
}

// ----------------------------------------------------------------- ModEngine

/// État runtime d'un modulateur (la config reste dans `ModulatorCfg`).
#[derive(Debug)]
enum ModState {
    Lfo {
        /// Phase accumulée en cycles. Jamais recalculée depuis `t` :
        /// c'est ce qui garantit la continuité au changement de fréquence.
        phase: f64,
    },
    AudioBand {
        /// Valeur d'enveloppe courante 0..1.
        env: f32,
    },
}

#[derive(Debug)]
struct ModSlot {
    cfg: ModulatorCfg,
    seed: u32,
    state: ModState,
    /// Dernière valeur calculée (LFO −1..1, AudioBand 0..1), pour l'UI.
    value: f32,
}

#[derive(Debug)]
struct RouteSlot {
    route: ModRoute,
    /// Profondeur effective : celle du show, écrasée par les cues.
    depth: f32,
    enabled: bool,
    /// Index du modulateur source dans `mods`, résolu au `load`.
    source_idx: Option<usize>,
}

/// Moteur de modulation : évalue les modulateurs chaque frame et agrège les
/// routes en offsets par adresse cible (consommés par `params::apply_modulation`).
#[derive(Debug, Default)]
pub struct ModEngine {
    mods: Vec<ModSlot>,
    routes: Vec<RouteSlot>,
    last_now_s: Option<f64>,
    tap: TapTempo,
}

impl ModEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// (Re)charge modulateurs et routes depuis le show.
    ///
    /// La phase des LFO et l'enveloppe des bandes sont **préservées** pour les
    /// ids déjà présents avec le même genre (édition à chaud sans saut). Les
    /// profondeurs/activations de routes repartent des valeurs du show — les
    /// cues les réappliquent via [`Self::apply_route_states`] à chaque GO.
    pub fn load(&mut self, cfgs: &[ModulatorCfg], routes: &[ModRoute]) {
        let old = std::mem::take(&mut self.mods);
        self.mods = cfgs
            .iter()
            .map(|cfg| {
                let carried = old
                    .iter()
                    .find(|s| s.cfg.id == cfg.id)
                    .map(|s| &s.state);
                let state = match (carried, &cfg.kind) {
                    (Some(ModState::Lfo { phase }), ModKind::Lfo { .. }) => {
                        ModState::Lfo { phase: *phase }
                    }
                    (Some(ModState::AudioBand { env }), ModKind::AudioBand { .. }) => {
                        ModState::AudioBand { env: *env }
                    }
                    _ => fresh_state(&cfg.kind),
                };
                ModSlot {
                    seed: seed_from_id(cfg.id),
                    state,
                    value: 0.0,
                    cfg: cfg.clone(),
                }
            })
            .collect();

        self.routes = routes
            .iter()
            .map(|r| {
                let source_idx = self.mods.iter().position(|m| m.cfg.id == r.source);
                if source_idx.is_none() {
                    warn!(
                        target: "modulation",
                        route = r.id,
                        source = r.source,
                        "route vers un modulateur inconnu : ignorée"
                    );
                }
                RouteSlot {
                    depth: r.depth,
                    enabled: true,
                    source_idx,
                    route: r.clone(),
                }
            })
            .collect();
    }

    /// Reset des LFO à leur phase configurée (appelé sur GO de cue).
    /// Les bandes audio ne sont pas affectées.
    pub fn retrigger(&mut self) {
        for slot in &mut self.mods {
            if let (ModKind::Lfo { phase: init, .. }, ModState::Lfo { phase }) =
                (&slot.cfg.kind, &mut slot.state)
            {
                *phase = f64::from(*init).rem_euclid(1.0);
            }
        }
    }

    /// Tap tempo : BPM = 60 / médiane des 3 à 7 derniers intervalles.
    /// `None` tant que la mesure est insuffisante ; un écart > 2 s remet
    /// la mesure à zéro.
    pub fn tap(&mut self, now_s: f64) -> Option<f32> {
        self.tap.tap(now_s)
    }

    /// Force la profondeur d'une route (pilotage par cue ou UI).
    pub fn set_route_depth(&mut self, route_id: u32, depth: f32) {
        if let Some(r) = self.routes.iter_mut().find(|r| r.route.id == route_id) {
            r.depth = depth;
        } else {
            warn!(target: "modulation", route = route_id, "set_route_depth : route inconnue");
        }
    }

    /// Active/désactive une route (pilotage par cue ou UI).
    pub fn set_route_enabled(&mut self, route_id: u32, enabled: bool) {
        if let Some(r) = self.routes.iter_mut().find(|r| r.route.id == route_id) {
            r.enabled = enabled;
        } else {
            warn!(target: "modulation", route = route_id, "set_route_enabled : route inconnue");
        }
    }

    /// Application groupée des états de routes portés par une cue.
    pub fn apply_route_states(&mut self, states: &[ModRouteState]) {
        for s in states {
            self.set_route_depth(s.route_id, s.depth);
            self.set_route_enabled(s.route_id, s.enabled);
        }
    }

    /// Dernières valeurs des modulateurs (feedback UI, `RuntimeStatus::mod_levels`).
    pub fn levels(&self) -> impl Iterator<Item = (ModId, f32)> + '_ {
        self.mods.iter().map(|m| (m.cfg.id, m.value))
    }

    /// Évalue tous les modulateurs à `now_s` (horloge monotone de l'appelant)
    /// et retourne les offsets agrégés par adresse cible.
    ///
    /// Agrégation par adresse, routes prises dans l'ordre du show :
    /// - `Add`     : `acc += signal × depth`
    /// - `Mul`     : `acc ×= 1 − depth + signal × depth`
    /// - `Replace` : `acc = signal × depth`
    pub fn tick(&mut self, now_s: f64, bpm: f32, fft: &FftFrame) -> Vec<(String, f32)> {
        let dt_s = match self.last_now_s {
            Some(last) if now_s > last => now_s - last,
            // Première frame ou horloge qui recule : on n'avance pas.
            _ => 0.0,
        };
        self.last_now_s = Some(now_s);

        for slot in &mut self.mods {
            slot.value = match (&slot.cfg.kind, &mut slot.state) {
                (ModKind::Lfo { wave, freq, .. }, ModState::Lfo { phase }) => {
                    let hz = resolve_freq(*freq, bpm);
                    let hz = if hz.is_finite() { hz.max(0.0) } else { 0.0 };
                    *phase += f64::from(hz) * dt_s;
                    eval_wave(*wave, *phase, slot.seed)
                }
                (
                    ModKind::AudioBand {
                        low_hz,
                        high_hz,
                        gain,
                        floor,
                        attack_ms,
                        release_ms,
                    },
                    ModState::AudioBand { env },
                ) => {
                    let target = band_level(fft, *low_hz, *high_hz, *gain, *floor);
                    *env = envelope_step(*env, target, dt_s, *attack_ms, *release_ms);
                    *env
                }
                // Genre et état désalignés : impossible après `load`, valeur neutre.
                _ => 0.0,
            };
        }

        let mut out: Vec<(String, f32)> = Vec::with_capacity(self.routes.len());
        for r in &self.routes {
            if !r.enabled {
                continue;
            }
            let Some(idx) = r.source_idx else { continue };
            let Some(signal) = self.mods.get(idx).map(|m| m.value) else {
                continue;
            };
            let addr = &r.route.target_addr;
            let slot = match out.iter().position(|(a, _)| a == addr) {
                Some(p) => p,
                None => {
                    out.push((addr.clone(), 0.0));
                    out.len() - 1
                }
            };
            let Some((_, acc)) = out.get_mut(slot) else {
                continue;
            };
            match r.route.mode {
                RouteMode::Add => *acc += signal * r.depth,
                RouteMode::Mul => *acc *= 1.0 - r.depth + signal * r.depth,
                RouteMode::Replace => *acc = signal * r.depth,
            }
        }
        out
    }
}

// ------------------------------------------------------------------- Interne

/// État runtime initial pour un genre de modulateur.
fn fresh_state(kind: &ModKind) -> ModState {
    match kind {
        ModKind::Lfo { phase, .. } => ModState::Lfo {
            phase: f64::from(*phase).rem_euclid(1.0),
        },
        ModKind::AudioBand { .. } => ModState::AudioBand { env: 0.0 },
    }
}

/// Fréquence effective en Hz : fixe ou synchronisée BPM (`bpm/60 × mult`).
fn resolve_freq(freq: Freq, bpm: f32) -> f32 {
    match freq {
        Freq::Hz(hz) => hz,
        Freq::BpmSync { mult } => bpm.max(0.0) / 60.0 * mult,
    }
}

/// Évalue une forme d'onde à la phase accumulée donnée. Sortie −1..1.
///
/// Sine/Tri sont alignées (valeur 0 en phase 0, crête à 0.25) ; Saw monte de
/// −1 à 1 sur le cycle ; Square vaut +1 tant que `frac < pw`.
fn eval_wave(wave: Wave, phase: f64, seed: u32) -> f32 {
    // `phase >= 0` garanti (fréquence clampée ≥ 0, phase initiale normalisée).
    let frac = phase.fract();
    match wave {
        Wave::Sine => (frac * TAU).sin() as f32,
        Wave::Tri => {
            let t = (frac + 0.75).fract() as f32;
            4.0 * (t - 0.5).abs() - 1.0
        }
        Wave::Square { pw } => {
            let pw = f64::from(pw.clamp(0.0, 1.0));
            if frac < pw {
                1.0
            } else {
                -1.0
            }
        }
        Wave::Saw => (2.0 * frac - 1.0) as f32,
        // S&H : une valeur par cycle (tenue 1/freq s), déterministe par seed.
        Wave::RandomSh => cell_noise(seed, phase.floor() as i64),
        // Value-noise : valeurs de lattice seedées, interpolation smoothstep.
        Wave::Drift => {
            let n = phase.floor() as i64;
            let a = cell_noise(seed, n);
            let b = cell_noise(seed, n.wrapping_add(1));
            a + (b - a) * smoothstep(phase.fract() as f32)
        }
    }
}

/// Niveau instantané d'une bande : somme des magnitudes des bins dont la
/// fréquence centrale tombe dans `[low_hz, high_hz]`, plancher soustrait,
/// gain appliqué, clampé 0..1.
fn band_level(fft: &FftFrame, low_hz: f32, high_hz: f32, gain: f32, floor: f32) -> f32 {
    if fft.bins_hz <= 0.0 || fft.magnitudes.is_empty() {
        return 0.0;
    }
    let mut sum = 0.0f32;
    for (i, m) in fft.magnitudes.iter().enumerate() {
        let f = i as f32 * fft.bins_hz;
        if f >= low_hz && f <= high_hz {
            sum += m;
        }
    }
    ((sum - floor).max(0.0) * gain).clamp(0.0, 1.0)
}

/// Un pas d'enveloppe exponentielle : constante de temps `attack_ms` à la
/// montée, `release_ms` à la descente. `tau ≤ 0` ⇒ suivi instantané.
fn envelope_step(env: f32, target: f32, dt_s: f64, attack_ms: f32, release_ms: f32) -> f32 {
    let tau_ms = if target > env { attack_ms } else { release_ms };
    let alpha = if tau_ms <= 0.0 {
        1.0
    } else {
        1.0 - (-(dt_s * 1000.0) / f64::from(tau_ms)).exp()
    };
    (env + (target - env) * alpha as f32).clamp(0.0, 1.0)
}

// -------------------------------------------------------------------- Tests

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    fn lfo(id: ModId, wave: Wave, freq: Freq, phase: f32) -> ModulatorCfg {
        ModulatorCfg {
            id,
            name: format!("lfo{id}"),
            kind: ModKind::Lfo { wave, freq, phase },
        }
    }

    fn band_cfg(
        id: ModId,
        low_hz: f32,
        high_hz: f32,
        gain: f32,
        floor: f32,
        attack_ms: f32,
        release_ms: f32,
    ) -> ModulatorCfg {
        ModulatorCfg {
            id,
            name: format!("band{id}"),
            kind: ModKind::AudioBand {
                low_hz,
                high_hz,
                gain,
                floor,
                attack_ms,
                release_ms,
            },
        }
    }

    fn route(id: u32, source: ModId, addr: &str, depth: f32, mode: RouteMode) -> ModRoute {
        ModRoute {
            id,
            source,
            target_addr: addr.into(),
            depth,
            mode,
        }
    }

    fn offset(out: &[(String, f32)], addr: &str) -> f32 {
        out.iter()
            .find(|(a, _)| a == addr)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| panic!("adresse {addr} absente de {out:?}"))
    }

    /// Moteur avec un seul LFO routé Add depth 1 sur "t".
    fn single_lfo(wave: Wave, freq: Freq, phase: f32) -> ModEngine {
        let mut e = ModEngine::new();
        e.load(
            &[lfo(1, wave, freq, phase)],
            &[route(1, 1, "t", 1.0, RouteMode::Add)],
        );
        e
    }

    fn value_at(e: &mut ModEngine, now_s: f64) -> f32 {
        let out = e.tick(now_s, 120.0, &FftFrame::empty());
        offset(&out, "t")
    }

    // -------------------------------------------------------- Formes d'onde

    #[test]
    fn sine_known_values() {
        let mut e = single_lfo(Wave::Sine, Freq::Hz(1.0), 0.0);
        assert!(value_at(&mut e, 0.0).abs() < EPS);
        assert!((value_at(&mut e, 0.25) - 1.0).abs() < EPS);
        assert!(value_at(&mut e, 0.5).abs() < 1e-4);
        assert!((value_at(&mut e, 0.75) + 1.0).abs() < EPS);
    }

    #[test]
    fn tri_known_values() {
        let mut e = single_lfo(Wave::Tri, Freq::Hz(1.0), 0.0);
        assert!(value_at(&mut e, 0.0).abs() < EPS, "tri(0) = 0");
        assert!((value_at(&mut e, 0.125) - 0.5).abs() < EPS);
        assert!((value_at(&mut e, 0.25) - 1.0).abs() < EPS, "crête à 0.25");
        assert!(value_at(&mut e, 0.5).abs() < EPS);
        assert!((value_at(&mut e, 0.75) + 1.0).abs() < EPS, "creux à 0.75");
    }

    #[test]
    fn square_pulse_width() {
        let mut e = single_lfo(Wave::Square { pw: 0.25 }, Freq::Hz(1.0), 0.0);
        value_at(&mut e, 0.0); // ancre l'horloge (première frame, dt = 0)
        assert!((value_at(&mut e, 0.1) - 1.0).abs() < EPS, "dans l'impulsion");
        assert!((value_at(&mut e, 0.3) + 1.0).abs() < EPS, "hors impulsion");
        assert!((value_at(&mut e, 0.9) + 1.0).abs() < EPS);
        assert!((value_at(&mut e, 1.1) - 1.0).abs() < EPS, "cycle suivant");
    }

    #[test]
    fn saw_known_values() {
        let mut e = single_lfo(Wave::Saw, Freq::Hz(1.0), 0.0);
        assert!((value_at(&mut e, 0.0) + 1.0).abs() < EPS, "saw(0) = -1");
        assert!(value_at(&mut e, 0.5).abs() < EPS);
        assert!((value_at(&mut e, 0.75) - 0.5).abs() < EPS);
        // Retour à -1 au cycle suivant.
        assert!((value_at(&mut e, 1.0) + 1.0).abs() < 1e-4);
    }

    #[test]
    fn initial_phase_offsets_the_wave() {
        let mut e = single_lfo(Wave::Saw, Freq::Hz(1.0), 0.25);
        // saw(0.25) = -0.5 dès la première frame.
        assert!((value_at(&mut e, 0.0) + 0.5).abs() < EPS);
    }

    // --------------------------------------------------- Continuité de phase

    #[test]
    fn phase_is_continuous_when_freq_changes() {
        let mut e = ModEngine::new();
        let routes = [route(1, 1, "t", 1.0, RouteMode::Add)];
        e.load(&[lfo(1, Wave::Saw, Freq::Hz(1.0), 0.0)], &routes);
        value_at(&mut e, 0.0);
        let before = value_at(&mut e, 0.25); // phase 0.25 → -0.5
        assert!((before + 0.5).abs() < EPS);

        // Changement de fréquence à chaud : 1 Hz → 4 Hz.
        e.load(&[lfo(1, Wave::Saw, Freq::Hz(4.0), 0.0)], &routes);
        let after = value_at(&mut e, 0.26); // phase 0.25 + 4×0.01 = 0.29
        assert!(
            (after - before - 2.0 * 0.04).abs() < 1e-4,
            "petit pas continu, obtenu {after} depuis {before}"
        );
        // Le calcul naïf `phase = t × freq` aurait donné saw(0.26×4 = 1.04) ≈ -0.92.
        assert!(
            (after - (2.0 * (0.26 * 4.0 - 1.0) - 1.0)).abs() > 0.3,
            "la phase ne doit PAS être recalculée depuis t"
        );
    }

    #[test]
    fn phase_is_continuous_when_bpm_changes() {
        let mut e = single_lfo(Wave::Saw, Freq::BpmSync { mult: 1.0 }, 0.0);
        let fft = FftFrame::empty();
        e.tick(0.0, 60.0, &fft); // 1 Hz
        let out = e.tick(0.25, 60.0, &fft);
        let before = offset(&out, "t"); // phase 0.25 → -0.5
        assert!((before + 0.5).abs() < EPS);
        // BPM doublé : la phase continue d'avancer, pas de saut.
        let out = e.tick(0.30, 120.0, &fft); // phase 0.25 + 2×0.05 = 0.35
        let after = offset(&out, "t");
        assert!((after + 0.3).abs() < 1e-4, "obtenu {after}");
    }

    #[test]
    fn bpm_sync_frequency_is_bpm_over_60_times_mult() {
        // 120 BPM, mult 0.25 → 0.5 Hz : un demi-cycle en 1 s.
        let mut e = single_lfo(Wave::Saw, Freq::BpmSync { mult: 0.25 }, 0.0);
        let fft = FftFrame::empty();
        e.tick(0.0, 120.0, &fft);
        let out = e.tick(1.0, 120.0, &fft);
        assert!(offset(&out, "t").abs() < EPS, "phase 0.5 → saw = 0");
    }

    // ------------------------------------------------------ RandomSh & Drift

    #[test]
    fn random_sh_holds_one_value_per_cycle() {
        let mut e = single_lfo(Wave::RandomSh, Freq::Hz(1.0), 0.0);
        let v1 = value_at(&mut e, 0.1);
        let v2 = value_at(&mut e, 0.5);
        let v3 = value_at(&mut e, 0.9);
        assert_eq!(v1, v2, "tenue pendant 1/freq s");
        assert_eq!(v2, v3);
        // Sur plusieurs cycles, la valeur doit changer au moins une fois.
        let mut changed = false;
        for c in 1..10 {
            if value_at(&mut e, c as f64 + 0.5) != v1 {
                changed = true;
            }
        }
        assert!(changed, "S&H figé sur 10 cycles");
    }

    #[test]
    fn random_sh_is_seeded_by_mod_id() {
        // Même id ⇒ même séquence ; ids différents ⇒ séquences différentes.
        let mut a1 = single_lfo(Wave::RandomSh, Freq::Hz(1.0), 0.0);
        let mut a2 = single_lfo(Wave::RandomSh, Freq::Hz(1.0), 0.0);
        let mut b = ModEngine::new();
        b.load(
            &[lfo(2, Wave::RandomSh, Freq::Hz(1.0), 0.0)],
            &[route(1, 2, "t", 1.0, RouteMode::Add)],
        );
        let mut same = true;
        let mut differs = false;
        for c in 0..10 {
            let t = c as f64 + 0.5;
            let v1 = value_at(&mut a1, t);
            let v2 = value_at(&mut a2, t);
            let v3 = value_at(&mut b, t);
            same &= v1 == v2;
            differs |= v1 != v3;
        }
        assert!(same, "même seed ⇒ même séquence");
        assert!(differs, "seeds différents ⇒ séquences différentes");
    }

    #[test]
    fn drift_is_smooth_and_bounded() {
        let mut e = single_lfo(Wave::Drift, Freq::Hz(1.0), 0.0);
        let mut prev = value_at(&mut e, 0.0);
        for i in 1..=400 {
            let v = value_at(&mut e, i as f64 * 0.01);
            assert!((-1.0..=1.0).contains(&v), "drift hors bornes : {v}");
            // Pente maxi d'un value-noise smoothstep : 1.5 × amplitude par cycle.
            assert!(
                (v - prev).abs() < 3.0 * 1.5 * 0.01 + 1e-4,
                "saut de drift à t={} : {prev} → {v}",
                i as f64 * 0.01
            );
            prev = v;
        }
    }

    // ------------------------------------------------------------ Retrigger

    #[test]
    fn retrigger_resets_lfo_to_configured_phase() {
        let mut e = single_lfo(Wave::Saw, Freq::Hz(1.0), 0.25);
        value_at(&mut e, 0.0);
        let far = value_at(&mut e, 0.6); // phase 0.85
        assert!((far - 0.7).abs() < EPS);
        e.retrigger();
        // Phase repart à 0.25 ; +0.01 s d'avance au tick suivant.
        let v = value_at(&mut e, 0.61);
        assert!((v + 0.48).abs() < 1e-4, "obtenu {v}");
    }

    // ------------------------------------------------------------ Bande FFT

    /// Trame synthétique : bins de 100 Hz, magnitude = index du bin.
    fn synth_frame() -> FftFrame {
        FftFrame {
            bins_hz: 100.0,
            magnitudes: (0..11).map(|i| i as f32).collect(),
        }
    }

    #[test]
    fn audio_band_sums_bins_in_range() {
        // Bande [150, 450] : bins 2, 3, 4 (200/300/400 Hz) → somme 9.
        // (9 − plancher 1) × gain 0.1 = 0.8 ; attack 0 ⇒ suivi instantané.
        let mut e = ModEngine::new();
        e.load(
            &[band_cfg(1, 150.0, 450.0, 0.1, 1.0, 0.0, 0.0)],
            &[route(1, 1, "t", 1.0, RouteMode::Add)],
        );
        let out = e.tick(0.0, 120.0, &synth_frame());
        assert!((offset(&out, "t") - 0.8).abs() < EPS);
    }

    #[test]
    fn audio_band_bounds_are_inclusive_and_output_clamped() {
        let mut e = ModEngine::new();
        e.load(
            &[
                // Bornes inclusives : [200, 400] prend exactement les bins 2..4.
                band_cfg(1, 200.0, 400.0, 0.1, 1.0, 0.0, 0.0),
                // Somme énorme × gain 1 : clamp à 1.
                band_cfg(2, 0.0, 2000.0, 1.0, 0.0, 0.0, 0.0),
                // Plancher au-dessus de la somme : sortie 0 (jamais négative).
                band_cfg(3, 150.0, 450.0, 1.0, 100.0, 0.0, 0.0),
            ],
            &[
                route(1, 1, "a", 1.0, RouteMode::Add),
                route(2, 2, "b", 1.0, RouteMode::Add),
                route(3, 3, "c", 1.0, RouteMode::Add),
            ],
        );
        let out = e.tick(0.0, 120.0, &synth_frame());
        assert!((offset(&out, "a") - 0.8).abs() < EPS);
        assert!((offset(&out, "b") - 1.0).abs() < EPS);
        assert!(offset(&out, "c").abs() < EPS);
    }

    #[test]
    fn envelope_attack_and_release_are_exponential() {
        // Attack 100 ms, release 200 ms, cible 1 quand la trame est forte.
        let mut e = ModEngine::new();
        e.load(
            &[band_cfg(1, 0.0, 1000.0, 1.0, 0.0, 100.0, 200.0)],
            &[route(1, 1, "t", 1.0, RouteMode::Add)],
        );
        let loud = FftFrame {
            bins_hz: 100.0,
            magnitudes: vec![10.0; 4],
        };
        // Première frame : dt = 0, l'enveloppe ne bouge pas.
        let out = e.tick(0.0, 120.0, &loud);
        assert!(offset(&out, "t").abs() < EPS);
        // Après 1 constante de temps : 1 − e⁻¹ ≈ 0.632.
        let out = e.tick(0.1, 120.0, &loud);
        assert!((offset(&out, "t") - 0.6321).abs() < 1e-3);
        // Après 2 τ : 1 − e⁻² ≈ 0.865.
        let out = e.tick(0.2, 120.0, &loud);
        let peak = offset(&out, "t");
        assert!((peak - 0.8647).abs() < 1e-3);
        // Silence pendant 200 ms = 1 τ de release : peak × e⁻¹.
        let out = e.tick(0.4, 120.0, &FftFrame::empty());
        assert!((offset(&out, "t") - peak * (-1.0f32).exp()).abs() < 1e-3);
    }

    // --------------------------------------------------------------- Routes

    /// Trois sources constantes : A = +1, B = −1, C = 0.
    fn three_sources() -> ModEngine {
        let mut e = ModEngine::new();
        e.load(
            &[
                lfo(1, Wave::Square { pw: 1.0 }, Freq::Hz(1.0), 0.0), // +1
                lfo(2, Wave::Square { pw: 0.0 }, Freq::Hz(1.0), 0.0), // -1
                lfo(3, Wave::Sine, Freq::Hz(1.0), 0.0),               // 0 à t=0
            ],
            &[],
        );
        e
    }

    fn load_routes(e: &mut ModEngine, routes: &[ModRoute]) {
        let cfgs = [
            lfo(1, Wave::Square { pw: 1.0 }, Freq::Hz(1.0), 0.0),
            lfo(2, Wave::Square { pw: 0.0 }, Freq::Hz(1.0), 0.0),
            lfo(3, Wave::Sine, Freq::Hz(1.0), 0.0),
        ];
        e.load(&cfgs, routes);
    }

    #[test]
    fn routes_add_sum_per_address() {
        let mut e = three_sources();
        load_routes(
            &mut e,
            &[
                route(1, 1, "x", 0.25, RouteMode::Add),
                route(2, 2, "x", 0.5, RouteMode::Add),
                route(3, 1, "y", 0.7, RouteMode::Add),
            ],
        );
        let out = e.tick(0.0, 120.0, &FftFrame::empty());
        // x : (+1)(0.25) + (−1)(0.5) = −0.25 ; y : 0.7.
        assert!((offset(&out, "x") + 0.25).abs() < EPS);
        assert!((offset(&out, "y") - 0.7).abs() < EPS);
        assert_eq!(out.len(), 2, "une entrée par adresse");
    }

    #[test]
    fn route_mul_scales_accumulated_offset() {
        let mut e = three_sources();
        load_routes(
            &mut e,
            &[
                route(1, 1, "x", 1.0, RouteMode::Add), // acc = 1
                route(2, 3, "x", 0.5, RouteMode::Mul), // ×(1−0.5+0×0.5) = ×0.5
            ],
        );
        let out = e.tick(0.0, 120.0, &FftFrame::empty());
        assert!((offset(&out, "x") - 0.5).abs() < EPS);
    }

    #[test]
    fn route_replace_overrides_previous_routes() {
        let mut e = three_sources();
        load_routes(
            &mut e,
            &[
                route(1, 1, "x", 1.0, RouteMode::Add),
                route(2, 2, "x", 0.5, RouteMode::Replace), // = (−1)(0.5)
            ],
        );
        let out = e.tick(0.0, 120.0, &FftFrame::empty());
        assert!((offset(&out, "x") + 0.5).abs() < EPS);
    }

    #[test]
    fn route_depth_and_enable_are_cue_controllable() {
        let mut e = three_sources();
        load_routes(
            &mut e,
            &[
                route(1, 1, "x", 0.25, RouteMode::Add),
                route(2, 2, "y", 1.0, RouteMode::Add),
            ],
        );
        // Une cue change la profondeur de la route 1 et coupe la route 2.
        e.apply_route_states(&[
            ModRouteState {
                route_id: 1,
                depth: 0.8,
                enabled: true,
            },
            ModRouteState {
                route_id: 2,
                depth: 1.0,
                enabled: false,
            },
        ]);
        let out = e.tick(0.0, 120.0, &FftFrame::empty());
        assert!((offset(&out, "x") - 0.8).abs() < EPS);
        assert!(!out.iter().any(|(a, _)| a == "y"), "route coupée : pas d'offset");

        // Réactivation directe.
        e.set_route_enabled(2, true);
        e.set_route_depth(2, 0.5);
        let out = e.tick(0.01, 120.0, &FftFrame::empty());
        assert!((offset(&out, "y") + 0.5).abs() < EPS);
    }

    #[test]
    fn route_to_unknown_modulator_is_ignored() {
        let mut e = ModEngine::new();
        e.load(
            &[lfo(1, Wave::Sine, Freq::Hz(1.0), 0.0)],
            &[route(1, 99, "x", 1.0, RouteMode::Add)],
        );
        let out = e.tick(0.0, 120.0, &FftFrame::empty());
        assert!(out.is_empty());
    }

    // ------------------------------------------------------------ Divers

    #[test]
    fn tap_tempo_through_engine() {
        let mut e = ModEngine::new();
        assert_eq!(e.tap(0.0), None);
        assert_eq!(e.tap(0.5), None);
        assert_eq!(e.tap(1.0), None);
        let bpm = e.tap(1.5).expect("4 taps");
        assert!((bpm - 120.0).abs() < 0.01);
    }

    #[test]
    fn non_monotonic_clock_does_not_advance_phase() {
        let mut e = single_lfo(Wave::Saw, Freq::Hz(1.0), 0.0);
        value_at(&mut e, 1.0);
        let v1 = value_at(&mut e, 1.25);
        // L'horloge recule : la phase ne bouge pas.
        let v2 = value_at(&mut e, 0.5);
        assert_eq!(v1, v2);
    }

    #[test]
    fn load_preserves_phase_for_existing_ids_and_resets_new_ones() {
        let mut e = ModEngine::new();
        let routes = [
            route(1, 1, "t", 1.0, RouteMode::Add),
            route(2, 2, "u", 1.0, RouteMode::Add),
        ];
        e.load(&[lfo(1, Wave::Saw, Freq::Hz(1.0), 0.0)], &routes[..1]);
        value_at(&mut e, 0.0);
        value_at(&mut e, 0.4); // phase du LFO 1 = 0.4
        // Rechargement avec un modulateur en plus.
        e.load(
            &[
                lfo(1, Wave::Saw, Freq::Hz(1.0), 0.0),
                lfo(2, Wave::Saw, Freq::Hz(1.0), 0.0),
            ],
            &routes,
        );
        let out = e.tick(0.4, 120.0, &FftFrame::empty());
        // LFO 1 : phase préservée (0.4) ; LFO 2 : phase fraîche (0).
        assert!((offset(&out, "t") - (2.0 * 0.4 - 1.0)).abs() < 1e-4);
        assert!((offset(&out, "u") + 1.0).abs() < 1e-4);
    }

    #[test]
    fn levels_report_last_values() {
        let mut e = three_sources();
        e.tick(0.0, 120.0, &FftFrame::empty());
        let levels: Vec<(ModId, f32)> = e.levels().collect();
        assert_eq!(levels.len(), 3);
        assert!((levels[0].1 - 1.0).abs() < EPS); // A = +1
        assert!((levels[1].1 + 1.0).abs() < EPS); // B = −1
        assert!(levels[2].1.abs() < EPS); // C = 0
    }
}
