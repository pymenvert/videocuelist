// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Sources GLSL du programme de composition (warp 4 coins + correction
//! couleur + crossfade A/B + blend modes + mires).
//!
//! Base : `toolbox/crates/engine/shaders/warp.vert` / `warp.frag` (GLES 3.0),
//! adaptés en deux variantes sélectionnées à l'init : `#version 330 core`
//! (desktop) et `#version 300 es` (Pi / ANGLE). Le corps est commun, seuls
//! les en-têtes diffèrent.
//!
//! Ajouts par rapport à Lanterne : `u_opacity`, `u_mix` (crossfade des
//! contenus A/B AVANT correction couleur), `u_black` (through-black),
//! `u_master` (intensity × DBO), `u_blend_mode`, mires `bars` et `ident`
//! (damier + numéro du slice en 7 segments).

use conduite_core::PatternKind;

/// Dialecte GLSL choisi à l'initialisation selon le contexte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlslVersion {
    /// Desktop OpenGL 3.3+.
    Core330,
    /// OpenGL ES 3.0 (Raspberry Pi, ANGLE).
    Es300,
}

impl GlslVersion {
    fn vertex_header(self) -> &'static str {
        match self {
            GlslVersion::Core330 => "#version 330 core\n",
            GlslVersion::Es300 => "#version 300 es\nprecision highp float;\nprecision highp int;\n",
        }
    }

    fn fragment_header(self) -> &'static str {
        match self {
            GlslVersion::Core330 => "#version 330 core\n",
            GlslVersion::Es300 => "#version 300 es\nprecision highp float;\nprecision highp int;\n",
        }
    }
}

/// Codes de mire côté shader (`u_pattern`). 0 = média.
pub const PATTERN_NONE: i32 = 0;
pub const PATTERN_GRID: i32 = 1;
pub const PATTERN_CHECKER: i32 = 2;
pub const PATTERN_BARS: i32 = 3;
pub const PATTERN_IDENT: i32 = 4;

/// Traduit une [`PatternKind`] du modèle en code `u_pattern`.
pub fn pattern_code(kind: Option<PatternKind>) -> i32 {
    match kind {
        None => PATTERN_NONE,
        Some(PatternKind::Grid) => PATTERN_GRID,
        Some(PatternKind::Checker) => PATTERN_CHECKER,
        Some(PatternKind::Bars) => PATTERN_BARS,
        Some(PatternKind::Ident) => PATTERN_IDENT,
        // Variantes P2 additives : repli provisoire sur la mire la plus
        // proche tant que le shader dédié n'est pas implémenté (contrat
        // Mires — voir docs/INTERFACES.md).
        Some(PatternKind::Grid4) | Some(PatternKind::Grid16) => PATTERN_GRID,
        Some(PatternKind::ColorBars) => PATTERN_BARS,
    }
}

/// Codes de blend mode côté shader (`u_blend_mode`).
pub const BLEND_NORMAL: i32 = 0;
pub const BLEND_ADD: i32 = 1;
pub const BLEND_SCREEN: i32 = 2;
pub const BLEND_MULTIPLY: i32 = 3;

// Vertex : warp 4 coins par homographie (voir warp.vert de Lanterne).
// Quad unité dérivé de gl_VertexID (TRIANGLE_STRIP, 4 sommets, aucun VBO).
// Coordonnée de texture homogène pour une interpolation perspective correcte.
const VERTEX_BODY: &str = r#"
uniform mat3 u_homography;

out vec3 v_texcoord;

void main() {
    // Ordre strip : (0,0) (1,0) (0,1) (1,1) dans [0,1]².
    vec2 a_position = vec2(
        (gl_VertexID == 1 || gl_VertexID == 3) ? 1.0 : 0.0,
        (gl_VertexID >= 2) ? 1.0 : 0.0
    );

    // Position de sortie : coin déformé, converti de [0,1]² vers le clip
    // space [-1,1]² (y inversé : (0,0) = haut-gauche dans notre convention).
    vec3 warped = u_homography * vec3(a_position, 1.0);
    vec2 ndc = (warped.xy / warped.z) * 2.0 - 1.0;
    gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);

    // Coordonnée de texture : la position SOURCE (non déformée), pondérée par
    // w du point déformé pour une interpolation perspective correcte.
    v_texcoord = vec3(a_position * warped.z, warped.z);
}
"#;

