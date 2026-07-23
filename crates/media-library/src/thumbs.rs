//! Vignettes JPEG du pool (largeur 320, nommées `{media_id}.jpg` pour
//! `GET /thumb/{media_id}.jpg`).
//!
//! - Vidéos : `ffmpeg -ss <10 % de la durée> -frames:v 1 -vf scale=320:-2`
//!   (`-2` et non `-1` : l'encodeur JPEG exige une hauteur paire).
//! - Images : redimensionnement via la crate `image` (aucun ffmpeg requis).
//!
//! **Jamais sur le thread de rendu** : [`generate_thumbs`] est prévu pour
//! une tâche de fond ; une vignette déjà à jour n'est pas régénérée.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use conduite_core::{MediaId, MediaRef};

use crate::scan::{media_kind, MediaKind};

/// Largeur des vignettes (hauteur au prorata).
pub const THUMB_WIDTH: u32 = 320;

/// Chemin de la vignette d'un média dans le cache : `{cache_dir}/{id}.jpg`.
pub fn thumb_path(cache_dir: &Path, id: MediaId) -> PathBuf {
    cache_dir.join(format!("{id}.jpg"))
}

/// Localise ffmpeg : `./bin/ffmpeg(.exe)` du dossier portable d'abord,
/// sinon le PATH — même ordre que `conduite_engine::resolve_ffmpeg`.
pub fn resolve_ffmpeg() -> PathBuf {
    let exe_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let portable = dir.join("bin").join(exe_name);
            if portable.is_file() {
                return portable;
            }
        }
    }
    PathBuf::from("ffmpeg")
}

