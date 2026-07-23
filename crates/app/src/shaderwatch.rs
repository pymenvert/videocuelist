//! Watcher du dossier `shaders/` : hot-reload des matériaux ISF à la
//! sauvegarde du fichier (promis par la SPEC). Débounce 150 ms et
//! compatible « write-temp + rename » (les éditeurs sérieux écrivent un
//! temporaire puis renomment) : on ne retient que les chemins FINAUX
//! `*.fs` existants, coalescés par lot.
//!
//! Le watcher et le débounceur vivent sur leurs threads ; la session draine
//! les lots dans son tick et rebranche le chemin de recompilation existant
//! (`comp_reload`), qui gère déjà l'échec de compilation proprement.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::Receiver;
use notify::{RecursiveMode, Watcher as _};
use tracing::{debug, warn};

/// Fenêtre de coalescence des événements fichier.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Poignée du watcher : drop = arrêt propre — lâcher le watcher ferme le
/// canal brut, le thread de débounce sort de `recv()` et se termine seul
/// (JoinHandle détaché, aucun travail en vol à attendre).
pub struct ShaderWatch {
    /// Lots de chemins RELATIFS (séparateur `/`) de `.fs` modifiés.
    batches: Receiver<Vec<String>>,
    _watcher: notify::RecommendedWatcher,
}

impl ShaderWatch {
    /// Démarre la surveillance de `shaders_dir`. `None` si le watcher est
    /// indisponible (plateforme, dossier) — l'app continue sans hot-reload.
    pub fn spawn(shaders_dir: &Path) -> Option<ShaderWatch> {
        let root = shaders_dir.to_path_buf();
        let (raw_tx, raw_rx) = crossbeam_channel::unbounded::<PathBuf>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                // Créations, écritures, renames : tout finit filtré sur
                // « le chemin final existe et se termine par .fs ».
                for path in event.paths {
                    let _ = raw_tx.send(path);
                }
            }
        });
        let mut watcher = match watcher {
            Ok(w) => w,
            Err(e) => {
                warn!(target: "app::shaderwatch", error = %e,
                    "watcher de shaders indisponible : pas de hot-reload");
                return None;
            }
        };
        if let Err(e) = watcher.watch(shaders_dir, RecursiveMode::Recursive) {
            warn!(target: "app::shaderwatch", dir = %shaders_dir.display(), error = %e,
                "surveillance de shaders/ impossible : pas de hot-reload");
            return None;
        }

        let (batch_tx, batches) = crossbeam_channel::bounded::<Vec<String>>(16);
        let debouncer = std::thread::Builder::new()
            .name("conduite-shaderwatch".into())
            .spawn(move || {
                let mut pending: BTreeSet<String> = BTreeSet::new();
                loop {
                    // Attente d'un premier événement (bloquant) puis
                    // coalescence tant que ça bouge (fenêtre 150 ms).
                    let first = if pending.is_empty() {
                        match raw_rx.recv() {
                            Ok(p) => Some(p),
                            Err(_) => break, // watcher lâché : fin propre
                        }
                    } else {
                        None
                    };
                    if let Some(p) = first {
                        note_path(&root, p, &mut pending);
                    }
                    loop {
                        match raw_rx.recv_timeout(DEBOUNCE) {
                            Ok(p) => note_path(&root, p, &mut pending),
                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
                            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                                return; // fin propre
                            }
                        }
                    }
                    if pending.is_empty() {
                        continue;
                    }
                    let batch: Vec<String> = std::mem::take(&mut pending).into_iter().collect();
                    debug!(target: "app::shaderwatch", count = batch.len(),
                        "shaders modifiés (lot débouncé)");
                    // Lot perdu si la session est en retard de 16 lots :
                    // très improbable, et un rescan matériaux rattrape tout.
                    let _ = batch_tx.try_send(batch);
                }
            });
        if let Err(e) = debouncer {
            warn!(target: "app::shaderwatch", error = %e,
                "thread de débounce impossible : pas de hot-reload");
            return None;
        }
        debug!(target: "app::shaderwatch", dir = %shaders_dir.display(),
            "hot-reload des shaders actif");
        Some(ShaderWatch {
            batches,
            _watcher: watcher,
        })
    }

    /// Prochain lot de chemins relatifs modifiés, sans attendre.
    pub fn try_recv(&self) -> Option<Vec<String>> {
        self.batches.try_recv().ok()
    }
}

/// Retient un chemin s'il désigne un `.fs` FINAL existant sous `root`
/// (chemin relatif, séparateur `/`).
fn note_path(root: &Path, path: PathBuf, pending: &mut BTreeSet<String>) {
    let is_fs = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("fs"))
        .unwrap_or(false);
    if !is_fs || !path.is_file() {
        return; // temporaire d'éditeur, suppression, dossier…
    }
    let Ok(rel) = path.strip_prefix(root) else { return };
    let Some(rel) = rel.to_str() else { return };
    pending.insert(rel.replace('\\', "/"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "conduite-shaderwatch-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// Seuls les `.fs` finaux existants sont retenus, en relatif `/`.
    #[test]
    fn note_path_filters_and_relativizes() {
        let root = tmp("note");
        std::fs::create_dir_all(root.join("pack")).expect("mkdir");
        std::fs::write(root.join("pack").join("glow.fs"), b"x").expect("write");
        std::fs::write(root.join("notes.txt"), b"x").expect("write");

        let mut pending = BTreeSet::new();
        note_path(&root, root.join("pack").join("glow.fs"), &mut pending);
        note_path(&root, root.join("notes.txt"), &mut pending); // pas un .fs
        note_path(&root, root.join("efface.fs"), &mut pending); // n'existe pas
        note_path(&root, root.join("pack"), &mut pending); // dossier

        assert_eq!(
            pending.into_iter().collect::<Vec<_>>(),
            vec!["pack/glow.fs".to_string()]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Bout en bout : write-temp + rename produit UN lot débouncé portant
    /// le chemin final.
    #[test]
    fn watcher_delivers_debounced_batch_on_rename() {
        let root = tmp("watch");
        let Some(watch) = ShaderWatch::spawn(&root) else {
            // Plateforme sans watcher : le produit dégrade sans hot-reload.
            return;
        };
        // Éditeur sérieux : écrit un temporaire puis renomme.
        std::fs::write(root.join("kaleido.fs.tmp"), b"void main(){}").expect("write tmp");
        std::fs::rename(root.join("kaleido.fs.tmp"), root.join("kaleido.fs")).expect("rename");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got: Vec<String> = Vec::new();
        while std::time::Instant::now() < deadline {
            if let Some(batch) = watch.try_recv() {
                got.extend(batch);
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(got, vec!["kaleido.fs".to_string()], "lot débouncé attendu");
        let _ = std::fs::remove_dir_all(&root);
    }
}
