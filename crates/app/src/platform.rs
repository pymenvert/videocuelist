//! Intégration plateforme : anti-veille, pacing temps réel Windows et
//! arrêt propre sur Ctrl-C. Tout est en **dégradation silencieuse** : un
//! refus de l'OS est loggué en debug et n'empêche jamais le show.

use std::sync::atomic::{AtomicBool, Ordering};

/// Drapeau global posé par le handler Ctrl-C/console : les boucles de
/// l'application le consultent à chaque tick et sortent proprement (code 0).
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Un arrêt (Ctrl-C, fermeture de console) a été demandé.
pub fn quit_requested() -> bool {
    QUIT_REQUESTED.load(Ordering::Relaxed)
}

/// Demande d'arrêt programmatique (partagée avec le handler OS).
pub fn request_quit() {
    QUIT_REQUESTED.store(true, Ordering::Relaxed);
}

// ------------------------------------------------------------------ windows

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};

    use tracing::{debug, info, warn};
    use windows_sys::Win32::Foundation::TRUE;
    use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    };
    use windows_sys::Win32::System::Threading::AvSetMmThreadCharacteristicsW;

    /// État courant de l'anti-veille (évite les appels répétés par frame).
    static AWAKE: AtomicBool = AtomicBool::new(false);

    /// État courant du boost de priorité process (évite les appels répétés).
    static BOOSTED: AtomicBool = AtomicBool::new(false);

    /// Priorité process en OPTION (`ShowSettings::boost_priority`, défaut
    /// faux) : ABOVE_NORMAL au passage en mode Show, retour à Normal en mode
    /// Edit. Filet de sécurité machine partagée — HIGH est volontairement
    /// évité (affame l'OS en continu). Dégradation silencieuse si refusé.
    pub fn boost_process_priority(boost: bool) {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS,
            NORMAL_PRIORITY_CLASS,
        };
        if BOOSTED.swap(boost, Ordering::Relaxed) == boost {
            return; // pas de changement
        }
        let class = if boost {
            ABOVE_NORMAL_PRIORITY_CLASS
        } else {
            NORMAL_PRIORITY_CLASS
        };
        let ok = unsafe { SetPriorityClass(GetCurrentProcess(), class) };
        if ok == 0 {
            warn!(target: "app::platform", boost,
                "SetPriorityClass refusé : priorité process inchangée");
        } else if boost {
            info!(target: "app::platform",
                "priorité process ABOVE_NORMAL (mode Show, option boost_priority)");
        } else {
            info!(target: "app::platform", "priorité process Normal (mode Edit)");
        }
    }

    /// Anti-veille : dès qu'au moins une sortie vidéo est active, l'écran et
    /// le système ne doivent JAMAIS s'endormir (cause n°1 d'écran noir en
    /// salle : veille écran après 10-15 min sans souris). Relâché quand plus
    /// aucune sortie n'est active. À appeler depuis le même thread (main).
    pub fn keep_awake(active: bool) {
        if AWAKE.swap(active, Ordering::Relaxed) == active {
            return; // pas de changement
        }
        let flags = if active {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        let prev = unsafe { SetThreadExecutionState(flags) };
        if prev == 0 {
            warn!(target: "app::platform", active,
                "SetThreadExecutionState refusé : la veille écran reste possible");
        } else if active {
            info!(target: "app::platform",
                "anti-veille engagé (sortie vidéo active : écran et système maintenus)");
        } else {
            info!(target: "app::platform", "anti-veille relâché (aucune sortie active)");
        }
    }

    /// Résolution du timer système à 1 ms pendant toute la vie de la garde
    /// (jitter de cadencement ±15,6 ms → ±0,5 ms). Drop = restauration.
    pub struct TimerResolution {
        engaged: bool,
    }

    impl TimerResolution {
        pub fn new() -> TimerResolution {
            // TIMERR_NOERROR == 0.
            let engaged = unsafe { timeBeginPeriod(1) } == 0;
            if engaged {
                debug!(target: "app::platform", "timeBeginPeriod(1) actif");
            } else {
                debug!(target: "app::platform",
                    "timeBeginPeriod(1) refusé : cadencement standard");
            }
            TimerResolution { engaged }
        }
    }

    impl Drop for TimerResolution {
        fn drop(&mut self) {
            if self.engaged {
                unsafe { timeEndPeriod(1) };
            }
        }
    }

    /// Promeut le thread appelant (rendu) dans la classe MMCSS « Pro Audio »
    /// : bursts de priorité planifiés par l'OS sans affamer le système.
    /// Dégradation silencieuse si refusé (stratégie audio pro standard).
    pub fn promote_render_thread() {
        // "Pro Audio" en UTF-16, terminé par NUL.
        let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index: u32 = 0;
        let handle = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut index) };
        if handle.is_null() {
            debug!(target: "app::platform",
                "MMCSS « Pro Audio » refusé : priorité de thread standard");
        } else {
            // La caractéristique reste posée pour la vie du thread (pas de
            // revert : le thread de rendu vit aussi longtemps que le process).
            debug!(target: "app::platform", "thread de rendu promu MMCSS « Pro Audio »");
        }
    }

    /// Handler console : Ctrl-C, Ctrl-Break, fermeture de fenêtre console.
    unsafe extern "system" fn ctrl_handler(_kind: u32) -> windows_sys::core::BOOL {
        super::request_quit();
        TRUE
    }

    /// Installe le handler d'arrêt propre.
    pub fn install_quit_handler() {
        let ok = unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), TRUE) };
        if ok == 0 {
            warn!(target: "app::platform",
                "SetConsoleCtrlHandler refusé : Ctrl-C ne fera pas d'arrêt propre");
        }
    }
}

