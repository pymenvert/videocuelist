//! Fixture de non-régression « show maximal » — le contrat de FICHIER d'un
//! produit commercial (AUDIT 2026-07-24, P2 n°12).
//!
//! Un show exerçant CHAQUE variante d'enum sérialisée dans [`Show`] (tous
//! les `PatternKind`, `EndMode`, `TransitionKind`, `Curve`, `FollowMode`,
//! `ModKind`, `Wave`, `Freq`, `RouteMode`, `DmxBits`, `MidiBinding`,
//! `Content`, `ParamValue`, `CommandTemplate`…) et chaque `Option` remplie.
//!
//! Deux garanties :
//! 1. **Round-trip strict** : sérialiser puis relire rend un show ÉGAL.
//! 2. **Compat ascendante** : le JSON figé dans `tests/fixtures/` (écrit par
//!    la version courante du format, v1) doit se charger tel quel dans
//!    toutes les versions futures. Si ce test casse, c'est qu'un changement
//!    de schéma a rompu la lecture des shows existants des clients —
//!    interdit sans migration (`persist::load_show`).
//!
//! Pour régénérer la fixture après un ajout ADDITIF (nouvelle variante,
//! nouveau champ avec défaut serde) : compléter `show_maximal()`, puis
//! `cargo test -p conduite-core --test show_maximal -- --ignored regen`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use conduite_core::*;

/// Chemin de la fixture figée (lue depuis le dossier de la crate).
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("show-maximal.json")
}

/// Un `SliceState` complet : contenu + lecture + un paramètre de chaque type.
fn state(slice: SliceId, content: Content, end: EndMode) -> SliceState {
    let mut params = BTreeMap::new();
    params.insert(format!("slice/{slice}/opacity"), ParamValue::F(0.75));
    params.insert(format!("slice/{slice}/blendmode"), ParamValue::I(2));
    params.insert(format!("slice/{slice}/enabled"), ParamValue::B(true));
    params.insert(
        format!("slice/{slice}/tint"),
        ParamValue::Color([1.0, 0.5, 0.25, 1.0]),
    );
    params.insert(format!("slice/{slice}/pos"), ParamValue::P2([0.25, 0.75]));
    params.insert(
        "material/1/mode".to_string(),
        ParamValue::S("miroir".to_string()),
    );
    SliceState {
        slice,
        content,
        playback: Some(Playback {
            in_s: 1.5,
            out_s: Some(42.25),
            speed: 1.5,
            end,
        }),
        params,
    }
}

