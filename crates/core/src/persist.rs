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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::CoreError;
use crate::model::{Show, FORMAT_VERSION};
use crate::paths::validate_relative_path;

/// Nom du fichier show dans son dossier.
pub const SHOW_FILE: &str = "show.json";
/// Sous-dossier des backups rotatifs.
pub const BACKUP_DIR: &str = "backups";
/// Nombre de backups conservés.
pub const BACKUP_KEEP: usize = 20;
/// Nom du fichier de verrou mono-instance (dossier portable).
pub const LOCK_FILE: &str = "conduite.lock";

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

/// Chemin temporaire UNIQUE à côté du fichier cible : suffixe pid + compteur,
/// terminé par `.tmp`. Deux instances (ou deux écritures concurrentes) ne se
/// disputent ainsi jamais le même fichier temporaire — un nom fixe permettait
/// à l'instance B de tronquer le tmp pendant que A le renommait.
fn tmp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{}-{n}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// `fsync` du répertoire après un rename : sur ext4/FAT (Pi, clé USB), le
/// rename est une métadonnée non flushée — sans ce sync, une coupure secteur
/// juste après « show sauvegardé » peut faire revenir le fichier à sa version
/// précédente au reboot. No-op propre hors Unix (NTFS journalise, et Windows
/// n'ouvre pas un répertoire comme un fichier).
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), CoreError> {
    if dir.as_os_str().is_empty() {
        return Ok(()); // chemin relatif sans parent explicite : rien à ouvrir
    }
    let d = fs::File::open(dir).map_err(|e| CoreError::io(dir.display().to_string(), e))?;
    d.sync_all()
        .map_err(|e| CoreError::io(dir.display().to_string(), e))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), CoreError> {
    Ok(())
}

/// Écriture atomique ET durable d'un fichier : temporaire unique à côté,
/// `sync_all` (flush disque — un Pi peut perdre le courant à tout instant,
/// et un rename sans flush peut laisser un fichier VIDE au reboot sur
/// ext4/FAT), rename par-dessus l'ancien, puis `fsync` du répertoire parent
/// (durabilité du rename lui-même). En cas d'échec, le temporaire est
/// nettoyé (best-effort).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let tmp = tmp_path(path);
    let write = || -> Result<(), CoreError> {
        {
            let mut file =
                fs::File::create(&tmp).map_err(|e| CoreError::io(tmp.display().to_string(), e))?;
            file.write_all(bytes)
                .map_err(|e| CoreError::io(tmp.display().to_string(), e))?;
            file.sync_all()
                .map_err(|e| CoreError::io(tmp.display().to_string(), e))?;
        }
        fs::rename(&tmp, path).map_err(|e| CoreError::io(path.display().to_string(), e))
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&tmp); // pas de .tmp orphelin après un échec
        return Err(e);
    }
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

/// Verrou mono-instance : tant que la poignée vit, aucune autre instance ne
/// peut l'acquérir sur le même dossier. Verrou de fichier **OS** (advisory) :
/// il disparaît automatiquement à la mort du process, crash compris — jamais
/// de verrou orphelin. Le fichier lui-même reste sur disque (marqueur
/// inoffensif contenant le pid, pour diagnostic).
#[derive(Debug)]
pub struct InstanceLock {
    /// Tenu pour la durée de vie : fermer le fichier libère le verrou.
    _file: fs::File,
    path: PathBuf,
}

impl InstanceLock {
    /// Chemin du fichier de verrou.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Tente de prendre le verrou mono-instance dans `dir` (dossier portable de
/// l'app). À appeler au démarrage, AVANT toute écriture : deux instances qui
/// sauvent le même show peuvent se corrompre mutuellement (backups, rescan).
///
/// `Err(CoreError::InstanceLocked)` si une autre instance vivante le tient —
/// à l'appelant de refuser net le démarrage (message clair, pas de panic).
pub fn acquire_instance_lock(dir: &Path) -> Result<InstanceLock, CoreError> {
    fs::create_dir_all(dir).map_err(|e| CoreError::io(dir.display().to_string(), e))?;
    let path = dir.join(LOCK_FILE);
    let display = path.display().to_string();
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Pas de troncature à l'ouverture : le contenu (pid) n'est réécrit
        // qu'une fois le verrou OS acquis (set_len ci-dessous).
        .truncate(false)
        .open(&path)
        .map_err(|e| CoreError::io(&*display, e))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            return Err(CoreError::InstanceLocked { path: display });
        }
        Err(fs::TryLockError::Error(e)) => return Err(CoreError::io(&*display, e)),
    }
    // PID écrit pour diagnostic — best-effort : c'est le verrou OS qui fait foi.
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    Ok(InstanceLock { _file: file, path })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "conduite-persist-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// Deux écritures ne partagent JAMAIS le même temporaire (pid + compteur),
    /// et le nom reste repérable : à côté du fichier cible, suffixe `.tmp`.
    #[test]
    fn tmp_paths_are_unique_and_recognizable() {
        let target = Path::new("shows/demo/show.json");
        let a = tmp_path(target);
        let b = tmp_path(target);
        assert_ne!(a, b, "deux écritures = deux temporaires distincts");
        for t in [&a, &b] {
            assert_eq!(t.parent(), target.parent(), "tmp à côté de la cible");
            let name = t.file_name().and_then(|n| n.to_str()).expect("nom utf-8");
            assert!(name.starts_with("show.json."), "préfixe cible : {name}");
            assert!(name.ends_with(".tmp"), "suffixe .tmp : {name}");
            assert!(
                name.contains(&std::process::id().to_string()),
                "pid dans le nom : {name}"
            );
        }
    }

    /// write_atomic avec le nouveau nommage : contenu remplacé, aucun .tmp
    /// restant (succès comme échec de la cible).
    #[test]
    fn write_atomic_leaves_no_tmp_behind() {
        let dir = temp_dir("tmp-clean");
        let path = dir.join("show.json");
        write_atomic(&path, b"v1").expect("write 1");
        write_atomic(&path, b"v2").expect("write 2");
        assert_eq!(fs::read(&path).expect("read"), b"v2");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .expect("read_dir")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporaires restants : {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Le verrou mono-instance refuse une seconde acquisition tant que la
    /// première poignée vit, puis redevient disponible à sa libération.
    #[test]
    fn instance_lock_is_exclusive_then_released() {
        let dir = temp_dir("lock");
        let first = acquire_instance_lock(&dir).expect("premier verrou");
        assert!(first.path().is_file(), "fichier de verrou créé");

        match acquire_instance_lock(&dir) {
            Err(CoreError::InstanceLocked { path }) => {
                assert!(path.ends_with(LOCK_FILE), "chemin du verrou : {path}");
            }
            other => panic!("attendu InstanceLocked, obtenu {other:?}"),
        }

        drop(first);
        let second = acquire_instance_lock(&dir).expect("verrou repris après libération");
        drop(second);
        let _ = fs::remove_dir_all(&dir);
    }
}
