//! Gestion des lecteurs média par `(slice, deck)` : préchargement de la
//! standby, continuité (le player n'est jamais recréé quand le contenu ne
//! change pas), fins de média (Black/Hold/FollowNext), images fixes et
//! couleurs unies (upload unique).
//!
//! Le préchargement (ffprobe + spawn ffmpeg, décodage d'image) vit sur un
//! worker dédié : le tick poste une demande, le slot reste `Pending` (noir ou
//! dernière texture à l'écran) jusqu'à la réponse — JAMAIS d'I/O disque ni
//! d'attente de sous-process sur le thread de rendu. Les players évincés
//! sont aussi droppés sur ce worker (kill ffmpeg hors tick).
//!
//! Les matériaux ISF et les mires ne passent pas ici : ils sont rendus par
//! le compositor. Tout process ffmpeg est tué au drop (garanti par `engine`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use conduite_core::{Content, EndMode, MediaId, Playback, Show, SliceId};
use conduite_cue::{CueFrame, SceneTarget};
use conduite_engine::{FrameRgba, Player};
use conduite_media_library::{media_kind, ImageRgba, MediaKind};
use crossbeam_channel::{Receiver, Sender};
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
    /// Préchargement en cours sur le worker (génération de la demande) :
    /// rien à uploader, le compositor garde sa dernière texture.
    Pending(u64),
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

/// Demande postée au worker de préchargement.
enum LoadRequest {
    Video {
        gen: u64,
        slice: SliceId,
        path: PathBuf,
        playback: Playback,
    },
    Image {
        gen: u64,
        slice: SliceId,
        path: PathBuf,
    },
    /// Player évincé : droppé sur le worker (kill ffmpeg hors tick).
    Dispose(Box<dyn Player>),
}

/// Réponse du worker de préchargement.
enum LoadDone {
    Video {
        gen: u64,
        slice: SliceId,
        player: Box<dyn Player>,
    },
    Image {
        gen: u64,
        slice: SliceId,
        image: ImageRgba,
    },
    Failed {
        gen: u64,
        slice: SliceId,
        error: String,
    },
}

impl LoadDone {
    fn gen(&self) -> u64 {
        match self {
            LoadDone::Video { gen, .. } | LoadDone::Image { gen, .. } | LoadDone::Failed { gen, .. } => *gen,
        }
    }
}

