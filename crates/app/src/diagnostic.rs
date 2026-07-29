//! Rapport de diagnostic (AUDIT P2 n°15) : un zip horodaté à joindre au
//! mail de support — logs récents (500 dernières lignes de chaque fichier),
//! `config.toml`, `show.json`, versions (app + ffmpeg) et instantané santé.
//!
//! Contrat :
//! - généré en TÂCHE DE FOND (`Command::DiagnosticReport` → thread
//!   `conduite-diagnostic`, jamais sur le tick) ;
//! - chemins personnels EXPURGÉS (`C:\Users\<user>` → `~`) dans chaque
//!   fichier embarqué ;
//! - écrit dans `logs/diagnostic-<horodatage>.zip`, borné à ~10 Mo de
//!   contenu ; l'app publie `StateEvent::DiagnosticReady { path }` à la fin.
//! - AUCUN envoi réseau : le zip reste local, c'est l'utilisateur qui le
//!   joint à son ticket.

use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};

use tracing::{debug, warn};
use zip::write::SimpleFileOptions;

/// Nombre de lignes conservées en fin de chaque fichier de log.
const LOG_TAIL_LINES: usize = 500;
/// Nombre de fichiers de log récents embarqués.
const LOG_FILES_MAX: usize = 8;
/// Fenêtre de lecture en fin de fichier de log (les 500 dernières lignes
/// tiennent largement dedans — jamais de lecture d'un log de 2 Go entier).
const LOG_TAIL_WINDOW: u64 = 1024 * 1024;
/// Budget TOTAL de contenu embarqué (avant compression) : ~10 Mo.
const MAX_TOTAL_BYTES: usize = 10 * 1024 * 1024;
/// Taille maximale lue pour un fichier isolé (show.json pathologique…).
const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;

/// Entrées nécessaires à la génération (clonées par la session : le thread
/// de fond ne touche jamais à son état).
pub struct DiagnosticInput {
    /// Dossier `logs/` (source des logs ET destination du zip).
    pub logs_dir: PathBuf,
    /// Dossier portable racine (`config.toml`).
    pub base_dir: PathBuf,
    /// Dossier du show courant (`show.json`).
    pub show_dir: PathBuf,
    /// Version affichable de l'app (avec hash git).
    pub version: String,
    /// Instantané santé + statut des protocoles, déjà sérialisé en JSON.
    pub health_json: String,
}

/// Génère le zip et retourne son chemin ABSOLU (à expurger pour l'UI).
pub fn generate(input: &DiagnosticInput) -> Result<PathBuf, String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let zip_path = input.logs_dir.join(format!("diagnostic-{stamp}.zip"));
    let file = std::fs::File::create(&zip_path)
        .map_err(|e| format!("création de {} : {e}", zip_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut budget = MAX_TOTAL_BYTES;
    let mut truncated: Vec<String> = Vec::new();

    // 1. Versions : app, OS, ffmpeg (une seule invocation `-version`).
    let versions = format!(
        "Conduite {}\nOS : {} {}\n\n{}",
        input.version,
        std::env::consts::OS,
        std::env::consts::ARCH,
        ffmpeg_version()
    );
    add_entry(&mut zip, opts, "versions.txt", &versions, &mut budget, &mut truncated);

    // 2. Santé machine + statut des protocoles (instantané de la session).
    add_entry(&mut zip, opts, "sante.json", &input.health_json, &mut budget, &mut truncated);

    // 3. config.toml (réglages machine).
    match read_capped(&input.base_dir.join("config.toml")) {
        Ok(text) => add_entry(&mut zip, opts, "config.toml", &text, &mut budget, &mut truncated),
        Err(e) => add_entry(
            &mut zip, opts, "config.toml.absent.txt",
            &format!("config.toml illisible : {e}"), &mut budget, &mut truncated,
        ),
    }

    // 4. show.json (le show persisté sur disque).
    match read_capped(&input.show_dir.join(conduite_core::SHOW_FILE)) {
        Ok(text) => add_entry(&mut zip, opts, "show.json", &text, &mut budget, &mut truncated),
        Err(e) => add_entry(
            &mut zip, opts, "show.json.absent.txt",
            &format!("show.json illisible : {e}"), &mut budget, &mut truncated,
        ),
    }

    // 5. Logs récents : les N derniers fichiers, 500 dernières lignes chacun.
    for path in recent_log_files(&input.logs_dir, LOG_FILES_MAX) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "journal.log".to_string());
        match tail_lines(&path, LOG_TAIL_LINES) {
            Ok(tail) => add_entry(
                &mut zip, opts, &format!("logs/{name}"), &tail, &mut budget, &mut truncated,
            ),
            Err(e) => debug!(target: "app::diagnostic", path = %path.display(),
                error = %e, "log illisible, ignoré"),
        }
    }

    if !truncated.is_empty() {
        let note = format!(
            "Rapport tronqué (budget {} Mo dépassé).\nEntrées omises ou coupées :\n{}\n",
            MAX_TOTAL_BYTES / (1024 * 1024),
            truncated.join("\n")
        );
        let _ = zip
            .start_file("TRONQUE.txt", opts)
            .and_then(|()| zip.write_all(note.as_bytes()).map_err(Into::into));
    }

    zip.finish()
        .map_err(|e| format!("finalisation du zip : {e}"))?;
    Ok(zip_path)
}

