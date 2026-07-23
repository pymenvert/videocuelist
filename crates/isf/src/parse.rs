// Extraction de l'en-tête JSON /*{ ... }*/ et des inputs ISF.

use serde_json::Value;

use crate::{IsfDoc, IsfError, IsfInput, IsfInputKind};

/// Parse une source ISF : en-tête JSON en commentaire + corps GLSL.
///
/// Tolère les variantes d'espacement (`/*{`, `/* {`, `/*\n{`, BOM,
/// lignes vides ou commentaires `//` avant le bloc).
pub fn parse(src: &str) -> Result<IsfDoc, IsfError> {
    let (json_str, body) = split_header(src)?;
    let meta: Value = serde_json::from_str(json_str).map_err(IsfError::InvalidJson)?;
    check_version(&meta)?;
    let inputs = parse_inputs(&meta)?;
    tracing::debug!(
        target: "isf::parse",
        inputs = inputs.len(),
        "en-tête ISF parsé"
    );
    Ok(IsfDoc {
        meta,
        inputs,
        body: body.to_string(),
    })
}

/// Trouve le premier bloc `/* { ... } */` dont le contenu est du JSON,
/// et renvoie (json, corps GLSL après le bloc).
fn split_header(src: &str) -> Result<(&str, &str), IsfError> {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'/' && bytes[i + 1] == b'*' {
            // Position du premier caractère non blanc après "/*".
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'{' {
                let end = find_matching_brace(bytes, j)?;
                // Après le '}' fermant : blancs optionnels puis "*/".
                let mut k = end + 1;
                while k < bytes.len() && (bytes[k] as char).is_whitespace() {
                    k += 1;
                }
                if k + 1 < bytes.len() && bytes[k] == b'*' && bytes[k + 1] == b'/' {
                    return Ok((&src[j..=end], &src[k + 2..]));
                }
                return Err(IsfError::MalformedHeader(
                    "le JSON est suivi d'autre chose que `*/`".to_string(),
                ));
            }
            // Commentaire bloc ordinaire : on saute jusqu'à sa fin.
            let mut k = i + 2;
            while k + 1 < bytes.len() && !(bytes[k] == b'*' && bytes[k + 1] == b'/') {
                k += 1;
            }
            i = k + 2;
            continue;
        }
        i += 1;
    }
    Err(IsfError::MissingHeader)
}

/// Index du '}' qui ferme le '{' à `open`, en respectant les chaînes JSON.
fn find_matching_brace(bytes: &[u8], open: usize) -> Result<usize, IsfError> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = open;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else {
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    Err(IsfError::MalformedHeader(
        "accolade fermante du JSON introuvable".to_string(),
    ))
}

/// ISFVSN 1 et 2 acceptés (absent = v1). Toute autre version ⇒ erreur.
fn check_version(meta: &Value) -> Result<(), IsfError> {
    let Some(v) = meta.get("ISFVSN") else {
        return Ok(()); // ISF v1 : pas de champ ISFVSN
    };
    let text = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    };
    let major = text.split('.').next().unwrap_or("").trim();
    match major {
        "1" | "2" => Ok(()),
        _ => Err(IsfError::UnsupportedVersion(text)),
    }
}

fn parse_inputs(meta: &Value) -> Result<Vec<IsfInput>, IsfError> {
    let Some(list) = meta.get("INPUTS") else {
        return Ok(Vec::new());
    };
    let Some(list) = list.as_array() else {
        return Err(IsfError::InvalidInput(
            "INPUTS doit être un tableau".to_string(),
        ));
    };
    let mut inputs = Vec::with_capacity(list.len());
    for (idx, item) in list.iter().enumerate() {
        inputs.push(parse_input(idx, item)?);
    }
    // Doublons de noms ⇒ uniforms en conflit : on refuse tôt.
    for (a, input) in inputs.iter().enumerate() {
        if inputs[..a].iter().any(|other: &IsfInput| other.name == input.name) {
            return Err(IsfError::InvalidInput(format!(
                "nom d'input dupliqué : `{}`",
                input.name
            )));
        }
    }
    Ok(inputs)
}

