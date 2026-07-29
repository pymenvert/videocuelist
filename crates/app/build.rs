//! Script de build du binaire `conduite` :
//! - embarque le hash git court dans `CONDUITE_GIT_HASH` (pour `--version`) ;
//! - sur cible Windows, compile les ressources exe : icône multi-résolutions
//!   et bloc VERSIONINFO (ProductName, FileVersion/ProductVersion depuis
//!   CARGO_PKG_VERSION, copyright) — l'exe est identifiable dans
//!   l'Explorateur et le Gestionnaire des tâches.

use std::process::Command;

fn main() {
    // Hash git court pour `--version` (chaîne vide si git/dépôt indisponible :
    // le binaire doit se construire depuis une archive source sans .git).
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=CONDUITE_GIT_HASH={git_hash}");
    // Recompiler quand le commit courant change (chemins depuis crates/app/).
    // HEAD seul ne suffit pas : il ne bouge qu'au changement de branche ; un
    // nouveau commit met à jour .git/refs/heads/<branche> (ou packed-refs si
    // la réf est empaquetée par git gc). Dégradation silencieuse hors dépôt.
    let git_dir = std::path::Path::new("../../.git");
    let head_path = git_dir.join("HEAD");
    if head_path.exists() {
        println!("cargo:rerun-if-changed={}", head_path.display());
        if let Ok(head) = std::fs::read_to_string(&head_path) {
            if let Some(branch_ref) = head.trim().strip_prefix("ref: ") {
                // Déclaré même si le fichier de réf n'existe pas encore
                // (réf empaquetée) : cargo relance alors le script à chaque
                // build, ce qui garantit un hash à jour dans tous les cas.
                println!("cargo:rerun-if-changed={}", git_dir.join(branch_ref).display());
            }
        }
        let packed = git_dir.join("packed-refs");
        if packed.exists() {
            println!("cargo:rerun-if-changed={}", packed.display());
        }
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        compile_windows_resources();
    }
}

fn compile_windows_resources() {
    const ICON: &str = "assets/conduite.ico";
    println!("cargo:rerun-if-changed={ICON}");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ICON);
    res.set("ProductName", "Conduite");
    res.set("FileDescription", "Conduite — régie vidéo de spectacle");
    res.set("LegalCopyright", "© 2026 Pym — licence MIT");
    res.set("CompanyName", "Conduite");
    res.set("InternalName", "conduite");
    res.set("OriginalFilename", "conduite.exe");
    // FileVersion / ProductVersion (chaînes) ; les champs numériques sont
    // dérivés de CARGO_PKG_VERSION par winresource.
    res.set("FileVersion", env!("CARGO_PKG_VERSION"));
    res.set("ProductVersion", env!("CARGO_PKG_VERSION"));

    if let Err(e) = res.compile() {
        // Échec = binaire anonyme (0.0.0.0, sans icône) : on refuse de
        // produire silencieusement un exe non identifiable.
        println!("cargo:warning=compilation des ressources Windows échouée : {e}");
        std::process::exit(1);
    }
}
