// Adapté de Lanterne (pymenvert/toolbox), MIT (écriture atomique).
//! Persistance du show : écritures atomiques ET durables, backups rotatifs,
//! chargement tolérant (média manquant = placeholder, jamais un refus),
//! versionnage du format avec hook de migration.
//!
//! Convention de dossier : `dir` est le dossier du show — il contient
//! `show.json`, `backups/` et (par défaut) le dossier `media/` servant à
//! vérifier la présence des fichiers. Si les médias vivent ailleurs
//! (dossier portable global), utiliser [`load_show_with_media`].

use std::fs;
use std::io::Write as _;
use std::path::Path;

use crate::error::CoreError;
use crate::model::{Show, FORMAT_VERSION};
use crate::paths::validate_relative_path;

/// Nom du fichier show dans son dossier.
pub const SHOW_FILE: &str = "show.json";
/// Sous-dossier des backups rotatifs.
pub const BACKUP_DIR: &str = "backups";
/// Nombre de backups conservés.
pub const BACKUP_KEEP: usize = 20;

/// Avertissement non bloquant émis au chargement d'un show.
#[derive(Debug, Clone, PartialEq)]
pub enum LoadWarning {
    /// Fichier média absent du disque : marqué `missing`, placeholder à l'écran.
    MissingMedia { id: u32, path: String },
    /// Chemin de média invalide (absolu, `..`…) : marqué `missing`, jamais lu.
    InvalidMediaPath { id: u32, path: String },
    /// Fichier matériau (shader) absent du disque.
    MissingMaterial { id: u32, path: String },
    /// Le show a été migré depuis une version antérieure du format.
    MigratedFrom { from: u32 },
}

impl std::fmt::Display for LoadWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadWarning::MissingMedia { id, path } => {
                write!(f, "média {id} introuvable : {path:?}")
            }
            LoadWarning::InvalidMediaPath { id, path } => {
                write!(f, "média {id} au chemin invalide : {path:?}")
            }
            LoadWarning::MissingMaterial { id, path } => {
                write!(f, "matériau {id} introuvable : {path:?}")
            }
            LoadWarning::MigratedFrom { from } => {
                write!(f, "show migré du format v{from} vers v{FORMAT_VERSION}")
            }
        }
    }
}

/// Écriture atomique ET durable d'un fichier : temporaire à côté,
/// `sync_all` (flush disque — un Pi peut perdre le courant à tout instant,
/// et un rename sans flush peut laisser un fichier VIDE au reboot sur
/// ext4/FAT), puis rename par-dessus l'ancien.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut file =
            fs::File::create(&tmp).map_err(|e| CoreError::io(tmp.display().to_string(), e))?;
        file.write_all(bytes)
            .map_err(|e| CoreError::io(tmp.display().to_string(), e))?;
        file.sync_all()
            .map_err(|e| CoreError::io(tmp.display().to_string(), e))?;
    }
    fs::rename(&tmp, path).map_err(|e| CoreError::io(path.display().to_string(), e))
}

/// Sauvegarde atomique du show dans `dir/show.json`, plus une copie de
/// backup `dir/backups/show-YYYYMMDD-HHMMSS.json` (rotation : 20 gardés).
///
/// L'échec du backup n'annule pas la sauvegarde principale (loggué en warn).
pub fn save_show_atomic(dir: &Path, show: &Show) -> Result<(), CoreError> {
    fs::create_dir_all(dir).map_err(|e| CoreError::io(dir.display().to_string(), e))?;
    let json = serde_json::to_vec_pretty(show)
        .map_err(|e| CoreError::json(dir.join(SHOW_FILE).display().to_string(), e))?;
    write_atomic(&dir.join(SHOW_FILE), &json)?;

    // Backup rotatif — best-effort : un disque plein ne doit pas faire
    // échouer la sauvegarde principale déjà écrite.
    if let Err(e) = write_backup(dir, &json) {
        tracing::warn!(target: "core::persist", error = %e, "backup du show impossible");
    }
    Ok(())
}

/// Écrit la copie de backup horodatée puis élague au-delà de [`BACKUP_KEEP`].
fn write_backup(dir: &Path, json: &[u8]) -> Result<(), CoreError> {
    let backups = dir.join(BACKUP_DIR);
    fs::create_dir_all(&backups).map_err(|e| CoreError::io(backups.display().to_string(), e))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = backups.join(format!("show-{stamp}.json"));
    // Deux sauvegardes dans la même seconde : on écrase, même contenu d'époque.
    write_atomic(&path, json)?;
    prune_backups(&backups)?;
    Ok(())
}

