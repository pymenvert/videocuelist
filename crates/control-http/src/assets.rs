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

/// Mode développement : si `CONDUITE_WEBUI_DIR` est défini, sert les quatre
/// fichiers connus depuis ce dossier (relu à chaque requête, pour itérer sur
/// l'UI sans recompiler). Liste blanche stricte : aucun autre nom n'est lu.
pub fn asset_dev(path: &str) -> Option<(&'static str, String)> {
    let dir = std::env::var("CONDUITE_WEBUI_DIR").ok()?;
    let content_type = match path {
        "app.js" | "ws.js" => "application/javascript; charset=utf-8",
        "style.css" => "text/css; charset=utf-8",
        "index.html" => "text/html; charset=utf-8",
        _ => return None,
    };
    match std::fs::read_to_string(std::path::Path::new(&dir).join(path)) {
        Ok(body) => Some((content_type, body)),
        Err(e) => {
            tracing::warn!(target: "http::assets", "webui dev : lecture {path} impossible : {e}");
            None
        }
    }
}
