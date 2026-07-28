//! Vérification de mise à jour OPT-IN (AUDIT P2 n°16) — contrat :
//! `ShowSettings::update_check` (défaut FAUX), UNE seule requête au
//! démarrage, en mode Edit uniquement, timeout 3 s, JAMAIS de
//! téléchargement. Le manifeste (`latest.json`, JSON statique servi par
//! raw.githubusercontent) porte `{version, url, notes}` ; si la version est
//! strictement plus récente (semver), le résultat remonte dans
//! `runtime.update` — badge discret côté UI, rien de bloquant.
//!
//! Tout tourne sur le thread `conduite-update` : le tick ne fait qu'un
//! `try_recv`. Hors-ligne / DNS mort / manifeste invalide = un log INFO et
//! c'est tout (aucun retry, aucun blocage).

use std::time::Duration;

use conduite_core::UpdateInfo;
use crossbeam_channel::Receiver;
use serde::Deserialize;
use tracing::{info, warn};

/// Timeout global de la requête (connexion + lecture) — contrat : 3 s.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Manifeste `latest.json` (champs inconnus ignorés : le manifeste peut
/// s'enrichir sans casser les vieux clients).
#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    url: String,
    #[serde(default)]
    notes: String,
}

/// Lance la vérification en tâche de fond. Le canal rend AU PLUS UNE
/// `UpdateInfo`, uniquement si une version STRICTEMENT plus récente que
/// `current` est publiée. À l'appelant de `try_recv` sur son tick.
pub fn spawn(url: String, current: &str) -> Receiver<UpdateInfo> {
    let current = current.to_string();
    let (tx, rx) = crossbeam_channel::bounded::<UpdateInfo>(1);
    let spawned = std::thread::Builder::new()
        .name("conduite-update".into())
        .spawn(move || {
            if let Some(info) = check(&url, &current) {
                let _ = tx.send(info);
            }
        });
    if let Err(e) = spawned {
        warn!(target: "app::update", error = %e,
            "thread de vérification de mise à jour impossible");
    }
    rx
}

/// GET du manifeste + comparaison semver. `None` = à jour, hors-ligne ou
/// manifeste invalide (loggué, jamais bloquant).
fn check(url: &str, current: &str) -> Option<UpdateInfo> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let body = match agent.get(url).call() {
        Ok(mut res) => match res.body_mut().read_to_string() {
            Ok(b) => b,
            Err(e) => {
                info!(target: "app::update", error = %e,
                    "manifeste de mise à jour illisible");
                return None;
            }
        },
        Err(e) => {
            // Hors-ligne / DNS / 404 : parfaitement normal en salle.
            info!(target: "app::update", error = %e,
                "vérification de mise à jour impossible (hors-ligne ?)");
            return None;
        }
    };
    let manifest: Manifest = match serde_json::from_str(&body) {
        Ok(m) => m,
        Err(e) => {
            info!(target: "app::update", error = %e, "manifeste de mise à jour invalide");
            return None;
        }
    };
    if is_newer(&manifest.version, current) {
        info!(target: "app::update", latest = %manifest.version, current = %current,
            "mise à jour disponible (aucun téléchargement automatique)");
        Some(UpdateInfo {
            version: manifest.version,
            url: manifest.url,
            notes: manifest.notes,
        })
    } else {
        info!(target: "app::update", current = %current, "Conduite est à jour");
        None
    }
}

/// `latest` est STRICTEMENT plus récent que `current` ? Comparaison semver
/// simple `MAJEUR.MINEUR.PATCH` (préfixe `v` toléré, suffixe pré-release
/// ignoré) ; une version imparsable n'est jamais « plus récente ».
fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Parse `[v]MAJEUR.MINEUR.PATCH[-pre][+meta]` en triplet comparable.
fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().strip_prefix('v').unwrap_or_else(|| s.trim());
    // Coupe pré-release / métadonnées : "1.2.3-rc.1+build" → "1.2.3".
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let maj = parts.next()?.parse().ok()?;
    let min = parts.next()?.parse().ok()?;
    let pat = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None; // "1.2.3.4" n'est pas du semver
    }
    Some((maj, min, pat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parses_and_compares() {
        assert_eq!(parse_semver("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3+build.5"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver("abc"), None);

        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"), "égal n'est pas plus récent");
        assert!(!is_newer("0.1.0", "0.2.0"), "plus ancien");
        assert!(!is_newer("n/a", "0.1.0"), "imparsable = jamais plus récent");
        assert!(!is_newer("0.2.0", "n/a"));
    }

    /// Le manifeste tolère les champs inconnus et l'absence de `notes`.
    #[test]
    fn manifest_parses_tolerantly() {
        let m: Manifest = serde_json::from_str(
            r#"{"version":"0.2.0","url":"https://example.org","extra":42}"#,
        )
        .expect("manifeste minimal");
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.notes, "");
    }

    /// Le latest.json du dépôt (racine) reste parsable et cohérent avec le
    /// format attendu par les clients déployés.
    #[test]
    fn repo_manifest_is_valid() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("latest.json");
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("latest.json illisible ({}) : {e}", path.display()));
        let m: Manifest = serde_json::from_str(&body).expect("latest.json parsable");
        assert!(parse_semver(&m.version).is_some(), "version semver : {}", m.version);
        assert!(m.url.starts_with("https://"), "url https : {}", m.url);
    }
}