/// Construit le show maximal : chaque variante d'enum, chaque Option remplie.
fn show_maximal() -> Show {
    let mut show = Show::new("show-maximal");

    show.outputs = vec![OutputCfg {
        id: 1,
        name: "Projecteur jardin".to_string(),
        monitor_index: Some(2),
        width: 1920,
        height: 1080,
        fullscreen: true,
        enabled: true,
    }];

    show.slices = vec![Slice {
        id: 1,
        name: "Fond".to_string(),
        output: 1,
        corners: [[0.0, 0.0], [1.0, 0.125], [0.875, 1.0], [0.0, 1.0]],
        src: Rect {
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        },
        z: -3,
        enabled: true,
    }];

    show.media = vec![MediaRef {
        id: 1,
        path: "boucle.mp4".to_string(),
        name: "Boucle".to_string(),
        duration_s: Some(12.5),
        fps: Some(25.0),
        width: 1920,
        height: 1080,
        missing: true,
    }];

    show.materials = vec![MaterialRef {
        id: 1,
        path: "plasma.fs".to_string(),
        name: "Plasma".to_string(),
    }];

    // Cues : chaque TransitionKind, Curve, FollowMode, Content, EndMode et
    // PatternKind est posé quelque part. Toutes les Options remplies.
    let transitions = [
        (TransitionKind::Cut, Curve::Linear),
        (TransitionKind::Crossfade, Curve::EaseIn),
        (TransitionKind::ThroughBlack, Curve::EaseOut),
        (TransitionKind::Crossfade, Curve::EaseInOut),
        (TransitionKind::Cut, Curve::SCurve),
    ];
    let follows = [
        FollowMode::Manual,
        FollowMode::AfterMedia,
        FollowMode::Wait(2.5),
        FollowMode::Manual,
        FollowMode::AfterMedia,
    ];
    let contents = [
        Content::None,
        Content::Media(1),
        Content::Material(1),
        Content::Color([0.125, 0.25, 0.5, 1.0]),
        Content::Pattern(PatternKind::Grid),
    ];
    let ends = [
        EndMode::Loop,
        EndMode::PingPong,
        EndMode::Hold,
        EndMode::Black,
        EndMode::FollowNext,
    ];
    let patterns = [
        PatternKind::Grid,
        PatternKind::Checker,
        PatternKind::Ident,
        PatternKind::Bars,
        PatternKind::Grid4,
        PatternKind::Grid16,
        PatternKind::ColorBars,
    ];
    for i in 0..5u32 {
        let idx = i as usize;
        show.cues.push(Cue {
            number: CueNumber::new(i + 1, 500),
            name: format!("Cue {}", i + 1),
            color: Some("#3fa9f5".to_string()),
            notes: "Attendre le noir complet avant GO.".to_string(),
            armed: i % 2 == 0,
            transition: Transition {
                kind: transitions[idx].0,
                dur_s: 1.5,
                curve: transitions[idx].1,
            },
            follow: follows[idx],
            goto_after: Some(CueNumber::new(1, 0)),
            states: vec![state(1, contents[idx].clone(), ends[idx])],
            mod_routes: vec![ModRouteState {
                route_id: 1,
                depth: 0.5,
                enabled: i % 2 == 0,
            }],
            triggers: CueTriggers {
                midi_note: Some((0, 60)),
                osc: Some(format!("/conduite/cue/{}", i + 1)),
            },
        });
    }
    // Une cue « mires » qui pose TOUTES les variantes de PatternKind.
    show.cues.push(Cue {
        number: CueNumber::new(9, 999),
        name: "Mires".to_string(),
        color: Some("#ff8800".to_string()),
        notes: "Calage projecteurs.".to_string(),
        armed: true,
        transition: Transition {
            kind: TransitionKind::Cut,
            dur_s: 0.0,
            curve: Curve::Linear,
        },
        follow: FollowMode::Manual,
        goto_after: None,
        states: patterns
            .iter()
            .map(|p| SliceState {
                slice: 1,
                content: Content::Pattern(*p),
                playback: None,
                params: BTreeMap::new(),
            })
            .collect(),
        mod_routes: Vec::new(),
        triggers: CueTriggers::default(),
    });

    // Modulateurs : chaque ModKind, chaque Wave, chaque Freq.
    let waves = [
        Wave::Sine,
        Wave::Tri,
        Wave::Square { pw: 0.25 },
        Wave::Saw,
        Wave::RandomSh,
        Wave::Drift,
    ];
    for (i, wave) in waves.iter().enumerate() {
        let freq = if i % 2 == 0 {
            Freq::Hz(1.5)
        } else {
            Freq::BpmSync { mult: 0.25 }
        };
        show.modulators.push(ModulatorCfg {
            id: i as u32 + 1,
            name: format!("LFO {}", i + 1),
            kind: ModKind::Lfo {
                wave: *wave,
                freq,
                phase: 0.125,
            },
        });
    }
    show.modulators.push(ModulatorCfg {
        id: 10,
        name: "Basses".to_string(),
        kind: ModKind::AudioBand {
            low_hz: 20.0,
            high_hz: 200.0,
            gain: 1.5,
            floor: 0.125,
            attack_ms: 10.0,
            release_ms: 80.0,
            normalize: false,
        },
    });
    show.modulators.push(ModulatorCfg {
        id: 11,
        name: "Timecode (réservé v2)".to_string(),
        kind: ModKind::TimecodeChase {},
    });

    // Routes : chaque RouteMode.
    let modes = [RouteMode::Add, RouteMode::Mul, RouteMode::Replace];
    for (i, mode) in modes.iter().enumerate() {
        show.routes.push(ModRoute {
            id: i as u32 + 1,
            source: 1,
            target_addr: "slice/1/opacity".to_string(),
            depth: 0.5,
            mode: *mode,
        });
    }

    // Patch : chaque DmxBits, chaque variante de MidiBinding, OSC sortant,
    // et un KeyBinding par variante de CommandTemplate.
    show.patch.artnet = vec![
        PatchEntry {
            universe: 0,
            channel: 1,
            bits: DmxBits::Eight,
            addr: "master/intensity".to_string(),
            min: 0.0,
            max: 1.0,
            smoothing_ms: 50.0,
        },
        PatchEntry {
            universe: 3,
            channel: 511,
            bits: DmxBits::Sixteen,
            addr: "slice/1/opacity".to_string(),
            min: 0.25,
            max: 0.75,
            smoothing_ms: 0.0,
        },
    ];
    show.patch.midi = vec![
        MidiBinding::Note {
            channel: 0,
            note: 60,
            command: CommandTemplate::Go,
        },
        MidiBinding::Cc {
            channel: 15,
            cc: 7,
            fourteen_bits: true,
            addr: "master/intensity".to_string(),
            min: 0.0,
            max: 1.0,
            pickup: true,
        },
    ];
    show.patch.osc_out = Some(OscOutCfg {
        host: "192.168.1.50".to_string(),
        port: 9001,
    });
    let templates = [
        CommandTemplate::Go,
        CommandTemplate::Back,
        CommandTemplate::Goto {
            cue: CueNumber::new(2, 500),
        },
        CommandTemplate::Standby {
            cue: CueNumber::new(3, 0),
        },
        CommandTemplate::Panic { fade_s: 3.0 },
        CommandTemplate::Dbo { fade_s: 0.5 },
        CommandTemplate::DboRelease,
        CommandTemplate::TapTempo,
        CommandTemplate::BpmSet { bpm: 120.5 },
        CommandTemplate::ParamSet {
            addr: "master/intensity".to_string(),
            value: ParamValue::F(0.5),
        },
        CommandTemplate::ModeSet {
            mode: AppMode::Show,
        },
        CommandTemplate::ModeSet { mode: AppMode::Edit },
    ];
    for (i, command) in templates.iter().enumerate() {
        show.patch.keys.push(KeyBinding {
            key: format!("Ctrl+F{}", i + 1),
            command: command.clone(),
        });
    }

    // Réglages : chaque Option remplie, gabarits de cue compris.
    show.settings = ShowSettings {
        osc_in_port: 9100,
        osc_out_port: 9101,
        artnet_enabled: true,
        artnet_universes: vec![0, 3],
        language: "fr".to_string(),
        mjpeg_fps: 12,
        mjpeg_width: 640,
        mjpeg_height: 360,
        autosave_debounce_s: 2.5,
        autosave_interval_s: 90.0,
        audio_input: Some("default".to_string()),
        min_go_interval_ms: 400,
        cue_defaults: CueDefaults {
            transition: Some(Transition {
                kind: TransitionKind::Crossfade,
                dur_s: 2.0,
                curve: Curve::SCurve,
            }),
            follow: Some(FollowMode::Wait(1.5)),
            color: Some("#3ff59a".to_string()),
        },
        update_check: true,
        update_url: UPDATE_URL_DEFAULT.to_string(),
        boost_priority: true,
    };

    show
}

