// Génération des sources GLSL 330 core à partir d'un IsfDoc.

use std::fmt::Write as _;

use serde_json::Value;

use crate::{IsfDoc, IsfError, IsfInputKind, IsfSources};

/// Vertex shader standard : quad plein écran (TRIANGLE_STRIP, 4 sommets,
/// aucun VBO nécessaire — tout est dérivé de gl_VertexID).
const VERTEX_SHADER: &str = "\
#version 330 core
out vec2 isf_FragNormCoord;
void main() {
    // Ordre strip : (-1,-1) (1,-1) (-1,1) (1,1)
    vec2 pos = vec2(
        (gl_VertexID == 1 || gl_VertexID == 3) ? 1.0 : -1.0,
        (gl_VertexID >= 2) ? 1.0 : -1.0
    );
    isf_FragNormCoord = pos * 0.5 + 0.5;
    gl_Position = vec4(pos, 0.0, 1.0);
}
";

/// Génère les sources GLSL 330 core (vertex + fragment) pour un document ISF.
///
/// Multi-pass (`PASSES` > 1), buffers persistants et `IMPORTED` sont hors
/// périmètre v1 ⇒ `Err(Unsupported)` explicite.
pub fn generate_glsl(doc: &IsfDoc) -> Result<IsfSources, IsfError> {
    check_unsupported(&doc.meta)?;

    let mut frag = String::with_capacity(doc.body.len() + 2048);
    frag.push_str("#version 330 core\n");
    frag.push_str("// Généré par conduite-isf — préambule ISF standard.\n");
    frag.push_str("uniform float TIME;\n");
    frag.push_str("uniform float TIMEDELTA;\n");
    frag.push_str("uniform vec2 RENDERSIZE;\n");
    frag.push_str("uniform int FRAMEINDEX;\n");
    frag.push_str("uniform vec4 DATE;\n");
    frag.push_str("in vec2 isf_FragNormCoord;\n");
    frag.push_str("out vec4 fragColor;\n");
    frag.push_str("#define gl_FragColor fragColor\n");
    frag.push_str("#define texture2D texture\n");
    // Macros ISF de lecture d'images.
    frag.push_str("#define IMG_SIZE(img) vec2(textureSize(img, 0))\n");
    frag.push_str("#define IMG_PIXEL(img, px) texture(img, (px) / vec2(textureSize(img, 0)))\n");
    frag.push_str("#define IMG_NORM_PIXEL(img, norm) texture(img, (norm))\n");
    frag.push_str("#define IMG_THIS_PIXEL(img) texture(img, isf_FragNormCoord)\n");
    frag.push_str("#define IMG_THIS_NORM_PIXEL(img) texture(img, isf_FragNormCoord)\n");
    if doc.body.contains("vv_FragNormCoord") {
        // Compat ISF v1 (préfixe VDMX historique).
        frag.push_str("#define vv_FragNormCoord isf_FragNormCoord\n");
    }

    frag.push_str("\n// Uniforms des inputs déclarés dans l'en-tête ISF.\n");
    for input in &doc.inputs {
        let glsl_type = match &input.kind {
            IsfInputKind::Float { .. } => "float",
            // event : impulsion momentanée, exposée comme bool.
            IsfInputKind::Bool { .. } | IsfInputKind::Event => "bool",
            IsfInputKind::Long { .. } => "int",
            IsfInputKind::Color { .. } => "vec4",
            IsfInputKind::Point2D { .. } => "vec2",
            // audio / audioFFT : sampler2D nourri à zéro en v1 (pas de crash).
            IsfInputKind::Image | IsfInputKind::Audio | IsfInputKind::AudioFft => "sampler2D",
        };
        // Les noms sont validés par le parseur ; write! sur String est infaillible.
        let _ = writeln!(frag, "uniform {glsl_type} {};", input.name);
    }
    frag.push_str("\n// ---- corps du shader ISF ----\n");
    frag.push_str(&doc.body);
    if !frag.ends_with('\n') {
        frag.push('\n');
    }

    tracing::debug!(
        target: "isf::generate",
        inputs = doc.inputs.len(),
        fragment_len = frag.len(),
        "GLSL 330 généré"
    );
    Ok(IsfSources {
        vertex: VERTEX_SHADER.to_string(),
        fragment: frag,
    })
}

