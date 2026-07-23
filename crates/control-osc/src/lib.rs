//! # conduite-control-osc
//!
//! Pont OSC ↔ [`conduite_core::Command`] — voir docs/INTERFACES.md
//! (§ control-osc, normatif).
//!
//! Entrée ([`OscServer`], UDP port 9000 par défaut côté app) :
//! `/conduite/cue/go` `/conduite/cue/back` `/conduite/cue/goto 12.5|"12.5"`
//! `/conduite/param/<addr> f` `/conduite/master f` `/conduite/dbo f(fade_s)`
//! `/conduite/bpm f` `/conduite/bpm/tap`
//!
//! Sortie ([`OscFeedback`], hôte configurable) :
//! `/conduite/status/active s` `/conduite/status/standby s`
//! `/conduite/status/progress f` `/conduite/status/remaining f`
//!
//! Tolérant sur les types d'arguments (int/float/double/bool/string
//! numérique — pensé Chataigne/TouchOSC). Un message invalide est tracé et
//! ignoré : l'OSC ne plante jamais la régie.

mod feedback;
mod map;
mod packet;
mod ratelimit;
mod server;

pub use feedback::{FeedbackEvent, FeedbackState, OscFeedback, OscFeedbackHandle};
pub use map::map_message;
pub use packet::{count_bundles, flatten, MAX_BUNDLES};
pub use server::{OscServer, OscServerHandle};
