//! Sondage des médias via `ffprobe` (JSON) + résolution des exécutables.
//!
//! Ordre de résolution : `./bin/ffmpeg(.exe)` à côté de l'exécutable
//! (dossier portable), puis `./bin/` du répertoire courant, puis le PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::MediaInfo;

/// Résout un outil : `bin/` portable (exe puis cwd), sinon PATH.
fn resolve_tool(name: &str) -> PathBuf {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("bin").join(&file);
            if cand.is_file() {
                return cand;
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let cand = cwd.join("bin").join(&file);
        if cand.is_file() {
            return cand;
        }
    }
    PathBuf::from(name)
}

/// Chemin de `ffmpeg` (portable `./bin/` puis PATH).
pub fn resolve_ffmpeg() -> PathBuf {
    resolve_tool("ffmpeg")
}

/// Chemin de `ffprobe` (portable `./bin/` puis PATH).
pub fn resolve_ffprobe() -> PathBuf {
    resolve_tool("ffprobe")
}

/// Sous Windows : pas de fenêtre console pour les process enfants.
pub(crate) fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

// ---- Parsing de la sortie JSON de ffprobe (pur, testé) ----

#[derive(Debug, Deserialize)]
struct ProbeOut {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

/// Parse un rationnel ffprobe (`"30000/1001"`, `"30/1"`, `"0/0"` → None).
fn parse_rate(s: &str) -> Option<f64> {
    let (num, den) = match s.split_once('/') {
        Some((n, d)) => (n.trim().parse::<f64>().ok()?, d.trim().parse::<f64>().ok()?),
        None => (s.trim().parse::<f64>().ok()?, 1.0),
    };
    if den <= 0.0 || !num.is_finite() || num <= 0.0 {
        return None;
    }
    Some(num / den)
}

/// Extrait un [`MediaInfo`] du JSON ffprobe. Pur (raccourci de test).
#[cfg(test)]
pub(crate) fn parse_probe_json(json: &str) -> anyhow::Result<MediaInfo> {
    parse_probe_json_full(json).map(|(info, _)| info)
}

/// Comme [`parse_probe_json`], avec le nom du codec vidéo (minuscules,
/// ex. `h264`, `hevc`, `hap`) — sert à décider du décodage matériel.
pub(crate) fn parse_probe_json_full(json: &str) -> anyhow::Result<(MediaInfo, Option<String>)> {
    let out: ProbeOut = serde_json::from_str(json).context("JSON ffprobe illisible")?;
    let video = out
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"))
        .context("aucun flux vidéo dans le média")?;
    let width = video.width.unwrap_or(0);
    let height = video.height.unwrap_or(0);
    if width == 0 || height == 0 {
        bail!("dimensions vidéo invalides ({width}x{height})");
    }
    let fps = video
        .avg_frame_rate
        .as_deref()
        .and_then(parse_rate)
        .or_else(|| video.r_frame_rate.as_deref().and_then(parse_rate))
        .unwrap_or_else(|| {
            tracing::warn!(target: "engine::probe", "fps introuvable, repli sur 30");
            30.0
        });
    let duration_s = video
        .duration
        .as_deref()
        .and_then(|d| d.trim().parse::<f64>().ok())
        .or_else(|| {
            out.format
                .as_ref()
                .and_then(|f| f.duration.as_deref())
                .and_then(|d| d.trim().parse::<f64>().ok())
        })
        .filter(|d| d.is_finite() && *d >= 0.0)
        .unwrap_or(0.0); // 0.0 = inconnue (image fixe, flux…)
    let codec = video.codec_name.as_deref().map(|c| c.trim().to_ascii_lowercase());
    Ok((MediaInfo { duration_s, fps, width, height }, codec))
}

/// Sonde un média avec `ffprobe -v quiet -print_format json -show_streams -show_format`.
pub fn probe(path: &Path) -> anyhow::Result<MediaInfo> {
    probe_with_codec(path).map(|(info, _)| info)
}

/// Comme [`probe`], avec le nom du codec vidéo (minuscules) s'il est connu.
pub(crate) fn probe_with_codec(path: &Path) -> anyhow::Result<(MediaInfo, Option<String>)> {
    let ffprobe = resolve_ffprobe();
    let mut cmd = Command::new(&ffprobe);
    cmd.arg("-v")
        .arg("quiet")
        .arg("-print_format")
        .arg("json")
        .arg("-show_streams")
        .arg("-show_format")
        .arg(path);
    no_window(&mut cmd);
    let output = cmd
        .output()
        .with_context(|| format!("lancement de ffprobe impossible ({})", ffprobe.display()))?;
    if !output.status.success() {
        bail!("ffprobe a échoué sur {} (code {:?})", path.display(), output.status.code());
    }
    let json = String::from_utf8_lossy(&output.stdout);
    parse_probe_json_full(&json).with_context(|| format!("sondage de {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "streams": [
            { "codec_type": "audio", "sample_rate": "48000" },
            { "codec_type": "video", "width": 1920, "height": 1080,
              "avg_frame_rate": "30000/1001", "r_frame_rate": "30000/1001",
              "duration": "12.500000" }
        ],
        "format": { "duration": "12.545000" }
    }"#;

    #[test]
    fn parse_nominal() {
        let info = parse_probe_json(SAMPLE).unwrap();
        assert_eq!(info.width, 1920);
        assert_eq!(info.height, 1080);
        assert!((info.fps - 29.97).abs() < 0.01);
        assert!((info.duration_s - 12.5).abs() < 1e-6);
    }

    #[test]
    fn duree_depuis_format_si_absente_du_flux() {
        let json = r#"{
            "streams": [ { "codec_type": "video", "width": 64, "height": 64,
                           "avg_frame_rate": "30/1" } ],
            "format": { "duration": "1.000000" }
        }"#;
        let info = parse_probe_json(json).unwrap();
        assert!((info.duration_s - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fps_replie_sur_r_frame_rate_puis_30() {
        let json = r#"{
            "streams": [ { "codec_type": "video", "width": 64, "height": 64,
                           "avg_frame_rate": "0/0", "r_frame_rate": "25/1" } ]
        }"#;
        let info = parse_probe_json(json).unwrap();
        assert!((info.fps - 25.0).abs() < 1e-9);
        assert_eq!(info.duration_s, 0.0);

        let json = r#"{
            "streams": [ { "codec_type": "video", "width": 64, "height": 64,
                           "avg_frame_rate": "0/0", "r_frame_rate": "0/0" } ]
        }"#;
        let info = parse_probe_json(json).unwrap();
        assert!((info.fps - 30.0).abs() < 1e-9);
    }

    #[test]
    fn codec_name_extrait_et_normalise() {
        let json = r#"{
            "streams": [ { "codec_type": "video", "codec_name": "H264",
                           "width": 64, "height": 64, "avg_frame_rate": "30/1" } ]
        }"#;
        let (_, codec) = parse_probe_json_full(json).unwrap();
        assert_eq!(codec.as_deref(), Some("h264"));

        // Sans codec_name : None, sans erreur.
        let json = r#"{
            "streams": [ { "codec_type": "video", "width": 64, "height": 64,
                           "avg_frame_rate": "30/1" } ]
        }"#;
        let (_, codec) = parse_probe_json_full(json).unwrap();
        assert!(codec.is_none());
    }

    #[test]
    fn erreur_sans_flux_video() {
        let json = r#"{ "streams": [ { "codec_type": "audio" } ] }"#;
        assert!(parse_probe_json(json).is_err());
    }

    #[test]
    fn erreur_dimensions_nulles() {
        let json = r#"{ "streams": [ { "codec_type": "video", "width": 0, "height": 0 } ] }"#;
        assert!(parse_probe_json(json).is_err());
    }

    #[test]
    fn erreur_json_invalide() {
        assert!(parse_probe_json("pas du json").is_err());
    }

    #[test]
    fn parse_rate_variantes() {
        assert_eq!(parse_rate("30/1"), Some(30.0));
        assert_eq!(parse_rate("0/0"), None);
        assert_eq!(parse_rate("25"), Some(25.0));
        assert_eq!(parse_rate("abc"), None);
        assert_eq!(parse_rate("30/0"), None);
    }

    #[test]
    fn resolve_retourne_un_chemin_non_vide() {
        assert!(!resolve_ffmpeg().as_os_str().is_empty());
        assert!(!resolve_ffprobe().as_os_str().is_empty());
    }
}
