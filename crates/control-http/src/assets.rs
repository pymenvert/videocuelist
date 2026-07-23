//! Webui embarquée dans le binaire (`include_str!`) — servie par le routeur.
//!
//! Les fichiers vivent dans `webui/` à la racine du repo ; ils sont figés à
//! la compilation : aucun accès disque au runtime pour l'UI.

/// Page principale.
pub const INDEX_HTML: &str = include_str!("../../../webui/index.html");
/// Application (vanilla JS).
pub const APP_JS: &str = include_str!("../../../webui/app.js");
/// Couche WebSocket (reconnexion, backoff).
pub const WS_JS: &str = include_str!("../../../webui/ws.js");
/// Thème sombre régie.
pub const STYLE_CSS: &str = include_str!("../../../webui/style.css");

/// Résout un chemin d'asset (`/assets/<path>`) vers `(content-type, corps)`.
pub fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    match path {
        "app.js" => Some(("application/javascript; charset=utf-8", APP_JS)),
        "ws.js" => Some(("application/javascript; charset=utf-8", WS_JS)),
        "style.css" => Some(("text/css; charset=utf-8", STYLE_CSS)),
        "index.html" => Some(("text/html; charset=utf-8", INDEX_HTML)),
        _ => None,
    }
}
