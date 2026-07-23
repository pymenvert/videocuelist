//! Gestion des lecteurs média par `(slice, deck)` : préchargement de la
//! standby, continuité (le player n'est jamais recréé quand le contenu ne
//! change pas), fins de média (Black/Hold/FollowNext), images fixes et
//! couleurs unies (upload unique).
//!
//! Les matériaux ISF et les mires ne passent pas ici : ils sont rendus par
//! le compositor. Tout process ffmpeg est tué au drop (garanti par `engine`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use conduite_core::{Content, EndMode, MediaId, Playback, Show, SliceId};
use conduite_cue::{CueFrame, SceneTarget};
use conduite_engine::{FrameRgba, Player};
use conduite_media_library::{media_kind, MediaKind};
use tracing::{debug, info, warn};

/// Deck d'un slice (A = programme, B = préparation). Copie locale avec
/// `Hash` pour les clés de table (les enums de `cue`/`compositor` n'en ont pas).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Deck {
    A,
    B,
}

/// Identité du contenu d'un slot : contenu + lecture SANS la vitesse
/// (la vitesse est réalisée par l'horloge, elle ne recrée jamais un player).
#[derive(Debug, Clone, PartialEq)]
enum SlotKey {
    Video {
        media: MediaId,
        in_ms: u64,
        out_ms: Option<u64>,
        end: EndMode,
    },
    Image {
        media: MediaId,
    },
    Color([u32; 4]),
}

/// Contenu vivant d'un slot.
enum SlotKind {
    Video(Box<dyn Player>),
    /// Image fixe décodée (None = chargement raté ⇒ noir + log déjà fait).
    Image(Option<conduite_media_library::ImageRgba>),
    Color([f32; 4]),
    /// Média manquant / illisible : rien à uploader (placeholder au rendu).
    Missing,
}

/// Un lecteur actif sur un deck d'un slice.
struct Slot {
    key: SlotKey,
    kind: SlotKind,
    /// Horloge média (secondes sur la ligne de temps du média, part de in_s).
    clock_s: f64,
    /// Vitesse de base de la cue (multipliée par le paramètre live).
    base_speed: f32,
    end: EndMode,
    /// Première frame décodée et uploadée (préchargement standby fait).
    preloaded: bool,
    /// Upload unique effectué (images, couleurs, frame noire de fin).
    static_uploaded: bool,
    /// Frame noire de fin (EndMode::Black) déjà poussée.
    black_pushed: bool,
    /// Dernière frame servie — ré-upload après déplacement de deck.
    last_frame: Option<FrameRgba>,
    /// Le slot vient de changer de deck : ré-upload de la dernière frame.
    needs_reupload: bool,
}

impl Slot {
    fn video_eof(&self) -> bool {
        match &self.kind {
            SlotKind::Video(p) => p.eof(),
            _ => false,
        }
    }
}

/// Table des lecteurs par `(slice, deck)`.
pub struct Players {
    media_dir: PathBuf,
    slots: HashMap<(SliceId, Deck), Slot>,
    /// Slices dont le deck B a un contenu identique au deck A : un seul
    /// player, upload sur les deux decks.
    alias_b: HashSet<SliceId>,
    /// Aliases créés à la dernière synchro : forcer un upload B initial.
    alias_fresh: HashSet<SliceId>,
    /// Une transition est en cours (le deck B avance).
    transitioning: bool,
}

impl Players {
    pub fn new(media_dir: PathBuf) -> Self {
        Players {
            media_dir,
            slots: HashMap::new(),
            alias_b: HashSet::new(),
            alias_fresh: HashSet::new(),
            transitioning: false,
        }
    }

    /// Détruit tous les lecteurs (chargement d'un autre show).
    pub fn clear(&mut self) {
        self.slots.clear();
        self.alias_b.clear();
        self.alias_fresh.clear();
    }

    /// Oracle de fin de média pour le moteur de cues : le média du deck A
    /// du slice est-il terminé ?
    pub fn media_eof(&self, slice: SliceId) -> bool {
        self.slots
            .get(&(slice, Deck::A))
            .map(|s| s.video_eof())
            .unwrap_or(false)
    }

