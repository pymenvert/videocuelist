//! # conduite-media-library
//!
//! Bibliothèque de médias de Conduite : scan des dossiers `media/` et
//! `shaders/`, réconciliation des ids au rescan (ids stables par chemin),
//! sondage tolérant des métadonnées, vignettes JPEG via ffmpeg, chargement
//! d'images fixes en RGBA et « collecter le show » (dossier autonome).
//!
//! **Tout ici fait de l'IO disque** : à appeler depuis des tâches de fond,
//! jamais depuis le thread de rendu (doctrine SPEC §10).
//!
//! Contrat : `docs/INTERFACES.md`. Le sondage vidéo est injecté (voir
//! [`probe_all`]) : l'app branche `conduite_engine::probe` en une ligne.

pub mod collect;
pub mod images;
pub mod probe;
pub mod scan;
pub mod thumbs;

pub use collect::{collect_show, CollectReport};
pub use images::{load_image_rgba, ImageRgba};
pub use probe::{probe_all, ProbeInfo};
pub use scan::{
    media_kind, reconcile, reconcile_materials, scan, scan_materials, MediaKind, IMAGE_EXTS,
    MATERIAL_EXT, VIDEO_EXTS,
};
pub use thumbs::{
    ensure_thumb, ffmpeg_available, generate_thumbs, resolve_ffmpeg, thumb_path, ThumbReport,
    THUMB_WIDTH,
};
