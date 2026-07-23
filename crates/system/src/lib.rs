//! # conduite-system
//!
//! Santé machine pour le bandeau Live et la page Journal :
//! - [`HealthSampler`] : CPU (processus + global), mémoire, température,
//!   via `sysinfo`, rafraîchi au plus une fois par seconde ;
//! - [`SamplerThread`] : le même échantillonnage sur un **thread dédié**
//!   (le rafraîchissement sysinfo — WMI sous Windows — peut bloquer des
//!   dizaines de ms : jamais sur le thread de rendu) ; le tick ne fait que
//!   [`SamplerThread::latest`], copie sous mutex très court ;
//! - [`FpsCounter`] : FPS lissé et frames perdues par sortie — **pur**,
//!   horloge injectée, testé avec une horloge simulée ;
//! - [`merge`] : fusion des compteurs et de l'échantillon système en
//!   [`HealthSnapshot`] (publié via `StateEvent::HealthTick`).

mod fps;
mod sampler;

pub use fps::FpsCounter;
pub use sampler::{HealthSampler, SamplerThread, SysSample};

use conduite_core::{HealthSnapshot, OutputId};

/// Fusionne les compteurs FPS par sortie et l'échantillon système en un
/// instantané santé prêt à publier.
///
/// `cpu_pct` reprend la charge **globale** machine et `mem_mb` la mémoire
/// du **processus** (la charge processus reste disponible dans [`SysSample`]).
pub fn merge(fps: &[(OutputId, &FpsCounter)], sys: SysSample) -> HealthSnapshot {
    HealthSnapshot {
        fps: fps.iter().map(|(id, c)| (*id, c.fps())).collect(),
        drops: fps.iter().map(|(id, c)| (*id, c.drops())).collect(),
        cpu_pct: sys.cpu_pct,
        mem_mb: sys.mem_mb,
        temp_c: sys.temp_c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_builds_snapshot_per_output() {
        // Sortie 1 : cadence tenue ; sortie 2 : une frame perdue.
        let mut c1 = FpsCounter::new(60.0);
        let mut c2 = FpsCounter::new(60.0);
        let mut now = 0.0;
        for _ in 0..120 {
            now += 1.0 / 60.0;
            c1.tick(now);
            c2.tick(now);
        }
        c2.tick(now + 2.0 / 60.0);

        let sys = SysSample {
            cpu_pct: 42.0,
            process_cpu_pct: 12.5,
            mem_mb: 256.0,
            temp_c: Some(55.0),
        };
        let snap = merge(&[(1, &c1), (2, &c2)], sys);

        assert_eq!(snap.fps.len(), 2);
        assert_eq!(snap.fps[0].0, 1);
        assert!((snap.fps[0].1 - 60.0).abs() < 0.01);
        assert_eq!(snap.drops, vec![(1, 0), (2, 1)]);
        assert_eq!(snap.cpu_pct, 42.0);
        assert_eq!(snap.mem_mb, 256.0);
        assert_eq!(snap.temp_c, Some(55.0));
    }

    #[test]
    fn merge_with_no_outputs_is_empty_but_valid() {
        let snap = merge(&[], SysSample::default());
        assert!(snap.fps.is_empty());
        assert!(snap.drops.is_empty());
        assert_eq!(snap.temp_c, None);
    }
}
