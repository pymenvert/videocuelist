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
#[ignore = "exige ffmpeg : boucle avec in/out (respawn hors thread appelant)"]
fn loop_avec_in_out_reboucle_par_respawn() {
    // Cas du finding « respawn sur le thread de rendu » : in/out désactivent
    // -stream_loop, la boucle passe par le respawn du superviseur. On vérifie
    // que le player continue de produire des frames DANS le segment sur
    // plusieurs cycles, sans eof et sans blocage du thread appelant.
    let dir = tempfile::tempdir().expect("tempdir");
    let video = make_test_video(dir.path()).expect("génération testsrc");
    let pb = Playback { in_s: 0.2, out_s: Some(0.5), speed: 1.0, end: EndMode::Loop };
    let mut p = open_ffmpeg(&video, &pb).expect("open");
    p.play();

    let mut frames = 0usize;
    let mut t = 0.2f64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    // Segment de 0,3 s à 30 fps ≈ 9 frames/cycle : 40 frames ≈ 4+ cycles.
    while frames < 40 && std::time::Instant::now() < deadline {
        if let Some(f) = p.poll_frame(t) {
            assert!(
                f.pts_s >= 0.2 - 1e-6 && f.pts_s < 0.5 + 1.0 / 30.0,
                "pts média dans le segment in/out, obtenu {}",
                f.pts_s
            );
            frames += 1;
            t += 1.0 / 30.0;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!p.eof(), "une boucle in/out ne doit jamais passer eof");
    }
    assert!(frames >= 40, "la boucle in/out doit traverser plusieurs respawns ({frames} frames)");
    assert!(p.healthy());
}

#[test]
#[ignore = "exige ffmpeg : seek après eof (ring rouvert par le superviseur)"]
fn seek_apres_eof_reprend_la_lecture() {
    let dir = tempfile::tempdir().expect("tempdir");
    let video = make_test_video(dir.path()).expect("génération testsrc");
    let mut p = open_ffmpeg(&video, &playback(EndMode::Hold)).expect("open");
    p.play();

    // Lire jusqu'à l'eof (Hold).
    let mut t = 0.0f64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !p.eof() && std::time::Instant::now() < deadline {
        if p.poll_frame(t).is_some() {
            t += 1.0 / 30.0;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(2));
            t += 0.002;
        }
    }
    assert!(p.eof(), "Hold doit finir en eof");

    // Seek : le superviseur relance le process et rouvre le ring.
    p.seek(0.2);
    assert!(!p.eof(), "le seek doit sortir de l'eof");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut got = None;
    while got.is_none() && std::time::Instant::now() < deadline {
        got = p.poll_frame(0.2);
        if got.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    let f = got.expect("frame après seek post-eof");
    assert!((f.pts_s - 0.2).abs() < 0.2, "pts proche du seek, obtenu {}", f.pts_s);
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

#[test]
#[ignore = "exige ffmpeg + h264_mf (Windows) : encodeur de préview réel"]
fn preview_encoder_produit_des_access_units() {
    use conduite_engine::{h264_mf_available, PreviewEncoder};

    if !h264_mf_available() {
        eprintln!("SKIP : h264_mf indisponible dans ce ffmpeg");
        return;
    }
    let (w, h, fps) = (128u32, 72u32, 15u32);
    let mut enc = PreviewEncoder::new(w, h, fps).expect("lancement de l'encodeur");

    // ~2 s de frames RGBA (dégradé animé pour donner du grain à encoder).
    let mut frame = vec![0u8; (w * h * 4) as usize];
    let mut pushed = 0u32;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut aus = Vec::new();
    while pushed < 30 && std::time::Instant::now() < deadline {
        for (i, px) in frame.chunks_exact_mut(4).enumerate() {
            let v = ((i as u32).wrapping_mul(7).wrapping_add(pushed * 31) & 0xFF) as u8;
            px.copy_from_slice(&[v, 255 - v, (pushed * 8) as u8, 255]);
        }
        if enc.push_frame(&frame) {
            pushed += 1;
        }
        while let Some(au) = enc.poll_access_unit() {
            aus.push(au);
        }
        std::thread::sleep(std::time::Duration::from_millis(1000 / fps as u64));
    }
    // Laisse l'encodeur cracher ce qui reste en file.
    let settle = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < settle && aus.len() < 8 {
        if let Some(au) = enc.recv_access_unit(std::time::Duration::from_millis(200)) {
            aus.push(au);
        }
    }
    assert!(enc.is_alive(), "l'encodeur doit tourner tant qu'on le nourrit");
    assert!(
        aus.len() >= 8,
        "au moins ~8 access units pour 30 frames poussées, obtenu {}",
        aus.len()
    );
    assert!(aus[0].keyframe, "le flux commence par un keyframe (SPS/PPS/IDR)");
    assert!(
        aus[0].data.windows(4).any(|w| w == [0, 0, 0, 1]) || aus[0].data.windows(3).any(|w| w == [0, 0, 1]),
        "les access units gardent leurs start codes Annex-B"
    );
    // Drop : kill + wait + join — le test se termine sans process orphelin.
    drop(enc);
}
