//! # conduite-params
//!
//! La colonne vertébrale des paramètres de Conduite : le [`Registry`] tient
//! toutes les valeurs contrôlables (opacité, gains, uniforms ISF, master…)
//! sous des adresses stables (`slice/1/opacity`, `master/intensity`…), avec :
//!
//! - clamp typé par [`ParamKind`] ;
//! - lissage exponentiel par paramètre (`smoothing_ms`, override possible,
//!   ex. réception DMX) via [`Registry::tick`] ;
//! - fondus de cue **stables** via [`Registry::blend_toward`] : l'interpolation
//!   part des valeurs mémorisées au début du fondu, pas de re-départ compound ;
//! - overrides « live » ([`Registry::set_live_override`]) : l'adresse n'est
//!   plus écrasée par les cues ;
//! - offsets de modulation additifs et **non persistants**
//!   ([`Registry::apply_modulation`]), réappliqués chaque frame.
//!
//! Contrat normatif : `docs/INTERFACES.md`, section `params`.

mod registry;
mod spec;

pub use registry::Registry;
pub use spec::{ParamKind, ParamSpec};
