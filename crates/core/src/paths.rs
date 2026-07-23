// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Validation des chemins de médias/matériaux : relatifs au dossier portable,
//! canoniques, sans traversée.
//!
//! Accepter un chemin absolu ou `..` permettrait de lire n'importe quel
//! fichier de la machine via l'API réseau (la web UI est accessible à
//! distance). Refusé ici = refusé pour TOUTES les interfaces.

use crate::error::CoreError;

/// Valide un chemin relatif (média sous `media/`, matériau sous `shaders/`).
///
/// Refusé : vide, `\0`, absolu (`/…`, `\…`, `C:\…`), tout schéma `xx://`
/// (`file://` surtout), toute composante `..`.
pub fn validate_relative_path(path: &str) -> Result<(), CoreError> {
    let err = || CoreError::InvalidPath(path.to_string());
    if path.is_empty() || path.contains('\0') {
        return Err(err());
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(err());
    }
    // Chemin Windows absolu type `C:\...` ou `C:/...`.
    if path.as_bytes().get(1) == Some(&b':') {
        return Err(err());
    }
    // Schéma d'URL (`file://`, `ftp://`…) : refusé plutôt qu'interprété
    // comme un chemin — un `file:///etc/passwd` ne doit pas passer.
    if path.contains("://") {
        return Err(err());
    }
    if path.split(['/', '\\']).any(|part| part == "..") {
        return Err(err());
    }
    Ok(())
}
