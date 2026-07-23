// conduite-isf — parseur ISF + génération GLSL 330 core.
// Voir docs/INTERFACES.md (section isf) pour le contrat.
//
// Autonome : aucune dépendance vers les autres crates du workspace.
// La conversion des inputs en ParamSpec se fait dans `app`.

mod generate;
mod parse;

pub use generate::generate_glsl;
pub use parse::parse;

use thiserror::Error;

/// Erreurs du parseur / générateur ISF.
#[derive(Debug, Error)]
pub enum IsfError {
    /// Aucun bloc de commentaire JSON `/*{ ... }*/` en tête du fichier.
    #[error("en-tête ISF introuvable (bloc /*{{ ... }}*/ attendu en tête de fichier)")]
    MissingHeader,
    /// Bloc ouvert mais jamais refermé correctement.
    #[error("en-tête ISF mal terminé : {0}")]
    MalformedHeader(String),
    /// Le JSON de l'en-tête ne parse pas.
    #[error("JSON de l'en-tête ISF invalide : {0}")]
    InvalidJson(#[source] serde_json::Error),
    /// Un input est mal déclaré (NAME manquant, TYPE inconnu…).
    #[error("input ISF invalide : {0}")]
    InvalidInput(String),
    /// Version ISF non gérée (on accepte 1 et 2).
    #[error("version ISF non supportée : {0}")]
    UnsupportedVersion(String),
    /// Fonctionnalité ISF hors périmètre v1 (multi-pass, buffers persistants…).
    #[error("fonctionnalité ISF non supportée : {0}")]
    Unsupported(String),
}

/// Valeur scalaire/vectorielle attachée à un input (min/max/default normalisés).
#[derive(Debug, Clone, PartialEq)]
pub enum IsfInputKind {
    /// Curseur flottant. Défauts normalisés : min 0, max 1, default clampé.
    Float { min: f32, max: f32, default: f32 },
    /// Interrupteur (DEFAULT accepté en bool ou en nombre, cf. DomePack).
    Bool { default: bool },
    /// Entier, éventuellement énuméré via VALUES/LABELS.
    Long {
        min: i64,
        max: i64,
        default: i64,
        values: Vec<i64>,
        labels: Vec<String>,
    },
    /// Couleur RGBA (DEFAULT à 3 composantes ⇒ alpha 1).
    Color { default: [f32; 4] },
    /// Point 2D normalisé.
    Point2D {
        min: [f32; 2],
        max: [f32; 2],
        default: [f32; 2],
    },
    /// Texture d'entrée (le shader devient un effet/mixeur).
    Image,
    /// Impulsion momentanée — uniform bool côté GLSL.
    Event,
    /// Forme d'onde audio — sampler2D nourri à zéro en v1.
    Audio,
    /// FFT audio — sampler2D nourri à zéro en v1.
    AudioFft,
}

/// Un input déclaré dans l'en-tête JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct IsfInput {
    pub name: String,
    /// LABEL si présent, sinon le nom.
    pub label: String,
    pub kind: IsfInputKind,
}

/// Document ISF parsé : métadonnées brutes, inputs typés, corps GLSL.
#[derive(Debug, Clone)]
pub struct IsfDoc {
    /// En-tête JSON complet (DESCRIPTION, CREDIT, CATEGORIES, PASSES…).
    pub meta: serde_json::Value,
    pub inputs: Vec<IsfInput>,
    /// Corps GLSL, sans l'en-tête JSON.
    pub body: String,
}

/// Sources GLSL 330 core prêtes à compiler par le compositor.
#[derive(Debug, Clone)]
pub struct IsfSources {
    pub vertex: String,
    pub fragment: String,
}
