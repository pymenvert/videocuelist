//! `config.toml` à la racine du dossier portable — réglages machine
//! (ports, entrée audio, dernier show, cadence), distincts des réglages du
//! show (qui vivent dans `Show::settings`).
//!
//! Lecture tolérante : champ inconnu ignoré ; fichier corrompu ⇒ défauts +
//! log error + copie `.corrompu` (le fichier fautif reste inspectable).

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

pub const CONFIG_FILE: &str = "config.toml";

/// Configuration machine (auto-créée au premier lancement).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Port du serveur web de contrôle.
    pub http_port: u16,
    /// Adresse d'écoute du serveur web.
    pub http_bind: String,
    /// Nom du périphérique d'entrée audio (FFT), `None` = désactivé.
    pub audio_input: Option<String>,
    /// Show chargé au démarrage (dossier sous `shows/`).
    pub last_show: String,
    /// Cadence cible de la boucle de rendu.
    pub target_fps: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            http_port: 9820,
            http_bind: "0.0.0.0".to_string(),
            audio_input: None,
            last_show: "demo".to_string(),
            target_fps: 60,
        }
    }
}

impl AppConfig {
    /// Charge `base/config.toml`. Absent ⇒ défauts écrits sur disque.
    /// Corrompu ⇒ défauts + copie `.corrompu` + log error.
    pub fn load(base: &Path) -> AppConfig {
        let path = base.join(CONFIG_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => {
                info!(target: "app::config", path = %path.display(),
                    "config absente, création avec les défauts");
                let cfg = AppConfig::default();
                cfg.save(base);
                return cfg;
            }
        };
        match toml::from_str::<AppConfig>(&text) {
            Ok(mut cfg) => {
                if cfg.target_fps == 0 || cfg.target_fps > 240 {
                    warn!(target: "app::config", target_fps = cfg.target_fps,
                        "target_fps invalide, repli sur 60");
                    cfg.target_fps = 60;
                }
                cfg
            }
            Err(e) => {
                error!(target: "app::config", path = %path.display(), error = %e,
                    "config.toml corrompu : défauts appliqués, copie .corrompu");
                let backup = base.join(format!("{CONFIG_FILE}.corrompu"));
                if let Err(e) = std::fs::write(&backup, &text) {
                    warn!(target: "app::config", error = %e, "copie .corrompu impossible");
                }
                let cfg = AppConfig::default();
                cfg.save(base);
                cfg
            }
        }
    }

    /// Sauvegarde vers `base/config.toml` (best-effort, loggué).
    pub fn save(&self, base: &Path) {
        let path = base.join(CONFIG_FILE);
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    warn!(target: "app::config", path = %path.display(), error = %e,
                        "écriture de la config impossible");
                }
            }
            Err(e) => warn!(target: "app::config", error = %e, "sérialisation de la config impossible"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("conduite-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn defaults_created_on_first_launch() {
        let dir = tmp("first");
        let cfg = AppConfig::load(&dir);
        assert_eq!(cfg, AppConfig::default());
        assert!(dir.join(CONFIG_FILE).is_file(), "config auto-créée");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let dir = tmp("unknown");
        std::fs::write(
            dir.join(CONFIG_FILE),
            "http_port = 9999\nchamp_mystere = true\n",
        )
        .expect("write");
        let cfg = AppConfig::load(&dir);
        assert_eq!(cfg.http_port, 9999);
        assert_eq!(cfg.last_show, "demo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_falls_back_and_is_kept() {
        let dir = tmp("corrupt");
        std::fs::write(dir.join(CONFIG_FILE), "pas du { toml").expect("write");
        let cfg = AppConfig::load(&dir);
        assert_eq!(cfg, AppConfig::default());
        assert!(dir.join("config.toml.corrompu").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
