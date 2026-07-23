//! Échantillonneur santé machine (CPU, mémoire, température) via `sysinfo`,
//! rafraîchi au plus une fois par seconde (cache interne).
//!
//! Le rafraîchissement réel est COÛTEUX (sous Windows, la lecture des
//! capteurs passe par WMI : blocages de plusieurs à dizaines de ms
//! possibles) : en production, utiliser [`SamplerThread`] — le sysinfo
//! tourne sur un thread dédié et le thread de rendu ne fait que copier le
//! dernier échantillon publié.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sysinfo::{Components, Pid, ProcessesToUpdate, System};

/// Intervalle minimal entre deux rafraîchissements sysinfo (max 1 Hz).
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
/// Période de la boucle du [`SamplerThread`] : courte pour un arrêt réactif
/// (drop ≤ ~100 ms + rafraîchissement en cours), la cadence réelle des
/// mesures restant imposée par [`HealthSampler`] (1 Hz max).
const THREAD_TICK: Duration = Duration::from_millis(100);

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

/// Échantillonneur santé sur thread dédié.
///
/// Le rafraîchissement `sysinfo` (WMI sous Windows, /proc et hwmon sous
/// Linux) ne doit JAMAIS tourner sur le thread de rendu : ce thread nommé
/// `conduite-health` fait les mesures et publie son [`SysSample`] dans un
/// slot partagé. Le tick de rendu consomme via [`SamplerThread::latest`] —
/// copie d'un `Copy` sous mutex très court, aucun appel sysinfo, jamais
/// bloquant plus que la durée d'une copie.
///
/// Arrêt propre au [`Drop`] : drapeau + join (≤ ~100 ms hors mesure en cours).
pub struct SamplerThread {
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<SysSample>>,
    handle: Option<JoinHandle<()>>,
}

impl SamplerThread {
    /// Démarre le thread d'échantillonnage. `Err` si l'OS refuse de créer le
    /// thread (rarissime) : à l'appelant de tracer et de continuer sans
    /// santé machine — jamais de panic en régie.
    pub fn spawn() -> std::io::Result<SamplerThread> {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(SysSample::default()));
        let thread_stop = Arc::clone(&stop);
        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("conduite-health".into())
            .spawn(move || {
                let mut sampler = HealthSampler::new();
                while !thread_stop.load(Ordering::Relaxed) {
                    // Coûteux au plus 1×/s (cadence interne du sampler) ;
                    // entre deux rafraîchissements, republie le cache.
                    let sample = sampler.sample();
                    match thread_shared.lock() {
                        Ok(mut slot) => *slot = sample,
                        // Mutex empoisonné (panic d'un lecteur) : on republie
                        // quand même — la santé machine ne s'arrête pas.
                        Err(poisoned) => *poisoned.into_inner() = sample,
                    }
                    std::thread::sleep(THREAD_TICK);
                }
                tracing::debug!(target: "system", "thread santé arrêté");
            })?;
        Ok(SamplerThread {
            stop,
            shared,
            handle: Some(handle),
        })
    }

    /// Dernier échantillon publié — verrou court, aucun appel sysinfo.
    /// Vaut [`SysSample::default`] tant que le thread n'a rien publié, et la
    /// première mesure CPU vaut 0 (voir [`HealthSampler`]).
    pub fn latest(&self) -> SysSample {
        match self.shared.lock() {
            Ok(slot) => *slot,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                tracing::warn!(target: "system", "le thread santé a paniqué");
            }
        }
    }
}

impl Drop for SamplerThread {
    fn drop(&mut self) {
        self.stop_and_join();
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

    /// Le thread dédié publie un échantillon plausible et s'arrête vite au
    /// drop (le thread de rendu ne doit jamais attendre longtemps).
    #[test]
    fn sampler_thread_publishes_and_stops_quickly() {
        let thread = SamplerThread::spawn().expect("spawn du thread santé");
        // latest() ne bloque pas et finit par refléter une vraie mesure
        // (mem_mb > 0 dès la première publication).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let s = thread.latest();
            if s.mem_mb > 0.0 {
                assert!((0.0..=100.5).contains(&s.cpu_pct), "cpu_pct = {}", s.cpu_pct);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "aucun échantillon publié en 5 s"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let started = Instant::now();
        drop(thread);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "arrêt trop lent : {:?}",
            started.elapsed()
        );
    }
}
