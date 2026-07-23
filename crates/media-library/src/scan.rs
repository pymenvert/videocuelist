//! Scan des dossiers `media/` et `shaders/` → références du pool.
//!
//! Les chemins produits sont **relatifs** à la racine scannée, normalisés
//! avec des `/` (portables dans le JSON du show, valides pour
//! `conduite_core::validate_relative_path`). Les ids d'un scan brut sont
//! séquentiels par ordre de chemin ; la stabilité entre rescans est
//! garantie par [`reconcile`] / [`reconcile_materials`], qui préservent
//! les ids existants du show.

use std::fs;
use std::path::Path;

use conduite_core::{MaterialRef, MediaRef};

/// Extensions vidéo reconnues (comparaison insensible à la casse).
pub const VIDEO_EXTS: &[&str] = &["mov", "mp4", "avi", "mkv", "webm"];
/// Extensions image reconnues (comparaison insensible à la casse).
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg"];
/// Extension des matériaux ISF/GLSL.
pub const MATERIAL_EXT: &str = "fs";

/// Profondeur maximale de récursion (garde-fou contre les cycles de liens).
const MAX_DEPTH: u32 = 32;

/// Nature d'un fichier média du pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Image,
}

/// Nature d'un fichier selon son extension, ou `None` s'il n'est pas un
/// média reconnu.
pub fn media_kind(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if VIDEO_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else if IMAGE_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::Image)
    } else {
        None
    }
}

/// Liste récursive des fichiers sous `dir`, en chemins relatifs `/`.
/// Tolérant : dossier illisible ou nom non UTF-8 ⇒ warn + on continue.
fn walk(dir: &Path, prefix: &str, depth: u32, out: &mut Vec<String>) {
    if depth > MAX_DEPTH {
        tracing::warn!(target: "media_library::scan", path = %dir.display(),
            "profondeur maximale atteinte, sous-arbre ignoré");
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(target: "media_library::scan", path = %dir.display(),
                error = %e, "dossier illisible, ignoré");
            return;
        }
    };
    for entry in entries.flatten() {
        let name_os = entry.file_name();
        let Some(name) = name_os.to_str() else {
            tracing::warn!(target: "media_library::scan", path = %dir.display(),
                "nom de fichier non UTF-8 ignoré : {name_os:?}");
            continue;
        };
        // Fichiers/dossiers cachés (".DS_Store", ".git"…) ignorés.
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            walk(&path, &format!("{prefix}{name}/"), depth + 1, out);
        } else if path.is_file() {
            out.push(format!("{prefix}{name}"));
        }
    }
}

/// Nom d'affichage : dernier composant du chemin, sans extension.
fn display_name(rel: &str) -> String {
    let file = rel.rsplit('/').next().unwrap_or(rel);
    match file.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => file.to_string(),
    }
}

/// Scanne `media_dir` (récursif) : vidéos mov/mp4/avi/mkv/webm + images
/// png/jpg/jpeg. Ids séquentiels par ordre de chemin (déterministes pour un
/// contenu donné) ; métadonnées vides — voir [`crate::probe_all`] pour les
/// remplir, et [`reconcile`] pour préserver les ids d'un show existant.
pub fn scan(media_dir: &Path) -> Vec<MediaRef> {
    let mut rels = Vec::new();
    walk(media_dir, "", 0, &mut rels);
    rels.retain(|r| media_kind(Path::new(r)).is_some());
    rels.sort();
    tracing::info!(target: "media_library::scan", dir = %media_dir.display(),
        count = rels.len(), "scan des médias");
    rels.into_iter()
        .enumerate()
        .map(|(i, path)| MediaRef {
            // Troncature théorique au-delà de 4 milliards de fichiers : non réaliste.
            id: (i + 1) as u32,
            name: display_name(&path),
            path,
            duration_s: None,
            fps: None,
            width: 0,
            height: 0,
            missing: false,
        })
        .collect()
}

/// Scanne `shaders_dir` (récursif) : matériaux `*.fs`. Ids séquentiels par
/// ordre de chemin — voir [`reconcile_materials`] pour un rescan.
pub fn scan_materials(shaders_dir: &Path) -> Vec<MaterialRef> {
    let mut rels = Vec::new();
    walk(shaders_dir, "", 0, &mut rels);
    rels.retain(|r| {
        Path::new(r)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case(MATERIAL_EXT))
            .unwrap_or(false)
    });
    rels.sort();
    tracing::info!(target: "media_library::scan", dir = %shaders_dir.display(),
        count = rels.len(), "scan des matériaux");
    rels.into_iter()
        .enumerate()
        .map(|(i, path)| MaterialRef {
            id: (i + 1) as u32,
            name: display_name(&path),
            path,
        })
        .collect()
}