/// Garde les [`BACKUP_KEEP`] backups les plus récents (tri lexicographique
/// des noms = tri chronologique, format horodaté à largeur fixe).
fn prune_backups(backups: &Path) -> Result<(), CoreError> {
    let mut names: Vec<String> = Vec::new();
    let entries =
        fs::read_dir(backups).map_err(|e| CoreError::io(backups.display().to_string(), e))?;
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with("show-") && name.ends_with(".json") {
                names.push(name.to_string());
            }
        }
    }
    if names.len() <= BACKUP_KEEP {
        return Ok(());
    }
    names.sort(); // ordre croissant = plus ancien d'abord
    let excess = names.len() - BACKUP_KEEP;
    for name in names.into_iter().take(excess) {
        let path = backups.join(&name);
        if let Err(e) = fs::remove_file(&path) {
            tracing::warn!(target: "core::persist", path = %path.display(), error = %e,
                "élagage de backup impossible");
        }
    }
    Ok(())
}

/// Hook de migration : amène un show JSON d'une version antérieure au format
/// courant. Un format PLUS RÉCENT que le logiciel est refusé (sémantique
/// inconnue). Aucune migration nécessaire tant que `FORMAT_VERSION == 1`.
fn migrate(
    value: serde_json::Value,
    from: u32,
    warnings: &mut Vec<LoadWarning>,
) -> Result<serde_json::Value, CoreError> {
    if from > FORMAT_VERSION {
        return Err(CoreError::UnsupportedVersion(from, FORMAT_VERSION));
    }
    if from < FORMAT_VERSION {
        // Chaîne de migrations version par version (à remplir au fil des
        // évolutions du format : match from { 1 => …, 2 => …, _ => {} }).
        warnings.push(LoadWarning::MigratedFrom { from });
        tracing::info!(target: "core::persist", from, to = FORMAT_VERSION, "migration du show");
    }
    Ok(value)
}

/// Charge `dir/show.json` avec tolérance. Les médias sont cherchés sous
/// `dir/media/` — voir [`load_show_with_media`] pour un autre emplacement.
///
/// Un média absent ⇒ `missing: true` + warning, **jamais un échec** : le
/// show se charge toujours (doctrine de fiabilité, SPEC §10).
pub fn load_show(dir: &Path) -> Result<(Show, Vec<LoadWarning>), CoreError> {
    load_show_with_media(dir, &dir.join("media"))
}

/// Variante de [`load_show`] avec racine médias explicite (dossier portable
/// global `media/` du binaire, par exemple). Les matériaux sont cherchés
/// sous `<media_root>/../shaders` s'il existe, sinon ignorés.
pub fn load_show_with_media(
    dir: &Path,
    media_root: &Path,
) -> Result<(Show, Vec<LoadWarning>), CoreError> {
    let path = dir.join(SHOW_FILE);
    let display = path.display().to_string();
    let bytes = fs::read(&path).map_err(|e| CoreError::io(&*display, e))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| CoreError::json(&*display, e))?;

    let mut warnings = Vec::new();
    let from = value
        .get("format_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;
    let value = migrate(value, from, &mut warnings)?;

    let mut show: Show =
        serde_json::from_value(value).map_err(|e| CoreError::json(&*display, e))?;
    show.format_version = FORMAT_VERSION;

    // Vérification tolérante des médias : chemin invalide ou fichier absent
    // ⇒ missing + warning, on continue.
    for media in &mut show.media {
        if validate_relative_path(&media.path).is_err() {
            media.missing = true;
            tracing::warn!(target: "core::persist", id = media.id, path = %media.path,
                "chemin de média invalide, marqué manquant");
            warnings.push(LoadWarning::InvalidMediaPath {
                id: media.id,
                path: media.path.clone(),
            });
        } else if media_root.join(&media.path).is_file() {
            media.missing = false;
        } else {
            media.missing = true;
            tracing::warn!(target: "core::persist", id = media.id, path = %media.path,
                "média introuvable, placeholder");
            warnings.push(LoadWarning::MissingMedia {
                id: media.id,
                path: media.path.clone(),
            });
        }
    }

    // Matériaux : mêmes règles, sans champ `missing` (le compositor affiche
    // un placeholder si le fichier manque à la compilation).
    if let Some(shader_root) = media_root.parent().map(|p| p.join("shaders")) {
        if shader_root.is_dir() {
            for material in &show.materials {
                let bad_path = validate_relative_path(&material.path).is_err();
                if bad_path || !shader_root.join(&material.path).is_file() {
                    tracing::warn!(target: "core::persist", id = material.id,
                        path = %material.path, "matériau introuvable");
                    warnings.push(LoadWarning::MissingMaterial {
                        id: material.id,
                        path: material.path.clone(),
                    });
                }
            }
        }
    }

    Ok((show, warnings))
}