/// Jetons snake_case qui DOIVENT apparaître dans le JSON du show maximal —
/// un par variante d'enum sérialisée. Si une variante est ajoutée au modèle
/// sans être posée ici et dans `show_maximal()`, ce test le rappelle.
const EXPECTED_TOKENS: &[&str] = &[
    // PatternKind
    "\"grid\"",
    "\"checker\"",
    "\"ident\"",
    "\"bars\"",
    "\"grid4\"",
    "\"grid16\"",
    "\"color_bars\"",
    // Content
    "\"none\"",
    "\"media\"",
    "\"material\"",
    "\"pattern\"",
    // EndMode
    "\"loop\"",
    "\"ping_pong\"",
    "\"hold\"",
    "\"black\"",
    "\"follow_next\"",
    // TransitionKind
    "\"cut\"",
    "\"crossfade\"",
    "\"through_black\"",
    // Curve
    "\"linear\"",
    "\"ease_in\"",
    "\"ease_out\"",
    "\"ease_in_out\"",
    "\"s_curve\"",
    // FollowMode
    "\"manual\"",
    "\"after_media\"",
    "\"wait\"",
    // Wave
    "\"sine\"",
    "\"tri\"",
    "\"square\"",
    "\"saw\"",
    "\"random_sh\"",
    "\"drift\"",
    // Freq
    "\"hz\"",
    "\"bpm_sync\"",
    // ModKind
    "\"lfo\"",
    "\"audio_band\"",
    "\"timecode_chase\"",
    // RouteMode
    "\"add\"",
    "\"mul\"",
    "\"replace\"",
    // DmxBits
    "\"eight\"",
    "\"sixteen\"",
    // MidiBinding
    "\"note\"",
    "\"cc\"",
    // CommandTemplate (tag "cmd")
    "\"go\"",
    "\"back\"",
    "\"goto\"",
    "\"standby\"",
    "\"panic\"",
    "\"dbo\"",
    "\"dbo_release\"",
    "\"tap_tempo\"",
    "\"bpm_set\"",
    "\"param_set\"",
    "\"mode_set\"",
    // AppMode
    "\"edit\"",
    "\"show\"",
    // ParamValue
    "\"f\"",
    "\"i\"",
    "\"b\"",
    "\"color\"",
    "\"p2\"",
    "\"s\"",
];

