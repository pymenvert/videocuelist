//! # conduite-compositor
//!
//! Tout le GL du produit, via [`glow`]. **Aucune création de fenêtre ici** :
//! le `glow::Context` est injecté par `app` (contexte partagé entre les
//! fenêtres de sortie).
//!
//! Pipeline par slice : textures vidéo A/B (uploads [`Compositor::upload_frame`])
//! ou rendu ISF offscreen (FBO par slice/deck), puis composition warpée
//! (homographie 4 coins de Lanterne), crossfade A/B avant correction couleur,
//! blend modes, master/DBO en multiplicateur final.
//!
//! Contrat normatif : `docs/INTERFACES.md` (section compositor).

mod cache;
mod gl;
pub mod homography;
pub mod shaders;

pub use cache::ProgramCache;
pub use gl::Compositor;
pub use shaders::GlslVersion;

use conduite_core::{PatternKind, Rect, SliceId};
use thiserror::Error;

/// Deck de lecture d'un slice. Doublon volontaire de `conduite_cue::DeckSlot`
/// (le graphe de dépendances interdit compositor → cue) ; `app` convertit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckSlot {
    A,
    B,
}

impl DeckSlot {
    /// Index 0/1 pour les tableaux internes `[T; 2]`.
    pub fn index(self) -> usize {
        match self {
            DeckSlot::A => 0,
            DeckSlot::B => 1,
        }
    }
}

/// Mode de fusion d'un slice sur la sortie (composition dans le framebuffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Add,
    Screen,
    Multiply,
}

/// État de dessin d'un slice pour une frame (résolu par `app` depuis les
/// paramètres lissés). Les textures sont retrouvées par `slice` (gérées par
/// le compositor).
#[derive(Debug, Clone)]
pub struct SliceDraw {
    pub slice: SliceId,
    /// Coins dans l'espace sortie normalisé 0..1, ordre TL,TR,BR,BL.
    pub corners: [[f32; 2]; 4],
    /// Fenêtre source (portion du média affichée), normalisée 0..1.
    pub src_rect: Rect,
    /// Ordre de composition (z croissant = dessiné au-dessus).
    pub z: i32,
    /// Opacité 0..1.
    pub opacity: f32,
    /// Gains RGB (neutre : 1,1,1).
    pub gains: [f32; 3],
    /// Luminosité 0..2 (neutre : 1).
    pub brightness: f32,
    /// Contraste 0..2 (neutre : 1).
    pub contrast: f32,
    /// Gamma 0.2..4 (neutre : 1).
    pub gamma: f32,
    pub blend_mode: BlendMode,
    /// Crossfade A→B des contenus : 0 = deck A, 1 = deck B.
    pub mix: f32,
    /// Through-black : 1 = noir complet (transition par le noir).
    pub black: f32,
    /// Mire de test à la place du contenu.
    pub pattern: Option<PatternKind>,
}

impl SliceDraw {
    /// État neutre : plein cadre, opaque, couleurs neutres, deck A.
    pub fn new(slice: SliceId) -> Self {
        Self {
            slice,
            corners: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            src_rect: Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            z: 0,
            opacity: 1.0,
            gains: [1.0, 1.0, 1.0],
            brightness: 1.0,
            contrast: 1.0,
            gamma: 1.0,
            blend_mode: BlendMode::Normal,
            mix: 0.0,
            black: 0.0,
            pattern: None,
        }
    }
}

/// Une sortie complète à rendre dans le framebuffer courant.
#[derive(Debug, Clone)]
pub struct OutputView<'a> {
    /// Dimensions du framebuffer cible en pixels.
    pub output_size: (u32, u32),
    /// Master intensity 0..1 (multiplicateur final).
    pub master: f32,
    /// DBO 0..1 (1 = blackout complet), combiné au master.
    pub dbo: f32,
    pub slices: &'a [SliceDraw],
}