/// Refuse explicitement ce que le compositor v1 ne sait pas rendre.
fn check_unsupported(meta: &Value) -> Result<(), IsfError> {
    if let Some(passes) = meta.get("PASSES") {
        let passes = passes
            .as_array()
            .ok_or_else(|| IsfError::Unsupported("PASSES non-tableau".to_string()))?;
        if passes.len() > 1 {
            return Err(IsfError::Unsupported("multi-pass".to_string()));
        }
        let persistent = passes.iter().any(|p| {
            p.get("PERSISTENT")
                .map(|v| match v {
                    Value::Bool(b) => *b,
                    Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
                    _ => false,
                })
                .unwrap_or(false)
        });
        if persistent {
            return Err(IsfError::Unsupported(
                "buffer persistant (feedback)".to_string(),
            ));
        }
    }
    if meta
        .get("IMPORTED")
        .is_some_and(|v| !v.is_null() && v.as_object().is_none_or(|o| !o.is_empty()))
    {
        // Textures chargées depuis des fichiers : pas encore côté host.
        return Err(IsfError::Unsupported("IMPORTED (textures fichier)".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{generate_glsl, parse, IsfError};

    /// Mini ISF synthétique couvrant chaque type d'input.
    const SYNTHETIC: &str = r#"/*{
        "ISFVSN": "2",
        "DESCRIPTION": "test synthétique",
        "INPUTS": [
            { "NAME": "level",   "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 1.0 },
            { "NAME": "enabled", "TYPE": "bool",  "DEFAULT": true },
            { "NAME": "mode",    "TYPE": "long",  "VALUES": [0, 1], "LABELS": ["A", "B"], "DEFAULT": 1 },
            { "NAME": "tint",    "TYPE": "color", "DEFAULT": [1.0, 0.5, 0.25, 1.0] },
            { "NAME": "center",  "TYPE": "point2D", "DEFAULT": [0.5, 0.5] },
            { "NAME": "inputImage", "TYPE": "image" },
            { "NAME": "flash",   "TYPE": "event" },
            { "NAME": "wave",    "TYPE": "audio" },
            { "NAME": "spectrum", "TYPE": "audioFFT" }
        ]
    }*/
    void main() {
        vec4 img = IMG_THIS_NORM_PIXEL(inputImage);
        vec4 fft = IMG_NORM_PIXEL(spectrum, vec2(0.1, 0.5));
        vec4 wav = IMG_PIXEL(wave, vec2(3.0, 0.0));
        vec2 sz = IMG_SIZE(inputImage);
        float x = enabled ? level : float(mode);
        if (flash) { x += 1.0; }
        gl_FragColor = img * tint * x + fft * 0.001 + wav * 0.001
            + vec4(vv_FragNormCoord + center + sz * 0.0, 0.0, 0.0) * 0.001;
    }
    "#;

    #[test]
    fn synthetic_generates_all_uniforms() {
        let doc = parse(SYNTHETIC).expect("parse");
        let sources = generate_glsl(&doc).expect("generate");
        let f = &sources.fragment;
        assert!(f.starts_with("#version 330 core\n"));
        for expected in [
            "uniform float TIME;",
            "uniform float TIMEDELTA;",
            "uniform vec2 RENDERSIZE;",
            "uniform int FRAMEINDEX;",
            "uniform vec4 DATE;",
            "uniform float level;",
            "uniform bool enabled;",
            "uniform int mode;",
            "uniform vec4 tint;",
            "uniform vec2 center;",
            "uniform sampler2D inputImage;",
            "uniform bool flash;",
            "uniform sampler2D wave;",
            "uniform sampler2D spectrum;",
            "in vec2 isf_FragNormCoord;",
            "out vec4 fragColor;",
            "#define gl_FragColor fragColor",
            "#define texture2D texture",
            "#define IMG_SIZE(img)",
            "#define IMG_PIXEL(img, px)",
            "#define IMG_NORM_PIXEL(img, norm)",
            "#define IMG_THIS_PIXEL(img)",
            "#define IMG_THIS_NORM_PIXEL(img)",
            // le corps utilise vv_FragNormCoord ⇒ alias présent
            "#define vv_FragNormCoord isf_FragNormCoord",
        ] {
            assert!(f.contains(expected), "fragment sans `{expected}`\n{f}");
        }
        // Pas d'en-tête JSON résiduel.
        assert!(!f.contains("/*{"));
        assert!(!f.contains("\"INPUTS\""));
        // Vertex standard.
        assert!(sources.vertex.starts_with("#version 330 core"));
        assert!(sources.vertex.contains("out vec2 isf_FragNormCoord;"));
        assert!(sources.vertex.contains("gl_Position"));
    }

    #[test]
    fn no_vv_alias_when_unused() {
        let src = r#"/*{ "INPUTS": [] }*/ void main() { gl_FragColor = vec4(isf_FragNormCoord, 0.0, 1.0); }"#;
        let doc = parse(src).expect("parse");
        let sources = generate_glsl(&doc).expect("generate");
        assert!(!sources.fragment.contains("vv_FragNormCoord"));
    }

    #[test]
    fn multipass_is_unsupported() {
        let src = r#"/*{
            "ISFVSN": "2",
            "PASSES": [ { "TARGET": "bufA" }, { } ],
            "INPUTS": []
        }*/ void main() { gl_FragColor = vec4(1.0); }"#;
        let doc = parse(src).expect("parse");
        match generate_glsl(&doc) {
            Err(IsfError::Unsupported(msg)) => assert_eq!(msg, "multi-pass"),
            other => panic!("attendu Unsupported(multi-pass), obtenu {other:?}"),
        }
    }

    #[test]
    fn single_pass_without_persistence_is_ok() {
        let src = r#"/*{ "PASSES": [ { } ], "INPUTS": [] }*/ void main() { gl_FragColor = vec4(1.0); }"#;
        let doc = parse(src).expect("parse");
        assert!(generate_glsl(&doc).is_ok());
    }

    #[test]
    fn persistent_buffer_is_unsupported() {
        let src = r#"/*{
            "PASSES": [ { "TARGET": "fb", "PERSISTENT": true } ],
            "INPUTS": []
        }*/ void main() { gl_FragColor = vec4(1.0); }"#;
        let doc = parse(src).expect("parse");
        assert!(matches!(
            generate_glsl(&doc),
            Err(IsfError::Unsupported(_))
        ));
    }

    #[test]
    fn imported_textures_are_unsupported() {
        let src = r#"/*{
            "IMPORTED": { "noiseTex": { "PATH": "noise.png" } },
            "INPUTS": []
        }*/ void main() { gl_FragColor = vec4(1.0); }"#;
        let doc = parse(src).expect("parse");
        assert!(matches!(
            generate_glsl(&doc),
            Err(IsfError::Unsupported(_))
        ));
    }
}