    /// Synchronise les slots sur l'état désiré des decks (préchargement de
    /// la standby compris). Déplace les slots au swap B→A sans les recréer ;
    /// contenu identique A/B ⇒ un seul player (alias, upload sur les deux
    /// decks). L'élagage droppe les players inutiles (kill ffmpeg garanti).
    pub fn sync(&mut self, frame: &CueFrame, show: &Show, transitioning: bool) {
        self.transitioning = transitioning;
        self.alias_fresh.clear();

        let desired_a = normalize_keys(show, targets_of(frame.deck_a.as_ref()));
        let desired_b = normalize_keys(show, targets_of(frame.deck_b.as_ref()));

        // Deck A d'abord : le swap de fin de transition déplace l'ancien B.
        for (slice, key, pb) in &desired_a {
            self.ensure_slot(*slice, Deck::A, key, pb, show);
        }

        let mut wanted: HashSet<(SliceId, Deck)> =
            desired_a.iter().map(|(s, _, _)| (*s, Deck::A)).collect();
        let prev_alias = std::mem::take(&mut self.alias_b);

        for (slice, key, pb) in &desired_b {
            let same_as_a = self
                .slots
                .get(&(*slice, Deck::A))
                .map(|slot| &slot.key == key)
                .unwrap_or(false);
            if same_as_a {
                // Continuité : même contenu sur les deux decks, un seul player.
                if !prev_alias.contains(slice) {
                    self.alias_fresh.insert(*slice);
                }
                self.alias_b.insert(*slice);
                self.slots.remove(&(*slice, Deck::B));
            } else {
                self.ensure_slot(*slice, Deck::B, key, pb, show);
                wanted.insert((*slice, Deck::B));
            }
        }

        // Élagage des slots qui ne servent plus (drop = kill ffmpeg).
        self.slots.retain(|k, _| wanted.contains(k));
    }

    /// Avance les horloges média. `speed_mult` : multiplicateur live
    /// (`slice/{id}/media/speed`). Le deck B n'avance qu'en transition.
    pub fn advance(&mut self, dt_s: f64, speed_mult: impl Fn(SliceId) -> f32) {
        for ((slice, deck), slot) in &mut self.slots {
            let running = matches!(deck, Deck::A) || self.transitioning;
            if !running {
                continue;
            }
            if let SlotKind::Video(player) = &mut slot.kind {
                if player.eof() {
                    continue; // Hold/Black/FollowNext : on n'avance plus.
                }
                if !slot.preloaded {
                    continue; // la première frame cale l'horloge sur in_s
                }
                player.play();
                let mult = speed_mult(*slice).max(0.0);
                slot.clock_s += dt_s * f64::from(slot.base_speed.max(0.0)) * f64::from(mult);
            }
        }
    }

    /// Sonde les lecteurs et pousse les frames à uploader. Le callback est
    /// branché sur `Compositor::upload_frame` (no-op en headless).
    pub fn poll_uploads(&mut self, upload: &mut dyn FnMut(SliceId, Deck, &FrameRgba)) {
        for ((slice, deck), slot) in &mut self.slots {
            let alias = matches!(deck, Deck::A) && self.alias_b.contains(slice);
            match &mut slot.kind {
                SlotKind::Video(player) => {
                    // Standby (deck B hors transition) : décoder UNE frame
                    // (préchargement), puis pause via backpressure.
                    let want_frame = match deck {
                        Deck::A => true,
                        Deck::B => self.transitioning || !slot.preloaded,
                    };
                    if slot.needs_reupload {
                        if let Some(f) = &slot.last_frame {
                            upload(*slice, *deck, f);
                        }
                        slot.needs_reupload = false;
                    }
                    if player.eof() {
                        if slot.end == EndMode::Black && !slot.black_pushed {
                            let black = black_frame();
                            upload(*slice, *deck, &black);
                            if alias {
                                upload(*slice, Deck::B, &black);
                            }
                            slot.black_pushed = true;
                        }
                        continue;
                    }
                    if !want_frame {
                        player.pause();
                        continue;
                    }
                    player.play();
                    if let Some(frame) = player.poll_frame(slot.clock_s) {
                        upload(*slice, *deck, &frame);
                        if alias {
                            upload(*slice, Deck::B, &frame);
                        }
                        if !slot.preloaded {
                            slot.preloaded = true;
                            // L'horloge repart de la position réelle servie.
                            slot.clock_s = frame.pts_s.max(slot.clock_s);
                        }
                        slot.last_frame = Some(frame);
                    } else if alias && self.alias_fresh.contains(slice) {
                        if let Some(f) = &slot.last_frame {
                            upload(*slice, Deck::B, f);
                        }
                    }
                }
                SlotKind::Image(img) => {
                    if slot.static_uploaded && !slot.needs_reupload && !alias {
                        continue;
                    }
                    if let Some(img) = img {
                        let frame = FrameRgba {
                            width: img.width,
                            height: img.height,
                            data: img.data.clone(),
                            pts_s: 0.0,
                        };
                        if !slot.static_uploaded || slot.needs_reupload {
                            upload(*slice, *deck, &frame);
                        }
                        if alias && self.alias_fresh.contains(slice) {
                            upload(*slice, Deck::B, &frame);
                        }
                    }
                    slot.static_uploaded = true;
                    slot.needs_reupload = false;
                }
                SlotKind::Color(rgba) => {
                    let need_self = !slot.static_uploaded || slot.needs_reupload;
                    let need_alias = alias && self.alias_fresh.contains(slice);
                    if need_self || need_alias {
                        let frame = color_frame(*rgba);
                        if need_self {
                            upload(*slice, *deck, &frame);
                        }
                        if need_alias {
                            upload(*slice, Deck::B, &frame);
                        }
                    }
                    slot.static_uploaded = true;
                    slot.needs_reupload = false;
                }
                SlotKind::Missing => {}
            }
        }
    }