/// Ajoute une entrée texte EXPURGÉE au zip, dans la limite du budget.
fn add_entry(
    zip: &mut zip::ZipWriter<std::fs::File>,
    opts: SimpleFileOptions,
    name: &str,
    text: &str,
    budget: &mut usize,
    truncated: &mut Vec<String>,
) {
    let clean = redact(text);
    let bytes = clean.as_bytes();
    if bytes.len() > *budget {
        truncated.push(name.to_string());
        return;
    }
    *budget -= bytes.len();
    let write = zip
        .start_file(name, opts)
        .and_then(|()| zip.write_all(bytes).map_err(Into::into));
    if let Err(e) = write {
        warn!(target: "app::diagnostic", entry = name, error = %e,
            "entrée de diagnostic non écrite");
    }
}

/// Expurge les chemins personnels : le dossier utilisateur (`C:\Users\pym`,
/// `/home/pym`…) devient `~` — sous ses formes brute, JSON-échappée
/// (`C:\\Users\\pym`) et à séparateurs `/`. Insensible à la casse (Windows).
pub fn redact(text: &str) -> String {
    let Some(home) = home_dir() else {
        return text.to_string();
    };
    let mut out = text.to_string();
    let json_escaped = home.replace('\\', "\\\\");
    let forward = home.replace('\\', "/");
    for needle in [json_escaped.as_str(), home.as_str(), forward.as_str()] {
        if !needle.is_empty() {
            out = replace_ci(&out, needle, "~");
        }
    }
    out
}

/// Dossier personnel de l'utilisateur (sans dépendance : variables d'env).
fn home_dir() -> Option<String> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .filter(|h| h.len() > 3) // jamais expurger "C:\" entier
}

/// Remplacement insensible à la casse (les chemins Windows arrivent en
/// `C:\Users\…` comme en `c:\users\…`).
fn replace_ci(hay: &str, needle: &str, replacement: &str) -> String {
    let hay_lower = hay.to_lowercase();
    let needle_lower = needle.to_lowercase();
    if needle_lower.is_empty() {
        return hay.to_string();
    }
    let mut out = String::with_capacity(hay.len());
    let mut last = 0;
    let mut from = 0;
    while let Some(pos) = hay_lower[from..].find(&needle_lower) {
        let start = from + pos;
        // Les indices de `hay_lower` valent pour `hay` : to_lowercase peut
        // changer la longueur sur certains caractères Unicode — on vérifie
        // la frontière avant de couper (sinon on rend le texte intact).
        if !hay.is_char_boundary(start) || !hay.is_char_boundary(start + needle.len()) {
            return hay.to_string();
        }
        out.push_str(&hay[last..start]);
        out.push_str(replacement);
        last = start + needle.len();
        from = last;
    }
    out.push_str(&hay[last..]);
    out
}