// Fragment : crossfade A/B, correction couleur, through-black, master/DBO,
// mires, préparation du blend mode (le glBlendFunc est réglé côté CPU).
const FRAGMENT_BODY: &str = r#"
in vec3 v_texcoord;
out vec4 frag_color;

uniform sampler2D u_src_a;      // deck A
uniform sampler2D u_src_b;      // deck B
uniform float u_mix;            // crossfade A→B : 0 = A, 1 = B (avant couleur)
uniform vec4 u_src_rect;        // fenêtre source x, y, w, h (normalisée)

// Correction couleur (neutres : 1, 1, 1, (1,1,1)).
uniform float u_brightness;     // 0..2
uniform float u_contrast;       // 0..2
uniform float u_gamma;          // 0.2..4
uniform vec3 u_gain;            // gains RGB, 0..2 chacun

uniform float u_opacity;        // opacité du slice 0..1
uniform float u_black;          // through-black : 1 = noir complet
uniform float u_master;         // master intensity × (1 - DBO)

// Mire : 0 = média, 1 = grille, 2 = damier, 3 = barres, 4 = ident.
uniform int u_pattern;
uniform int u_slice_num;        // numéro affiché par la mire ident

// 0 = normal, 1 = add, 2 = screen, 3 = multiply (glBlendFunc côté CPU).
uniform int u_blend_mode;

// Grille de convergence 8×8 : lignes blanches d'~2 px sur fond sombre.
vec3 pattern_grid(vec2 uv) {
    vec2 cell = fract(uv * 8.0);
    vec2 width = fwidth(uv) * 8.0 * 1.5;
    vec2 line = step(cell, width) + step(1.0 - width, cell);
    float on = clamp(line.x + line.y, 0.0, 1.0);
    return mix(vec3(0.08), vec3(1.0), on);
}

// Damier 8×8.
vec3 pattern_checker(vec2 uv) {
    vec2 cells = floor(uv * 8.0);
    float parity = mod(cells.x + cells.y, 2.0);
    return mix(vec3(0.1), vec3(0.9), parity);
}

// Barres de couleurs (8 barres verticales façon SMPTE).
vec3 pattern_bars(vec2 uv) {
    int i = int(floor(uv.x * 8.0));
    if (i <= 0) { return vec3(1.0); }
    if (i == 1) { return vec3(1.0, 1.0, 0.0); }
    if (i == 2) { return vec3(0.0, 1.0, 1.0); }
    if (i == 3) { return vec3(0.0, 1.0, 0.0); }
    if (i == 4) { return vec3(1.0, 0.0, 1.0); }
    if (i == 5) { return vec3(1.0, 0.0, 0.0); }
    if (i == 6) { return vec3(0.0, 0.0, 1.0); }
    return vec3(0.05);
}

// Rectangle plein : 1.0 si p est dans le rectangle centré en c.
float seg_rect(vec2 p, vec2 c, vec2 half_size) {
    vec2 d = abs(p - c) - half_size;
    return (max(d.x, d.y) < 0.0) ? 1.0 : 0.0;
}

// Afficheur 7 segments : chiffre d dans la cellule p ∈ [0,1]²
// (y vers le bas). Bits : 0=haut, 1=haut-droit, 2=bas-droit, 3=bas,
// 4=bas-gauche, 5=haut-gauche, 6=milieu.
float digit_7seg(int d, vec2 p) {
    int m = 63;                                     // 0
    if (d == 1) { m = 6; }  else if (d == 2) { m = 91; }
    else if (d == 3) { m = 79; } else if (d == 4) { m = 102; }
    else if (d == 5) { m = 109; } else if (d == 6) { m = 125; }
    else if (d == 7) { m = 7; }  else if (d == 8) { m = 127; }
    else if (d == 9) { m = 111; }
    float on = 0.0;
    float t = 0.07;   // demi-épaisseur d'un segment
    float w = 0.26;   // demi-largeur des segments horizontaux
    float h = 0.19;   // demi-hauteur des segments verticaux
    if ((m & 1)  != 0) { on = max(on, seg_rect(p, vec2(0.50, 0.08), vec2(w, t))); }
    if ((m & 2)  != 0) { on = max(on, seg_rect(p, vec2(0.80, 0.28), vec2(t, h))); }
    if ((m & 4)  != 0) { on = max(on, seg_rect(p, vec2(0.80, 0.72), vec2(t, h))); }
    if ((m & 8)  != 0) { on = max(on, seg_rect(p, vec2(0.50, 0.92), vec2(w, t))); }
    if ((m & 16) != 0) { on = max(on, seg_rect(p, vec2(0.20, 0.72), vec2(t, h))); }
    if ((m & 32) != 0) { on = max(on, seg_rect(p, vec2(0.20, 0.28), vec2(t, h))); }
    if ((m & 64) != 0) { on = max(on, seg_rect(p, vec2(0.50, 0.50), vec2(w, t))); }
    return on;
}

