// Tests d'intégration sur des shaders réels du DomePack
// (C:\Users\pymenvert\Claude\Projects\Materiaux IFS\dist\ISF, copiés en fixtures).

use conduite_isf::{generate_glsl, parse, IsfInputKind, IsfSources};

fn load_fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("lecture fixture {name}: {e}"))
}

/// Validation syntaxique du fragment généré via glsl-lang.
///
/// Note : naga (glsl-in) a été essayé d'abord mais son front GLSL vise le
/// GLSL Vulkan (rejette `#version 330` et les uniforms libres). glsl-lang
/// parse la grammaire GLSL complète, y compris les directives #define en
/// tête (les usages de macros se lisent comme des appels de fonction), ce
/// qui suffit pour une validation syntaxique.
fn validate_fragment_syntax(fragment: &str, context: &str) {
    use glsl_lang::parse::DefaultParse;
    let unit = glsl_lang::ast::TranslationUnit::parse(fragment);
    if let Err(e) = unit {
        panic!("glsl-lang: fragment invalide pour {context}:\n{e}");
    }
}

/// Contrôles communs à tous les fragments générés.
fn assert_fragment_wellformed(sources: &IsfSources, context: &str) {
    let f = &sources.fragment;
    assert!(f.starts_with("#version 330 core\n"), "{context}: #version manquant");
    // Un seul #version, en tête.
    assert_eq!(f.matches("#version").count(), 1, "{context}: #version dupliqué");
    for expected in [
        "uniform float TIME;",
        "uniform float TIMEDELTA;",
        "uniform vec2 RENDERSIZE;",
        "uniform int FRAMEINDEX;",
        "uniform vec4 DATE;",
        "in vec2 isf_FragNormCoord;",
        "out vec4 fragColor;",
        "#define gl_FragColor fragColor",
    ] {
        assert!(f.contains(expected), "{context}: fragment sans `{expected}`");
    }
    // Pas d'en-tête JSON résiduel.
    assert!(!f.contains("/*{"), "{context}: en-tête /*{{ résiduel");
    assert!(!f.contains("\"INPUTS\""), "{context}: JSON résiduel");
    assert!(sources.vertex.contains("isf_FragNormCoord"));
}

#[test]
fn kaleido_parses_with_expected_inputs() {
    let doc = parse(&load_fixture("Dome Kaleido.fs")).expect("parse Dome Kaleido");
    assert_eq!(doc.inputs.len(), 26);
    assert_eq!(doc.meta["ISFVSN"], "2");

    let orientation = &doc.inputs[0];
    assert_eq!(orientation.name, "orientation");
    assert_eq!(orientation.label, "Orientation (yaw °)");
    assert_eq!(
        orientation.kind,
        IsfInputKind::Float { min: -180.0, max: 180.0, default: 0.0 }
    );

    // Couleur avec DEFAULT à 4 composantes.
    let color_a = doc
        .inputs
        .iter()
        .find(|i| i.name == "colorA")
        .expect("colorA");
    assert_eq!(
        color_a.kind,
        IsfInputKind::Color { default: [0.05, 0.12, 0.35, 1.0] }
    );

    // Long énuméré VALUES/LABELS.
    let palette = doc
        .inputs
        .iter()
        .find(|i| i.name == "paletteMode")
        .expect("paletteMode");
    match &palette.kind {
        IsfInputKind::Long { min, max, default, values, labels } => {
            assert_eq!((*min, *max, *default), (0, 2, 2));
            assert_eq!(values, &[0, 1, 2]);
            assert_eq!(labels, &["RGB", "HSV", "OKLab"]);
        }
        other => panic!("paletteMode devrait être Long, obtenu {other:?}"),
    }

    // Bool avec DEFAULT numérique (1.0).
    let mirror = doc
        .inputs
        .iter()
        .find(|i| i.name == "mirrorFold")
        .expect("mirrorFold");
    assert_eq!(mirror.kind, IsfInputKind::Bool { default: true });

    // Le corps GLSL est complet et sans en-tête.
    assert!(doc.body.contains("void main()"));
    assert!(!doc.body.contains("\"INPUTS\""));
}

#[test]
fn kaleido_generates_valid_glsl() {
    let doc = parse(&load_fixture("Dome Kaleido.fs")).expect("parse");
    let sources = generate_glsl(&doc).expect("generate");
    assert_fragment_wellformed(&sources, "Dome Kaleido");
    for expected in [
        "uniform float orientation;",
        "uniform float intensity;",
        "uniform vec4 colorA;",
        "uniform vec4 colorD;",
        "uniform int segments;",
        "uniform int paletteMode;",
        "uniform bool mirrorFold;",
    ] {
        assert!(sources.fragment.contains(expected), "sans `{expected}`");
    }
    validate_fragment_syntax(&sources.fragment, "Dome Kaleido");
}

#[test]
fn horizon_bands_parses_and_generates() {
    let doc = parse(&load_fixture("Dome Horizon Bands.fs")).expect("parse Horizon Bands");
    assert!(!doc.inputs.is_empty());
    assert!(doc.inputs.iter().any(|i| i.name == "orientation"));
    let sources = generate_glsl(&doc).expect("generate");
    assert_fragment_wellformed(&sources, "Dome Horizon Bands");
    assert!(sources.fragment.contains("uniform float orientation;"));
    validate_fragment_syntax(&sources.fragment, "Dome Horizon Bands");
}

#[test]
fn calibration_grid_parses_and_generates() {
    let doc = parse(&load_fixture("Dome Calibration Grid.fs")).expect("parse Calibration Grid");
    assert!(doc.inputs.iter().any(|i| matches!(i.kind, IsfInputKind::Bool { .. })));
    let sources = generate_glsl(&doc).expect("generate");
    assert_fragment_wellformed(&sources, "Dome Calibration Grid");
    validate_fragment_syntax(&sources.fragment, "Dome Calibration Grid");
}

#[test]
fn media_placer_has_image_input_and_img_macros() {
    // Ce shader consomme une texture (input `image`) et les macros IMG_*.
    let doc = parse(&load_fixture("Dome Media Placer.fs")).expect("parse Media Placer");
    let image = doc
        .inputs
        .iter()
        .find(|i| i.name == "inputImage")
        .expect("inputImage");
    assert_eq!(image.kind, IsfInputKind::Image);

    let sources = generate_glsl(&doc).expect("generate");
    assert_fragment_wellformed(&sources, "Dome Media Placer");
    assert!(sources.fragment.contains("uniform sampler2D inputImage;"));
    assert!(sources.fragment.contains("#define IMG_NORM_PIXEL(img, norm)"));
    validate_fragment_syntax(&sources.fragment, "Dome Media Placer");
}

#[test]
fn whole_dome_pack_would_load() {
    // Critère INTERFACES.md : les .fs du DomePack parsent et génèrent.
    // Les 4 fixtures sont un échantillon représentatif du pack.
    for name in [
        "Dome Kaleido.fs",
        "Dome Horizon Bands.fs",
        "Dome Calibration Grid.fs",
        "Dome Media Placer.fs",
    ] {
        let doc = parse(&load_fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        generate_glsl(&doc).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}