fn parse_input(idx: usize, item: &Value) -> Result<IsfInput, IsfError> {
    let obj = item.as_object().ok_or_else(|| {
        IsfError::InvalidInput(format!("INPUTS[{idx}] n'est pas un objet"))
    })?;
    let name = obj
        .get("NAME")
        .and_then(Value::as_str)
        .ok_or_else(|| IsfError::InvalidInput(format!("INPUTS[{idx}] : NAME manquant")))?
        .to_string();
    if !is_valid_glsl_ident(&name) {
        return Err(IsfError::InvalidInput(format!(
            "INPUTS[{idx}] : NAME `{name}` n'est pas un identifiant GLSL valide"
        )));
    }
    let type_name = obj
        .get("TYPE")
        .and_then(Value::as_str)
        .ok_or_else(|| IsfError::InvalidInput(format!("input `{name}` : TYPE manquant")))?;
    let label = obj
        .get("LABEL")
        .and_then(Value::as_str)
        .unwrap_or(&name)
        .to_string();

    let min = obj.get("MIN");
    let max = obj.get("MAX");
    let default = obj.get("DEFAULT");

    let kind = match type_name {
        "float" => {
            let min = min.and_then(as_f32).unwrap_or(0.0);
            let max = max.and_then(as_f32).unwrap_or(1.0);
            let default = default.and_then(as_f32).unwrap_or(0.0).clamp(min, max);
            IsfInputKind::Float { min, max, default }
        }
        "bool" => IsfInputKind::Bool {
            default: default.map(truthy).unwrap_or(false),
        },
        "long" => {
            let values: Vec<i64> = obj
                .get("VALUES")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(as_i64).collect())
                .unwrap_or_default();
            let labels: Vec<String> = obj
                .get("LABELS")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let (vmin, vmax) = if values.is_empty() {
                (None, None)
            } else {
                (values.iter().min().copied(), values.iter().max().copied())
            };
            let min = min.and_then(as_i64).or(vmin).unwrap_or(0);
            let max = max.and_then(as_i64).or(vmax).unwrap_or(min.max(1));
            let default = default
                .and_then(as_i64)
                .or_else(|| values.first().copied())
                .unwrap_or(min)
                .clamp(min, max);
            IsfInputKind::Long {
                min,
                max,
                default,
                values,
                labels,
            }
        }
        "color" => {
            let default = default
                .and_then(as_color)
                .unwrap_or([0.0, 0.0, 0.0, 1.0]);
            IsfInputKind::Color { default }
        }
        "point2D" => {
            let min = min.and_then(as_point2).unwrap_or([0.0, 0.0]);
            let max = max.and_then(as_point2).unwrap_or([1.0, 1.0]);
            let default = default.and_then(as_point2).unwrap_or([0.0, 0.0]);
            IsfInputKind::Point2D { min, max, default }
        }
        "image" => IsfInputKind::Image,
        "event" => IsfInputKind::Event,
        "audio" => IsfInputKind::Audio,
        "audioFFT" => IsfInputKind::AudioFft,
        other => {
            return Err(IsfError::InvalidInput(format!(
                "input `{name}` : TYPE `{other}` inconnu"
            )));
        }
    };
    Ok(IsfInput { name, label, kind })
}

/// Identifiant GLSL : [A-Za-z_][A-Za-z0-9_]*, sans préfixe réservé `gl_`.
fn is_valid_glsl_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.starts_with("gl_")
}

fn as_f32(v: &Value) -> Option<f32> {
    v.as_f64().map(|f| f as f32)
}

fn as_i64(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

/// Vrai si bool `true` ou nombre non nul (le DomePack écrit `"DEFAULT": 1.0`).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => false,
    }
}

fn as_color(v: &Value) -> Option<[f32; 4]> {
    let arr = v.as_array()?;
    let comps: Vec<f32> = arr.iter().filter_map(as_f32).collect();
    match comps.len() {
        3 => Some([comps[0], comps[1], comps[2], 1.0]),
        4 => Some([comps[0], comps[1], comps[2], comps[3]]),
        _ => None,
    }
}