// Mire d'identification : damier atténué + numéro du slice en 7 segments
// (jusqu'à 3 chiffres, centrés). Le nom du slice est affiché par l'UI.
vec3 pattern_ident(vec2 uv, int num) {
    vec3 base = pattern_checker(uv) * 0.35;
    int n = (num >= 100) ? 3 : ((num >= 10) ? 2 : 1);
    float dw = 0.16;                       // largeur d'une cellule chiffre en UV
    float x0 = 0.5 - dw * float(n) * 0.5;
    vec2 box = vec2((uv.x - x0) / dw, (uv.y - 0.32) / 0.36);
    int idx = int(floor(box.x));
    if (box.x >= 0.0 && idx < n && box.y >= 0.0 && box.y <= 1.0) {
        vec2 p = vec2(fract(box.x), box.y);
        int div = 1;
        for (int i = 0; i < n - 1 - idx; i++) { div *= 10; }
        int d = (num / div) % 10;
        if (digit_7seg(d, p) > 0.5) { return vec3(1.0, 0.85, 0.1); }
    }
    return base;
}

void main() {
    // Division perspective de la coordonnée homogène (voir vertex).
    vec2 uv = v_texcoord.xy / v_texcoord.z;

    // Hors du quad source : rien (discard pour laisser le blend intact).
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        discard;
    }

    vec3 color;
    if (u_pattern == 1) {
        color = pattern_grid(uv);
    } else if (u_pattern == 2) {
        color = pattern_checker(uv);
    } else if (u_pattern == 3) {
        color = pattern_bars(uv);
    } else if (u_pattern == 4) {
        color = pattern_ident(uv, u_slice_num);
    } else {
        // Fenêtre source puis crossfade des contenus AVANT la correction.
        vec2 suv = u_src_rect.xy + uv * u_src_rect.zw;
        vec3 a = texture(u_src_a, suv).rgb;
        vec3 b = texture(u_src_b, suv).rgb;
        color = mix(a, b, u_mix);
    }

    // Correction couleur (ordre Lanterne : contraste → luminosité → gains →
    // gamma ; clamp avant pow, pow(x<0) est indéfini en GLSL).
    color = (color - 0.5) * u_contrast + 0.5;
    color *= u_brightness;
    color *= u_gain;
    color = pow(clamp(color, 0.0, 1.0), vec3(1.0 / u_gamma));

    // Through-black puis multiplicateur final master/DBO.
    color *= (1.0 - u_black);
    color *= u_master;

    // Préparation selon le blend mode (le glBlendFunc correspondant est
    // réglé côté CPU — voir blend_func_for) :
    if (u_blend_mode == 1 || u_blend_mode == 2) {
        // Add (ONE, ONE) et Screen (ONE, ONE_MINUS_SRC_COLOR) :
        // opacité prémultipliée dans la couleur.
        frag_color = vec4(color * u_opacity, 1.0);
    } else if (u_blend_mode == 3) {
        // Multiply (DST_COLOR, ZERO) : fondre vers blanc quand opacité → 0.
        frag_color = vec4(mix(vec3(1.0), color, u_opacity), 1.0);
    } else {
        // Normal (SRC_ALPHA, ONE_MINUS_SRC_ALPHA).
        frag_color = vec4(color, u_opacity);
    }
}
"#;

/// Source du vertex shader de composition pour le dialecte donné.
pub fn composite_vertex_source(version: GlslVersion) -> String {
    let mut s = String::with_capacity(VERTEX_BODY.len() + 64);
    s.push_str(version.vertex_header());
    s.push_str(VERTEX_BODY);
    s
}

