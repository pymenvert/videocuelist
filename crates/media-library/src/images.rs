//! Chargement d'images fixes en RGBA 8 bits (PNG/JPEG, alpha préservé).

use std::path::Path;

use anyhow::Context as _;

/// Image décodée en RGBA8, prête pour l'upload GPU — même convention que
/// `conduite_engine::FrameRgba` (sans horodatage) : `data.len() == w*h*4`,
/// lignes du haut vers le bas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Charge une image fixe en RGBA8. IO + décodage : tâche de fond ou
/// préchargement de cue, jamais le thread de rendu.
pub fn load_image_rgba(path: &Path) -> anyhow::Result<ImageRgba> {
    let img = image::open(path).with_context(|| format!("image illisible : {}", path.display()))?;
    let rgba = img.to_rgba8();
    Ok(ImageRgba {
        width: rgba.width(),
        height: rgba.height(),
        data: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_png_with_alpha_as_rgba8() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pix.png");
        let mut img = image::RgbaImage::new(3, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(2, 1, image::Rgba([0, 0, 255, 128]));
        img.save(&path).expect("png");

        let loaded = load_image_rgba(&path).expect("load");
        assert_eq!((loaded.width, loaded.height), (3, 2));
        assert_eq!(loaded.data.len(), 3 * 2 * 4);
        assert_eq!(&loaded.data[0..4], &[255, 0, 0, 255], "pixel (0,0)");
        let last = (3 + 2) * 4; // pixel (2,1) : y*largeur + x, lignes de haut en bas
        assert_eq!(&loaded.data[last..last + 4], &[0, 0, 255, 128]);
    }

    #[test]
    fn unreadable_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_image_rgba(&dir.path().join("nexiste.png")).is_err());
        let bad = dir.path().join("faux.png");
        std::fs::write(&bad, b"pas un png").expect("write");
        assert!(load_image_rgba(&bad).is_err());
    }
}