    /// Un player vidéo est-il en mauvaise santé ? (bandeau santé)
    pub fn any_unhealthy(&self) -> bool {
        self.slots.values().any(|s| match &s.kind {
            SlotKind::Video(p) => !p.healthy(),
            _ => false,
        })
    }

    // -------------------------------------------------------------- interne

    /// Garantit un slot au bon contenu : réutilise, déplace depuis l'autre
    /// deck (swap de fin de transition), ou crée (IO de préchargement —
    /// uniquement quand le contenu désiré change, jamais en régime établi).
    fn ensure_slot(
        &mut self,
        slice: SliceId,
        deck: Deck,
        key: &SlotKey,
        pb: &Option<Playback>,
        show: &Show,
    ) {
        let base_speed = pb.as_ref().map(|p| p.speed).unwrap_or(1.0);
        if let Some(slot) = self.slots.get_mut(&(slice, deck)) {
            if &slot.key == key {
                slot.base_speed = base_speed;
                return;
            }
        }
        // Swap : le contenu désiré vit sur l'autre deck → déplacement.
        let other = match deck {
            Deck::A => Deck::B,
            Deck::B => Deck::A,
        };
        let movable = self
            .slots
            .get(&(slice, other))
            .map(|slot| &slot.key == key)
            .unwrap_or(false);
        if movable {
            if let Some(mut slot) = self.slots.remove(&(slice, other)) {
                slot.needs_reupload = true;
                slot.base_speed = base_speed;
                debug!(target: "app::players", slice, ?deck, "slot déplacé (continuité)");
                self.slots.insert((slice, deck), slot);
                return;
            }
        }
        // Création (préchargement).
        let slot = self.create_slot(slice, key, pb, show, base_speed);
        self.slots.insert((slice, deck), slot);
    }

    fn create_slot(
        &self,
        slice: SliceId,
        key: &SlotKey,
        pb: &Option<Playback>,
        show: &Show,
        base_speed: f32,
    ) -> Slot {
        let playback = pb.clone().unwrap_or_default();
        let end = playback.end;
        let (kind, clock_s) = match key {
            SlotKey::Color(bits) => {
                let rgba = [
                    f32::from_bits(bits[0]),
                    f32::from_bits(bits[1]),
                    f32::from_bits(bits[2]),
                    f32::from_bits(bits[3]),
                ];
                (SlotKind::Color(rgba), 0.0)
            }
            SlotKey::Image { media } => match resolve_media_path(show, *media, &self.media_dir) {
                Some(path) => match conduite_media_library::load_image_rgba(&path) {
                    Ok(img) => (SlotKind::Image(Some(img)), 0.0),
                    Err(e) => {
                        warn!(target: "app::players", slice, media, error = %e,
                            "image illisible : placeholder");
                        (SlotKind::Missing, 0.0)
                    }
                },
                None => (SlotKind::Missing, 0.0),
            },
            SlotKey::Video { media, .. } => {
                match resolve_media_path(show, *media, &self.media_dir) {
                    Some(path) => match conduite_engine::open_ffmpeg(&path, &playback) {
                        Ok(player) => {
                            info!(target: "app::players", slice, media,
                                path = %path.display(), "player préchargé");
                            (SlotKind::Video(player), playback.in_s)
                        }
                        Err(e) => {
                            warn!(target: "app::players", slice, media, error = %e,
                                "ouverture ffmpeg impossible : placeholder");
                            (SlotKind::Missing, 0.0)
                        }
                    },
                    None => (SlotKind::Missing, 0.0),
                }
            }
        };
        Slot {
            key: key.clone(),
            kind,
            clock_s,
            base_speed,
            end,
            preloaded: false,
            static_uploaded: false,
            black_pushed: false,
            last_frame: None,
            needs_reupload: false,
        }
    }
}

