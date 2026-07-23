//! Chemins portables : tout est relatif au dossier de l'exécutable.
//!
//! En développement (exe sous `target/debug` ou `target/release`), la base
//! est la racine du workspace (le parent du dossier `target` qui contient un
//! `Cargo.toml`) : les dossiers `media/`, `shows/`, `shaders/`… du dépôt
//! sont utilisés directement.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

/// Dossiers de travail de l'application (créés au besoin).
#[derive(Debug, Clone)]
pub struct Dirs {
    /// Dossier portable racine (config.toml, bin/ffmpeg…).
    pub base: PathBuf,
    pub media: PathBuf,
    pub shows: PathBuf,
    pub shaders: PathBuf,
    pub logs: PathBuf,
    pub thumbs: PathBuf,
}

impl Dirs {
    /// Détecte la base et crée les sous-dossiers manquants.
    pub fn detect() -> Dirs {
        let base = detect_base();
        let dirs = Dirs {
            media: base.join("media"),
            shows: base.join("shows"),
            shaders: base.join("shaders"),
            logs: base.join("logs"),
            thumbs: base.join("thumbs"),
            base,
        };
        for dir in [
            &dirs.media,
            &dirs.shows,
            &dirs.shaders,
            &dirs.logs,
            &dirs.thumbs,
        ] {
            if let Err(e) = std::fs::create_dir_all(dir) {
                warn!(target: "app::dirs", path = %dir.display(), error = %e,
                    "création de dossier impossible");
            }
        }
        info!(target: "app::dirs", base = %dirs.base.display(), "dossier portable");
        dirs
    }

    /// Dossier d'un show : `shows/<nom>/` (contient `show.json` + `backups/`).
    pub fn show_dir(&self, name: &str) -> PathBuf {
        self.shows.join(name)
    }
}

/// Base = dossier de l'exe ; en dev (`target/debug|release`), racine du
/// workspace (ancêtre `target` dont le parent contient `Cargo.toml`).
fn detect_base() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(root) = workspace_root_of(dir) {
                return root;
            }
            return dir.to_path_buf();
        }
    }
    warn!(target: "app::dirs", "exécutable introuvable, repli sur le répertoire courant");
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Si `dir` est sous un dossier `target` dont le parent porte un
/// `Cargo.toml`, retourne ce parent (dev depuis `cargo run` / `target\...`).
fn workspace_root_of(dir: &Path) -> Option<PathBuf> {
    for anc in dir.ancestors() {
        if anc.file_name().and_then(|n| n.to_str()) == Some("target") {
            let parent = anc.parent()?;
            if parent.join("Cargo.toml").is_file() {
                return Some(parent.to_path_buf());
            }
        }
    }
    None
}

/// Nettoie un nom de show pour l'utiliser comme nom de dossier (pas de
/// séparateurs, pas de traversée). Vide après nettoyage ⇒ "show".
pub fn safe_show_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "show".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names() {
        assert_eq!(safe_show_name("gala 2026"), "gala 2026");
        assert_eq!(safe_show_name("../evil"), "_evil");
        assert_eq!(safe_show_name("a/b\\c"), "a_b_c");
        assert_eq!(safe_show_name(""), "show");
        assert_eq!(safe_show_name(".."), "show");
    }

    #[test]
    fn workspace_root_detection() {
        let tmp = std::env::temp_dir().join(format!("conduite-dirs-{}", std::process::id()));
        let deep = tmp.join("target").join("debug");
        std::fs::create_dir_all(&deep).expect("mkdir");
        std::fs::write(tmp.join("Cargo.toml"), b"[workspace]").expect("write");
        assert_eq!(workspace_root_of(&deep), Some(tmp.clone()));
        // Sans Cargo.toml : pas une racine de workspace.
        let other = std::env::temp_dir().join(format!("conduite-dirs2-{}", std::process::id()));
        let deep2 = other.join("target").join("release");
        std::fs::create_dir_all(&deep2).expect("mkdir");
        assert_eq!(workspace_root_of(&deep2), None);
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&other);
    }
}
