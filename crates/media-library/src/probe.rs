//! Sondage **tolérant** des métadonnées du pool (durée, fps, dimensions).
//!
//! Le sondage vidéo est injecté : l'app passe `conduite_engine::probe`
//! (ffprobe) — `conduite-media-library` ne dépend ainsi d'aucun binaire.
//! Un échec de sonde n'est JAMAIS bloquant : métadonnées remises à zéro,
//! `missing` inchangé, warn loggué (doctrine SPEC §10).

use std::path::Path;

use conduite_core::MediaRef;

use crate::scan::{media_kind, MediaKind};

/// Métadonnées sondées d'une vidéo — même forme que
/// `conduite_engine::MediaInfo` : l'adaptateur côté app est trivial :
///
/// ```text
/// probe_all(&mut show.media, &media_dir, |p| {
///     conduite_engine::probe(p).map(|i| ProbeInfo {
///         duration_s: i.duration_s, fps: i.fps, width: i.width, height: i.height,
///     })
/// });
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeInfo {
    pub duration_s: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
}

/// Sonde tous les médias non manquants du pool, en place.
///
/// - Vidéos : via `probe` (injectée). Échec ⇒ métadonnées `None`/0,
///   `missing` reste `false`, warn.
/// - Images : dimensions via la crate `image` (pas de durée ni fps).
/// - Médias `missing` : ignorés (rien à sonder).
///
/// IO disque + sous-processus : à appeler en tâche de fond uniquement.
pub fn probe_all<F>(media: &mut [MediaRef], media_dir: &Path, probe: F)
where
    F: Fn(&Path) -> anyhow::Result<ProbeInfo>,
{
    for m in media.iter_mut() {
        if m.missing {
            continue;
        }
        let Some(kind) = media_kind(Path::new(&m.path)) else {
            continue;
        };
        let full = media_dir.join(&m.path);
        match kind {
            MediaKind::Video => match probe(&full) {
                Ok(info) => {
                    m.duration_s = Some(info.duration_s);
                    m.fps = Some(info.fps);
                    m.width = info.width;
                    m.height = info.height;
                }
                Err(e) => {
                    tracing::warn!(target: "media_library::probe", id = m.id,
                        path = %m.path, error = %e,
                        "sonde vidéo échouée, métadonnées inconnues (média conservé)");
                    m.duration_s = None;
                    m.fps = None;
                    m.width = 0;
                    m.height = 0;
                }
            },
            MediaKind::Image => match image::image_dimensions(&full) {
                Ok((w, h)) => {
                    m.duration_s = None;
                    m.fps = None;
                    m.width = w;
                    m.height = h;
                }
                Err(e) => {
                    tracing::warn!(target: "media_library::probe", id = m.id,
                        path = %m.path, error = %e,
                        "dimensions d'image illisibles (média conservé)");
                    m.duration_s = None;
                    m.fps = None;
                    m.width = 0;
                    m.height = 0;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn media(id: u32, path: &str, missing: bool) -> MediaRef {
        MediaRef {
            id,
            path: path.to_string(),
            name: path.to_string(),
            duration_s: None,
            fps: None,
            width: 0,
            height: 0,
            missing,
        }
    }

    #[test]
    fn probe_all_fills_video_metadata_and_tolerates_failures() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Une vraie image 8×6 pour la branche image.
        image::RgbaImage::new(8, 6)
            .save(dir.path().join("photo.png"))
            .expect("png");

        let mut pool = vec![
            media(1, "ok.mp4", false),
            media(2, "casse.mp4", false),
            media(3, "photo.png", false),
            media(4, "absent.mp4", true), // missing : ne doit pas être sondé
        ];
        let calls = Cell::new(0u32);
        probe_all(&mut pool, dir.path(), |p| {
            calls.set(calls.get() + 1);
            if p.to_string_lossy().contains("ok.mp4") {
                Ok(ProbeInfo {
                    duration_s: 42.5,
                    fps: 30.0,
                    width: 1280,
                    height: 720,
                })
            } else {
                anyhow::bail!("fichier corrompu")
            }
        });

        // Vidéo sondée avec succès.
        assert_eq!(pool[0].duration_s, Some(42.5));
        assert_eq!(pool[0].fps, Some(30.0));
        assert_eq!((pool[0].width, pool[0].height), (1280, 720));
        // Échec de sonde : métadonnées None, missing reste false.
        assert_eq!(pool[1].duration_s, None);
        assert_eq!(pool[1].fps, None);
        assert_eq!((pool[1].width, pool[1].height), (0, 0));
        assert!(!pool[1].missing, "échec de sonde ≠ média manquant");
        // Image : dimensions réelles, pas de durée.
        assert_eq!((pool[2].width, pool[2].height), (8, 6));
        assert_eq!(pool[2].duration_s, None);
        // Le média manquant n'a pas été sondé : 2 appels vidéo seulement.
        assert_eq!(calls.get(), 2);
        assert!(pool[3].missing);
    }

    #[test]
    fn probe_all_tolerates_unreadable_image() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("faux.png"), b"pas un png").expect("write");
        let mut pool = vec![media(1, "faux.png", false)];
        probe_all(&mut pool, dir.path(), |_| anyhow::bail!("jamais appelé"));
        assert_eq!((pool[0].width, pool[0].height), (0, 0));
        assert!(!pool[0].missing);
    }
}