/// Chemin absolu d'un média du show (None si manquant/invalide, déjà loggué
/// au chargement du show).
fn resolve_media_path(show: &Show, id: MediaId, media_dir: &std::path::Path) -> Option<PathBuf> {
    let m = show.media.iter().find(|m| m.id == id)?;
    if m.missing || conduite_core::validate_relative_path(&m.path).is_err() {
        return None;
    }
    Some(media_dir.join(&m.path))
}

/// Extrait les cibles « à player » d'une scène (médias et couleurs — les
/// mires, matériaux et `None` ne consomment pas de lecteur).
fn targets_of(scene: Option<&SceneTarget>) -> Vec<(SliceId, SlotKey, Option<Playback>)> {
    let Some(scene) = scene else {
        return Vec::new();
    };
    scene
        .per_slice
        .iter()
        .filter_map(|t| {
            let key = match &t.content {
                Content::Media(id) => {
                    // Image ou vidéo ? Décidé par l'extension du chemin, mais
                    // ici on n'a pas le show : on repousse au create ? Non —
                    // la clé doit être stable : Media = vidéo par défaut,
                    // corrigé par `key_for_media` côté appelant. Voir sync().
                    let pb = t.playback.clone().unwrap_or_default();
                    SlotKey::Video {
                        media: *id,
                        in_ms: (pb.in_s.max(0.0) * 1000.0) as u64,
                        out_ms: pb.out_s.map(|o| (o.max(0.0) * 1000.0) as u64),
                        end: pb.end,
                    }
                }
                Content::Color(rgba) => SlotKey::Color([
                    rgba[0].to_bits(),
                    rgba[1].to_bits(),
                    rgba[2].to_bits(),
                    rgba[3].to_bits(),
                ]),
                Content::None | Content::Material(_) | Content::Pattern(_) => return None,
            };
            Some((t.slice, key, t.playback.clone()))
        })
        .collect()
}

impl Players {
    /// Corrige la clé d'un média selon sa nature réelle (image fixe vs
    /// vidéo) — appelé par `sync` via `normalize_keys`.
    fn normalize_key(show: &Show, key: SlotKey) -> SlotKey {
        if let SlotKey::Video { media, .. } = &key {
            if let Some(m) = show.media.iter().find(|m| m.id == *media) {
                if media_kind(std::path::Path::new(&m.path)) == Some(MediaKind::Image) {
                    return SlotKey::Image { media: *media };
                }
            }
        }
        key
    }
}

/// Applique [`Players::normalize_key`] à une liste de cibles.
fn normalize_keys(
    show: &Show,
    targets: Vec<(SliceId, SlotKey, Option<Playback>)>,
) -> Vec<(SliceId, SlotKey, Option<Playback>)> {
    targets
        .into_iter()
        .map(|(s, k, p)| (s, Players::normalize_key(show, k), p))
        .collect()
}

/// Frame noire 1×1 (fin de média `EndMode::Black`).
fn black_frame() -> FrameRgba {
    FrameRgba {
        width: 1,
        height: 1,
        data: vec![0, 0, 0, 255],
        pts_s: 0.0,
    }
}

/// Frame 1×1 d'une couleur unie.
fn color_frame(rgba: [f32; 4]) -> FrameRgba {
    let to8 = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    FrameRgba {
        width: 1,
        height: 1,
        data: vec![to8(rgba[0]), to8(rgba[1]), to8(rgba[2]), to8(rgba[3])],
        pts_s: 0.0,
    }
}