/// Lit un fichier texte, borné à [`MAX_FILE_BYTES`].
fn read_capped(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    file.take(MAX_FILE_BYTES as u64).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Les `count` dernières lignes d'un fichier (lecture bornée en fin de
/// fichier : jamais un log entier en mémoire).
pub fn tail_lines(path: &Path, count: usize) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(LOG_TAIL_WINDOW);
    file.seek(std::io::SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().collect();
    let skip = lines.len().saturating_sub(count);
    Ok(lines[skip..].join("\n"))
}

/// Fichiers `conduite*.log` du dossier, du plus récent au plus ancien.
fn recent_log_files(logs_dir: &Path, max: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(logs_dir) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("conduite") || !name.ends_with(".log") {
                return None;
            }
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files.truncate(max);
    files.into_iter().map(|(_, p)| p).collect()
}

/// Première ligne de `ffmpeg -version` (résolution `bin/` puis PATH, comme
/// le moteur) — une seule invocation, jamais sur le tick.
fn ffmpeg_version() -> String {
    let ffmpeg = conduite_engine::resolve_ffmpeg();
    let mut cmd = std::process::Command::new(&ffmpeg);
    cmd.arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines().take(2).collect::<Vec<_>>().join("\n")
        }
        Ok(out) => format!("ffmpeg -version : code {:?}", out.status.code()),
        Err(e) => format!("ffmpeg introuvable ({e}) — décodage vidéo indisponible ?"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("conduite-diag-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    /// L'expurgation couvre les trois formes du chemin personnel (brute,
    /// JSON-échappée, séparateurs `/`) et reste insensible à la casse.
    #[test]
    fn redaction_covers_all_path_forms() {
        let home = home_dir().expect("USERPROFILE/HOME défini");
        let raw = format!("chemin {home}\\shows\\demo");
        assert!(!redact(&raw).contains(&home), "{}", redact(&raw));
        assert!(redact(&raw).contains("~"), "{}", redact(&raw));

        let json = format!("{{\"path\":\"{}\\\\media\"}}", home.replace('\\', "\\\\"));
        let red = redact(&json);
        assert!(!red.to_lowercase().contains(&home.to_lowercase()), "{red}");

        let fwd = format!("file://{}/logs", home.replace('\\', "/"));
        assert!(!redact(&fwd).contains(&home.replace('\\', "/")));

        let upper = raw.to_uppercase();
        assert!(
            !redact(&upper).to_lowercase().contains(&home.to_lowercase()),
            "insensible à la casse"
        );
    }

    #[test]
    fn replace_ci_basic() {
        assert_eq!(replace_ci("aXbXc", "x", "-"), "a-b-c");
        assert_eq!(replace_ci("abc", "zz", "-"), "abc");
        assert_eq!(replace_ci("C:\\Users\\pym et c:\\users\\pym", "c:\\users\\pym", "~"), "~ et ~");
    }

    /// tail_lines rend exactement les N dernières lignes.
    #[test]
    fn tail_keeps_last_lines_only() {
        let dir = tmp("tail");
        let path = dir.join("conduite.test.log");
        let content: Vec<String> = (0..1000).map(|i| format!("ligne {i}")).collect();
        std::fs::write(&path, content.join("\n")).expect("write");
        let tail = tail_lines(&path, 500).expect("tail");
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines.len(), 500);
        assert_eq!(lines[0], "ligne 500");
        assert_eq!(lines[499], "ligne 999");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Génération complète : le zip existe, contient les entrées attendues,
    /// et AUCUNE trace du chemin personnel n'y survit.
    #[test]
    fn generate_produces_redacted_zip() {
        let dir = tmp("gen");
        let logs = dir.join("logs");
        let show = dir.join("shows").join("t");
        std::fs::create_dir_all(&logs).expect("mkdir logs");
        std::fs::create_dir_all(&show).expect("mkdir show");
        let home = home_dir().expect("home");
        std::fs::write(
            logs.join("conduite.2026-07-28.log"),
            format!("INFO demarrage base={home}\\portable\n"),
        )
        .expect("log");
        std::fs::write(dir.join("config.toml"), "http_port = 9820\n").expect("config");
        std::fs::write(
            show.join(conduite_core::SHOW_FILE),
            format!("{{\"name\":\"t\",\"chemin\":\"{}\"}}", home.replace('\\', "\\\\")),
        )
        .expect("show");

        let input = DiagnosticInput {
            logs_dir: logs.clone(),
            base_dir: dir.clone(),
            show_dir: show,
            version: "0.1.0-test".to_string(),
            health_json: "{\"cpu_pct\":1.0}".to_string(),
        };
        let path = generate(&input).expect("génération");
        assert!(path.is_file(), "zip écrit : {}", path.display());
        assert!(path.metadata().unwrap().len() < 11 * 1024 * 1024, "borné");

        let file = std::fs::File::open(&path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip lisible");
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        for expected in ["versions.txt", "sante.json", "config.toml", "show.json"] {
            assert!(names.iter().any(|n| n == expected), "{expected} absent : {names:?}");
        }
        assert!(
            names.iter().any(|n| n.starts_with("logs/conduite")),
            "log embarqué : {names:?}"
        );
        // Aucune trace du chemin personnel dans AUCUNE entrée.
        let home_lower = home.to_lowercase();
        for (i, name) in names.iter().enumerate() {
            let mut entry = archive.by_index(i).expect("entrée");
            let mut text = String::new();
            let _ = entry.read_to_string(&mut text);
            assert!(
                !text.to_lowercase().contains(&home_lower),
                "chemin personnel non expurgé dans {name}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