/// ffmpeg répond-il ? (sonde `-version`, utile pour un skip propre.)
pub fn ffmpeg_available() -> bool {
    Command::new(resolve_ffmpeg())
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Point de capture : 10 % de la durée, 0 si durée inconnue ou invalide.
fn seek_seconds(duration_s: Option<f64>) -> f64 {
    match duration_s {
        Some(d) if d.is_finite() && d > 0.0 => d * 0.10,
        _ => 0.0,
    }
}

/// La vignette existe-t-elle, non vide et plus récente que la source ?
fn is_fresh(thumb: &Path, src: &Path) -> bool {
    let fresh = || -> Option<bool> {
        let t = fs::metadata(thumb).ok()?;
        if t.len() == 0 {
            return Some(false);
        }
        let t_time = t.modified().ok()?;
        let s_time = fs::metadata(src).ok()?.modified().ok()?;
        Some(t_time >= s_time)
    };
    fresh().unwrap_or(false)
}

/// Extrait une frame vidéo en JPEG 320 px de large.
fn ffmpeg_thumb(src: &Path, out: &Path, seek_s: f64) -> anyhow::Result<()> {
    let ffmpeg = resolve_ffmpeg();
    let output = Command::new(&ffmpeg)
        .arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-ss")
        .arg(format!("{seek_s:.3}"))
        .arg("-i")
        .arg(src)
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg(format!("scale={THUMB_WIDTH}:-2"))
        .arg("-f")
        .arg("image2")
        .arg(out)
        .output()
        .with_context(|| format!("lancement de {} impossible", ffmpeg.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "ffmpeg a échoué ({}) sur {} : {}",
            output.status,
            src.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Garantit la vignette d'un média et retourne son chemin.
///
/// Ne régénère pas une vignette déjà à jour (plus récente que la source).
/// IO + sous-processus : tâche de fond uniquement, jamais le thread de rendu.
pub fn ensure_thumb(media: &MediaRef, media_dir: &Path, cache_dir: &Path) -> anyhow::Result<PathBuf> {
    conduite_core::validate_relative_path(&media.path)
        .map_err(|e| anyhow::anyhow!("chemin de média refusé : {e}"))?;
    let src = media_dir.join(&media.path);
    if !src.is_file() {
        anyhow::bail!("média {} introuvable : {}", media.id, src.display());
    }
    let out = thumb_path(cache_dir, media.id);
    if is_fresh(&out, &src) {
        return Ok(out);
    }
    fs::create_dir_all(cache_dir)
        .with_context(|| format!("création du cache {}", cache_dir.display()))?;

    match media_kind(Path::new(&media.path)) {
        Some(MediaKind::Video) => {
            let seek = seek_seconds(media.duration_s);
            ffmpeg_thumb(&src, &out, seek)?;
            // Seek au-delà de la fin (durée surestimée) : ffmpeg sort en
            // succès sans écrire de frame — on retente au tout début.
            let empty = fs::metadata(&out).map(|m| m.len() == 0).unwrap_or(true);
            if empty && seek > 0.0 {
                ffmpeg_thumb(&src, &out, 0.0)?;
            }
            let empty = fs::metadata(&out).map(|m| m.len() == 0).unwrap_or(true);
            if empty {
                anyhow::bail!("ffmpeg n'a produit aucune image pour {}", src.display());
            }
        }
        Some(MediaKind::Image) => {
            let img = image::open(&src)
                .with_context(|| format!("image illisible : {}", src.display()))?;
            img.thumbnail(THUMB_WIDTH, u32::MAX)
                .to_rgb8() // JPEG sans alpha
                .save(&out)
                .with_context(|| format!("écriture de la vignette {}", out.display()))?;
        }
        None => anyhow::bail!("extension non média : {}", media.path),
    }
    tracing::debug!(target: "media_library::thumbs", id = media.id, path = %media.path,
        thumb = %out.display(), "vignette générée");
    Ok(out)
}

/// Rapport d'une génération par lot.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ThumbReport {
    /// Vignettes garanties (générées ou déjà à jour).
    pub ok: usize,
    /// Échecs (warn logués, jamais bloquants).
    pub failed: usize,
    /// Médias `missing` sautés.
    pub skipped_missing: usize,
}

/// Génération par lot, **à appeler depuis une tâche de fond** (thread ou
/// pool de l'app) : ne bloque jamais le rendu, n'échoue jamais — chaque
/// raté est loggué et compté.
pub fn generate_thumbs(media: &[MediaRef], media_dir: &Path, cache_dir: &Path) -> ThumbReport {
    let mut report = ThumbReport::default();
    for m in media {
        if m.missing {
            report.skipped_missing += 1;
            continue;
        }
        match ensure_thumb(m, media_dir, cache_dir) {
            Ok(_) => report.ok += 1,
            Err(e) => {
                tracing::warn!(target: "media_library::thumbs", id = m.id, path = %m.path,
                    error = %e, "vignette impossible");
                report.failed += 1;
            }
        }
    }
    tracing::info!(target: "media_library::thumbs", ok = report.ok, failed = report.failed,
        skipped = report.skipped_missing, "génération des vignettes terminée");
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(id: u32, path: &str, duration_s: Option<f64>) -> MediaRef {
        MediaRef {
            id,
            path: path.to_string(),
            name: path.to_string(),
            duration_s,
            fps: None,
            width: 0,
            height: 0,
            missing: false,
        }
    }

    #[test]
    fn thumb_path_is_id_dot_jpg() {
        assert_eq!(
            thumb_path(Path::new("cache"), 42),
            Path::new("cache").join("42.jpg")
        );
    }

    #[test]
    fn seek_is_ten_percent_or_zero() {
        assert_eq!(seek_seconds(Some(10.0)), 1.0);
        assert_eq!(seek_seconds(Some(0.0)), 0.0);
        assert_eq!(seek_seconds(Some(-5.0)), 0.0);
        assert_eq!(seek_seconds(Some(f64::NAN)), 0.0);
        assert_eq!(seek_seconds(Some(f64::INFINITY)), 0.0);
        assert_eq!(seek_seconds(None), 0.0);
    }

    #[test]
    fn ensure_thumb_resizes_image_without_ffmpeg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let media_dir = dir.path().join("media");
        let cache = dir.path().join("thumbnails");
        std::fs::create_dir_all(&media_dir).expect("mkdir");
        image::RgbaImage::new(640, 480)
            .save(media_dir.join("photo.png"))
            .expect("png");

        let m = media(7, "photo.png", None);
        let thumb = ensure_thumb(&m, &media_dir, &cache).expect("thumb");
        assert_eq!(thumb, cache.join("7.jpg"));
        let (w, h) = image::image_dimensions(&thumb).expect("dims");
        assert_eq!((w, h), (320, 240), "320 de large, prorata");
    }

    #[test]
    fn ensure_thumb_skips_when_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let media_dir = dir.path().join("media");
        let cache = dir.path().join("thumbnails");
        std::fs::create_dir_all(&media_dir).expect("mkdir");
        image::RgbaImage::new(64, 64)
            .save(media_dir.join("p.png"))
            .expect("png");

        let m = media(1, "p.png", None);
        let thumb = ensure_thumb(&m, &media_dir, &cache).expect("thumb");
        // On remplace la vignette par une sentinelle PLUS RÉCENTE que la
        // source : un second appel ne doit pas la régénérer.
        std::fs::write(&thumb, b"sentinelle").expect("write");
        let again = ensure_thumb(&m, &media_dir, &cache).expect("thumb 2");
        assert_eq!(again, thumb);
        assert_eq!(std::fs::read(&thumb).expect("read"), b"sentinelle");
    }

    #[test]
    fn ensure_thumb_rejects_missing_file_and_bad_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = media(1, "fantome.mp4", None);
        assert!(ensure_thumb(&m, dir.path(), dir.path()).is_err());
        let evil = media(2, "../evasion.mp4", None);
        assert!(ensure_thumb(&evil, dir.path(), dir.path()).is_err());
    }

    #[test]
    fn generate_thumbs_counts_and_never_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let media_dir = dir.path().join("media");
        let cache = dir.path().join("thumbnails");
        std::fs::create_dir_all(&media_dir).expect("mkdir");
        image::RgbaImage::new(32, 32)
            .save(media_dir.join("ok.png"))
            .expect("png");
        std::fs::write(media_dir.join("casse.png"), b"pas un png").expect("write");

        let mut absent = media(3, "absent.png", None);
        absent.missing = true;
        let pool = vec![media(1, "ok.png", None), media(2, "casse.png", None), absent];
        let report = generate_thumbs(&pool, &media_dir, &cache);
        assert_eq!(
            report,
            ThumbReport {
                ok: 1,
                failed: 1,
                skipped_missing: 1
            }
        );
    }

    /// Vignette vidéo réelle si ffmpeg et le média de démo sont présents ;
    /// skip propre (message + retour) sinon.
    #[test]
    fn video_thumb_with_real_ffmpeg_if_present() {
        let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../media/demo-bars.mp4");
        if !demo.is_file() {
            eprintln!("SKIP : media/demo-bars.mp4 absent");
            return;
        }
        if !ffmpeg_available() {
            eprintln!("SKIP : ffmpeg introuvable (bin/ portable et PATH)");
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let media_dir = demo.parent().expect("parent");
        let m = media(9, "demo-bars.mp4", None);
        let thumb = ensure_thumb(&m, media_dir, dir.path()).expect("thumb vidéo");
        let (w, _h) = image::image_dimensions(&thumb).expect("jpg lisible");
        assert_eq!(w, THUMB_WIDTH);
    }

    /// Lot complet sur le dossier media/ du repo — dépend de ffmpeg.
    #[test]
    #[ignore = "dépend de ffmpeg et des médias de démo du repo"]
    fn batch_thumbs_on_repo_demo_media() {
        let media_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../media");
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::scan::scan(&media_dir);
        assert!(!pool.is_empty());
        let report = generate_thumbs(&pool, &media_dir, dir.path());
        assert_eq!(report.failed, 0, "{report:?}");
        assert_eq!(report.ok, pool.len());
    }
}
