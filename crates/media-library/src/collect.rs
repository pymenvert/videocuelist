//! « Collecter le show » : produit un dossier **autonome** (clé USB, autre
//! machine) — `show.json` + copies de tous les médias et matériaux du pool,
//! aux mêmes chemins relatifs (`media/…`, `shaders/…`).
//!
//! Tolérant : un fichier manquant est compté et signalé, jamais bloquant —
//! le show collecté marque ces médias `missing`.

use std::fs;
use std::path::Path;

use anyhow::Context as _;
use conduite_core::{validate_relative_path, write_atomic, Show, SHOW_FILE};

/// Rapport de collecte : { copiés, manquants, octets }.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CollectReport {
    /// Fichiers effectivement copiés.
    pub copied: usize,
    /// Chemins relatifs introuvables (ou refusés) — non copiés.
    pub missing: Vec<String>,
    /// Octets copiés au total.
    pub bytes: u64,
}

/// Copie `src_root/rel` vers `dest_root/rel` (création des parents).
/// Retourne le nombre d'octets copiés.
fn copy_rel(src_root: &Path, rel: &str, dest_root: &Path) -> anyhow::Result<u64> {
    // Jamais de lecture hors racine, même depuis un show trafiqué.
    validate_relative_path(rel).map_err(|e| anyhow::anyhow!("chemin refusé : {e}"))?;
    let src = src_root.join(rel);
    if !src.is_file() {
        anyhow::bail!("introuvable : {}", src.display());
    }
    let dest = dest_root.join(rel);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("création de {}", parent.display()))?;
    }
    fs::copy(&src, &dest)
        .with_context(|| format!("copie {} → {}", src.display(), dest.display()))
}

/// Collecte le show dans `dest_dir` : écrit `dest_dir/show.json` (atomique)
/// et copie tout le pool depuis `media_dir`/`shaders_dir` vers
/// `dest_dir/media/` et `dest_dir/shaders/`.
///
/// Le show écrit reflète la réalité du dossier collecté : `missing` remis à
/// jour média par média. IO massif : tâche de fond uniquement.
pub fn collect_show(
    show: &Show,
    media_dir: &Path,
    shaders_dir: &Path,
    dest_dir: &Path,
) -> anyhow::Result<CollectReport> {
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("création de {}", dest_dir.display()))?;
    let dest_media = dest_dir.join("media");
    let dest_shaders = dest_dir.join("shaders");
    let mut report = CollectReport::default();

    // Copie du pool ; le show collecté porte les flags `missing` à jour.
    let mut collected = show.clone();
    for media in &mut collected.media {
        match copy_rel(media_dir, &media.path, &dest_media) {
            Ok(bytes) => {
                report.copied += 1;
                report.bytes += bytes;
                media.missing = false;
            }
            Err(e) => {
                tracing::warn!(target: "media_library::collect", id = media.id,
                    path = %media.path, error = %e, "média non collecté");
                report.missing.push(media.path.clone());
                media.missing = true;
            }
        }
    }
    for material in &collected.materials {
        match copy_rel(shaders_dir, &material.path, &dest_shaders) {
            Ok(bytes) => {
                report.copied += 1;
                report.bytes += bytes;
            }
            Err(e) => {
                tracing::warn!(target: "media_library::collect", id = material.id,
                    path = %material.path, error = %e, "matériau non collecté");
                report.missing.push(material.path.clone());
            }
        }
    }

    let json = serde_json::to_vec_pretty(&collected).context("sérialisation du show")?;
    write_atomic(&dest_dir.join(SHOW_FILE), &json)
        .map_err(|e| anyhow::anyhow!("écriture de show.json : {e}"))?;

    tracing::info!(target: "media_library::collect", dest = %dest_dir.display(),
        copied = report.copied, missing = report.missing.len(), bytes = report.bytes,
        "show collecté");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use conduite_core::{MaterialRef, MediaRef};

    use super::*;

    fn media(id: u32, path: &str) -> MediaRef {
        MediaRef {
            id,
            path: path.to_string(),
            name: path.to_string(),
            duration_s: None,
            fps: None,
            width: 0,
            height: 0,
            missing: false,
        }
    }

    #[test]
    fn collect_copies_pool_writes_show_and_reports() {
        let dir = tempfile::tempdir().expect("tempdir");
        let media_dir = dir.path().join("media");
        let shaders_dir = dir.path().join("shaders");
        let dest = dir.path().join("collecte");
        std::fs::create_dir_all(media_dir.join("clips")).expect("mkdir");
        std::fs::create_dir_all(shaders_dir.join("fx")).expect("mkdir");
        std::fs::write(media_dir.join("clips/a.mp4"), b"AAAA").expect("write"); // 4 octets
        std::fs::write(media_dir.join("img.png"), b"BB").expect("write"); // 2 octets
        std::fs::write(shaders_dir.join("fx/glow.fs"), b"CCC").expect("write"); // 3 octets

        let mut show = Show::new("Tournée");
        show.media.push(media(1, "clips/a.mp4"));
        show.media.push(media(2, "img.png"));
        show.media.push(media(3, "fantome.mp4")); // absent du disque
        show.materials.push(MaterialRef {
            id: 1,
            path: "fx/glow.fs".into(),
            name: "glow".into(),
        });
        show.materials.push(MaterialRef {
            id: 2,
            path: "fantome.fs".into(),
            name: "fantome".into(),
        });

        let report = collect_show(&show, &media_dir, &shaders_dir, &dest).expect("collect");
        assert_eq!(report.copied, 3);
        assert_eq!(report.bytes, 4 + 2 + 3);
        assert_eq!(report.missing, ["fantome.mp4", "fantome.fs"]);

        // Le dossier est autonome : mêmes chemins relatifs.
        assert_eq!(
            std::fs::read(dest.join("media/clips/a.mp4")).expect("copie"),
            b"AAAA"
        );
        assert!(dest.join("media/img.png").is_file());
        assert!(dest.join("shaders/fx/glow.fs").is_file());

        // show.json écrit, flags missing à jour.
        let bytes = std::fs::read(dest.join(SHOW_FILE)).expect("show.json");
        let collected: Show = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(collected.name, "Tournée");
        let missing: Vec<bool> = collected.media.iter().map(|m| m.missing).collect();
        assert_eq!(missing, [false, false, true]);
    }

    #[test]
    fn collect_refuses_path_traversal_without_failing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let media_dir = dir.path().join("media");
        std::fs::create_dir_all(&media_dir).expect("mkdir");
        // Une cible réelle HORS de la racine média : ne doit jamais être copiée.
        std::fs::write(dir.path().join("secret.mp4"), b"secret").expect("write");

        let mut show = Show::new("piégé");
        show.media.push(media(1, "../secret.mp4"));

        let dest = dir.path().join("out");
        let report =
            collect_show(&show, &media_dir, &media_dir, &dest).expect("collect tolérant");
        assert_eq!(report.copied, 0);
        assert_eq!(report.missing, ["../secret.mp4"]);
        assert!(
            !dir.path().join("secret_copie").exists() && !dest.join("secret.mp4").exists(),
            "rien ne doit fuir hors racine"
        );
        assert!(dest.join(SHOW_FILE).is_file(), "show.json écrit malgré tout");
    }

    #[test]
    fn collect_empty_show_still_writes_show_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("out");
        let report = collect_show(&Show::new("vide"), dir.path(), dir.path(), &dest)
            .expect("collect");
        assert_eq!(report, CollectReport::default());
        assert!(dest.join(SHOW_FILE).is_file());
    }
}