/// Réconcilie le pool du show avec un nouveau scan : **ids stables par
/// chemin**.
///
/// - Chaque média existant est conservé (id, nom, métadonnées) ; s'il a
///   disparu du disque il est marqué `missing` (les cues qui le référencent
///   affichent un placeholder, jamais une erreur).
/// - Les fichiers réapparus repassent `missing: false`.
/// - Les nouveaux fichiers reçoivent des ids frais (> max existant), dans
///   l'ordre des chemins.
pub fn reconcile(existing: &[MediaRef], scanned: Vec<MediaRef>) -> Vec<MediaRef> {
    use std::collections::BTreeSet;

    let scanned_paths: BTreeSet<&str> = scanned.iter().map(|m| m.path.as_str()).collect();
    let known_paths: BTreeSet<&str> = existing.iter().map(|m| m.path.as_str()).collect();
    let mut next_id = existing
        .iter()
        .map(|m| m.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);

    let mut result = Vec::with_capacity(existing.len() + scanned.len());
    for media in existing {
        let mut kept = media.clone();
        kept.missing = !scanned_paths.contains(media.path.as_str());
        if kept.missing {
            tracing::warn!(target: "media_library::scan", id = kept.id, path = %kept.path,
                "média disparu du disque, marqué manquant");
        }
        result.push(kept);
    }
    for media in scanned {
        if known_paths.contains(media.path.as_str()) {
            continue; // déjà dans le show, id existant préservé
        }
        result.push(MediaRef {
            id: next_id,
            ..media
        });
        next_id = next_id.saturating_add(1);
    }
    result
}