// -------------------------------------------------------------------- unix

#[cfg(unix)]
mod imp {
    use tracing::warn;

    /// Anti-veille : pas d'équivalent portable simple hors Windows (le
    /// besoin produit vise d'abord les machines de spectacle Windows) —
    /// no-op documenté, équivalents macOS/Linux à venir.
    pub fn keep_awake(_active: bool) {}

    /// Boost de priorité process : réglage Windows uniquement (no-op ici,
    /// documenté — `nice`/scheduling Unix demanderait des droits).
    pub fn boost_process_priority(_boost: bool) {}

    /// No-op hors Windows.
    pub struct TimerResolution;

    impl TimerResolution {
        pub fn new() -> TimerResolution {
            TimerResolution
        }
    }

    pub fn promote_render_thread() {}

    extern "C" fn on_signal(_sig: libc::c_int) {
        super::request_quit();
    }

    /// SIGINT/SIGTERM → arrêt propre.
    pub fn install_quit_handler() {
        // Passage explicite par un pointeur : la conversion directe
        // fonction → entier est refusée par clippy (`function_casts_as_integer`)
        // parce qu'elle vaut l'ADRESSE de l'item, pas un pointeur de fonction
        // en bonne et due forme. `as *const ()` rend l'intention explicite.
        let handler = on_signal as *const () as libc::sighandler_t;
        unsafe {
            if libc::signal(libc::SIGINT, handler) == libc::SIG_ERR {
                warn!(target: "app::platform", "handler SIGINT refusé");
            }
            if libc::signal(libc::SIGTERM, handler) == libc::SIG_ERR {
                warn!(target: "app::platform", "handler SIGTERM refusé");
            }
        }
    }
}

// ------------------------------------------------------------ autres cibles

#[cfg(not(any(windows, unix)))]
mod imp {
    pub fn keep_awake(_active: bool) {}

    pub fn boost_process_priority(_boost: bool) {}

    pub struct TimerResolution;

    impl TimerResolution {
        pub fn new() -> TimerResolution {
            TimerResolution
        }
    }

    pub fn promote_render_thread() {}

    pub fn install_quit_handler() {}
}

pub use imp::{
    boost_process_priority, install_quit_handler, keep_awake, promote_render_thread,
    TimerResolution,
};