/// Round-trip strict : sérialiser le show maximal puis le relire rend un
/// show STRICTEMENT égal (aucune perte, aucun défaut appliqué en douce).
#[test]
fn maximal_show_roundtrips_exactly() {
    let show = show_maximal();
    let json = serde_json::to_string_pretty(&show).expect("sérialisation");
    let back: Show = serde_json::from_str(&json).expect("désérialisation");
    assert_eq!(show, back, "round-trip serde non identitaire");
}

/// Le show maximal exerce bien TOUTES les variantes attendues (garde-fou :
/// une variante ajoutée au modèle doit être posée dans la fixture).
#[test]
fn maximal_show_covers_every_variant() {
    let json = serde_json::to_string(&show_maximal()).expect("sérialisation");
    let missing: Vec<&str> = EXPECTED_TOKENS
        .iter()
        .filter(|t| !json.contains(**t))
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "variantes absentes du show maximal : {missing:?}"
    );
}

/// COMPAT ASCENDANTE : la fixture JSON figée (format v1, committée) doit se
/// désérialiser telle quelle et rester STRICTEMENT égale au show maximal.
/// Si ce test casse, le format de fichier a divergé : les shows des clients
/// ne se rechargent plus — migration obligatoire dans `persist::load_show`.
#[test]
fn frozen_fixture_still_loads_identically() {
    let path = fixture_path();
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture illisible ({}) : {e}", path.display()));
    let loaded: Show = serde_json::from_str(&json).expect("fixture désérialisable");
    assert_eq!(loaded.format_version, FORMAT_VERSION);
    assert_eq!(
        loaded,
        show_maximal(),
        "la fixture figée ne correspond plus au show maximal"
    );
}

/// La fixture passe aussi par le chemin de chargement PRODUIT
/// (`load_show_with_media`) : tolérant, médias absents = warning, jamais un
/// refus.
#[test]
fn frozen_fixture_loads_through_persist() {
    let dir = std::env::temp_dir().join(format!(
        "conduite-show-maximal-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::copy(fixture_path(), dir.join(SHOW_FILE)).expect("copie fixture");
    let media_dir = dir.join("media"); // vide : le média est manquant
    std::fs::create_dir_all(&media_dir).expect("mkdir media");
    let (show, warnings) =
        load_show_with_media(&dir, &media_dir).expect("chargement tolérant");
    assert_eq!(show.name, "show-maximal");
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, LoadWarning::MissingMedia { id: 1, .. })),
        "média absent = warning attendu, obtenu {warnings:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Régénération de la fixture (ignoré par défaut) :
/// `cargo test -p conduite-core --test show_maximal -- --ignored regen`
#[test]
#[ignore = "régénère tests/fixtures/show-maximal.json"]
fn regen() {
    let json = serde_json::to_string_pretty(&show_maximal()).expect("sérialisation");
    let path = fixture_path();
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir fixtures");
    std::fs::write(&path, json.as_bytes()).expect("écriture fixture");
    println!("fixture régénérée : {}", path.display());
}