/// Worker de préchargement (thread `conduite-preload`). Drop du `tx` =
/// arrêt propre du thread (les demandes en vol sont terminées puis jetées).
struct Loader {
    tx: Option<Sender<LoadRequest>>,
    rx: Receiver<LoadDone>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

impl Loader {
    fn spawn() -> Loader {
        let (tx, req_rx) = crossbeam_channel::unbounded::<LoadRequest>();
        let (done_tx, rx) = crossbeam_channel::unbounded::<LoadDone>();
        let thread = std::thread::Builder::new()
            .name("conduite-preload".into())
            .spawn(move || {
                while let Ok(req) = req_rx.recv() {
                    let done = match req {
                        LoadRequest::Video { gen, slice, path, playback } => {
                            match conduite_engine::open_ffmpeg(&path, &playback) {
                                Ok(player) => LoadDone::Video { gen, slice, player },
                                Err(e) => LoadDone::Failed { gen, slice, error: e.to_string() },
                            }
                        }
                        LoadRequest::Image { gen, slice, path } => {
                            match conduite_media_library::load_image_rgba(&path) {
                                Ok(image) => LoadDone::Image { gen, slice, image },
                                Err(e) => LoadDone::Failed { gen, slice, error: e.to_string() },
                            }
                        }
                        LoadRequest::Dispose(player) => {
                            drop(player); // kill + wait ffmpeg, hors tick
                            continue;
                        }
                    };
                    if done_tx.send(done).is_err() {
                        break;
                    }
                }
                debug!(target: "app::players", "worker de préchargement arrêté");
            });
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                warn!(target: "app::players", error = %e,
                    "worker de préchargement impossible : médias indisponibles");
                None
            }
        };
        Loader {
            tx: thread.is_some().then_some(tx),
            rx,
            _thread: thread,
        }
    }

    /// Poste une demande ; `false` si le worker est indisponible.
    fn request(&self, req: LoadRequest) -> bool {
        match &self.tx {
            Some(tx) => tx.send(req).is_ok(),
            None => false,
        }
    }

    /// Droppe un player hors tick (repli : drop local si le worker est mort).
    fn dispose(&self, player: Box<dyn Player>) {
        if let Some(tx) = &self.tx {
            if let Err(e) = tx.send(LoadRequest::Dispose(player)) {
                drop(e.0); // worker mort : drop local (rare)
            }
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
    /// ThroughBlack avant la bascule : le deck B est GELÉ (ni horloge ni
    /// décodage) — le média de la cible démarre à la bascule à mi-course.
    freeze_b: bool,
    /// Worker de préchargement (jamais d'I/O sur le tick).
    loader: Loader,
    /// Générateur des générations de demandes de préchargement.
    next_gen: u64,
    /// Slices dont la cue vient d'être (ré)activée : un slot au même contenu
    /// mais déjà avancé/EOF doit repartir de son point d'entrée.
    restart_pending: HashSet<SliceId>,
}

impl Players {
    pub fn new(media_dir: PathBuf) -> Self {
        Players {
            media_dir,
            slots: HashMap::new(),
            alias_b: HashSet::new(),
            alias_fresh: HashSet::new(),
            transitioning: false,
            freeze_b: false,
            loader: Loader::spawn(),
            next_gen: 0,
            restart_pending: HashSet::new(),
        }
    }

    /// Détruit tous les lecteurs (chargement d'un autre show). Les players
    /// vidéo sont droppés sur le worker (kill ffmpeg hors tick).
    pub fn clear(&mut self) {
        for (_, slot) in self.slots.drain() {
            if let SlotKind::Video(p) = slot.kind {
                self.loader.dispose(p);
            }
        }
        self.alias_b.clear();
        self.alias_fresh.clear();
        self.restart_pending.clear();
    }

    /// Une cue vient d'être (ré)activée sur ce slice : si le contenu désiré
    /// est identique au slot existant mais que le player a déjà avancé (ou
    /// est en fin de média), le média repart de son point d'entrée au
    /// prochain `sync` — goto_after vers soi-même, deux cues consécutives au
    /// même média. Sans ce signal, le player EOF n'est jamais recréé (clé
    /// identique) et AfterMedia refire en boucle.
    pub fn request_restart(&mut self, slice: SliceId) {
        self.restart_pending.insert(slice);
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
    /// decks). L'élagage évince les players inutiles (kill ffmpeg hors tick).
    pub fn sync(&mut self, frame: &CueFrame, show: &Show, transitioning: bool) {
        self.transitioning = transitioning;
        self.freeze_b = frame.freeze_b;
        self.alias_fresh.clear();
        self.drain_loader();

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

        // Élagage des slots qui ne servent plus (players droppés hors tick).
        let dead: Vec<(SliceId, Deck)> = self
            .slots
            .keys()
            .filter(|k| !wanted.contains(*k))
            .copied()
            .collect();
        for k in dead {
            if let Some(slot) = self.slots.remove(&k) {
                if let SlotKind::Video(p) = slot.kind {
                    self.loader.dispose(p);
                }
            }
        }
        // Les demandes de restart ne valent que pour la synchro qui suit
        // l'activation.
        self.restart_pending.clear();
    }

    /// Installe les préchargements terminés par le worker dans leurs slots.
    /// Un slot disparu ou remplacé entre-temps ⇒ player évincé (hors tick).
    fn drain_loader(&mut self) {
        while let Ok(done) = self.loader.rx.try_recv() {
            let gen = done.gen();
            let found = self
                .slots
                .values_mut()
                .find(|s| matches!(s.kind, SlotKind::Pending(g) if g == gen));
            let Some(slot) = found else {
                if let LoadDone::Video { player, slice, .. } = done {
                    debug!(target: "app::players", slice, "préchargement obsolète : évincé");
                    self.loader.dispose(player);
                }
                continue;
            };
            match done {
                LoadDone::Video { slice, player, .. } => {
                    info!(target: "app::players", slice, "player préchargé (worker)");
                    slot.kind = SlotKind::Video(player);
                }
                LoadDone::Image { slice, image, .. } => {
                    debug!(target: "app::players", slice, "image préchargée (worker)");
                    slot.kind = SlotKind::Image(Some(image));
                }
                LoadDone::Failed { slice, error, .. } => {
                    warn!(target: "app::players", slice, error = %error,
                        "préchargement raté : placeholder");
                    slot.kind = SlotKind::Missing;
                }
            }
        }
    }

    /// Avance les horloges média. `speed_mult` : multiplicateur live
    /// (`slice/{id}/media/speed`). Le deck B n'avance qu'en transition — et
    /// jamais pendant la première moitié d'un ThroughBlack (`freeze_b`).
    pub fn advance(&mut self, dt_s: f64, speed_mult: impl Fn(SliceId) -> f32) {
        for ((slice, deck), slot) in &mut self.slots {
            let running =
                matches!(deck, Deck::A) || (self.transitioning && !self.freeze_b);
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
                    // (préchargement), puis pause via backpressure. Gelé
                    // pendant la première moitié d'un ThroughBlack (la frame
                    // de préchargement reste affichable à la bascule).
                    let want_frame = match deck {
                        Deck::A => true,
                        Deck::B => {
                            (self.transitioning && !self.freeze_b) || !slot.preloaded
                        }
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
                            data: img.data.clone().into(),
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
                // Préchargement en cours : rien à uploader, le compositor
                // garde sa dernière texture (jamais d'attente du worker).
                SlotKind::Pending(_) => {}
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
    /// deck (swap de fin de transition), ou poste un préchargement au worker
    /// (uniquement quand le contenu désiré change, jamais en régime établi).
    fn ensure_slot(
        &mut self,
        slice: SliceId,
        deck: Deck,
        key: &SlotKey,
        pb: &Option<Playback>,
        show: &Show,
    ) {
        let base_speed = pb.as_ref().map(|p| p.speed).unwrap_or(1.0);
        let restart = self.restart_pending.contains(&slice);
        let in_s = pb.as_ref().map(|p| p.in_s).unwrap_or(0.0);
        if let Some(slot) = self.slots.get_mut(&(slice, deck)) {
            if &slot.key == key {
                slot.base_speed = base_speed;
                if restart {
                    restart_if_advanced(slot, slice, in_s);
                }
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
                if restart {
                    restart_if_advanced(&mut slot, slice, in_s);
                }
                debug!(target: "app::players", slice, ?deck, "slot déplacé (continuité)");
                self.slots.insert((slice, deck), slot);
                return;
            }
        }
        // Création : préchargement posté au worker (slot `Pending`).
        let slot = self.create_slot(slice, key, pb, show, base_speed);
        self.slots.insert((slice, deck), slot);
    }

    fn create_slot(
        &mut self,
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
                Some(path) => {
                    let gen = self.fresh_gen();
                    if self.loader.request(LoadRequest::Image { gen, slice, path }) {
                        (SlotKind::Pending(gen), 0.0)
                    } else {
                        (SlotKind::Missing, 0.0)
                    }
                }
                None => (SlotKind::Missing, 0.0),
            },
            SlotKey::Video { media, .. } => {
                match resolve_media_path(show, *media, &self.media_dir) {
                    Some(path) => {
                        let gen = self.fresh_gen();
                        debug!(target: "app::players", slice, media,
                            path = %path.display(), "préchargement demandé");
                        let req = LoadRequest::Video {
                            gen,
                            slice,
                            path,
                            playback: playback.clone(),
                        };
                        if self.loader.request(req) {
                            (SlotKind::Pending(gen), playback.in_s)
                        } else {
                            (SlotKind::Missing, 0.0)
                        }
                    }
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

    fn fresh_gen(&mut self) -> u64 {
        self.next_gen = self.next_gen.wrapping_add(1);
        self.next_gen
    }
}

/// Redémarre le player d'un slot ré-activé au même contenu, s'il a déjà
/// avancé ou atteint sa fin : seek au point d'entrée (non bloquant, réalisé
/// par le superviseur du player), horloge et préchargement réinitialisés.
/// Une standby fraîchement préchargée (horloge encore au point d'entrée,
/// pas d'EOF) n'est pas touchée.
fn restart_if_advanced(slot: &mut Slot, slice: SliceId, in_s: f64) {
    if let SlotKind::Video(p) = &mut slot.kind {
        if p.eof() || slot.clock_s > in_s + 0.01 {
            p.seek(in_s);
            slot.clock_s = in_s;
            slot.preloaded = false;
            slot.black_pushed = false;
            debug!(target: "app::players", slice, in_s, "média redémarré (ré-activation)");
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
        data: vec![0, 0, 0, 255].into(),
        pts_s: 0.0,
    }
}

/// Frame 1×1 d'une couleur unie.
fn color_frame(rgba: [f32; 4]) -> FrameRgba {
    let to8 = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    FrameRgba {
        width: 1,
        height: 1,
        data: vec![to8(rgba[0]), to8(rgba[1]), to8(rgba[2]), to8(rgba[3])].into(),
        pts_s: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduite_engine::TestPlayer;

    fn video_key(media: u32) -> SlotKey {
        SlotKey::Video {
            media,
            in_ms: 0,
            out_ms: None,
            end: EndMode::Hold,
        }
    }

    /// Slot vidéo prêt (TestPlayer 1 s, Hold), horloge à `clock_s`.
    fn video_slot(clock_s: f64, preloaded: bool) -> Slot {
        let mut p = TestPlayer::new(8, 8, 30.0, 1.0);
        p.set_playback(&Playback {
            in_s: 0.0,
            out_s: None,
            speed: 1.0,
            end: EndMode::Hold,
        });
        p.play();
        Slot {
            key: video_key(1),
            kind: SlotKind::Video(Box::new(p)),
            clock_s,
            base_speed: 1.0,
            end: EndMode::Hold,
            preloaded,
            static_uploaded: false,
            black_pushed: false,
            last_frame: None,
            needs_reupload: false,
        }
    }

    fn players() -> Players {
        Players::new(std::env::temp_dir())
    }

    /// ThroughBlack première moitié : le deck B est GELÉ (horloge immobile),
    /// puis repart à la bascule (freeze levé).
    #[test]
    fn advance_freezes_deck_b_during_through_black_first_half() {
        let mut pl = players();
        pl.slots.insert((1, Deck::B), video_slot(0.0, true));
        pl.transitioning = true;
        pl.freeze_b = true;
        pl.advance(0.5, |_| 1.0);
        let clock = pl.slots[&(1, Deck::B)].clock_s;
        assert!(clock.abs() < 1e-9, "deck B gelé pendant la 1re moitié : {clock}");

        pl.freeze_b = false;
        pl.advance(0.5, |_| 1.0);
        let clock = pl.slots[&(1, Deck::B)].clock_s;
        assert!((clock - 0.5).abs() < 1e-9, "deck B repart à la bascule : {clock}");
    }

    /// Le deck A n'est jamais gelé par un ThroughBlack (il descend au noir
    /// mais son média continue).
    #[test]
    fn advance_never_freezes_deck_a() {
        let mut pl = players();
        pl.slots.insert((1, Deck::A), video_slot(0.0, true));
        pl.transitioning = true;
        pl.freeze_b = true;
        pl.advance(0.5, |_| 1.0);
        assert!((pl.slots[&(1, Deck::A)].clock_s - 0.5).abs() < 1e-9);
    }

    /// Ré-activation d'une cue au même contenu avec player en EOF : le média
    /// repart de son point d'entrée (seek + horloge + préchargement remis).
    #[test]
    fn restart_request_reseeds_eof_player() {
        let mut pl = players();
        let mut slot = video_slot(0.0, true);
        // Amène le TestPlayer (1 s, Hold) en EOF.
        if let SlotKind::Video(p) = &mut slot.kind {
            let _ = p.poll_frame(1.5);
            assert!(p.eof(), "préparation : player en EOF");
        }
        slot.clock_s = 1.5;
        pl.slots.insert((1, Deck::A), slot);

        pl.request_restart(1);
        let show = Show::new("test");
        pl.ensure_slot(1, Deck::A, &video_key(1), &Some(Playback::default()), &show);

        let slot = &pl.slots[&(1, Deck::A)];
        assert!(!slot.video_eof(), "seek : plus en EOF");
        assert!(slot.clock_s.abs() < 1e-9, "horloge recalée sur in_s");
        assert!(!slot.preloaded, "préchargement à refaire");
        assert!(!pl.media_eof(1), "l'oracle AfterMedia ne refire plus");
    }

    /// Une standby fraîchement préchargée (horloge au point d'entrée, pas
    /// d'EOF) n'est PAS redémarrée par le signal d'activation.
    #[test]
    fn restart_request_leaves_fresh_standby_alone() {
        let mut pl = players();
        pl.slots.insert((1, Deck::B), video_slot(0.0, true));
        pl.request_restart(1);
        let show = Show::new("test");
        pl.ensure_slot(1, Deck::B, &video_key(1), &Some(Playback::default()), &show);
        assert!(pl.slots[&(1, Deck::B)].preloaded, "standby fraîche non touchée");
    }

    /// Sans signal d'activation, un slot EOF au même contenu reste en EOF
    /// (Hold : on tient la dernière image, pas de redémarrage sauvage).
    #[test]
    fn no_restart_without_activation_signal() {
        let mut pl = players();
        let mut slot = video_slot(0.0, true);
        if let SlotKind::Video(p) = &mut slot.kind {
            let _ = p.poll_frame(1.5);
        }
        pl.slots.insert((1, Deck::A), slot);
        let show = Show::new("test");
        pl.ensure_slot(1, Deck::A, &video_key(1), &Some(Playback::default()), &show);
        assert!(pl.slots[&(1, Deck::A)].video_eof(), "Hold : EOF conservé");
    }

    /// Un slot `Pending` (préchargement en cours) n'est ni EOF ni malsain.
    #[test]
    fn pending_slot_is_neither_eof_nor_unhealthy() {
        let mut pl = players();
        let mut slot = video_slot(0.0, false);
        slot.kind = SlotKind::Pending(42);
        pl.slots.insert((1, Deck::A), slot);
        assert!(!pl.media_eof(1));
        assert!(!pl.any_unhealthy());
    }
}

