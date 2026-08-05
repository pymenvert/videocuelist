//! Textes émis par le MOTEUR et affichés tels quels par l'interface.
//!
//! L'interface est bilingue (FR/EN) et son catalogue vit dans
//! `webui/i18n.js`, indexé par la chaîne française. Or quelques textes ne
//! viennent pas du JavaScript : les avertissements du centre « État du
//! show » et le contenu de `GET /about`. Sans point de rassemblement, ils
//! resteraient français au milieu d'une interface anglaise — et personne ne
//! s'en apercevrait avant un client.
//!
//! Ce module est ce point de rassemblement : **la source unique** des deux
//! côtés. Le test `webui_i18n` de `control-http` parcourt [`all`] et exige
//! une traduction pour chaque entrée, donc un texte ajouté ici sans
//! traduction fait rougir la CI. Corollaire assumé : reformuler une
//! constante, c'est changer une clé de catalogue — le test le signale.

/// Gabarits des avertissements publiés dans `runtime.warnings`.
///
/// Contrat : `{level, msg, key, args, action?}` — `msg` est la phrase
/// française déjà composée (journal, rapport de diagnostic, clients
/// anciens), `key` est le gabarit ci-dessous et `args` ses valeurs brutes
/// (chemins, noms, messages système : jamais traduits). La web UI recompose
/// `trf(key, …args)`.
pub mod warnings {
    /// Un média du show est introuvable sur disque. `{0}` = chemin relatif.
    pub const MEDIA_MISSING: &str = "média manquant : {0}";

    /// Débordement de la liste des médias manquants. `{0}` = nombre restant.
    pub const MEDIA_MISSING_MORE: &str = "… et {0} autres médias manquants";

    /// Service OSC entrant en erreur (port occupé, interface absente).
    /// `{0}` = message système brut.
    pub const PROTO_OSC_IN: &str = "OSC entrée : {0}";

    /// Service OSC sortant en erreur. `{0}` = message système brut.
    pub const PROTO_OSC_OUT: &str = "OSC sortie : {0}";

    /// Nœud Art-Net en erreur. `{0}` = message système brut.
    pub const PROTO_ARTNET: &str = "Art-Net : {0}";

    /// Port MIDI en erreur ou débranché. `{0}` = message système brut.
    pub const PROTO_MIDI: &str = "MIDI : {0}";

    /// Le moniteur d'une sortie plein écran a disparu : repli fenêtré.
    /// `{0}` = nom de la sortie.
    pub const MONITOR_LOST: &str =
        "sortie « {0} » : moniteur perdu, repli fenêtré (rebranchez l’écran)";

    /// Tous les gabarits d'avertissement.
    pub const ALL: &[&str] = &[
        MEDIA_MISSING,
        MEDIA_MISSING_MORE,
        PROTO_OSC_IN,
        PROTO_OSC_OUT,
        PROTO_ARTNET,
        PROTO_MIDI,
        MONITOR_LOST,
    ];
}

/// Textes de `GET /about`, affichés dans Réglages → À propos.
///
/// Les noms propres (Conduite, FFmpeg, MIT…) n'y figurent pas : ils ne se
/// traduisent pas. Seules y vivent la description du produit et les rôles
/// des composants tiers, qui sont des phrases.
pub mod about {
    /// Description du produit, sous le nom dans le panneau À propos.
    pub const DESCRIPTION: &str = "Régie vidéo de spectacle — cues, mapping, ISF, MIDI/OSC/Art-Net";

    /// Rôle de FFmpeg dans le produit.
    pub const ROLE_FFMPEG: &str = "décodage vidéo (programme séparé, appelé en sous-processus)";

    /// Libellé de la ligne de crédit des bibliothèques Rust.
    pub const NAME_RUST_DEPS: &str = "Dépendances Rust";

    /// Rôle des bibliothèques Rust.
    pub const ROLE_RUST_DEPS: &str = "bibliothèques du moteur et des surfaces de contrôle";

    /// Libellé de la ligne de crédit des shaders embarqués.
    pub const NAME_SHADERS: &str = "Shaders DomePack";

    /// Rôle des shaders embarqués.
    pub const ROLE_SHADERS: &str = "matériaux ISF embarqués";

    /// Tous les textes du panneau À propos.
    pub const ALL: &[&str] = &[
        DESCRIPTION,
        ROLE_FFMPEG,
        NAME_RUST_DEPS,
        ROLE_RUST_DEPS,
        NAME_SHADERS,
        ROLE_SHADERS,
    ];
}

/// Tous les textes moteur à traduire, pour la vérification de couverture.
pub fn all() -> Vec<&'static str> {
    warnings::ALL
        .iter()
        .chain(about::ALL.iter())
        .copied()
        .collect()
}

/// Compose la phrase française d'un gabarit — le champ `msg` du contrat.
///
/// Substitution positionnelle `{0}`, `{1}`… identique à `trf()` côté web UI :
/// les deux langues affichent donc rigoureusement la même information.
pub fn render(key: &str, args: &[String]) -> String {
    let mut out = key.to_string();
    for (i, a) in args.iter().enumerate() {
        out = out.replace(&format!("{{{i}}}"), a);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitue_dans_l_ordre() {
        assert_eq!(
            render(warnings::MEDIA_MISSING, &["clips/intro.mov".into()]),
            "média manquant : clips/intro.mov"
        );
        assert_eq!(
            render(warnings::PROTO_OSC_IN, &["port 9000 occupé".into()]),
            "OSC entrée : port 9000 occupé"
        );
        assert_eq!(
            render(warnings::MONITOR_LOST, &["Façade".into()]),
            "sortie « Façade » : moniteur perdu, repli fenêtré (rebranchez l’écran)"
        );
    }

    #[test]
    fn render_ignore_les_arguments_en_trop_et_les_trous_vides() {
        // Un gabarit sans trou reste intact ; un argument surnuméraire est
        // ignoré plutôt que de faire apparaître du texte parasite à l'écran.
        assert_eq!(render("texte fixe", &["ignoré".into()]), "texte fixe");
        assert_eq!(render(warnings::MEDIA_MISSING, &[]), "média manquant : {0}");
    }

    #[test]
    fn tous_les_textes_sont_uniques() {
        let mut seen = std::collections::BTreeSet::new();
        for k in all() {
            assert!(seen.insert(k), "texte dupliqué dans ui_text::all() : {k}");
        }
    }
}
