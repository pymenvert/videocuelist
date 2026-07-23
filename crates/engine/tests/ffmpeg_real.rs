//! Tests d'intégration avec un vrai ffmpeg : on génère une vidéo de test
//! (testsrc, 1 s, 30 fps, 64x64) dans un tempdir.
//!
//! Le test de probe reste ACTIF (skip propre si ffmpeg absent) ; les tests
//! de lecture complets sont `#[ignore]` (lancer `cargo test -p conduite-engine
//! -- --ignored` sur une machine avec ffmpeg).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use conduite_core::{EndMode, Playback};
use conduite_engine::{ffmpeg_available, open_ffmpeg, probe, resolve_ffmpeg};

/// Génère `testsrc=duration=1:size=64x64:rate=30` dans `dir`. None si échec.
fn make_test_video(dir: &Path) -> Option<PathBuf> {
    let out = dir.join("testsrc.mp4");
    let status = Command::new(resolve_ffmpeg())
        .args([
            "-v", "error", "-y", "-f", "lavfi", "-i",
            "testsrc=duration=1:size=64x64:rate=30",
            "-pix_fmt", "yuv420p",
        ])
        .arg(&out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    (status.success() && out.is_file()).then_some(out)
}

fn playback(end: EndMode) -> Playback {
    Playback { in_s: 0.0, out_s: None, speed: 1.0, end }
}

/// Test ACTIF : probe d'une vidéo générée. Skip propre si ffmpeg manque.
#[test]
fn probe_video_generee() {
    if !ffmpeg_available() {
        eprintln!("SKIP : ffmpeg/ffprobe introuvables sur cette machine");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let video = make_test_video(dir.path()).expect("génération testsrc");
    let info = probe(&video).expect("probe");
    assert_eq!(info.width, 64);
    assert_eq!(info.height, 64);
    assert!((info.fps - 30.0).abs() < 0.5, "fps ≈ 30, obtenu {}", info.fps);
    assert!((info.duration_s - 1.0).abs() < 0.2, "durée ≈ 1 s, obtenu {}", info.duration_s);
}

#[test]
#[ignore = "exige ffmpeg : lecture réelle"]
fn lecture_produit_des_frames_puis_eof_hold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let video = make_test_video(dir.path()).expect("génération testsrc");
    let mut p = open_ffmpeg(&video, &playback(EndMode::Hold)).expect("open");
    p.play();

    let mut frames = 0usize;
    let mut t = 0.0f64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    // Horloge média simulée : on avance d'1/30 s à chaque frame obtenue.
    while !p.eof() && std::time::Instant::now() < deadline {
        if let Some(f) = p.poll_frame(t) {
            assert_eq!(f.width, 64);
            assert_eq!(f.height, 64);
            assert_eq!(f.data.len(), 64 * 64 * 4);
            frames += 1;
            t += 1.0 / 30.0;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(2));
            // Sans frame neuve, l'horloge avance quand même un peu (dup).
            t += 0.002;
        }
    }
    assert!(p.eof(), "Hold doit finir en eof");
    assert!(p.healthy());
    assert!(frames >= 25, "au moins ~1 s de frames, obtenu {frames}");
}

#[test]
#[ignore = "exige ffmpeg : boucle"]
fn loop_continue_au_dela_de_la_duree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let video = make_test_video(dir.path()).expect("génération testsrc");
    let mut p = open_ffmpeg(&video, &playback(EndMode::Loop)).expect("open");
    p.play();

    let mut frames = 0usize;
    let mut t = 0.0f64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    // On vise 1,5 s de média (au-delà de la durée d'1 s → la boucle a rejoué).
    while frames < 45 && std::time::Instant::now() < deadline {
        if let Some(_f) = p.poll_frame(t) {
            frames += 1;
            t += 1.0 / 30.0;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!p.eof(), "une boucle ne doit jamais passer eof");
    }
    assert!(frames >= 45, "la boucle doit dépasser la durée du média ({frames} frames)");
    assert!(p.healthy());
}

#[test]
#[ignore = "exige ffmpeg : seek"]
fn seek_relance_et_produit_des_frames() {
    let dir = tempfile::tempdir().expect("tempdir");
    let video = make_test_video(dir.path()).expect("génération testsrc");
    let mut p = open_ffmpeg(&video, &playback(EndMode::Hold)).expect("open");
    p.play();

    p.seek(0.5);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut got = None;
    while got.is_none() && std::time::Instant::now() < deadline {
        got = p.poll_frame(0.5);
        if got.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    let f = got.expect("frame après seek");
    assert!((f.pts_s - 0.5).abs() < 0.2, "pts proche du seek, obtenu {}", f.pts_s);
    assert!(p.healthy());
}

#[test]
#[ignore = "exige ffmpeg : préchargement et drop sans zombie"]
fn open_precharge_et_drop_tue_le_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let video = make_test_video(dir.path()).expect("génération testsrc");
    let started = std::time::Instant::now();
    {
        let _p = open_ffmpeg(&video, &playback(EndMode::Hold)).expect("open");
        // Préchargé, jamais joué : le drop doit tuer le process sans traîner.
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "open+drop ne doit pas bloquer"
    );
}

#[test]
#[ignore = "exige ffmpeg : segment in/out"]
fn segment_in_out_respecte_hold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let video = make_test_video(dir.path()).expect("génération testsrc");
    let pb = Playback { in_s: 0.25, out_s: Some(0.75), speed: 1.0, end: EndMode::Black };
    let mut p = open_ffmpeg(&video, &pb).expect("open");
    p.play();

    let mut t = 0.25f64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !p.eof() && std::time::Instant::now() < deadline {
        if p.poll_frame(t).is_some() {
            t += 1.0 / 30.0;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(2));
            t += 0.002;
        }
    }
    assert!(p.eof(), "Black doit finir en eof à la fin du segment");
    assert!(t < 1.4, "le segment 0.25..0.75 ne doit pas durer 1 s complète");
}
