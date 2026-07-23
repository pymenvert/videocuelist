//! # conduite-control-artnet
//!
//! Nœud Art-Net de Conduite — côté **récepteur** : la console lumière
//! (GrandMA, Chamsys, Dot2…) pilote le média-serveur en ArtDMX. Contrat
//! normatif : `docs/INTERFACES.md` (§ control-artnet).
//!
//! - Parsing des trames : [`parse_artdmx`], [`parse_artpoll`] ; réponse de
//!   découverte [`build_artpoll_reply`] (visible par DMX-Workshop & co).
//! - Cœur pur [`DmxMapper`] : patch [`conduite_core::PatchEntry`] →
//!   [`conduite_core::Command::ParamSet`] (8 bits = canal/255, 16 bits =
//!   (MSB<<8|LSB)/65535, mappé sur [min, max]), anti-spam par canal.
//! - [`SequenceTracker`] : tolère l'absence de séquençage, signale les gros
//!   sauts pour le journal.
//! - [`ArtnetNode`] : thread UDP (6454 en production), patch mis à jour à
//!   chaud via un canal.
//!
//! Le lissage fin vit dans la crate `params` : [`smoothing_overrides`]
//! produit les paires (addr, smoothing_ms) à passer à
//! `Registry::set_smoothing_override` au chargement du patch.

mod mapper;
mod node;
mod packet;

pub use mapper::{smoothing_overrides, DmxMapper, SequenceTracker, SEQUENCE_GAP_WARN};
pub use node::ArtnetNode;
pub use packet::{
    build_artdmx, build_artpoll, build_artpoll_reply, parse_artdmx, parse_artpoll, ArtDmx,
    ArtPoll, ARTNET_ID, ARTNET_PORT, OP_DMX, OP_POLL, OP_POLL_REPLY, POLL_REPLY_LEN, PROT_VER,
};
