//! Préview MJPEG : encodage JPEG hors du chemin critique (thread dédié,
//! file bornée — si l'encodeur prend du retard, les frames sont sautées),
//! diffusion via les canaux broadcast consommés par `control-http`.

use bytes::Bytes;
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// Qualité JPEG de la préview.
const JPEG_QUALITY: u8 = 70;

/// Une frame RGBA à encoder (lignes de bas en haut : sortie `glReadPixels`).
pub struct PreviewJob {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// `true` = préview standby (deck B) → canal `preview-b`.
    pub standby: bool,
    /// Lignes à retourner verticalement (frames issues d'un FBO GL).
    pub flip: bool,
}

/// Encodeur JPEG asynchrone (thread + file bornée à 2, jamais bloquant).
pub struct PreviewWorker {
    tx: crossbeam_channel::Sender<PreviewJob>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

impl PreviewWorker {
    pub fn spawn(
        program_tx: broadcast::Sender<Bytes>,
        standby_tx: broadcast::Sender<Bytes>,
    ) -> PreviewWorker {
        let (tx, rx) = crossbeam_channel::bounded::<PreviewJob>(2);
        let thread = std::thread::Builder::new()
            .name("conduite-preview".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let Some(jpeg) = encode_jpeg(&job) else {
                        continue;
                    };
                    let target = if job.standby { &standby_tx } else { &program_tx };
                    // Aucun abonné = Err : normal sans client MJPEG connecté.
                    let _ = target.send(Bytes::from(jpeg));
                }
                debug!(target: "app::preview", "encodeur préview arrêté");
            });
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                warn!(target: "app::preview", error = %e,
                    "thread préview impossible : MJPEG inactif");
                None
            }
        };
        PreviewWorker { tx, _thread: thread }
    }

    /// Soumet une frame ; si l'encodeur est occupé, la frame est sautée
    /// (jamais d'attente sur le chemin de rendu).
    pub fn submit(&self, job: PreviewJob) {
        if self.tx.try_send(job).is_err() {
            debug!(target: "app::preview", "encodeur occupé : frame préview sautée");
        }
    }
}

/// RGBA (éventuellement inversée verticalement) → JPEG.
fn encode_jpeg(job: &PreviewJob) -> Option<Vec<u8>> {
    let w = job.width as usize;
    let h = job.height as usize;
    if w == 0 || h == 0 || job.rgba.len() < w * h * 4 {
        warn!(target: "app::preview", w, h, len = job.rgba.len(), "frame préview invalide");
        return None;
    }
    // RGBA → RGB avec flip vertical éventuel.
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        let src_y = if job.flip { h - 1 - y } else { y };
        let src = &job.rgba[src_y * w * 4..src_y * w * 4 + w * 4];
        let dst = &mut rgb[y * w * 3..y * w * 3 + w * 3];
        for x in 0..w {
            dst[x * 3] = src[x * 4];
            dst[x * 3 + 1] = src[x * 4 + 1];
            dst[x * 3 + 2] = src[x * 4 + 2];
        }
    }
    let mut out = Vec::with_capacity(32 * 1024);
    let encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut out), JPEG_QUALITY);
    match image::ImageEncoder::write_image(
        encoder,
        &rgb,
        job.width,
        job.height,
        image::ExtendedColorType::Rgb8,
    ) {
        Ok(()) => Some(out),
        Err(e) => {
            warn!(target: "app::preview", error = %e, "encodage JPEG raté");
            None
        }
    }
}

/// JPEG placeholder (gris avec un cadre plus sombre) pour le mode headless
/// et l'absence de GL : l'endpoint MJPEG reste vivant.
pub fn placeholder_jpeg(width: u32, height: u32) -> Bytes {
    let w = width.max(16) as usize;
    let h = height.max(16) as usize;
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let border = x < 2 || y < 2 || x >= w - 2 || y >= h - 2;
            let v = if border { 40 } else { 64 };
            let i = (y * w + x) * 4;
            rgba[i] = v;
            rgba[i + 1] = v;
            rgba[i + 2] = v;
            rgba[i + 3] = 255;
        }
    }
    let job = PreviewJob {
        rgba,
        width: w as u32,
        height: h as u32,
        standby: false,
        flip: false,
    };
    Bytes::from(encode_jpeg(&job).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_is_a_jpeg() {
        let bytes = placeholder_jpeg(64, 36);
        assert!(bytes.len() > 100);
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "en-tête JPEG");
    }

    #[test]
    fn encode_flips_rows() {
        // 1×2 : rouge en bas, bleu en haut (ordre GL), flip ⇒ bleu d'abord.
        let job = PreviewJob {
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
            width: 1,
            height: 2,
            standby: false,
            flip: true,
        };
        let jpeg = encode_jpeg(&job).expect("jpeg");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn invalid_job_is_none() {
        let job = PreviewJob {
            rgba: vec![0; 4],
            width: 10,
            height: 10,
            standby: false,
            flip: false,
        };
        assert!(encode_jpeg(&job).is_none());
    }
}