/// Erreurs du compositor. Les logs GLSL sont COMPLETS (affichés dans l'UI).
#[derive(Debug, Error)]
pub enum CompositorError {
    #[error("initialisation GL : {0}")]
    Init(String),
    #[error("ressource GL : {0}")]
    Resource(String),
    #[error("compilation du shader {stage} :\n{log}")]
    ShaderCompile { stage: &'static str, log: String },
    #[error("édition de liens du programme GLSL :\n{log}")]
    ProgramLink { log: String },
    #[error("framebuffer incomplet (statut 0x{0:x})")]
    FramebufferIncomplete(u32),
}

/// Clés présentes dans `existing` mais absentes de `keep` — support pur de
/// l'élagage ([`Compositor::prune_slices`], [`ProgramCache::retain`]), testé
/// sans GL.
pub(crate) fn keys_not_kept<K: Copy + PartialEq>(
    existing: impl Iterator<Item = K>,
    keep: &[K],
) -> Vec<K> {
    existing.filter(|k| !keep.contains(k)).collect()
}

/// Remplit `indices` avec l'ordre de dessin des slices : z croissant,
/// **stable** (à z égal, l'ordre de déclaration est conservé). Le buffer est
/// réutilisé chaque frame — zéro allocation une fois la capacité atteinte.
pub(crate) fn sort_indices_by_z(indices: &mut Vec<usize>, slices: &[SliceDraw]) {
    indices.clear();
    indices.extend(0..slices.len());
    indices.sort_by_key(|&i| slices[i].z);
}

/// Facteurs `glBlendFunc(src, dst)` pour chaque mode. Le shader prémultiplie
/// la couleur en conséquence (voir `shaders::FRAGMENT_BODY`) :
/// - Normal : mélange alpha classique ;
/// - Add : `dst + src·opacité` ;
/// - Screen : `src + dst·(1 − src)` (opacité prémultipliée) ;
/// - Multiply : `dst · mix(1, src, opacité)`.
pub(crate) fn blend_func_for(mode: BlendMode) -> (u32, u32) {
    match mode {
        BlendMode::Normal => (glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA),
        BlendMode::Add => (glow::ONE, glow::ONE),
        BlendMode::Screen => (glow::ONE, glow::ONE_MINUS_SRC_COLOR),
        BlendMode::Multiply => (glow::DST_COLOR, glow::ZERO),
    }
}

/// Code `u_blend_mode` du shader pour chaque mode.
pub(crate) fn blend_code(mode: BlendMode) -> i32 {
    match mode {
        BlendMode::Normal => shaders::BLEND_NORMAL,
        BlendMode::Add => shaders::BLEND_ADD,
        BlendMode::Screen => shaders::BLEND_SCREEN,
        BlendMode::Multiply => shaders::BLEND_MULTIPLY,
    }
}

/// Fusionne des valeurs d'uniforms dans le stockage d'un matériau : mise à
/// jour en place par nom, ajout sinon. Pas de réallocation quand les noms
/// existent déjà (cas nominal : TIME/FRAMEINDEX chaque frame).
pub(crate) fn merge_uniforms(
    store: &mut Vec<(String, conduite_core::ParamValue)>,
    values: &[(String, conduite_core::ParamValue)],
) {
    for (name, value) in values {
        if let Some(slot) = store.iter_mut().find(|(n, _)| n == name) {
            slot.1 = value.clone();
        } else {
            store.push((name.clone(), value.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduite_core::ParamValue;

    fn draw(slice: SliceId, z: i32) -> SliceDraw {
        SliceDraw {
            z,
            ..SliceDraw::new(slice)
        }
    }

    #[test]
    fn keys_not_kept_filters_only_absent_keys() {
        let existing = [1u32, 2, 3, 4];
        assert_eq!(keys_not_kept(existing.iter().copied(), &[2, 4]), [1, 3]);
        // Tout conservé : rien à élaguer.
        assert!(keys_not_kept(existing.iter().copied(), &[1, 2, 3, 4]).is_empty());
        // `keep` vide : tout est élagué.
        assert_eq!(keys_not_kept(existing.iter().copied(), &[]), [1, 2, 3, 4]);
        // Clés de `keep` inconnues : ignorées sans effet.
        assert_eq!(keys_not_kept(existing.iter().copied(), &[9, 2, 3, 4]), [1]);
        // Source vide : rien à élaguer.
        assert!(keys_not_kept(std::iter::empty::<u32>(), &[1]).is_empty());
    }

    #[test]
    fn z_sort_is_ascending_and_stable() {
        let slices = [
            draw(10, 5),
            draw(11, -2),
            draw(12, 5), // même z que 10 : doit rester après lui
            draw(13, 0),
            draw(14, -2), // même z que 11 : doit rester après lui
        ];
        let mut indices = Vec::new();
        sort_indices_by_z(&mut indices, &slices);
        let order: Vec<SliceId> = indices.iter().map(|&i| slices[i].slice).collect();
        assert_eq!(order, [11, 14, 13, 10, 12]);
    }

    #[test]
    fn z_sort_reuses_buffer_without_realloc() {
        let slices = [draw(1, 3), draw(2, 1), draw(3, 2)];
        let mut indices = Vec::with_capacity(16);
        let cap = indices.capacity();
        for _ in 0..10 {
            sort_indices_by_z(&mut indices, &slices);
        }
        assert_eq!(indices.capacity(), cap, "aucune réallocation par frame");
        let order: Vec<SliceId> = indices.iter().map(|&i| slices[i].slice).collect();
        assert_eq!(order, [2, 3, 1]);
    }

    #[test]
    fn z_sort_empty_is_fine() {
        let mut indices = vec![9usize];
        sort_indices_by_z(&mut indices, &[]);
        assert!(indices.is_empty());
    }

    #[test]
    fn blend_funcs_match_gl_factors() {
        assert_eq!(
            blend_func_for(BlendMode::Normal),
            (glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA)
        );
        assert_eq!(blend_func_for(BlendMode::Add), (glow::ONE, glow::ONE));
        assert_eq!(
            blend_func_for(BlendMode::Screen),
            (glow::ONE, glow::ONE_MINUS_SRC_COLOR)
        );
        assert_eq!(
            blend_func_for(BlendMode::Multiply),
            (glow::DST_COLOR, glow::ZERO)
        );
    }

    #[test]
    fn blend_codes_are_distinct_and_normal_is_zero() {
        let codes = [
            blend_code(BlendMode::Normal),
            blend_code(BlendMode::Add),
            blend_code(BlendMode::Screen),
            blend_code(BlendMode::Multiply),
        ];
        assert_eq!(codes[0], 0);
        for (i, a) in codes.iter().enumerate() {
            for b in codes.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn deck_indices() {
        assert_eq!(DeckSlot::A.index(), 0);
        assert_eq!(DeckSlot::B.index(), 1);
    }

    #[test]
    fn merge_uniforms_updates_in_place_and_appends() {
        let mut store: Vec<(String, ParamValue)> = vec![
            ("TIME".into(), ParamValue::F(1.0)),
            ("level".into(), ParamValue::F(0.5)),
        ];
        merge_uniforms(
            &mut store,
            &[
                ("TIME".into(), ParamValue::F(2.0)),
                ("tint".into(), ParamValue::Color([1.0, 0.0, 0.0, 1.0])),
            ],
        );
        assert_eq!(store.len(), 3);
        assert_eq!(store[0], ("TIME".into(), ParamValue::F(2.0)));
        assert_eq!(store[1], ("level".into(), ParamValue::F(0.5)));
        assert_eq!(
            store[2],
            ("tint".into(), ParamValue::Color([1.0, 0.0, 0.0, 1.0]))
        );

        // Mise à jour répétée du même nom : la taille n'augmente plus
        // (cas nominal par frame : TIME/FRAMEINDEX).
        let cap = store.capacity();
        for i in 0..100 {
            merge_uniforms(&mut store, &[("TIME".into(), ParamValue::F(i as f32))]);
        }
        assert_eq!(store.len(), 3);
        assert_eq!(store.capacity(), cap);
    }
}
