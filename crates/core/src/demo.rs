//! Show de démonstration — chargé au premier lancement pour que l'outil
//! soit explorable immédiatement (mires, couleurs, un LFO branché).

use std::collections::BTreeMap;

use crate::model::{
    Content, Cue, CueNumber, CueTriggers, Curve, FollowMode, OutputCfg, ParamValue, PatternKind,
    Rect, Show, ShowSettings, Slice, SliceState, Transition, TransitionKind,
};
use crate::modulation::{Freq, ModKind, ModRoute, ModRouteState, ModulatorCfg, RouteMode, Wave};
use crate::patch::PatchTable;

/// Un slice plein cadre par défaut.
fn slice(id: u32, name: &str, output: u32, corners: [[f32; 2]; 4], z: i32) -> Slice {
    Slice {
        id,
        name: name.to_string(),
        output,
        corners,
        src: Rect::full(),
        z,
        enabled: true,
    }
}

/// État de slice sans lecture média (pattern/couleur), avec opacité scénarisée.
fn state(slice_id: u32, content: Content, opacity: f32) -> SliceState {
    let mut params = BTreeMap::new();
    params.insert(
        format!("slice/{slice_id}/opacity"),
        ParamValue::F(opacity),
    );
    SliceState {
        slice: slice_id,
        content,
        playback: None,
        params,
    }
}

fn cue(
    number: &str,
    name: &str,
    color: Option<&str>,
    transition: Transition,
    states: Vec<SliceState>,
) -> Cue {
    Cue {
        // Littéraux du module : parse ne peut pas échouer (couvert par les tests).
        number: number.parse().unwrap_or(CueNumber(0)),
        name: name.to_string(),
        color: color.map(str::to_string),
        notes: String::new(),
        armed: true,
        transition,
        follow: FollowMode::Manual,
        goto_after: None,
        states,
        mod_routes: Vec::new(),
        triggers: CueTriggers::default(),
    }
}

/// Show de démonstration réaliste : 2 sorties (1 seule active), 3 slices,
/// cues mires/couleurs, 1 LFO routé sur une opacité.
pub fn demo_show() -> Show {
    let outputs = vec![
        OutputCfg {
            id: 1,
            name: "Principal".to_string(),
            monitor_index: Some(1),
            width: 1280,
            height: 720,
            // Fenêtré au premier lancement : on ne confisque pas l'écran de
            // l'utilisateur avant qu'il ait configuré ses sorties.
            fullscreen: false,
            enabled: true,
        },
        OutputCfg {
            id: 2,
            name: "Lointain (réserve)".to_string(),
            monitor_index: None,
            width: 1920,
            height: 1080,
            fullscreen: true,
            enabled: false,
        },
    ];

    let slices = vec![
        slice(1, "Fond", 1, Slice::default_corners(), 0),
        // Deux panneaux latéraux légèrement trapézoïdaux (décor incliné).
        slice(
            2,
            "Panneau jardin",
            1,
            [[0.02, 0.10], [0.45, 0.15], [0.45, 0.90], [0.02, 0.95]],
            1,
        ),
        slice(
            3,
            "Panneau cour",
            1,
            [[0.55, 0.15], [0.98, 0.10], [0.98, 0.95], [0.55, 0.90]],
            1,
        ),
    ];

    let modulators = vec![ModulatorCfg {
        id: 1,
        name: "LFO respiration".to_string(),
        kind: ModKind::Lfo {
            wave: Wave::Sine,
            freq: Freq::Hz(0.2),
            phase: 0.0,
        },
    }];

    let routes = vec![ModRoute {
        id: 1,
        source: 1,
        target_addr: "slice/1/opacity".to_string(),
        depth: 0.25,
        mode: RouteMode::Mul,
    }];

    let cut = Transition::default();
    let crossfade = |dur_s: f32| Transition {
        kind: TransitionKind::Crossfade,
        dur_s,
        curve: Curve::SCurve,
    };

    let mut cues = vec![
        cue(
            "1",
            "Noir plateau",
            Some("#222222"),
            cut.clone(),
            vec![
                state(1, Content::Color([0.0, 0.0, 0.0, 1.0]), 1.0),
                state(2, Content::None, 0.0),
                state(3, Content::None, 0.0),
            ],
        ),
        cue(
            "2",
            "Mires d'identification",
            Some("#3fa9f5"),
            cut,
            vec![
                state(1, Content::Pattern(PatternKind::Ident), 1.0),
                state(2, Content::Pattern(PatternKind::Ident), 1.0),
                state(3, Content::Pattern(PatternKind::Ident), 1.0),
            ],
        ),
        cue(
            "3",
            "Damier de calage",
            Some("#3fa9f5"),
            crossfade(1.0),
            vec![
                state(1, Content::Pattern(PatternKind::Checker), 1.0),
                state(2, Content::Pattern(PatternKind::Grid), 1.0),
                state(3, Content::Pattern(PatternKind::Grid), 1.0),
            ],
        ),
        cue(
            "4",
            "Ambiance chaude",
            Some("#f5a93f"),
            Transition {
                kind: TransitionKind::ThroughBlack,
                dur_s: 3.0,
                curve: Curve::EaseInOut,
            },
            vec![
                state(1, Content::Color([0.9, 0.45, 0.12, 1.0]), 0.8),
                state(2, Content::Color([0.95, 0.6, 0.2, 1.0]), 0.6),
                state(3, Content::Color([0.95, 0.6, 0.2, 1.0]), 0.6),
            ],
        ),
    ];

    // La cue 4 respire : le LFO module l'opacité du fond.
    if let Some(last) = cues.last_mut() {
        last.mod_routes.push(ModRouteState {
            route_id: 1,
            depth: 0.25,
            enabled: true,
        });
        last.notes = "Le fond respire doucement (LFO 0,2 Hz sur l'opacité).".to_string();
    }
    // Insertion décimale de démonstration : une cue 2.5 entre 2 et 3.
    let barres = cue(
        "2.5",
        "Barres de niveaux",
        None,
        crossfade(0.5),
        vec![
            state(1, Content::Pattern(PatternKind::Bars), 1.0),
            state(2, Content::None, 0.0),
            state(3, Content::None, 0.0),
        ],
    );
    cues.insert(2, barres);

    Show {
        format_version: crate::model::FORMAT_VERSION,
        name: "Démonstration".to_string(),
        outputs,
        slices,
        media: Vec::new(),
        materials: Vec::new(),
        cues,
        patch: PatchTable::default(),
        modulators,
        routes,
        settings: ShowSettings::default(),
    }
}