/// Source du fragment shader de composition pour le dialecte donné.
pub fn composite_fragment_source(version: GlslVersion) -> String {
    let mut s = String::with_capacity(FRAGMENT_BODY.len() + 64);
    s.push_str(version.fragment_header());
    s.push_str(FRAGMENT_BODY);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_sources_have_expected_headers_and_uniforms() {
        let core = composite_vertex_source(GlslVersion::Core330);
        assert!(core.starts_with("#version 330 core\n"));
        assert!(!core.contains("precision highp"));

        let es = composite_vertex_source(GlslVersion::Es300);
        assert!(es.starts_with("#version 300 es\n"));
        assert!(es.contains("precision highp float;"));

        for src in [&core, &es] {
            assert!(src.contains("uniform mat3 u_homography;"));
            assert!(src.contains("out vec3 v_texcoord;"));
            assert!(src.contains("gl_VertexID"));
            assert!(src.contains("gl_Position"));
        }
    }

    #[test]
    fn fragment_sources_have_all_uniforms() {
        for version in [GlslVersion::Core330, GlslVersion::Es300] {
            let f = composite_fragment_source(version);
            for expected in [
                "uniform sampler2D u_src_a;",
                "uniform sampler2D u_src_b;",
                "uniform float u_mix;",
                "uniform vec4 u_src_rect;",
                "uniform float u_brightness;",
                "uniform float u_contrast;",
                "uniform float u_gamma;",
                "uniform vec3 u_gain;",
                "uniform float u_opacity;",
                "uniform float u_black;",
                "uniform float u_master;",
                "uniform int u_pattern;",
                "uniform int u_slice_num;",
                "uniform int u_blend_mode;",
                // Mires : grille/damier de Lanterne + barres + ident 7 segments.
                "pattern_grid",
                "pattern_checker",
                "pattern_bars",
                "pattern_ident",
                "digit_7seg",
                // Crossfade avant correction couleur.
                "mix(a, b, u_mix)",
                "in vec3 v_texcoord;",
                "out vec4 frag_color;",
            ] {
                assert!(f.contains(expected), "{version:?} : fragment sans `{expected}`");
            }
        }
        let es = composite_fragment_source(GlslVersion::Es300);
        assert!(es.starts_with("#version 300 es\n"));
        assert!(es.contains("precision highp float;"));
        let core = composite_fragment_source(GlslVersion::Core330);
        assert!(core.starts_with("#version 330 core\n"));
    }

    #[test]
    fn crossfade_happens_before_color_correction() {
        // Le mix A/B doit précéder le contraste dans le source.
        let f = composite_fragment_source(GlslVersion::Core330);
        let mix_at = f.find("mix(a, b, u_mix)").expect("mix présent");
        let contrast_at = f.find("* u_contrast").expect("contraste présent");
        let gamma_at = f.find("1.0 / u_gamma").expect("gamma présent");
        assert!(mix_at < contrast_at, "crossfade avant correction couleur");
        assert!(contrast_at < gamma_at, "contraste avant gamma");
        // Master/DBO en multiplicateur final, après le gamma.
        let master_at = f.find("*= u_master").expect("master présent");
        assert!(gamma_at < master_at, "master après le gamma");
    }

    #[test]
    fn pattern_codes_match_model() {
        assert_eq!(pattern_code(None), PATTERN_NONE);
        assert_eq!(pattern_code(Some(PatternKind::Grid)), PATTERN_GRID);
        assert_eq!(pattern_code(Some(PatternKind::Checker)), PATTERN_CHECKER);
        assert_eq!(pattern_code(Some(PatternKind::Bars)), PATTERN_BARS);
        assert_eq!(pattern_code(Some(PatternKind::Ident)), PATTERN_IDENT);
        // Les codes sont distincts (dispatch du shader).
        let codes = [
            PATTERN_NONE,
            PATTERN_GRID,
            PATTERN_CHECKER,
            PATTERN_BARS,
            PATTERN_IDENT,
        ];
        for (i, a) in codes.iter().enumerate() {
            for b in codes.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn seven_segment_covers_all_digits() {
        // Chaque chiffre 0..9 a une branche dans le shader (masque 7 segments).
        let f = composite_fragment_source(GlslVersion::Core330);
        for mask in ["63", "6", "91", "79", "102", "109", "125", "7", "127", "111"] {
            assert!(
                f.contains(&format!("m = {mask};")) || f.contains(&format!("m = {mask}; ")),
                "masque {mask} absent"
            );
        }
    }
}
