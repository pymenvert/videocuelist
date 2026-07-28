//! # conduite-core
//!
//! Fondation de Conduite (régie vidéo de spectacle) : **tous les types du
//! modèle de show**, le vocabulaire de [`Command`]/[`EditOp`]/[`StateEvent`],
//! la persistance atomique avec backups rotatifs et chargement tolérant,
//! et la validation des chemins relatifs.
//!
//! Contrat normatif : `docs/INTERFACES.md`. Règle de propriété : tout type
//! sérialisé dans [`Show`] vit ici — les crates `modulation`/`control-*`
//! n'exportent que de la machinerie.

pub mod command;
pub mod demo;
pub mod error;
pub mod event;
pub mod model;
pub mod modulation;
pub mod patch;
pub mod paths;
pub mod persist;

pub use command::{Command, CommandTemplate, EditOp, Source};
pub use demo::demo_show;
pub use error::CoreError;
pub use event::{HealthSnapshot, RuntimeStatus, StateEvent, UpdateInfo};
pub use model::{
    AppMode, Content, Cue, CueNumber, CueTriggers, Curve, EndMode, FollowMode, MaterialId,
    MaterialRef, MediaId, MediaRef, ModId, OutputCfg, OutputId, ParamValue, PatternKind, Playback,
    Rect, Show, ShowSettings, Slice, SliceId, SliceState, Transition, TransitionKind,
    FORMAT_VERSION, UPDATE_URL_DEFAULT,
};
pub use modulation::{Freq, ModKind, ModRoute, ModRouteState, ModulatorCfg, RouteMode, Wave};
pub use patch::{DmxBits, KeyBinding, MidiBinding, OscOutCfg, PatchEntry, PatchTable};
pub use paths::validate_relative_path;
pub use persist::{
    acquire_instance_lock, load_show, load_show_with_media, save_show_atomic, write_atomic,
    InstanceLock, LoadWarning, BACKUP_DIR, BACKUP_KEEP, LOCK_FILE, SHOW_FILE,
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::str::FromStr;

    use super::*;

    // ---------------------------------------------------------------- CueNumber

    #[test]
    fn cue_number_display() {
        for (n, s) in [
            (1000u32, "1"),
            (1500, "1.5"),
            (12340, "12.34"),
            (12345, "12.345"),
            (0, "0"),
            (500, "0.5"),
            (2001, "2.001"),
            (2010, "2.01"),
            (2100, "2.1"),
        ] {
            assert_eq!(CueNumber(n).to_string(), s, "affichage de {n}");
        }
    }

    #[test]
    fn cue_number_parse() {
        for (s, n) in [
            ("1", 1000u32),
            ("1.5", 1500),
            ("12.34", 12340),
            ("12.345", 12345),
            ("0", 0),
            ("0.5", 500),
            ("2.001", 2001),
            (" 3.25 ", 3250),
            ("007", 7000),
        ] {
            assert_eq!(
                CueNumber::from_str(s).expect("parse"),
                CueNumber(n),
                "parse de {s:?}"
            );
        }
        for bad in ["", ".", "1.", ".5", "1.5.2", "-1", "+1", "1.2345", "abc", "1,5"] {
            assert!(CueNumber::from_str(bad).is_err(), "aurait dû refuser {bad:?}");
        }
    }

    #[test]
    fn cue_number_roundtrip_and_ord() {
        for n in [0u32, 1, 999, 1000, 1500, 12340, 12345, 4_000_000_000] {
            let s = CueNumber(n).to_string();
            assert_eq!(CueNumber::from_str(&s).expect("reparse"), CueNumber(n));
        }
        // Ordre total : insertion décimale sans renumérotation.
        let mut cues = [
            CueNumber::from_str("3").expect("n"),
            CueNumber::from_str("1").expect("n"),
            CueNumber::from_str("2.5").expect("n"),
            CueNumber::from_str("2").expect("n"),
            CueNumber::from_str("2.05").expect("n"),
        ];
        cues.sort();
        let sorted: Vec<String> = cues.iter().map(|c| c.to_string()).collect();
        assert_eq!(sorted, ["1", "2", "2.05", "2.5", "3"]);
    }

    #[test]
    fn cue_number_serde_is_millieme_integer() {
        let json = serde_json::to_string(&CueNumber(1500)).expect("ser");
        assert_eq!(json, "1500");
        let back: CueNumber = serde_json::from_str("1500").expect("de");
        assert_eq!(back, CueNumber(1500));
    }

    // ------------------------------------------------------------------ Courbes

    #[test]
    fn curves_hit_endpoints_and_midpoints() {
        let all = [
            Curve::Linear,
            Curve::EaseIn,
            Curve::EaseOut,
            Curve::EaseInOut,
            Curve::SCurve,
        ];
        for c in all {
            assert!(c.apply(0.0).abs() < 1e-6, "{c:?}(0) == 0");
            assert!((c.apply(1.0) - 1.0).abs() < 1e-6, "{c:?}(1) == 1");
            // Clamp hors bornes.
            assert!(c.apply(-1.0).abs() < 1e-6);
            assert!((c.apply(2.0) - 1.0).abs() < 1e-6);
            // Monotone croissante.
            let mut prev = 0.0f32;
            for i in 0..=100 {
                let v = c.apply(i as f32 / 100.0);
                assert!(v >= prev - 1e-6, "{c:?} non monotone à t={i}");
                prev = v;
            }
        }
        assert!((Curve::Linear.apply(0.5) - 0.5).abs() < 1e-6);
        assert!((Curve::EaseIn.apply(0.5) - 0.25).abs() < 1e-6);
        assert!((Curve::EaseOut.apply(0.5) - 0.75).abs() < 1e-6);
        assert!((Curve::EaseInOut.apply(0.5) - 0.5).abs() < 1e-6);
        assert!((Curve::SCurve.apply(0.5) - 0.5).abs() < 1e-6);
        // EaseIn démarre lentement, EaseOut vite.
        assert!(Curve::EaseIn.apply(0.25) < 0.25);
        assert!(Curve::EaseOut.apply(0.25) > 0.25);
    }

    // ------------------------------------------------------------- Serde du Show

    #[test]
    fn show_roundtrips_through_json() {
        let show = demo_show();
        let json = serde_json::to_string_pretty(&show).expect("ser");
        let back: Show = serde_json::from_str(&json).expect("de");
        assert_eq!(back, show);
    }

    #[test]
    fn show_with_missing_optional_fields_still_loads() {
        // Un show minimal écrit à la main un soir de première doit se charger.
        let json = r#"{"format_version":1,"name":"secours"}"#;
        let show: Show = serde_json::from_str(json).expect("de");
        assert_eq!(show.name, "secours");
        assert!(show.cues.is_empty());
        assert_eq!(show.settings.osc_in_port, 9000);
    }

    // ------------------------------------------------------- JSON des commandes

    /// Le format JSON est un contrat public (web UI, OSC bridge) : figé par test.
    #[test]
    fn command_json_format_is_stable() {
        let cases: Vec<(String, &str)> = vec![
            (
                serde_json::to_string(&Command::CueGo).expect("ser"),
                r#"{"cmd":"cue_go"}"#,
            ),
            (
                serde_json::to_string(&Command::CueBack).expect("ser"),
                r#"{"cmd":"cue_back"}"#,
            ),
            (
                serde_json::to_string(&Command::CueGoto {
                    cue: CueNumber(12500),
                })
                .expect("ser"),
                r#"{"cmd":"cue_goto","cue":12500}"#,
            ),
            (
                serde_json::to_string(&Command::CueStandby { cue: CueNumber(3000) }).expect("ser"),
                r#"{"cmd":"cue_standby","cue":3000}"#,
            ),
            (
                serde_json::to_string(&Command::CuePanic { fade_s: 2.0 }).expect("ser"),
                r#"{"cmd":"cue_panic","fade_s":2.0}"#,
            ),
            (
                serde_json::to_string(&Command::Dbo { fade_s: 0.5 }).expect("ser"),
                r#"{"cmd":"dbo","fade_s":0.5}"#,
            ),
            (
                serde_json::to_string(&Command::DboRelease).expect("ser"),
                r#"{"cmd":"dbo_release"}"#,
            ),
            (
                serde_json::to_string(&Command::TapTempo).expect("ser"),
                r#"{"cmd":"tap_tempo"}"#,
            ),
            (
                serde_json::to_string(&Command::BpmSet { bpm: 120.0 }).expect("ser"),
                r#"{"cmd":"bpm_set","bpm":120.0}"#,
            ),
            (
                serde_json::to_string(&Command::ParamSet {
                    addr: "slice/1/opacity".into(),
                    value: ParamValue::F(0.5),
                    source: Source::Osc,
                })
                .expect("ser"),
                r#"{"cmd":"param_set","addr":"slice/1/opacity","value":{"f":0.5},"source":"osc"}"#,
            ),
            (
                serde_json::to_string(&Command::ParamNudge {
                    addr: "master/intensity".into(),
                    delta: -0.1,
                    source: Source::Ui,
                })
                .expect("ser"),
                r#"{"cmd":"param_nudge","addr":"master/intensity","delta":-0.1,"source":"ui"}"#,
            ),
            (
                serde_json::to_string(&Command::ShowSaveAs { name: "gala".into() }).expect("ser"),
                r#"{"cmd":"show_save_as","name":"gala"}"#,
            ),
            (
                serde_json::to_string(&Command::ModeSet { mode: AppMode::Show }).expect("ser"),
                r#"{"cmd":"mode_set","mode":"show"}"#,
            ),
            (
                serde_json::to_string(&Command::Edit(EditOp::CornerSet {
                    slice: 1,
                    index: 2,
                    x: 0.95,
                    y: 1.0,
                }))
                .expect("ser"),
                r#"{"cmd":"edit","op":"corner_set","slice":1,"index":2,"x":0.95,"y":1.0}"#,
            ),
            (
                serde_json::to_string(&Command::Edit(EditOp::ShowRename {
                    name: "création".into(),
                }))
                .expect("ser"),
                r#"{"cmd":"edit","op":"show_rename","name":"création"}"#,
            ),
        ];
        for (got, want) in cases {
            assert_eq!(got, want);
            // Et le retour : le JSON figé redonne la même commande.
            let back: Command = serde_json::from_str(want).expect("de");
            assert_eq!(serde_json::to_string(&back).expect("reser"), want);
        }
    }

    /// Undo/redo et MIDI learn : format JSON figé (contrat public).
    #[test]
    fn undo_redo_and_midi_learn_json_stable() {
        let cases: Vec<(Command, &str)> = vec![
            (Command::Undo, r#"{"cmd":"undo"}"#),
            (Command::Redo, r#"{"cmd":"redo"}"#),
            (Command::MidiLearnStart, r#"{"cmd":"midi_learn_start"}"#),
            (Command::MidiLearnCancel, r#"{"cmd":"midi_learn_cancel"}"#),
        ];
        for (cmd, want) in cases {
            assert_eq!(serde_json::to_string(&cmd).expect("ser"), want);
            let back: Command = serde_json::from_str(want).expect("de");
            assert_eq!(back, cmd);
        }
    }

    /// Contrat P2 : diagnostic, clavier remappable, mires additionnelles,
    /// réglages de mise à jour — JSON figé.
    #[test]
    fn p2_contract_json_is_stable() {
        // Command::DiagnosticReport.
        let json = serde_json::to_string(&Command::DiagnosticReport).expect("ser");
        assert_eq!(json, r#"{"cmd":"diagnostic_report"}"#);
        let back: Command = serde_json::from_str(&json).expect("de");
        assert_eq!(back, Command::DiagnosticReport);

        // StateEvent::DiagnosticReady.
        let ev = StateEvent::DiagnosticReady {
            path: "logs/diagnostic-20260728-120000.zip".into(),
        };
        let json = serde_json::to_string(&ev).expect("ser");
        assert_eq!(
            json,
            r#"{"type":"diagnostic_ready","path":"logs/diagnostic-20260728-120000.zip"}"#
        );

        // EditOp KeyBindingAdd / KeyBindingRemove.
        let add = Command::Edit(EditOp::KeyBindingAdd {
            binding: KeyBinding {
                key: "Ctrl+3".into(),
                command: CommandTemplate::Goto { cue: CueNumber(3000) },
            },
        });
        let json = serde_json::to_string(&add).expect("ser");
        assert_eq!(
            json,
            r#"{"cmd":"edit","op":"key_binding_add","binding":{"key":"Ctrl+3","command":{"cmd":"goto","cue":3000}}}"#
        );
        let back: Command = serde_json::from_str(&json).expect("de");
        assert_eq!(back, add);
        let rm = Command::Edit(EditOp::KeyBindingRemove { index: 2 });
        let json = serde_json::to_string(&rm).expect("ser");
        assert_eq!(json, r#"{"cmd":"edit","op":"key_binding_remove","index":2}"#);

        // PatternKind : variantes additives figées.
        for (kind, want) in [
            (PatternKind::Grid4, r#""grid4""#),
            (PatternKind::Grid16, r#""grid16""#),
            (PatternKind::ColorBars, r#""color_bars""#),
        ] {
            assert_eq!(serde_json::to_string(&kind).expect("ser"), want);
            let back: PatternKind = serde_json::from_str(want).expect("de");
            assert_eq!(back, kind);
        }

        // UpdateInfo (runtime.update).
        let info = UpdateInfo {
            version: "0.2.0".into(),
            url: "https://github.com/pymenvert/videocuelist/releases".into(),
            notes: "Corrections".into(),
        };
        let json = serde_json::to_string(&info).expect("ser");
        assert_eq!(
            json,
            r#"{"version":"0.2.0","url":"https://github.com/pymenvert/videocuelist/releases","notes":"Corrections"}"#
        );
    }

    /// `runtime.update` est ABSENT de la trame quand `None` (compat) et
    /// présent quand une mise à jour est connue.
    #[test]
    fn runtime_update_field_is_optional() {
        let st = RuntimeStatus::default();
        let json = serde_json::to_string(&st).expect("ser");
        assert!(!json.contains("update"), "champ absent quand None : {json}");
        let back: RuntimeStatus = serde_json::from_str(&json).expect("de");
        assert_eq!(back, st);

        let st = RuntimeStatus {
            update: Some(UpdateInfo {
                version: "9.9.9".into(),
                url: "u".into(),
                notes: "n".into(),
            }),
            ..RuntimeStatus::default()
        };
        let json = serde_json::to_string(&st).expect("ser");
        assert!(json.contains(r#""update":{"version":"9.9.9""#));
        let back: RuntimeStatus = serde_json::from_str(&json).expect("de");
        assert_eq!(back, st);
    }

    /// Réglages de mise à jour : opt-in (défaut FAUX), URL par défaut sur le
    /// raw GitHub du dépôt ; `boost_priority` défaut faux ; un show antérieur
    /// (sans ces champs, sans `keys`) charge avec les défauts.
    #[test]
    fn update_and_priority_settings_default_off() {
        let s = ShowSettings::default();
        assert!(!s.update_check, "opt-in : défaut FAUX");
        assert_eq!(s.update_url, UPDATE_URL_DEFAULT);
        assert!(s.update_url.starts_with("https://raw.githubusercontent.com/"));
        assert!(!s.boost_priority);

        let json = r#"{"format_version":1,"name":"ancien","patch":{"artnet":[],"midi":[]}}"#;
        let show: Show = serde_json::from_str(json).expect("show antérieur");
        assert!(!show.settings.update_check);
        assert!(show.patch.keys.is_empty(), "keys absent ⇒ vide");
    }

    /// KeyBindingAdd/Remove mutent bien la table ; remove hors bornes = no-op.
    #[test]
    fn key_binding_ops_apply() {
        let mut show = Show::new("t");
        EditOp::KeyBindingAdd {
            binding: KeyBinding {
                key: "F5".into(),
                command: CommandTemplate::Go,
            },
        }
        .apply(&mut show);
        EditOp::KeyBindingAdd {
            binding: KeyBinding {
                key: "F6".into(),
                command: CommandTemplate::Back,
            },
        }
        .apply(&mut show);
        assert_eq!(show.patch.keys.len(), 2);
        EditOp::KeyBindingRemove { index: 99 }.apply(&mut show); // no-op
        assert_eq!(show.patch.keys.len(), 2);
        EditOp::KeyBindingRemove { index: 0 }.apply(&mut show);
        assert_eq!(show.patch.keys.len(), 1);
        assert_eq!(show.patch.keys[0].key, "F6");
    }

    #[test]
    fn command_source_artnet_spelling() {
        let json = serde_json::to_string(&Source::ArtNet).expect("ser");
        assert_eq!(json, r#""artnet""#);
    }

    #[test]
    fn unknown_command_is_rejected() {
        let res: Result<Command, _> = serde_json::from_str(r#"{"cmd":"self_destruct"}"#);
        assert!(res.is_err());
    }

    #[test]
    fn all_commands_roundtrip() {
        let show = demo_show();
        let cue = show.cues[0].clone();
        let cmds = vec![
            Command::CueGo,
            Command::CueBack,
            Command::CueGoto { cue: CueNumber(2500) },
            Command::CueStandby { cue: CueNumber(1000) },
            Command::CuePanic { fade_s: 3.0 },
            Command::Dbo { fade_s: 1.0 },
            Command::DboRelease,
            Command::TapTempo,
            Command::BpmSet { bpm: 98.5 },
            Command::ParamSet {
                addr: "slice/2/gain/r".into(),
                value: ParamValue::Color([1.0, 0.5, 0.0, 1.0]),
                source: Source::Midi,
            },
            Command::ParamNudge {
                addr: "bpm".into(),
                delta: 0.5,
                source: Source::Internal,
            },
            Command::ShowSave,
            Command::ShowSaveAs { name: "a".into() },
            Command::ShowLoad { name: "b".into() },
            Command::ShowNew,
            Command::MediaRescan,
            Command::ShowCollect,
            Command::ModeSet { mode: AppMode::Edit },
            Command::Edit(EditOp::CueAdd { cue }),
            Command::Edit(EditOp::SliceRemove { id: 3 }),
            Command::Edit(EditOp::PatchArtnetAdd {
                entry: PatchEntry {
                    universe: 0,
                    channel: 1,
                    bits: DmxBits::Sixteen,
                    addr: "master/intensity".into(),
                    min: 0.0,
                    max: 1.0,
                    smoothing_ms: 80.0,
                },
            }),
            Command::Edit(EditOp::PatchMidiAdd {
                binding: MidiBinding::Cc {
                    channel: 0,
                    cc: 7,
                    fourteen_bits: false,
                    addr: "master/intensity".into(),
                    min: 0.0,
                    max: 1.0,
                    pickup: true,
                },
            }),
            Command::Edit(EditOp::PatchMidiAdd {
                binding: MidiBinding::Note {
                    channel: 0,
                    note: 60,
                    command: CommandTemplate::Go,
                },
            }),
            Command::Edit(EditOp::PatchOscOutSet {
                cfg: Some(OscOutCfg {
                    host: "192.168.1.20".into(),
                    port: 9001,
                }),
            }),
            Command::Edit(EditOp::RouteAdd {
                route: ModRoute {
                    id: 9,
                    source: 1,
                    target_addr: "slice/1/opacity".into(),
                    depth: 0.5,
                    mode: RouteMode::Add,
                },
            }),
        ];
        for cmd in cmds {
            let json = serde_json::to_string(&cmd).expect("ser");
            let back: Command = serde_json::from_str(&json).expect("de");
            assert_eq!(back, cmd, "roundtrip raté pour {json}");
        }
    }

    #[test]
    fn command_template_json_and_instantiation() {
        let t = CommandTemplate::Goto { cue: CueNumber(5000) };
        let json = serde_json::to_string(&t).expect("ser");
        assert_eq!(json, r#"{"cmd":"goto","cue":5000}"#);
        assert_eq!(
            t.to_command(Source::Midi),
            Command::CueGoto { cue: CueNumber(5000) }
        );
        let t = CommandTemplate::ParamSet {
            addr: "slice/1/opacity".into(),
            value: ParamValue::F(1.0),
        };
        match t.to_command(Source::Midi) {
            Command::ParamSet { source, .. } => assert_eq!(source, Source::Midi),
            other => panic!("attendu ParamSet, obtenu {other:?}"),
        }
    }

    #[test]
    fn state_event_json_format() {
        let cases: Vec<(String, &str)> = vec![
            (
                serde_json::to_string(&StateEvent::CueChanged {
                    active: Some(CueNumber(2500)),
                })
                .expect("ser"),
                r#"{"type":"cue_changed","active":2500}"#,
            ),
            (
                serde_json::to_string(&StateEvent::CueChanged { active: None }).expect("ser"),
                r#"{"type":"cue_changed","active":null}"#,
            ),
            (
                serde_json::to_string(&StateEvent::TransitionProgress { progress: 0.5 })
                    .expect("ser"),
                r#"{"type":"transition_progress","progress":0.5}"#,
            ),
            (
                serde_json::to_string(&StateEvent::ModeChanged { mode: AppMode::Show })
                    .expect("ser"),
                r#"{"type":"mode_changed","mode":"show"}"#,
            ),
        ];
        for (got, want) in cases {
            assert_eq!(got, want);
        }
        // RuntimeStatus sérialisable et rechargeable.
        let status = RuntimeStatus {
            active: Some(CueNumber(1000)),
            mod_levels: vec![(1, 0.4)],
            ..RuntimeStatus::default()
        };
        let json = serde_json::to_string(&status).expect("ser");
        let back: RuntimeStatus = serde_json::from_str(&json).expect("de");
        assert_eq!(back, status);
    }

    // ----------------------------------------------------------- EditOp::apply

    #[test]
    fn edit_ops_apply_to_show() {
        let mut show = demo_show();
        let n_slices = show.slices.len();

        EditOp::CornerSet {
            slice: 1,
            index: 2,
            x: 0.9,
            y: 0.8,
        }
        .apply(&mut show);
        assert_eq!(show.slices[0].corners[2], [0.9, 0.8]);

        // Remove sur id inconnu : no-op, pas de panic.
        EditOp::SliceRemove { id: 999 }.apply(&mut show);
        assert_eq!(show.slices.len(), n_slices);

        EditOp::SliceRemove { id: 3 }.apply(&mut show);
        assert_eq!(show.slices.len(), n_slices - 1);

        // CueAdd garde la liste triée (insertion décimale).
        let mut inserted = show.cues[0].clone();
        inserted.number = CueNumber(1500);
        inserted.name = "Insérée".into();
        EditOp::CueAdd {
            cue: inserted.clone(),
        }
        .apply(&mut show);
        let numbers: Vec<u32> = show.cues.iter().map(|c| c.number.0).collect();
        let mut sorted = numbers.clone();
        sorted.sort_unstable();
        assert_eq!(numbers, sorted, "cues triées après insertion");
        assert!(show.cues.iter().any(|c| c.number == CueNumber(1500)));

        EditOp::ShowRename { name: "Gala".into() }.apply(&mut show);
        assert_eq!(show.name, "Gala");

        EditOp::RouteRemove { id: 1 }.apply(&mut show);
        assert!(show.routes.is_empty());

        // CueUpdateState remplace l'état du slice visé ou l'ajoute.
        let new_state = SliceState {
            slice: 1,
            content: Content::Pattern(PatternKind::Bars),
            playback: None,
            params: BTreeMap::new(),
        };
        EditOp::CueUpdateState {
            number: CueNumber(1000),
            state: new_state.clone(),
        }
        .apply(&mut show);
        let cue1 = show
            .cues
            .iter()
            .find(|c| c.number == CueNumber(1000))
            .expect("cue 1");
        assert_eq!(
            cue1.states.iter().find(|s| s.slice == 1),
            Some(&new_state)
        );
    }

    // ----------------------------------------------------------------- Chemins

    #[test]
    fn relative_paths_are_validated() {
        for ok in ["clips/a.mp4", "a.png", "sous/dossier/x.mov", "spé cial.mp4"] {
            assert!(validate_relative_path(ok).is_ok(), "aurait dû accepter {ok:?}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "\\reseau\\x.mp4",
            "C:\\evil.mp4",
            "C:/evil.mp4",
            "../secret.mp4",
            "sub/../../x.mp4",
            "nul\0.mp4",
            "file:///etc/passwd",
            "http://exemple.fr/x.mp4",
        ] {
            assert!(
                validate_relative_path(bad).is_err(),
                "aurait dû refuser {bad:?}"
            );
        }
    }

    // ------------------------------------------------------------- Persistance

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "conduite-core-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn save_and_load_roundtrip_atomically() {
        let dir = temp_dir("roundtrip");
        let show = demo_show();
        save_show_atomic(&dir, &show).expect("save");

        // Le fichier principal existe, aucun .tmp ne traîne.
        assert!(dir.join(SHOW_FILE).is_file());
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read_dir")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "fichier temporaire restant");

        // Un backup a été créé.
        let backups: Vec<_> = std::fs::read_dir(dir.join(BACKUP_DIR))
            .expect("backups")
            .flatten()
            .collect();
        assert_eq!(backups.len(), 1);

        let (loaded, warnings) = load_show(&dir).expect("load");
        assert_eq!(loaded, show);
        assert!(warnings.is_empty(), "warnings inattendus : {warnings:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backups_rotate_keeping_twenty() {
        let dir = temp_dir("rotation");
        let backups = dir.join(BACKUP_DIR);
        std::fs::create_dir_all(&backups).expect("mkdir");
        // 25 vieux backups pré-existants.
        for i in 0..25 {
            std::fs::write(
                backups.join(format!("show-20200101-{i:06}.json")),
                b"{}",
            )
            .expect("write");
        }
        // Un fichier étranger ne doit pas être compté ni supprimé.
        std::fs::write(backups.join("notes.txt"), b"garder").expect("write");

        save_show_atomic(&dir, &demo_show()).expect("save");

        let mut names: Vec<String> = std::fs::read_dir(&backups)
            .expect("read_dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("show-") && n.ends_with(".json"))
            .collect();
        names.sort();
        assert_eq!(names.len(), BACKUP_KEEP, "rotation à 20 : {names:?}");
        // Les plus anciens sont partis, le tout dernier est le backup frais.
        assert!(!names.contains(&"show-20200101-000000.json".to_string()));
        assert!(names.last().map(|n| n.as_str() > "show-2020").unwrap_or(false));
        assert!(backups.join("notes.txt").is_file(), "fichier étranger préservé");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_is_tolerant_to_missing_media() {
        let dir = temp_dir("tolerant");
        let mut show = demo_show();
        show.media.push(MediaRef {
            id: 1,
            path: "clips/present.mp4".into(),
            name: "Présent".into(),
            duration_s: Some(10.0),
            fps: Some(25.0),
            width: 1920,
            height: 1080,
            missing: false,
        });
        show.media.push(MediaRef {
            id: 2,
            path: "clips/fantome.mp4".into(),
            name: "Fantôme".into(),
            duration_s: None,
            fps: None,
            width: 0,
            height: 0,
            missing: false,
        });
        show.media.push(MediaRef {
            id: 3,
            path: "../evasion.mp4".into(),
            name: "Évasion".into(),
            duration_s: None,
            fps: None,
            width: 0,
            height: 0,
            missing: false,
        });
        // Seul le média 1 existe réellement.
        std::fs::create_dir_all(dir.join("media/clips")).expect("mkdir");
        std::fs::write(dir.join("media/clips/present.mp4"), b"fake").expect("write");

        save_show_atomic(&dir, &show).expect("save");
        let (loaded, warnings) = load_show(&dir).expect("le chargement ne doit JAMAIS échouer");

        let by_id = |id: u32| {
            loaded
                .media
                .iter()
                .find(|m| m.id == id)
                .expect("média présent dans le modèle")
        };
        assert!(!by_id(1).missing);
        assert!(by_id(2).missing, "média absent ⇒ missing");
        assert!(by_id(3).missing, "chemin invalide ⇒ missing");
        assert!(warnings
            .iter()
            .any(|w| matches!(w, LoadWarning::MissingMedia { id: 2, .. })));
        assert!(warnings
            .iter()
            .any(|w| matches!(w, LoadWarning::InvalidMediaPath { id: 3, .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_newer_format_version() {
        let dir = temp_dir("version");
        let mut show = demo_show();
        show.format_version = FORMAT_VERSION + 1;
        save_show_atomic(&dir, &show).expect("save");
        match load_show(&dir) {
            Err(CoreError::UnsupportedVersion(found, current)) => {
                assert_eq!(found, FORMAT_VERSION + 1);
                assert_eq!(current, FORMAT_VERSION);
            }
            other => panic!("attendu UnsupportedVersion, obtenu {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_replaces_existing_content() {
        let dir = temp_dir("atomic");
        let path = dir.join("show.json");
        write_atomic(&path, b"ancien").expect("write 1");
        write_atomic(&path, b"nouveau").expect("write 2");
        assert_eq!(std::fs::read(&path).expect("read"), b"nouveau");
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -------------------------------------------------------------------- Démo

    #[test]
    fn demo_show_is_realistic() {
        let show = demo_show();
        assert_eq!(show.format_version, FORMAT_VERSION);
        assert_eq!(show.outputs.len(), 2);
        assert_eq!(show.outputs.iter().filter(|o| o.enabled).count(), 1);
        assert_eq!(show.slices.len(), 3);
        assert!(show.cues.len() >= 4);
        // Cues triées par numéro (dont l'insertion décimale 2.5).
        let numbers: Vec<u32> = show.cues.iter().map(|c| c.number.0).collect();
        let mut sorted = numbers.clone();
        sorted.sort_unstable();
        assert_eq!(numbers, sorted);
        assert!(show.cues.iter().any(|c| c.number == CueNumber(2500)));
        // Un LFO routé sur une opacité, activé par au moins une cue.
        assert_eq!(show.modulators.len(), 1);
        assert!(matches!(show.modulators[0].kind, ModKind::Lfo { .. }));
        assert_eq!(show.routes.len(), 1);
        assert!(show.routes[0].target_addr.ends_with("/opacity"));
        assert!(show.cues.iter().any(|c| c
            .mod_routes
            .iter()
            .any(|r| r.route_id == show.routes[0].id && r.enabled)));
        // Tous les slices référencent une sortie existante.
        for s in &show.slices {
            assert!(show.outputs.iter().any(|o| o.id == s.output));
        }
        // Tous les états de cue référencent un slice existant.
        for c in &show.cues {
            for st in &c.states {
                assert!(show.slices.iter().any(|s| s.id == st.slice));
            }
        }
    }
}