fn as_point2(v: &Value) -> Option<[f32; 2]> {
    let arr = v.as_array()?;
    let comps: Vec<f32> = arr.iter().filter_map(as_f32).collect();
    (comps.len() == 2).then(|| [comps[0], comps[1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_compact_spacing() {
        let doc = parse("/*{\"INPUTS\":[]}*/\nvoid main(){}").expect("parse");
        assert!(doc.inputs.is_empty());
        assert_eq!(doc.body.trim(), "void main(){}");
    }

    #[test]
    fn header_loose_spacing() {
        let src = "\n\n/*  \n  { \"DESCRIPTION\": \"x\" }  \n  */\nvoid main(){}";
        let doc = parse(src).expect("parse");
        assert_eq!(doc.meta["DESCRIPTION"], "x");
        assert_eq!(doc.body.trim(), "void main(){}");
    }

    #[test]
    fn header_after_line_comment_block() {
        // Un commentaire bloc ordinaire avant l'en-tête ne doit pas le masquer.
        let src = "/* licence */\n/*{ \"ISFVSN\": \"2\" }*/\nvoid main(){}";
        let doc = parse(src).expect("parse");
        assert_eq!(doc.meta["ISFVSN"], "2");
    }

    #[test]
    fn json_with_braces_in_strings() {
        let src = "/*{ \"DESCRIPTION\": \"contient { et } et \\\" aussi\" }*/ void main(){}";
        let doc = parse(src).expect("parse");
        assert!(doc.body.contains("void main"));
    }

    #[test]
    fn missing_header_is_error() {
        assert!(matches!(
            parse("void main(){}"),
            Err(IsfError::MissingHeader)
        ));
    }

    #[test]
    fn unterminated_header_is_error() {
        assert!(matches!(
            parse("/*{ \"a\": 1 "),
            Err(IsfError::MalformedHeader(_))
        ));
    }

    #[test]
    fn invalid_json_is_error() {
        assert!(matches!(
            parse("/*{ pas du json }*/ void main(){}"),
            Err(IsfError::InvalidJson(_))
        ));
    }

    #[test]
    fn isfvsn_1_and_2_accepted_3_rejected() {
        assert!(parse("/*{ \"ISFVSN\": \"1\" }*/ void main(){}").is_ok());
        assert!(parse("/*{ \"ISFVSN\": \"2.1\" }*/ void main(){}").is_ok());
        assert!(parse("/*{ \"ISFVSN\": 2 }*/ void main(){}").is_ok());
        assert!(matches!(
            parse("/*{ \"ISFVSN\": \"3\" }*/ void main(){}"),
            Err(IsfError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn float_normalization() {
        let src = r#"/*{ "INPUTS": [
            { "NAME": "a", "TYPE": "float" },
            { "NAME": "b", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 5.0 }
        ] }*/ void main(){}"#;
        let doc = parse(src).expect("parse");
        assert_eq!(
            doc.inputs[0].kind,
            IsfInputKind::Float { min: 0.0, max: 1.0, default: 0.0 }
        );
        // DEFAULT hors plage ⇒ clampé.
        assert_eq!(
            doc.inputs[1].kind,
            IsfInputKind::Float { min: -2.0, max: 2.0, default: 2.0 }
        );
    }

    #[test]
    fn bool_default_accepts_number() {
        let src = r#"/*{ "INPUTS": [
            { "NAME": "m", "TYPE": "bool", "DEFAULT": 1.0 },
            { "NAME": "n", "TYPE": "bool", "DEFAULT": false },
            { "NAME": "o", "TYPE": "bool" }
        ] }*/ void main(){}"#;
        let doc = parse(src).expect("parse");
        assert_eq!(doc.inputs[0].kind, IsfInputKind::Bool { default: true });
        assert_eq!(doc.inputs[1].kind, IsfInputKind::Bool { default: false });
        assert_eq!(doc.inputs[2].kind, IsfInputKind::Bool { default: false });
    }

    #[test]
    fn long_with_values_and_labels() {
        let src = r#"/*{ "INPUTS": [
            { "NAME": "mode", "TYPE": "long", "VALUES": [0, 1, 2],
              "LABELS": ["RGB", "HSV", "OKLab"], "DEFAULT": 2 }
        ] }*/ void main(){}"#;
        let doc = parse(src).expect("parse");
        assert_eq!(
            doc.inputs[0].kind,
            IsfInputKind::Long {
                min: 0,
                max: 2,
                default: 2,
                values: vec![0, 1, 2],
                labels: vec!["RGB".into(), "HSV".into(), "OKLab".into()],
            }
        );
    }

    #[test]
    fn color_default_rgb_gets_alpha_one() {
        let src = r#"/*{ "INPUTS": [
            { "NAME": "c", "TYPE": "color", "DEFAULT": [0.1, 0.2, 0.3] }
        ] }*/ void main(){}"#;
        let doc = parse(src).expect("parse");
        assert_eq!(
            doc.inputs[0].kind,
            IsfInputKind::Color { default: [0.1, 0.2, 0.3, 1.0] }
        );
    }

    #[test]
    fn duplicate_input_name_rejected() {
        let src = r#"/*{ "INPUTS": [
            { "NAME": "x", "TYPE": "float" },
            { "NAME": "x", "TYPE": "bool" }
        ] }*/ void main(){}"#;
        assert!(matches!(parse(src), Err(IsfError::InvalidInput(_))));
    }

    #[test]
    fn reserved_or_invalid_names_rejected() {
        for bad in ["gl_Thing", "1abc", "a-b", ""] {
            let src = format!(
                r#"/*{{ "INPUTS": [ {{ "NAME": "{bad}", "TYPE": "float" }} ] }}*/ void main(){{}}"#
            );
            assert!(
                matches!(parse(&src), Err(IsfError::InvalidInput(_))),
                "`{bad}` aurait dû être rejeté"
            );
        }
    }

    #[test]
    fn unknown_type_rejected() {
        let src = r#"/*{ "INPUTS": [ { "NAME": "x", "TYPE": "matrix" } ] }*/ void main(){}"#;
        assert!(matches!(parse(src), Err(IsfError::InvalidInput(_))));
    }

    #[test]
    fn label_falls_back_to_name() {
        let src = r#"/*{ "INPUTS": [
            { "NAME": "speed", "TYPE": "float", "LABEL": "Vitesse" },
            { "NAME": "gain", "TYPE": "float" }
        ] }*/ void main(){}"#;
        let doc = parse(src).expect("parse");
        assert_eq!(doc.inputs[0].label, "Vitesse");
        assert_eq!(doc.inputs[1].label, "gain");
    }
}
