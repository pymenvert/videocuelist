//! Écritures disque HORS du thread de tick : sauvegarde du show (atomique +
//! backups, `fsync` compris) et instantané de récupération post-panic.
//!
//! Le tick ne fait que cloner le [`Show`] et poster un travail ; la
//! sérialisation JSON (`to_vec_pretty` / `to_string_pretty`) et les flushes
//! disque vivent sur le thread `conduite-saver` — plus jamais de gel de la
//! boucle de rendu pendant une sauvegarde (SD lente, autosave en mode Show).

use std::path::PathBuf;

use conduite_core::Show;
use crossbeam_channel::{Receiver, Sender};
use tracing::{debug, error, info, warn};

use crate::logsetup;

/// Travail posté par la session.
pub enum SaveJob {
    /// Sauvegarde complète : `show.json` + backup rotatif, puis mise à jour
    /// de l'instantané de récupération (même contenu).
    Save {
        dir: PathBuf,
        shows_dir: PathBuf,
        show: Box<Show>,
        /// Génération d'édition au moment du clone (le tick ne remet
        /// `dirty = false` que si rien n'a été édité depuis).
        gen: u64,
    },
    /// Instantané de récupération seul (après édition, débounce côté tick).
    Snapshot { shows_dir: PathBuf, show: Box<Show> },
}

/// Résultat d'une sauvegarde complète.
pub struct SaveOutcome {
    pub gen: u64,
    pub ok: bool,
}

/// Poignée du thread d'écriture. Drop = fin propre : le canal se ferme, le
/// worker termine les travaux en file puis s'arrête (join).
pub struct Saver {
    tx: Option<Sender<SaveJob>>,
    results: Receiver<SaveOutcome>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Saver {
    pub fn spawn() -> Saver {
        let (tx, rx) = crossbeam_channel::bounded::<SaveJob>(8);
        let (out_tx, results) = crossbeam_channel::unbounded::<SaveOutcome>();
        let thread = std::thread::Builder::new()
            .name("conduite-saver".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    match job {
                        SaveJob::Save { dir, shows_dir, show, gen } => {
                            let ok = match conduite_core::save_show_atomic(&dir, &show) {
                                Ok(()) => {
                                    info!(target: "app::saver", dir = %dir.display(),
                                        "show sauvegardé");
                                    true
                                }
                                Err(e) => {
                                    error!(target: "app::saver", error = %e,
                                        "sauvegarde du show impossible");
                                    false
                                }
                            };
                            update_snapshot(shows_dir, &show);
                            let _ = out_tx.send(SaveOutcome { gen, ok });
                        }
                        SaveJob::Snapshot { shows_dir, show } => {
                            update_snapshot(shows_dir, &show);
                        }
                    }
                }
                debug!(target: "app::saver", "thread d'écriture arrêté");
            });
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                error!(target: "app::saver", error = %e,
                    "thread d'écriture impossible : sauvegardes inactives");
                None
            }
        };
        Saver {
            tx: thread.is_some().then_some(tx),
            results,
            thread,
        }
    }

    /// Poste un travail (jamais bloquant). `false` si la file est pleine ou
    /// le worker mort — à l'appelant de garder `dirty` et de réessayer.
    pub fn submit(&self, job: SaveJob) -> bool {
        match &self.tx {
            Some(tx) => match tx.try_send(job) {
                Ok(()) => true,
                Err(e) => {
                    warn!(target: "app::saver", error = %e,
                        "file d'écriture saturée : travail reporté");
                    false
                }
            },
            None => false,
        }
    }

    /// Résultat de sauvegarde disponible, sans attendre.
    pub fn try_result(&self) -> Option<SaveOutcome> {
        self.results.try_recv().ok()
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        self.tx = None; // ferme le canal : le worker draine puis s'arrête
        if let Some(t) = self.thread.take() {
            if t.join().is_err() {
                warn!(target: "app::saver", "le thread d'écriture a paniqué");
            }
        }
    }
}

/// Sérialise le show et met à jour l'instantané du hook de panic.
fn update_snapshot(shows_dir: PathBuf, show: &Show) {
    match serde_json::to_string_pretty(show) {
        Ok(json) => logsetup::set_recover_snapshot(shows_dir, json),
        Err(e) => warn!(target: "app::saver", error = %e,
            "snapshot de récupération impossible"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("conduite-saver-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// La sauvegarde postée est écrite sur disque et le résultat porte la
    /// génération et le succès.
    #[test]
    fn save_job_writes_show_and_reports_outcome() {
        let base = tmp("save");
        let dir = base.join("shows").join("t");
        let saver = Saver::spawn();
        let show = conduite_core::Show::new("test-saver");
        assert!(saver.submit(SaveJob::Save {
            dir: dir.clone(),
            shows_dir: base.join("shows"),
            show: Box::new(show),
            gen: 7,
        }));
        // Le worker écrit en tâche de fond : on attend le résultat (borné).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let outcome = loop {
            if let Some(o) = saver.try_result() {
                break o;
            }
            assert!(std::time::Instant::now() < deadline, "résultat jamais publié");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(outcome.gen, 7);
        assert!(outcome.ok);
        assert!(dir.join(conduite_core::SHOW_FILE).is_file(), "show.json écrit");
        drop(saver);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Drop = fin propre : les travaux déjà en file sont terminés avant
    /// l'arrêt du thread (join dans Drop).
    #[test]
    fn drop_flushes_pending_jobs() {
        let base = tmp("flush");
        let dir = base.join("shows").join("t");
        {
            let saver = Saver::spawn();
            let show = conduite_core::Show::new("test-flush");
            assert!(saver.submit(SaveJob::Save {
                dir: dir.clone(),
                shows_dir: base.join("shows"),
                show: Box::new(show),
                gen: 1,
            }));
        } // drop : join du worker
        assert!(dir.join(conduite_core::SHOW_FILE).is_file(), "travail drainé au drop");
        let _ = std::fs::remove_dir_all(&base);
    }
}
