//! Déclaration des paramètres : type, plage, défaut, lissage.

use conduite_core::ParamValue;

/// Type d'un paramètre : détermine le clamp et la règle d'interpolation.
///
/// Interpolation typée : `Float`/`Color`/`Point2` interpolent linéairement,
/// `Int` interpole puis arrondit, `Bool`/`Enum` basculent à `alpha >= 0.5`.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamKind {
    Float { min: f32, max: f32 },
    Int { min: i64, max: i64 },
    Bool,
    Color,
    Point2,
    /// Variantes nommées ; la valeur canonique est `ParamValue::I(index)`,
    /// mais `ParamValue::S(label)` est acceptée en entrée.
    Enum(Vec<String>),
}

/// Déclaration d'un paramètre adressable du registre.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamSpec {
    /// Adresse stable (ex. `slice/1/opacity`) — clé du registre.
    pub addr: String,
    /// Libellé lisible pour l'UI.
    pub label: String,
    pub kind: ParamKind,
    /// Valeur de départ (clampée au `kind` à l'enregistrement).
    pub default: ParamValue,
    /// Constante de temps du lissage exponentiel (0 = instantané).
    pub smoothing_ms: f32,
    /// Enregistrable dans les cues (repris par `snapshot_scripted`).
    pub scriptable: bool,
}
