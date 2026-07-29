//! Crash dumps locaux HORS-PROCESS (AUDIT P1 n°8) — trio EmbarkStudios
//! `crash-handler` + `minidumper` (éprouvé Mozilla/Breakpad).
//!
//! Architecture : au démarrage, Conduite relance SON PROPRE exécutable
//! (depuis son emplacement d'origine — AV-friendly : aucun binaire copié ni
//! téléchargé) avec `--crash-server <nom> <dossier>` ; ce processus enfant
//! sert de serveur de minidump hors-process. En cas de crash du processus
//! principal (segfault, `__fastfail`, stack overflow… — tout ce que le hook
//! de panic Rust ne voit JAMAIS), le handler in-process minimal demande au
//! serveur d'écrire le dump : c'est le processus SAIN qui lit la mémoire du
//! processus mort, pas l'inverse.
//!
//! Dumps dans `logs/crash/crash-<horodatage>.dmp`, rétention 5. AUCUN envoi
//! réseau : les dumps restent locaux, joints manuellement à un ticket.
//! Dégradation silencieuse totale : si quoi que ce soit échoue, l'app
//! démarre sans capture de crash (log WARN et c'est tout).

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{debug, info, warn};

/// Drapeau CLI interne du mode serveur (jamais documenté dans `--help`).
pub const CRASH_SERVER_FLAG: &str = "--crash-server";
/// Nombre de dumps conservés.
const CRASH_KEEP: usize = 5;
/// Le serveur s'arrête de lui-même s'il reste sans client (parent mort ou
/// jamais connecté) — filet anti-processus orphelin.
const SERVER_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
/// Tentatives de connexion du client au serveur fraîchement lancé.
const CONNECT_ATTEMPTS: usize = 50;
const CONNECT_RETRY: Duration = Duration::from_millis(20);

// ------------------------------------------------------------------ serveur

/// Si le processus a été lancé en mode serveur de crash
/// (`--crash-server <nom> <dossier>`), le sert puis rend le code de sortie.
/// `None` = lancement normal. À appeler TOUT EN HAUT de `main` : le serveur
/// ne prend ni verrou mono-instance, ni ports, ni fichier de log.
pub fn maybe_run_server() -> Option<i32> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == CRASH_SERVER_FLAG {
            let name = args.next()?;
            let dir = PathBuf::from(args.next()?);
            return Some(run_server(&name, &dir));
        }
    }
    None
}

/// Boucle du serveur de minidump (processus enfant dédié). Journal sur
/// stderr uniquement : pas de fichier de log à disputer au parent.
fn run_server(name: &str, crash_dir: &Path) -> i32 {
    struct Handler {
        dir: PathBuf,
    }

    impl minidumper::ServerHandler for Handler {
        /// Fichier de destination du dump (dossier créé au besoin).
        fn create_minidump_file(&self) -> Result<(std::fs::File, PathBuf), std::io::Error> {
            std::fs::create_dir_all(&self.dir)?;
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let path = self.dir.join(format!("crash-{stamp}.dmp"));
            Ok((std::fs::File::create(&path)?, path))
        }

        /// Dump écrit (ou raté) : rétention puis fin du serveur — un crash
        /// du parent est un événement terminal des deux côtés.
        fn on_minidump_created(
            &self,
            result: Result<minidumper::MinidumpBinary, minidumper::Error>,
        ) -> minidumper::LoopAction {
            match result {
                Ok(bin) => eprintln!("conduite-crash : dump écrit dans {}", bin.path.display()),
                Err(e) => eprintln!("conduite-crash : écriture du dump impossible : {e}"),
            }
            prune_dumps(&self.dir, CRASH_KEEP);
            minidumper::LoopAction::Exit
        }

        fn on_message(&self, _kind: u32, _buffer: Vec<u8>) {}

        /// Parent parti sans crasher (arrêt normal) : fin du serveur.
        fn on_client_disconnected(&self, num_clients: usize) -> minidumper::LoopAction {
            if num_clients == 0 {
                minidumper::LoopAction::Exit
            } else {
                minidumper::LoopAction::Continue
            }
        }
    }

    let mut server = match minidumper::Server::with_name(minidumper::SocketName::Path(
        std::path::Path::new(name),
    )) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("conduite-crash : serveur impossible ({e})");
            return 1;
        }
    };
    let shutdown = std::sync::atomic::AtomicBool::new(false);
    let handler = Handler {
        dir: crash_dir.to_path_buf(),
    };
    let code = match server.run(Box::new(handler), &shutdown, Some(SERVER_IDLE_TIMEOUT)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("conduite-crash : boucle serveur terminée sur erreur ({e})");
            1
        }
    };
    // Le fichier socket ne doit jamais traîner (best-effort, doublé côté
    // client dans `CrashGuard::drop`).
    let _ = std::fs::remove_file(name);
    code
}

// ------------------------------------------------------------------- client

/// Garde côté application : handler de crash + processus serveur enfant.
/// À garder vivante toute la vie du process ; Drop = détache le handler et
/// termine l'enfant (qui sort de lui-même à la déconnexion, le `kill` est
/// une ceinture de sécurité).
pub struct CrashGuard {
    _handler: crash_handler::CrashHandler,
    child: std::process::Child,
    /// Chemin du fichier socket (nettoyé au drop, best-effort).
    socket: PathBuf,
}

