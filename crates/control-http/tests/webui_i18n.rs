//! Garde-fou du bilingue FR/EN : **aucune chaîne française de la web UI ne
//! peut échapper au catalogue anglais**.
//!
//! Le français est la langue source (les chaînes vivent en clair dans
//! `webui/app.js`), l'anglais vit dans `webui/i18n.js`. Sans ce test, ajouter
//! un bouton en français passerait la CI et laisserait un mot français au
//! milieu de l'interface anglaise — le genre de détail qui décrédibilise un
//! produit vendu à l'étranger. Ici, une chaîne française non traduite = CI
//! rouge, avec la liste exacte des oublis.
//!
//! Le scanner regarde les littéraux entre apostrophes simples de `app.js`
//! (hors commentaires), les `data-tip="…"` de `index.html` et les textes moteur de
//! `core::ui_text`. Il retient tout ce qui n'est pas **manifestement
//! technique** — voir [`is_translatable`] : chercher « est-ce du français ? »
//! laisserait passer « Fondu », « Grille 4 » ou « Mires », qui n'ont ni accent
//! ni mot outil et sont pourtant à traduire.

use conduite_control_http::assets::{APP_JS, I18N_JS, INDEX_HTML};
use std::collections::{BTreeMap, BTreeSet};

/// Lettres accentuées : leur seule présence suffit à classer une chaîne.
const ACCENTS: &str = "àâäçéèêëîïôöùûüÿœæÀÂÄÇÉÈÊËÎÏÔÖÙÛÜŸŒÆ";

/// Mots outils français : attrapent les chaînes sans accent
/// (« Pas de place entre… », « Nom du slice », « ajout de cue »).
const FRENCH_WORDS: &[&str] = &[
    "le",
    "la",
    "les",
    "de",
    "des",
    "du",
    "un",
    "une",
    "et",
    "ou",
    "est",
    "sont",
    "pas",
    "sur",
    "dans",
    "par",
    "pour",
    "avec",
    "aucun",
    "aucune",
    "tous",
    "toutes",
    "sans",
    "au",
    "aux",
    "en",
    "se",
    "ce",
    "cette",
    "qui",
    "que",
    "plus",
    "moins",
    "vers",
    "entre",
    "puis",
    "depuis",
    "chaque",
    "non",
    "oui",
    "si",
    "nom",
    "nombre",
    "touche",
    "clic",
    "fichier",
    "dossier",
    "valeur",
    "couleur",
    "nouvelle",
    "nouveau",
    "impossible",
    "erreur",
    "attention",
    "reste",
    "ajout",
    "modification",
    "suppression",
    "assignation",
    "renommage",
    "introuvable",
    "vignette",
    "collecte",
    "cours",
    "actif",
    "inactif",
];

/// Constantes DOM / JavaScript qui ressemblent à des libellés mais n'en sont
/// pas : noms de balises, valeurs de `KeyboardEvent.key`, `rel` de lien.
const DOM_TOKENS: &[&str] = &[
    "DOMContentLoaded",
    "INPUT",
    "SELECT",
    "TEXTAREA",
    "BUTTON",
    "Enter",
    "Escape",
    "Shift",
    "Control",
    "Alt",
    "Meta",
    "AltGraph",
    "Dead",
    "Space",
    "Spacebar",
    "Tab",
    "Backspace",
    "Delete",
    "use strict",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Arrow",
    "noopener noreferrer",
];

/// Vrai si la chaîne doit porter une décision de traduction.
///
/// Le filtre est **inversé** par rapport à l'intuition : on ne cherche pas
/// « est-ce du français ? » (« Fondu », « Grille 4 », « Mires » n'ont ni
/// accent ni mot outil et sont pourtant à traduire), on écarte ce qui est
/// **manifestement technique** et on exige une décision pour tout le reste.
/// Une entrée identité (EN = FR) est une décision parfaitement valable —
/// c'est le silence qui ne l'est pas.
///
/// Ordre des règles : les signaux POSITIFS (accent, mot outil) passent en
/// premier, sinon une phrase comme « Chemin du fichier, relatif au dossier
/// media/ : » serait écartée pour cause de barre oblique.
fn is_translatable(s: &str) -> bool {
    if s.chars().count() < 2 || !s.chars().any(|c| c.is_alphabetic()) {
        return false;
    }
    if s.chars().any(|c| ACCENTS.contains(c)) {
        return true;
    }
    // Découpage sur tout ce qui n'est ni lettre ni chiffre : on compare des
    // MOTS entiers, sinon « des » attraperait « description ».
    let lower = s.to_lowercase();
    if lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| FRENCH_WORDS.contains(&w))
    {
        return true;
    }
    if DOM_TOKENS.contains(&s) {
        return false;
    }
    let first = s.chars().next().unwrap_or(' ');
    // Sélecteur CSS, couleur, icône SVG inline, longueur CSS.
    if first == '#'
        || first == '<'
        || first == '.'
        || s.starts_with("rgba(")
        || s.starts_with("rgb(")
    {
        return false;
    }
    if s.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.')
        .starts_with("px")
        && first.is_ascii_digit()
    {
        return false;
    }
    let has_space = s.contains(' ');
    // Adresse OSC, chemin, nom de commande : un seul mot avec `/` ou `_`.
    if !has_space && (s.contains('/') || s.contains('_')) {
        return false;
    }
    // Déclaration CSS en ligne (`margin:6px 0`) : l'attribut `style` n'est
    // jamais traduit, mais l'espace entre les valeurs la ferait passer.
    if s.contains(':')
        && !s.chars().any(|c| c.is_uppercase())
        && s.split(';').all(|decl| {
            let mut it = decl.splitn(2, ':');
            match (it.next(), it.next()) {
                (Some(prop), Some(_)) => {
                    !prop.is_empty()
                        && prop
                            .trim()
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c == '-')
                }
                _ => false,
            }
        })
    {
        return false;
    }
    // Liste de classes CSS : que des minuscules, chiffres, tirets et espaces.
    if s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == ' ')
    {
        return false;
    }
    first.is_uppercase() || has_space
}

