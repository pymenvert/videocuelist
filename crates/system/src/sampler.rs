//! Échantillonneur santé machine (CPU, mémoire, température) via `sysinfo`,
//! rafraîchi au plus une fois par seconde (cache interne).

use std::time::{Duration, Instant};

use sysinfo::{Components, Pid, ProcessesToUpdate, System};

/// Intervalle minimal entre deux rafraîchissements sysinfo (max 1 Hz).
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Échantillon système partiel — complété par les FPS via [`crate::merge`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SysSample {
    /// Charge CPU globale de la machine, 0..100.
    pub cpu_pct: f32,
    /// Charge CPU du processus, normalisée machine (somme des cœurs / nb cœurs), 0..100.
    pub process_cpu_pct: f32,
    /// Mémoire résidente du processus, en Mo.
    pub mem_mb: f32,
    /// Température la plus élevée des capteurs exposés (°C).
    /// `None` si la machine n'en expose aucun (sur Pi : lecture /sys plus tard).
    pub temp_c: Option<f32>,
}

/// Échantillonneur santé : possède les états `sysinfo` et impose la cadence.
///
/// `sample()` peut être appelé à chaque frame : le rafraîchissement réel
/// n'a lieu qu'une fois par seconde au plus, sinon le cache est renvoyé.
/// Nota : la toute première mesure CPU vaut 0 (sysinfo a besoin de deux
/// rafraîchissements espacés pour calculer une charge).
pub struct HealthSampler {
    sys: System,
    components: Components,
    pid: Option<Pid>,
    last_refresh: Option<Instant>,
    cached: SysSample,
}

impl HealthSampler {
    /// Crée l'échantillonneur (aucune mesure tant que `sample()` n'est pas appelé).
    pub fn new() -> Self {
        let pid = match sysinfo::get_current_pid() {
            Ok(pid) => Some(pid),
            Err(e) => {
                tracing::warn!(target: "system", error = e, "pid du processus introuvable");
                None
            }
        };
        HealthSampler {
            sys: System::new(),
            components: Components::new(),
            pid,
            last_refresh: None,
            cached: SysSample::default(),
        }
    }

    /// Échantillon courant (cache interne, rafraîchi au plus une fois par seconde).
    pub fn sample(&mut self) -> SysSample {
        let now = Instant::now();
        if let Some(last) = self.last_refresh {
            if now.duration_since(last) < MIN_REFRESH_INTERVAL {
                return self.cached;
            }
        }
        self.last_refresh = Some(now);
        self.refresh();
        self.cached
    }

    /// Rafraîchit réellement les mesures sysinfo (appel coûteux, ~ms).
    fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let cpu_pct = self.sys.global_cpu_usage();
        let n_cpus = self.sys.cpus().len().max(1) as f32;

        let (process_cpu_pct, mem_mb) = match self.pid {
            Some(pid) => {
                self.sys
                    .refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
                match self.sys.process(pid) {
                    Some(p) => (
                        p.cpu_usage() / n_cpus,
                        p.memory() as f32 / (1024.0 * 1024.0),
                    ),
                    None => {
                        tracing::warn!(target: "system", pid = pid.as_u32(), "processus courant non listé par sysinfo");
                        (0.0, 0.0)
                    }
                }
            }
            None => (0.0, 0.0),
        };

        // Température : max des capteurs disponibles (le plus chaud alerte).
        self.components.refresh(true);
        let temp_c = self
            .components
            .iter()
            .filter_map(|c| c.temperature())
            .filter(|t| t.is_finite())
            .fold(None, |acc: Option<f32>, t| {
                Some(acc.map_or(t, |a| a.max(t)))
            });

        self.cached = SysSample {
            cpu_pct,
            process_cpu_pct,
            mem_mb,
            temp_c,
        };
    }
}

impl Default for HealthSampler {
    fn default() -> Self {
        HealthSampler::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fumée : l'échantillonneur mesure des valeurs plausibles sans paniquer.
    #[test]
    fn sample_returns_plausible_values() {
        let mut sampler = HealthSampler::new();
        let s = sampler.sample();
        // Le processus de test occupe forcément de la mémoire.
        assert!(s.mem_mb > 0.0, "mem_mb = {}", s.mem_mb);
        assert!(s.mem_mb < 1_000_000.0, "mem_mb = {}", s.mem_mb);
        assert!((0.0..=100.5).contains(&s.cpu_pct), "cpu_pct = {}", s.cpu_pct);
        assert!(s.process_cpu_pct >= 0.0);
        if let Some(t) = s.temp_c {
            assert!(t.is_finite());
        }
    }

    /// Deux appels immédiats : le second renvoie le cache (cadence max 1 Hz).
    #[test]
    fn immediate_resample_returns_cache() {
        let mut sampler = HealthSampler::new();
        let first = sampler.sample();
        for _ in 0..50 {
            assert_eq!(sampler.sample(), first, "le cache doit être renvoyé tel quel");
        }
    }
}