impl Drop for CrashGuard {
    fn drop(&mut self) {
        // L'enfant sort seul à la déconnexion du client ; on ne LAISSE
        // jamais traîner un processus au cas où.
        std::thread::sleep(Duration::from_millis(50));
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            _ => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        // Fichier socket : jamais de résidu (le serveur l'efface aussi).
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Démarre la capture de crash : relance NOTRE exécutable en serveur de
/// dump, s'y connecte, installe le handler. `None` (avec WARN loggué) si
/// n'importe quelle étape échoue — l'app tourne alors sans capture.
pub fn spawn(logs_dir: &Path) -> Option<CrashGuard> {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            warn!(target: "app::crash", error = %e,
                "exécutable introuvable : capture de crash inactive");
            return None;
        }
    };
    let crash_dir = logs_dir.join("crash");
    // Le fichier socket vit dans logs/crash/ (chemin ABSOLU : un nom relatif
    // atterrirait dans le cwd — un résidu à la racine du dossier portable) ;
    // le dossier doit exister AVANT le bind du serveur.
    if let Err(e) = std::fs::create_dir_all(&crash_dir) {
        warn!(target: "app::crash", error = %e,
            "dossier logs/crash impossible : capture de crash inactive");
        return None;
    }
    let socket = crash_dir.join(format!("conduite-crash-{}.sock", std::process::id()));
    // Résidu d'un lancement précédent (même PID recyclé) : on repart net.
    let _ = std::fs::remove_file(&socket);
    let name = socket.to_string_lossy().into_owned();

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(CRASH_SERVER_FLAG)
        .arg(&name)
        .arg(&crash_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!(target: "app::crash", error = %e,
                "serveur de crash impossible à lancer : capture inactive");
            return None;
        }
    };

    // Connexion au pipe du serveur (créé en quelques millisecondes).
    let mut client = None;
    for _ in 0..CONNECT_ATTEMPTS {
        match minidumper::Client::with_name(minidumper::SocketName::Path(
            std::path::Path::new(&name),
        )) {
            Ok(c) => {
                client = Some(c);
                break;
            }
            Err(_) => std::thread::sleep(CONNECT_RETRY),
        }
    }
    let Some(client) = client else {
        warn!(target: "app::crash",
            "connexion au serveur de crash impossible : capture inactive");
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };

    // Handler in-process MINIMAL : il ne fait que demander le dump au
    // serveur (le processus sain), signal-safe par construction.
    let attach = crash_handler::CrashHandler::attach(unsafe {
        crash_handler::make_crash_event(move |ctx: &crash_handler::CrashContext| {
            crash_handler::CrashEventResult::Handled(client.request_dump(ctx).is_ok())
        })
    });
    match attach {
        Ok(handler) => {
            info!(target: "app::crash", dir = %crash_dir.display(),
                "capture de crash hors-process active (rétention {CRASH_KEEP} dumps)");
            Some(CrashGuard {
                _handler: handler,
                child,
                socket,
            })
        }
        Err(e) => {
            warn!(target: "app::crash", error = %e,
                "handler de crash impossible : capture inactive");
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

// ---------------------------------------------------------------- rétention

/// Ne garde que les `keep` dumps les plus récents (`crash-*.dmp`).
fn prune_dumps(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dumps: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("crash-") || !name.ends_with(".dmp") {
                return None;
            }
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    if dumps.len() <= keep {
        return;
    }
    dumps.sort_by_key(|d| std::cmp::Reverse(d.0)); // récents d'abord
    for (_, path) in dumps.into_iter().skip(keep) {
        match std::fs::remove_file(&path) {
            Ok(()) => debug!(target: "app::crash", path = %path.display(), "vieux dump purgé"),
            Err(e) => eprintln!("conduite-crash : purge de {} impossible : {e}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La rétention garde les 5 dumps les plus récents, ignore le reste.
    #[test]
    fn prune_keeps_most_recent_dumps() {
        let dir = std::env::temp_dir().join(format!("conduite-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        for i in 0..8 {
            let path = dir.join(format!("crash-2026072{i}-000000.dmp"));
            std::fs::write(&path, b"dump").expect("write");
            // mtimes distincts et croissants.
            let t = filetime_now_minus(8 - i);
            let _ = set_mtime(&path, t);
        }
        std::fs::write(dir.join("notes.txt"), b"pas un dump").expect("write");
        prune_dumps(&dir, CRASH_KEEP);
        let remaining: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".dmp"))
            .collect();
        assert_eq!(remaining.len(), CRASH_KEEP, "{remaining:?}");
        assert!(
            remaining.contains(&"crash-20260727-000000.dmp".to_string()),
            "le plus récent survit : {remaining:?}"
        );
        assert!(
            !remaining.contains(&"crash-20260720-000000.dmp".to_string()),
            "le plus ancien est purgé : {remaining:?}"
        );
        assert!(dir.join("notes.txt").is_file(), "les non-dumps sont intouchés");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn filetime_now_minus(hours: u64) -> std::time::SystemTime {
        std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600)
    }

    /// Pose le mtime via un simple re-write différé impossible — on utilise
    /// l'API File::set_times (stable) sur le fichier existant.
    fn set_mtime(path: &Path, t: std::time::SystemTime) -> std::io::Result<()> {
        let file = std::fs::OpenOptions::new().write(true).open(path)?;
        let times = std::fs::FileTimes::new().set_modified(t);
        file.set_times(times)
    }
}