/// Réconcilie les matériaux : mêmes règles que [`reconcile`], sans champ
/// `missing` (un matériau absent est conservé — le compositor affiche un
/// placeholder à la compilation — et signalé en warn).
pub fn reconcile_materials(
    existing: &[MaterialRef],
    scanned: Vec<MaterialRef>,
) -> Vec<MaterialRef> {
    use std::collections::BTreeSet;

    let scanned_paths: BTreeSet<&str> = scanned.iter().map(|m| m.path.as_str()).collect();
    let known_paths: BTreeSet<&str> = existing.iter().map(|m| m.path.as_str()).collect();
    let mut next_id = existing
        .iter()
        .map(|m| m.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);

    let mut result = Vec::with_capacity(existing.len() + scanned.len());
    for material in existing {
        if !scanned_paths.contains(material.path.as_str()) {
            tracing::warn!(target: "media_library::scan", id = material.id,
                path = %material.path, "matériau disparu du disque");
        }
        result.push(material.clone());
    }
    for material in scanned {
        if known_paths.contains(material.path.as_str()) {
            continue;
        }
        result.push(MaterialRef {
            id: next_id,
            ..material
        });
        next_id = next_id.saturating_add(1);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------- validation d'extensions

    #[test]
    fn media_kind_matches_known_extensions() {
        for ok in ["a.mov", "a.mp4", "a.avi", "a.mkv", "a.webm", "A.MOV", "clip.Mp4"] {
            assert_eq!(media_kind(Path::new(ok)), Some(MediaKind::Video), "{ok}");
        }
        for ok in ["a.png", "a.jpg", "a.jpeg", "A.PNG", "photo.JPeG"] {
            assert_eq!(media_kind(Path::new(ok)), Some(MediaKind::Image), "{ok}");
        }
        for bad in ["a.txt", "a.fs", "a", "a.mp3", "a.mp4.txt", ".mp4", "dossier/"] {
            assert_eq!(media_kind(Path::new(bad)), None, "{bad}");
        }
    }

    #[test]
    fn display_name_strips_dirs_and_extension() {
        assert_eq!(display_name("clips/intro.mp4"), "intro");
        assert_eq!(display_name("photo.jpeg"), "photo");
        assert_eq!(display_name("sans_extension"), "sans_extension");
        assert_eq!(display_name("a/b/c.d.mp4"), "c.d");
    }

    // -------------------------------------------------------------------- scan

    #[test]
    fn scan_finds_media_recursively_sorted_with_sequential_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        for f in [
            "a.mp4",
            "B.PNG",
            "notes.txt",     // ignoré : pas un média
            "shader.fs",     // ignoré : matériau, pas média
            ".cache.mp4",    // ignoré : caché
            "sub/clip.mkv",
        ] {
            std::fs::write(root.join(f), b"x").expect("write");
        }

        let media = scan(root);
        let paths: Vec<&str> = media.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["B.PNG", "a.mp4", "sub/clip.mkv"], "tri par chemin");
        let ids: Vec<u32> = media.iter().map(|m| m.id).collect();
        assert_eq!(ids, [1, 2, 3]);
        let names: Vec<&str> = media.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["B", "a", "clip"]);
        assert!(media.iter().all(|m| !m.missing));
        assert!(media.iter().all(|m| m.duration_s.is_none() && m.fps.is_none()));
    }

    #[test]
    fn scan_is_deterministic() {
        let dir = tempfile::tempdir().expect("tempdir");
        for f in ["z.mp4", "a.png"] {
            std::fs::write(dir.path().join(f), b"x").expect("write");
        }
        assert_eq!(scan(dir.path()), scan(dir.path()));
    }

    #[test]
    fn scan_missing_dir_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let none = dir.path().join("nexiste-pas");
        assert!(scan(&none).is_empty());
        assert!(scan_materials(&none).is_empty());
    }

    #[test]
    fn scan_materials_finds_fs_files_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("pack")).expect("mkdir");
        for f in ["glow.fs", "pack/kaleido.fs", "readme.md", "clip.mp4", "UPPER.FS"] {
            std::fs::write(root.join(f), b"x").expect("write");
        }
        let materials = scan_materials(root);
        let paths: Vec<&str> = materials.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["UPPER.FS", "glow.fs", "pack/kaleido.fs"]);
        assert_eq!(materials[1].name, "glow");
        let ids: Vec<u32> = materials.iter().map(|m| m.id).collect();
        assert_eq!(ids, [1, 2, 3]);
    }

    // --------------------------------------------------------------- reconcile

    fn media(id: u32, path: &str) -> MediaRef {
        MediaRef {
            id,
            path: path.to_string(),
            name: display_name(path),
            duration_s: None,
            fps: None,
            width: 0,
            height: 0,
            missing: false,
        }
    }

    #[test]
    fn reconcile_preserves_ids_marks_missing_and_assigns_fresh_ids() {
        // Show existant : ids 7 et 3, métadonnées déjà sondées sur le 7.
        let mut kept = media(7, "a.mp4");
        kept.duration_s = Some(12.5);
        kept.fps = Some(25.0);
        kept.width = 1920;
        kept.height = 1080;
        let gone = media(3, "disparu.mp4");
        let existing = vec![kept.clone(), gone.clone()];

        // Nouveau scan : a.mp4 toujours là (id 1 du scan brut), nouveau.mp4 apparu.
        let scanned = vec![media(1, "a.mp4"), media(2, "nouveau.mp4")];
        let result = reconcile(&existing, scanned);

        assert_eq!(result.len(), 3);
        // a.mp4 : id 7 préservé, métadonnées préservées, pas manquant.
        assert_eq!(result[0].id, 7);
        assert_eq!(result[0].path, "a.mp4");
        assert_eq!(result[0].duration_s, Some(12.5));
        assert_eq!(result[0].width, 1920);
        assert!(!result[0].missing);
        // disparu.mp4 : id 3 préservé, marqué manquant (jamais supprimé).
        assert_eq!(result[1].id, 3);
        assert!(result[1].missing);
        // nouveau.mp4 : id frais > max existant (7) ⇒ 8.
        assert_eq!(result[2].id, 8);
        assert_eq!(result[2].path, "nouveau.mp4");
        assert!(!result[2].missing);
    }

    #[test]
    fn reconcile_clears_missing_when_file_reappears() {
        let mut ghost = media(4, "revenu.mp4");
        ghost.missing = true;
        let result = reconcile(&[ghost], vec![media(1, "revenu.mp4")]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, 4);
        assert!(!result[0].missing, "fichier réapparu ⇒ missing effacé");
    }

    #[test]
    fn reconcile_from_empty_show_keeps_scan_order_ids_from_one() {
        let scanned = vec![media(1, "a.mp4"), media(2, "b.mp4")];
        let result = reconcile(&[], scanned.clone());
        assert_eq!(result, scanned);
    }

    #[test]
    fn reconcile_keeps_user_renames() {
        let mut renamed = media(2, "a.mp4");
        renamed.name = "Ouverture".to_string();
        let result = reconcile(&[renamed], vec![media(1, "a.mp4")]);
        assert_eq!(result[0].name, "Ouverture", "nom personnalisé préservé");
    }

    #[test]
    fn reconcile_materials_preserves_ids_and_appends_new() {
        let existing = vec![MaterialRef {
            id: 5,
            path: "glow.fs".into(),
            name: "glow".into(),
        }];
        let scanned = vec![
            MaterialRef {
                id: 1,
                path: "glow.fs".into(),
                name: "glow".into(),
            },
            MaterialRef {
                id: 2,
                path: "kaleido.fs".into(),
                name: "kaleido".into(),
            },
        ];
        let result = reconcile_materials(&existing, scanned);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, 5);
        assert_eq!(result[1].id, 6);
        assert_eq!(result[1].path, "kaleido.fs");
    }
}