/// Littéraux `'…'` de `app.js`, commentaires et chaînes double-quote exclus.
///
/// Mini-scanner d'états plutôt qu'une regex : `//` dans une chaîne (`'https://'`)
/// et `'` dans un commentaire (« l'UI ») sont des pièges réels de ce fichier.
fn single_quoted_literals(src: &str) -> Vec<String> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            '/' if i + 1 < b.len() && b[i + 1] == '/' => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < b.len() && b[i + 1] == '*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            '"' | '`' => {
                let q = c;
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            '\'' => {
                i += 1;
                let mut s = String::new();
                while i < b.len() && b[i] != '\'' && b[i] != '\n' {
                    if b[i] == '\\' && i + 1 < b.len() {
                        // Échappements réellement utilisés dans app.js.
                        match b[i + 1] {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            '\\' => s.push('\\'),
                            '\'' => s.push('\''),
                            other => {
                                s.push('\\');
                                s.push(other);
                            }
                        }
                        i += 2;
                        continue;
                    }
                    s.push(b[i]);
                    i += 1;
                }
                i += 1;
                out.push(s);
            }
            _ => i += 1,
        }
    }
    out
}

/// Valeurs des attributs `data-tip="…"` de `index.html` (infobulles statiques,
/// traduites à l'affichage par `installTooltips`).
fn html_tips(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(p) = rest.find("data-tip=\"") {
        rest = &rest[p + "data-tip=\"".len()..];
        if let Some(end) = rest.find('"') {
            out.push(rest[..end].to_string());
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    out
}

/// Catalogue anglais de `i18n.js`. Le bloc `var EN = { … };` est écrit en JSON
/// strict exprès : il se relit ici sans interpréter du JavaScript.
fn catalog() -> BTreeMap<String, String> {
    let start = I18N_JS
        .find("var EN = {")
        .expect("i18n.js : bloc `var EN = {` introuvable");
    let open = start + "var EN = ".len();
    let end = I18N_JS[open..]
        .find("\n  };")
        .expect("i18n.js : fin de catalogue `\\n  };` introuvable")
        + open;
    let json = &I18N_JS[open..end + "\n  }".len()];
    serde_json::from_str(json).unwrap_or_else(|e| {
        panic!("i18n.js : le catalogue n'est pas du JSON strict ({e}) — clés et valeurs entre guillemets doubles, pas de virgule finale, pas de commentaire à l'intérieur")
    })
}

/// Toutes les chaînes françaises que l'UI est susceptible d'afficher :
/// littéraux de `app.js`, infobulles statiques de `index.html`, et gabarits
/// d'avertissement du moteur (`runtime.warnings.key`) que la web UI recompose
/// avec `trf` — ceux-là ne sont écrits nulle part dans le JS, mais ils
/// s'affichent dans le centre « État du show ».
fn translatable_strings() -> BTreeSet<String> {
    let mut set: BTreeSet<String> = single_quoted_literals(APP_JS)
        .into_iter()
        .filter(|s| is_translatable(s))
        .collect();
    set.extend(
        html_tips(INDEX_HTML)
            .into_iter()
            .filter(|s| is_translatable(s)),
    );
    set.extend(conduite_core::ui_text::all().iter().map(|s| s.to_string()));
    set
}

/// Outil de maintenance (ignoré par défaut) : écrit dans
/// `target/i18n-fr-strings.json` la liste EXACTE des chaînes que le test
/// exige, pour préparer ou compléter le catalogue.
///
/// `cargo test -p conduite-control-http --test webui_i18n -- --ignored --nocapture`
#[test]
#[ignore = "outil de maintenance du catalogue, pas une vérification"]
fn dump_chaines_a_traduire() {
    let strings: Vec<String> = translatable_strings().into_iter().collect();
    let cat = catalog();
    let missing: Vec<&String> = strings.iter().filter(|s| !cat.contains_key(*s)).collect();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/i18n-fr-strings.json");
    let payload = serde_json::json!({ "all": strings, "missing": missing });
    std::fs::write(&path, serde_json::to_string_pretty(&payload).unwrap()).unwrap();
    println!(
        "{} chaînes à traduire, {} sans traduction → {}",
        strings.len(),
        missing.len(),
        path.display()
    );
}

#[test]
fn toute_chaine_de_l_ui_a_une_traduction() {
    let cat = catalog();
    let strings = translatable_strings();
    let missing: Vec<&String> = strings.iter().filter(|s| !cat.contains_key(*s)).collect();
    assert!(
        missing.is_empty(),
        "{} chaîne(s) de l'interface sans traduction dans webui/i18n.js :\n{}",
        missing.len(),
        missing
            .iter()
            .map(|s| format!("  {s:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn aucune_traduction_vide_ni_orpheline() {
    let cat = catalog();
    let live = translatable_strings();

    let empty: Vec<&String> = cat
        .iter()
        .filter(|(_, v)| v.trim().is_empty())
        .map(|(k, _)| k)
        .collect();
    assert!(
        empty.is_empty(),
        "traductions vides (le repli afficherait du français sans prévenir) : {empty:?}"
    );

    // Une entrée orpheline n'est pas une faute — c'est du poids mort qui finit
    // par diverger du produit. Elle sort du catalogue au même titre qu'un
    // libellé supprimé de l'UI.
    let orphans: Vec<&String> = cat.keys().filter(|k| !live.contains(*k)).collect();
    assert!(
        orphans.is_empty(),
        "{} entrée(s) du catalogue ne correspondent à aucune chaîne de la web UI (libellé supprimé ou reformulé ?) :\n{}",
        orphans.len(),
        orphans
            .iter()
            .map(|s| format!("  {s:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn gabarits_coherents_entre_les_deux_langues() {
    // `trf('Cue {0} dupliquée en {1}', …)` : si la traduction perd un {n},
    // la valeur disparaît de l'écran anglais sans erreur visible.
    let cat = catalog();
    let placeholders = |s: &str| -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let b: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] == '{' {
                if let Some(close) = b[i..].iter().position(|c| *c == '}') {
                    let inner: String = b[i + 1..i + close].iter().collect();
                    if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                        out.insert(inner);
                    }
                    i += close;
                }
            }
            i += 1;
        }
        out
    };
    let bad: Vec<String> = cat
        .iter()
        .filter(|(fr, en)| placeholders(fr) != placeholders(en))
        .map(|(fr, en)| format!("  {fr:?}\n    -> {en:?}"))
        .collect();
    assert!(
        bad.is_empty(),
        "gabarits dont les {{n}} ne correspondent pas entre FR et EN :\n{}",
        bad.join("\n")
    );
}

#[test]
fn identifiants_techniques_non_traduits() {
    // Le scanner est volontairement large : il ramasse aussi des identifiants
    // (`cue-row`, `cue_add`, `cue/go`, `en-GB`) parce que « cue » et « slice »
    // sont dans la liste des mots français. C'est assumé — toute chaîne doit
    // porter une décision explicite — mais un identifiant TRADUIT casserait
    // une classe CSS ou une commande WebSocket. Ici, il doit rester identique.
    let cat = catalog();
    // Tout en MINUSCULES : c'est ce qui distingue `cue-row` (classe CSS) de
    // « Re-scanner » (bouton), qui portent tous deux un tiret.
    let technical = |s: &str| {
        !s.is_empty()
            && !s.chars().any(char::is_whitespace)
            && s.chars().any(|c| c == '_' || c == '/' || c == '-')
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_/-.".contains(c))
    };
    let bad: Vec<String> = cat
        .iter()
        .filter(|(fr, en)| technical(fr) && fr != en)
        .map(|(fr, en)| format!("  {fr:?} -> {en:?}"))
        .collect();
    assert!(
        bad.is_empty(),
        "identifiants techniques traduits par erreur (classe CSS, commande, adresse OSC…) :\n{}",
        bad.join("\n")
    );
}

#[test]
fn ponctuation_francaise_absente_des_traductions() {
    // L'espace insécable avant « : » et les guillemets « » sont des règles
    // typographiques FRANÇAISES : les laisser en anglais trahit la traduction
    // automatique. On tolère les deux dans une valeur identique au français
    // (entrée volontairement non traduite, ex. « MIDI »).
    let cat = catalog();
    let bad: Vec<String> = cat
        .iter()
        .filter(|(fr, en)| fr != en)
        .filter(|(_, en)| {
            en.contains('«')
                || en.contains('»')
                || en.contains('\u{202f}')
                || en.contains(" :")
                || en.contains(" ;")
        })
        .map(|(fr, en)| format!("  {fr:?}\n    -> {en:?}"))
        .collect();
    assert!(
        bad.is_empty(),
        "ponctuation française dans des traductions anglaises (guillemets « », espace avant : ou ;) :\n{}",
        bad.join("\n")
    );
}
