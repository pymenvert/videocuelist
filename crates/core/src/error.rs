//! Erreurs du cœur. Runtime : jamais de panic — les appelants loguent
//! (`tracing::error!`) et dégradent proprement.

/// Erreur du cœur (persistance, validation, parsing).
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Erreur d'entrée/sortie sur un chemin donné.
    #[error("E/S sur {path} : {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// JSON illisible ou de forme inattendue.
    #[error("JSON invalide dans {path} : {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    /// Chemin de média/matériau refusé (absolu, `..`, `\0`, schéma `://`).
    #[error("chemin invalide : {0:?}")]
    InvalidPath(String),

    /// Numéro de cue illisible (attendu : "12" ou "12.34", max 3 décimales).
    #[error("numéro de cue invalide : {0:?}")]
    InvalidCueNumber(String),

    /// Fichier show écrit par une version plus récente du logiciel.
    #[error("format de show v{0} plus récent que ce logiciel (v{1})")]
    UnsupportedVersion(u32, u32),

    /// Une autre instance vivante tient déjà le verrou mono-instance.
    #[error("une autre instance de Conduite tourne déjà (verrou {path})")]
    InstanceLocked { path: String },
}

impl CoreError {
    /// Raccourci pour construire une erreur d'E/S avec son chemin.
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        CoreError::Io {
            path: path.into(),
            source,
        }
    }

    /// Raccourci pour construire une erreur JSON avec son chemin.
    pub fn json(path: impl Into<String>, source: serde_json::Error) -> Self {
        CoreError::Json {
            path: path.into(),
            source,
        }
    }
}
