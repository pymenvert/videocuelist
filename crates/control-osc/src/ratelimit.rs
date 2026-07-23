//! Limiteur de débit pour les `warn!` des chemins de réception réseau.
//!
//! L'OSC écoute des paquets UDP non authentifiés : un flood d'adresses
//! inconnues ou de paquets malformés ne doit pas coûter un formatage de log
//! (console synchrone + rediffusion UI) PAR paquet — c'est un DoS log/CPU du
//! thread de réception. Au plus une émission par seconde et par catégorie ;
//! les messages tus sont comptés et le compte est publié au warn suivant.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// Fenêtre minimale entre deux émissions (1 s).
const WINDOW_MS: u64 = 1_000;

/// Limiteur d'émission de warn, partageable en `static` (atomiques, aucun
/// verrou). Logique pure : l'horloge est injectée en millisecondes.
pub(crate) struct WarnLimiter {
    /// Horodatage (ms) de la dernière émission ; 0 = jamais émis.
    last_emit_ms: AtomicU64,
    /// Messages tus depuis la dernière émission.
    suppressed: AtomicU64,
}

impl WarnLimiter {
    pub(crate) const fn new() -> Self {
        WarnLimiter {
            last_emit_ms: AtomicU64::new(0),
            suppressed: AtomicU64::new(0),
        }
    }

    /// À appeler avant d'émettre : `Some(n)` = émission autorisée, avec `n`
    /// messages tus depuis la dernière émission ; `None` = se taire.
    pub(crate) fn allow(&self, now_ms: u64) -> Option<u64> {
        let last = self.last_emit_ms.load(Ordering::Relaxed);
        if last == 0 || now_ms.saturating_sub(last) >= WINDOW_MS {
            // CAS : une seule émission par fenêtre, même en cas de course
            // entre threads. `max(1)` distingue « jamais émis » (0) d'une
            // émission à t=0.
            if self
                .last_emit_ms
                .compare_exchange(last, now_ms.max(1), Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return Some(self.suppressed.swap(0, Ordering::Relaxed));
            }
        }
        self.suppressed.fetch_add(1, Ordering::Relaxed);
        None
    }
}

/// Millisecondes écoulées depuis le premier appel (horloge process,
/// monotone) — l'entrée `now_ms` des limiteurs statiques.
pub(crate) fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_warn_passes_immediately() {
        let l = WarnLimiter::new();
        assert_eq!(l.allow(0), Some(0), "première émission autorisée, même à t=0");
    }

    #[test]
    fn flood_is_suppressed_and_counted() {
        let l = WarnLimiter::new();
        assert_eq!(l.allow(10), Some(0));
        // Flood dans la même seconde : tout est tu.
        for t in 11..500 {
            assert_eq!(l.allow(t), None, "t={t} : dans la fenêtre, silence");
        }
        // Fenêtre écoulée : une émission repart, avec le compte des tus.
        assert_eq!(l.allow(1_010), Some(489));
        // Et le compteur est bien remis à zéro.
        assert_eq!(l.allow(2_020), Some(0));
    }

    #[test]
    fn window_boundary_is_one_second() {
        let l = WarnLimiter::new();
        assert_eq!(l.allow(5), Some(0));
        assert_eq!(l.allow(1_004), None, "999 ms : encore dans la fenêtre");
        assert_eq!(l.allow(1_005), Some(1), "1000 ms : fenêtre rouverte");
    }
}
