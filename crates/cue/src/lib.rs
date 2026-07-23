//! # conduite-cue
//!
//! Moteur de conduite : GO/BACK/GOTO/STANDBY/PANIC, transitions A→B
//! (cut, crossfade, fondu par le noir), follows (fin de média, wait),
//! boucles de section. **Crate PURE** : aucune IO, aucun GL — l'appelant
//! fournit l'horloge et l'oracle de fin de média via [`EngineTick`].
//!
//! Contrat normatif : `docs/INTERFACES.md` (§ cue). La fiabilité du
//! spectacle repose sur ce module : jamais de panic, jamais d'état corrompu.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use conduite_core::{
    Content, Cue, CueNumber, Curve, EndMode, FollowMode, ParamValue, Playback, SliceId,
    Transition, TransitionKind,
};
use tracing::{debug, warn};

/// Emplacement de deck : A = programme, B = préparation / cible de transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckSlot {
    A,
    B,
}

/// Entrée d'un tick moteur : horloge monotone (secondes) et oracle de fin
/// de média (l'app interroge ses players, le moteur reste pur).
pub struct EngineTick<'a> {
    pub now_s: f64,
    pub media_eof: &'a dyn Fn(SliceId) -> bool,
}

/// Cible d'un slice : contenu + réglages de lecture. L'app compare
/// `(slice, content, playback)` entre A et B pour la continuité
/// (même média sur même slice ⇒ le player n'est pas recréé).
#[derive(Debug, Clone, PartialEq)]
pub struct SliceTarget {
    pub slice: SliceId,
    pub content: Content,
    pub playback: Option<Playback>,
}

/// Scène résolue d'une cue : cibles par slice + paramètres scénarisés
/// fusionnés. Résolue au `load` (préchargement : la standby est prête).
#[derive(Debug, Clone, PartialEq)]
pub struct SceneTarget {
    pub per_slice: Vec<SliceTarget>,
    pub params: BTreeMap<String, ParamValue>,
}

/// Événements émis par le moteur pendant un tick.
#[derive(Debug, Clone, PartialEq)]
pub enum CueEvent {
    /// GO accepté : la transition d'entrée de `cue` démarre.
    CueStarted { cue: CueNumber },
    /// Transition d'entrée terminée : `cue` est pleinement au programme.
    TransitionFinished { cue: CueNumber },
    /// Follow armé à la fin de la transition d'entrée (Wait ou AfterMedia).
    FollowArmed { cue: CueNumber, target: CueNumber },
    /// Follow déclenché : GO automatique de `cue` vers `target`.
    FollowFired { cue: CueNumber, target: CueNumber },
    /// Fondu au noir d'urgence engagé.
    PanicStarted { fade_s: f32 },
    /// Commande impossible (goto inexistant, GO en fin de conduite…) : no-op.
    Warning { message: String },
}

/// État désiré des decks pour la frame courante.
#[derive(Debug, Clone, PartialEq)]
pub struct CueFrame {
    /// Scène au programme (cue active). `None` avant le premier GO.
    pub deck_a: Option<SceneTarget>,
    /// Cible de la transition en cours, sinon la standby (préchargement).
    pub deck_b: Option<SceneTarget>,
    /// Alpha de blend A→B : 0 = A plein, 1 = B plein.
    pub blend: f32,
    /// Noir global 0..1 (fondu par le noir + panic), à appliquer au master.
    pub black: f32,
    /// Snapshot cible + alpha de courbe pour `params::Registry::blend_toward`.
    /// `Some(_, 1.0)` sur le tick de fin de transition, `None` au repos.
    pub params_target: Option<(BTreeMap<String, ParamValue>, f32)>,
    /// ThroughBlack, première moitié (avant la bascule à mi-course) : le
    /// deck B n'est pas encore révélé — l'app doit GELER ses players B
    /// (le média de la cible démarre à la bascule, pas au début du fondu).
    pub freeze_b: bool,
    pub events: Vec<CueEvent>,
}

/// État lisible de la conduite (UI, feedback OSC).
#[derive(Debug, Clone, PartialEq)]
pub struct CueStatus {
    pub active: Option<CueNumber>,
    pub standby: Option<CueNumber>,
    /// 0..1 : progression de la transition en cours, sinon du wait ou de la
    /// durée média connue (points IN/OUT) de la cue active.
    pub progress: f32,
    /// Temps restant en secondes quand une durée est connue.
    pub remaining_s: Option<f32>,
    pub transition_active: bool,
}

/// Transition d'entrée en cours (Cut n'est jamais stocké : bascule immédiate).
#[derive(Debug, Clone)]
struct ActiveTransition {
    to: usize,
    kind: TransitionKind,
    dur_s: f32,
    curve: Curve,
    start_s: f64,
}

/// Fondu au noir d'urgence en cours.
#[derive(Debug, Clone)]
struct PanicState {
    start_s: f64,
    fade_s: f32,
    /// Niveau de noir au moment du déclenchement (re-panic pendant un fondu).
    from: f32,
}

/// Commandes latchées entre deux ticks, appliquées dans l'ordre au tick
/// suivant sur l'horloge du tick (déterminisme, horloge simulée en test).
#[derive(Debug, Clone)]
enum Pending {
    Go,
    Back,
    Goto(CueNumber),
    Standby(CueNumber),
    Panic(f32),
}

/// Le moteur de conduite. Voir `docs/INTERFACES.md` (§ cue).
#[derive(Debug, Default)]
pub struct CueEngine {
    /// Cues triées par numéro (ordre total de CueNumber).
    cues: Vec<Cue>,
    /// Scènes résolues, index aligné sur `cues` (préchargement).
    targets: Vec<SceneTarget>,
    active: Option<usize>,
    standby: Option<usize>,
    transition: Option<ActiveTransition>,
    /// Instant de fin de la transition d'entrée de la cue active
    /// (origine des Wait et des progressions média).
    cue_start_s: f64,
    /// Le follow de la cue active a déjà tiré (une seule fois par activation).
    follow_fired: bool,
    panic: Option<PanicState>,
    pending: Vec<Pending>,
    events: Vec<CueEvent>,
    last_now_s: f64,
    /// Snapshots des cues activées CE tick, fusionnés dans l'ordre (le
    /// dernier l'emporte par adresse) : émis avec l'alpha final 1.0. Un GO
    /// pendant une transition peut activer deux cues dans le même tick
    /// (snap puis Cut) — chaque snapshot doit être posé, pas seulement
    /// celui de la dernière cue active.
    finished_params: Option<BTreeMap<String, ParamValue>>,
    /// Origine de l'horloge média de la cue active : début de la transition
    /// d'entrée (le deck B avance pendant la transition), mi-course pour
    /// ThroughBlack (deck B gelé avant la bascule, cf. `CueFrame::freeze_b`).
    media_start_s: f64,
    /// Multiplicateurs de vitesse live par slice (param
    /// `slice/{id}/media/speed` appliqué par l'app aux players), pour le
    /// compte à rebours AfterMedia. Absent = 1.0.
    speed_mult: BTreeMap<SliceId, f32>,
}

impl CueEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Charge la conduite : trie par numéro, résout toutes les scènes,
    /// réinitialise l'état (standby = première cue, rien au programme).
    pub fn load(&mut self, cues: &[Cue]) {
        let mut sorted = cues.to_vec();
        sorted.sort_by_key(|c| c.number);
        self.targets = sorted.iter().map(resolve_scene).collect();
        self.cues = sorted;
        self.active = None;
        self.standby = if self.cues.is_empty() { None } else { Some(0) };
        self.transition = None;
        self.cue_start_s = 0.0;
        self.follow_fired = false;
        self.panic = None;
        self.pending.clear();
        self.events.clear();
        self.finished_params = None;
        self.media_start_s = 0.0;
        self.speed_mult.clear();
        debug!(target: "cue", count = self.cues.len(), "conduite chargée");
    }

    /// Recharge la conduite À CHAUD (édition pendant la lecture) : re-résout
    /// les scènes en préservant la position par [`CueNumber`]. La cue active
    /// reste au programme — le deck A ne se vide pas, l'app ne tue pas ses
    /// players et le prochain GO avance au lieu de rejouer la cue courante.
    /// Les horloges de follow/média, le `follow_fired`, le panic et les
    /// multiplicateurs de vitesse sont conservés. Une transition en cours
    /// saute à sa fin (édition pendant un fondu : jamais d'état à cheval sur
    /// deux conduites). Cue active disparue ⇒ repli : plus rien au programme,
    /// standby sur l'ancienne standby si elle existe encore, sinon la
    /// première cue.
    pub fn load_hot(&mut self, cues: &[Cue]) {
        self.snap_transition(self.last_now_s);
        let active_n = self.active.map(|i| self.cues[i].number);
        let standby_n = self.standby.map(|i| self.cues[i].number);
        let keep_cue_start = self.cue_start_s;
        let keep_media_start = self.media_start_s;
        let keep_follow_fired = self.follow_fired;
        let keep_panic = self.panic.clone();
        let keep_speed = std::mem::take(&mut self.speed_mult);
        // Les événements du snap (et le snapshot de params à poser à 1.0)
        // doivent survivre au rechargement.
        let keep_events = std::mem::take(&mut self.events);
        let keep_finished = self.finished_params.take();

        self.load(cues);

        self.events = keep_events;
        self.finished_params = keep_finished;
        self.panic = keep_panic;
        self.speed_mult = keep_speed;
        match active_n.and_then(|n| self.index_of(n)) {
            Some(i) => {
                self.active = Some(i);
                self.cue_start_s = keep_cue_start;
                self.media_start_s = keep_media_start;
                self.follow_fired = keep_follow_fired;
                self.standby = standby_n
                    .and_then(|n| self.index_of(n))
                    .or_else(|| self.standby_after(i));
            }
            None => {
                if let Some(i) = standby_n.and_then(|n| self.index_of(n)) {
                    self.standby = Some(i);
                }
                // Sinon : `load` a déjà posé standby = première cue.
            }
        }
        debug!(target: "cue", count = self.cues.len(),
            active = ?self.active, "conduite rechargée à chaud");
    }

    /// GO : la cue en standby devient la cible du deck B, transition
    /// selon `Cue::transition` de la cible. Libère un éventuel panic.
    pub fn go(&mut self) {
        self.pending.push(Pending::Go);
    }

    /// BACK : GO inversé vers la cue précédente, avec la transition
    /// de la cue active (on ressort comme on est entré).
    pub fn back(&mut self) {
        self.pending.push(Pending::Back);
    }

    /// GOTO : GO direct vers un numéro. Inexistant ⇒ warning, no-op.
    pub fn goto(&mut self, n: CueNumber) {
        self.pending.push(Pending::Goto(n));
    }

    /// Change la cue en standby (cible du prochain GO / follow).
    pub fn standby(&mut self, n: CueNumber) {
        self.pending.push(Pending::Standby(n));
    }

    /// Fondu au noir global d'urgence en `fade_s` secondes. La conduite
    /// (active, standby, follows) continue sous le noir ; un GO/BACK/GOTO
    /// manuel relâche le noir.
    pub fn panic(&mut self, fade_s: f32) {
        self.pending.push(Pending::Panic(fade_s));
    }

    /// Multiplicateur de vitesse live d'un slice (param
    /// `slice/{id}/media/speed` que l'app applique à ses players) : pris en
    /// compte dans le compte à rebours AfterMedia de [`Self::status`].
    /// Valeur non finie ou ≤ 0 ⇒ ignorée (warning). Remis à 1.0 par `load`.
    pub fn set_speed_mult(&mut self, slice: SliceId, mult: f32) {
        if mult.is_finite() && mult > 0.0 {
            if (mult - 1.0).abs() < f32::EPSILON {
                self.speed_mult.remove(&slice);
            } else {
                self.speed_mult.insert(slice, mult);
            }
        } else {
            warn!(target: "cue", slice, mult, "multiplicateur de vitesse invalide ignoré");
        }
    }

    /// Appelé chaque frame. Retourne l'état désiré des decks, l'alpha de
    /// blend A→B, le noir global, le snapshot de paramètres interpolé et
    /// les événements du tick.
    pub fn tick(&mut self, t: EngineTick) -> CueFrame {
        let now = t.now_s;
        self.last_now_s = now;

        self.apply_pending(now);

        // La transition en cours a-t-elle atteint sa fin ?
        if let Some(tr) = &self.transition {
            if now - tr.start_s >= f64::from(tr.dur_s) {
                let to = tr.to;
                self.activate(to, now);
            }
        }

        self.check_follow(now, t.media_eof);

        let (blend, trans_black) = self.eval_transition(now);
        let black = trans_black.max(self.panic_black(now));

        let (deck_a, deck_b) = match &self.transition {
            Some(tr) => (
                self.active.map(|i| self.targets[i].clone()),
                Some(self.targets[tr.to].clone()),
            ),
            // Au repos : deck B expose la standby résolue (préchargement).
            None => (
                self.active.map(|i| self.targets[i].clone()),
                self.standby.map(|i| self.targets[i].clone()),
            ),
        };

        let params_target = if let Some(map) = self.finished_params.take() {
            // Tick de fin : alpha 1.0 pour poser exactement les valeurs
            // cibles (snapshots fusionnés de TOUTES les activations du tick).
            Some((map, 1.0))
        } else if let Some(tr) = &self.transition {
            let p = self.transition_progress(tr, now);
            Some((self.targets[tr.to].params.clone(), tr.curve.apply(p)))
        } else {
            None
        };

        // ThroughBlack avant la bascule : le deck B ne doit pas avancer.
        let freeze_b = match &self.transition {
            Some(tr) => {
                matches!(tr.kind, TransitionKind::ThroughBlack)
                    && self.transition_progress(tr, now) < 0.5
            }
            None => false,
        };

        CueFrame {
            deck_a,
            deck_b,
            blend,
            black,
            params_target,
            freeze_b,
            events: std::mem::take(&mut self.events),
        }
    }

    /// État lisible de la conduite (basé sur l'horloge du dernier tick).
    pub fn status(&self) -> CueStatus {
        let now = self.last_now_s;
        let active = self.active.map(|i| self.cues[i].number);
        let standby = self.standby.map(|i| self.cues[i].number);

        if let Some(tr) = &self.transition {
            let elapsed = (now - tr.start_s) as f32;
            return CueStatus {
                active,
                standby,
                progress: self.transition_progress(tr, now),
                remaining_s: Some((tr.dur_s - elapsed).max(0.0)),
                transition_active: true,
            };
        }

        let (progress, remaining_s) = match self.active.map(|i| &self.cues[i]) {
            None => (0.0, None),
            Some(cue) => match cue.follow {
                FollowMode::Wait(s) if s > 0.0 => {
                    // Le wait compte depuis la FIN de la transition d'entrée.
                    let elapsed = (now - self.cue_start_s) as f32;
                    ((elapsed / s).clamp(0.0, 1.0), Some((s - elapsed).max(0.0)))
                }
                FollowMode::Wait(_) => (1.0, Some(0.0)),
                // Manual/AfterMedia : durée média si les points IN/OUT la
                // donnent, sinon progression inconnue. Le média avance depuis
                // le DÉBUT de la transition d'entrée (mi-course pour
                // ThroughBlack), pas depuis sa fin : `media_start_s`.
                _ => {
                    let elapsed = (now - self.media_start_s) as f32;
                    media_progress(cue, elapsed, &self.speed_mult)
                }
            },
        };

        CueStatus {
            active,
            standby,
            progress,
            remaining_s,
            transition_active: false,
        }
    }

    // ------------------------------------------------------------- interne

    /// Applique les commandes latchées, dans l'ordre, sur l'horloge du tick.
    fn apply_pending(&mut self, now: f64) {
        let cmds = std::mem::take(&mut self.pending);
        for cmd in cmds {
            match cmd {
                Pending::Go => {
                    // Validation AVANT tout effet de bord : la standby
                    // effective est celle qui suivra le snap de la
                    // transition en cours. Un GO invalide (fin de conduite)
                    // ne doit pas faire sauter le fondu en cours.
                    let to = match &self.transition {
                        Some(tr) => self.standby_after(tr.to),
                        None => self.standby,
                    };
                    match to {
                        Some(to) => {
                            // GO pendant transition : elle SAUTE à sa fin,
                            // puis nouveau GO.
                            self.snap_transition(now);
                            self.panic = None;
                            let tr = self.cues[to].transition.clone();
                            self.start_go(to, tr, now);
                        }
                        None => self.warn_event("GO sans cue en standby (fin de conduite ou cuelist vide)".into()),
                    }
                }
                Pending::Back => {
                    // Même principe : valider sur l'état post-snap sans
                    // snapper si la commande est un no-op.
                    let ai = match &self.transition {
                        Some(tr) => Some(tr.to),
                        None => self.active,
                    };
                    match ai {
                        Some(ai) if ai > 0 => {
                            self.snap_transition(now);
                            self.panic = None;
                            // Transition de la cue active, pas de la cible.
                            let tr = self.cues[ai].transition.clone();
                            self.start_go(ai - 1, tr, now);
                        }
                        Some(_) => self.warn_event("BACK sur la première cue".into()),
                        None => self.warn_event("BACK sans cue active".into()),
                    }
                }
                Pending::Goto(n) => match self.index_of(n) {
                    Some(to) => {
                        self.snap_transition(now);
                        self.panic = None;
                        let tr = self.cues[to].transition.clone();
                        self.start_go(to, tr, now);
                    }
                    None => self.warn_event(format!("GOTO vers cue inexistante {n}")),
                },
                Pending::Standby(n) => match self.index_of(n) {
                    Some(i) => {
                        self.standby = Some(i);
                        debug!(target: "cue", cue = %n, "standby déplacée");
                    }
                    None => self.warn_event(format!("STANDBY vers cue inexistante {n}")),
                },
                Pending::Panic(fade_s) => {
                    // Le fondu part du noir RÉELLEMENT affiché : panic
                    // précédent OU noir de la transition en cours (panic
                    // pendant un ThroughBlack) — le noir de panic ne peut
                    // jamais redescendre sous ce niveau, même quand la
                    // transition se termine et relâche son propre noir.
                    let from = self.panic_black(now).max(self.eval_transition(now).1);
                    self.panic = Some(PanicState { start_s: now, fade_s, from });
                    self.events.push(CueEvent::PanicStarted { fade_s });
                    warn!(target: "cue", fade_s, "PANIC : fondu au noir global");
                }
            }
        }
    }

    /// Démarre un GO vers `to` avec la transition donnée. Cut ou durée
    /// nulle ⇒ bascule immédiate.
    fn start_go(&mut self, to: usize, tr: Transition, now: f64) {
        self.events.push(CueEvent::CueStarted { cue: self.cues[to].number });
        if matches!(tr.kind, TransitionKind::Cut) || tr.dur_s <= 0.0 {
            self.media_start_s = now;
            self.activate(to, now);
        } else {
            // Origine de l'horloge média : le deck B avance dès le début de
            // la transition — sauf ThroughBlack, où il est gelé jusqu'à la
            // bascule à mi-course (contrat `CueFrame::freeze_b`).
            self.media_start_s = match tr.kind {
                TransitionKind::ThroughBlack => now + f64::from(tr.dur_s) * 0.5,
                _ => now,
            };
            self.transition = Some(ActiveTransition {
                to,
                kind: tr.kind,
                dur_s: tr.dur_s,
                curve: tr.curve,
                start_s: now,
            });
        }
    }

    /// Fin de transition : B devient A, la standby avance, le follow s'arme.
    fn activate(&mut self, idx: usize, now: f64) {
        self.transition = None;
        self.active = Some(idx);
        self.cue_start_s = now;
        self.follow_fired = false;
        // Snapshot cible à poser à alpha 1.0 sur ce tick. En cas
        // d'activations multiples dans le même tick, les snapshots sont
        // fusionnés dans l'ordre (le dernier l'emporte par adresse) —
        // équivalent à les poser séquentiellement.
        let snapshot = self.targets[idx].params.clone();
        self.finished_params
            .get_or_insert_with(BTreeMap::new)
            .extend(snapshot);
        self.standby = self.standby_after(idx);
        let number = self.cues[idx].number;
        self.events.push(CueEvent::TransitionFinished { cue: number });
        if !matches!(self.cues[idx].follow, FollowMode::Manual) {
            if let Some(sb) = self.standby {
                self.events.push(CueEvent::FollowArmed {
                    cue: number,
                    target: self.cues[sb].number,
                });
            }
        }
        debug!(target: "cue", cue = %number, "cue au programme");
    }

    /// Standby après activation de `idx` : la cible de boucle `goto_after`
    /// si elle existe (préchargée et visible), sinon la cue suivante.
    fn standby_after(&self, idx: usize) -> Option<usize> {
        if let Some(n) = self.cues[idx].goto_after {
            if let Some(i) = self.index_of(n) {
                return Some(i);
            }
            warn!(target: "cue", cue = %self.cues[idx].number, target_cue = %n,
                "goto_after vers une cue inexistante — cue suivante utilisée");
        }
        if idx + 1 < self.cues.len() {
            Some(idx + 1)
        } else {
            None
        }
    }

    /// La transition en cours saute à sa fin (jamais d'état corrompu).
    fn snap_transition(&mut self, now: f64) {
        if let Some(tr) = self.transition.take() {
            self.activate(tr.to, now);
        }
    }

    /// Follow de la cue active : Wait sur l'horloge moteur (depuis la fin
    /// de la transition d'entrée), AfterMedia via l'oracle `media_eof` sur
    /// les slices porteurs d'un `EndMode::FollowNext`. Jamais pendant une
    /// transition, une seule fois par activation.
    fn check_follow(&mut self, now: f64, media_eof: &dyn Fn(SliceId) -> bool) {
        if self.transition.is_some() || self.follow_fired {
            return;
        }
        let Some(ai) = self.active else { return };
        let due = match self.cues[ai].follow {
            FollowMode::Manual => false,
            FollowMode::Wait(s) => now - self.cue_start_s >= f64::from(s),
            FollowMode::AfterMedia => self.cues[ai].states.iter().any(|st| {
                matches!(st.playback.as_ref().map(|p| p.end), Some(EndMode::FollowNext))
                    && media_eof(st.slice)
            }),
        };
        if !due {
            return;
        }
        self.follow_fired = true;
        let from = self.cues[ai].number;
        match self.standby {
            Some(to) => {
                let target = self.cues[to].number;
                self.events.push(CueEvent::FollowFired { cue: from, target });
                let tr = self.cues[to].transition.clone();
                self.start_go(to, tr, now);
            }
            None => {
                self.warn_event(format!("Follow de la cue {from} sans cue en standby"));
            }
        }
    }

    /// Progression brute 0..1 de la transition en cours.
    fn transition_progress(&self, tr: &ActiveTransition, now: f64) -> f32 {
        if tr.dur_s <= 0.0 {
            return 1.0;
        }
        (((now - tr.start_s) / f64::from(tr.dur_s)).clamp(0.0, 1.0)) as f32
    }

    /// (blend, noir de transition) pour la frame courante.
    fn eval_transition(&self, now: f64) -> (f32, f32) {
        let Some(tr) = &self.transition else { return (0.0, 0.0) };
        let p = self.transition_progress(tr, now);
        match tr.kind {
            TransitionKind::Crossfade => (tr.curve.apply(p), 0.0),
            TransitionKind::ThroughBlack => {
                // Descente à noir sur dur/2 (contenu A), bascule à mi-course,
                // remontée sur dur/2 (contenu B).
                if p < 0.5 {
                    (0.0, tr.curve.apply(p * 2.0))
                } else {
                    (1.0, tr.curve.apply((1.0 - p) * 2.0))
                }
            }
            // Jamais stocké : bascule immédiate dans start_go.
            TransitionKind::Cut => (0.0, 0.0),
        }
    }

    /// Niveau du fondu au noir d'urgence (reste à 1 une fois atteint).
    fn panic_black(&self, now: f64) -> f32 {
        let Some(p) = &self.panic else { return 0.0 };
        if p.fade_s <= 0.0 {
            return 1.0;
        }
        let q = (((now - p.start_s) / f64::from(p.fade_s)).clamp(0.0, 1.0)) as f32;
        p.from + (1.0 - p.from) * q
    }

    fn index_of(&self, n: CueNumber) -> Option<usize> {
        // Liste triée : recherche binaire sur le numéro.
        self.cues.binary_search_by_key(&n, |c| c.number).ok()
    }

    fn warn_event(&mut self, message: String) {
        warn!(target: "cue", "{message}");
        self.events.push(CueEvent::Warning { message });
    }
}

/// Résout une cue en scène : cibles par slice + paramètres fusionnés
/// (en cas de doublon d'adresse, le dernier état l'emporte).
fn resolve_scene(cue: &Cue) -> SceneTarget {
    let mut params = BTreeMap::new();
    let per_slice = cue
        .states
        .iter()
        .map(|st| {
            for (k, v) in &st.params {
                params.insert(k.clone(), v.clone());
            }
            SliceTarget {
                slice: st.slice,
                content: st.content.clone(),
                playback: st.playback.clone(),
            }
        })
        .collect();
    SceneTarget { per_slice, params }
}

/// Progression média de la cue depuis l'origine média (début de la
/// transition d'entrée) quand les points IN/OUT donnent une durée (slices
/// `FollowNext` prioritaires), sinon inconnue. `speed_mult` : multiplicateurs
/// de vitesse live par slice (absent = 1.0).
fn media_progress(
    cue: &Cue,
    elapsed: f32,
    speed_mult: &BTreeMap<SliceId, f32>,
) -> (f32, Option<f32>) {
    let duration_of = |slice: SliceId, pb: &Playback| -> Option<f32> {
        let out = pb.out_s?;
        let mult = speed_mult.get(&slice).copied().unwrap_or(1.0);
        let speed = f64::from(pb.speed) * f64::from(mult);
        if speed > 0.0 && out > pb.in_s {
            Some(((out - pb.in_s) / speed) as f32)
        } else {
            None
        }
    };
    let pick = |want_follow: bool| {
        cue.states.iter().find_map(|st| {
            let pb = st.playback.as_ref()?;
            if want_follow && !matches!(pb.end, EndMode::FollowNext) {
                return None;
            }
            duration_of(st.slice, pb)
        })
    };
    match pick(true).or_else(|| pick(false)) {
        Some(dur) if dur > 0.0 => (
            (elapsed / dur).clamp(0.0, 1.0),
            Some((dur - elapsed).max(0.0)),
        ),
        _ => (0.0, None),
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use conduite_core::{CueTriggers, SliceState};

    use super::*;

    // ------------------------------------------------------------ fabriques

    fn transition(kind: TransitionKind, dur_s: f32, curve: Curve) -> Transition {
        Transition { kind, dur_s, curve }
    }

    /// Cue minimale : numéro en millièmes, transition, follow Manual.
    fn cue(n: u32, kind: TransitionKind, dur_s: f32) -> Cue {
        Cue {
            number: CueNumber(n),
            name: format!("cue {}", CueNumber(n)),
            color: None,
            notes: String::new(),
            transition: transition(kind, dur_s, Curve::Linear),
            follow: FollowMode::Manual,
            goto_after: None,
            states: Vec::new(),
            mod_routes: Vec::new(),
            triggers: CueTriggers::default(),
        }
    }

    fn with_follow(mut c: Cue, follow: FollowMode) -> Cue {
        c.follow = follow;
        c
    }

    fn with_goto_after(mut c: Cue, n: u32) -> Cue {
        c.goto_after = Some(CueNumber(n));
        c
    }

    fn with_state(mut c: Cue, slice: SliceId, media: u32, end: EndMode, out_s: Option<f64>) -> Cue {
        c.states.push(SliceState {
            slice,
            content: Content::Media(media),
            playback: Some(Playback {
                in_s: 0.0,
                out_s,
                speed: 1.0,
                end,
            }),
            params: BTreeMap::new(),
        });
        c
    }

    fn with_param(mut c: Cue, addr: &str, v: f32) -> Cue {
        if let Some(st) = c.states.last_mut() {
            st.params.insert(addr.to_string(), ParamValue::F(v));
        }
        c
    }

    fn engine(cues: Vec<Cue>) -> CueEngine {
        let mut e = CueEngine::new();
        e.load(&cues);
        e
    }

    /// Tick sans fin de média.
    fn tk(e: &mut CueEngine, now_s: f64) -> CueFrame {
        e.tick(EngineTick { now_s, media_eof: &|_| false })
    }

    /// Tick avec oracle de fin de média.
    fn tk_eof(e: &mut CueEngine, now_s: f64, eof: &dyn Fn(SliceId) -> bool) -> CueFrame {
        e.tick(EngineTick { now_s, media_eof: eof })
    }

    fn has_warning(f: &CueFrame) -> bool {
        f.events.iter().any(|e| matches!(e, CueEvent::Warning { .. }))
    }

    fn active(e: &CueEngine) -> Option<u32> {
        e.status().active.map(|n| n.0)
    }

    fn standby_of(e: &CueEngine) -> Option<u32> {
        e.status().standby.map(|n| n.0)
    }

    const EPS: f32 = 1e-5;

    // -------------------------------------------------------------- load

    #[test]
    fn load_sorts_cues_and_arms_first_standby() {
        let e = engine(vec![
            cue(3000, TransitionKind::Cut, 0.0),
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2500, TransitionKind::Cut, 0.0),
        ]);
        let s = e.status();
        assert_eq!(s.active, None);
        assert_eq!(s.standby, Some(CueNumber(1000)), "standby = plus petit numéro");
        assert!(!s.transition_active);
    }

    #[test]
    fn load_resets_previous_state() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Crossfade, 4.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 1.0); // transition en cours
        e.panic(0.0);
        tk(&mut e, 2.0);
        // Rechargement : tout repart à zéro.
        e.load(&[cue(5000, TransitionKind::Cut, 0.0)]);
        let f = tk(&mut e, 3.0);
        assert_eq!(active(&e), None);
        assert_eq!(standby_of(&e), Some(5000));
        assert!(f.black.abs() < EPS, "panic oublié après load");
        assert!(f.deck_a.is_none());
        assert!(!e.status().transition_active);
    }

    // ---------------------------------------------------------- load_hot

    /// Édition pendant la lecture : la cue active reste au programme (deck A
    /// non vidé), la standby est conservée et le prochain GO avance.
    #[test]
    fn load_hot_preserves_active_and_deck_a() {
        let cues = vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
            cue(3000, TransitionKind::Cut, 0.0),
        ];
        let mut e = engine(cues.clone());
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 1.0); // cue 2000 au programme, standby 3000
        assert_eq!(active(&e), Some(2000));

        e.load_hot(&cues);
        let f = tk(&mut e, 2.0);
        assert_eq!(active(&e), Some(2000), "cue active conservée");
        assert_eq!(standby_of(&e), Some(3000), "standby conservée");
        assert!(f.deck_a.is_some(), "deck A non vidé : les players survivent");

        e.go();
        tk(&mut e, 3.0);
        assert_eq!(active(&e), Some(3000), "GO avance, ne rejoue pas la courante");
    }

    /// Les horloges du follow Wait survivent au rechargement : pas de refire
    /// immédiat, le follow part bien à l'échéance d'origine.
    #[test]
    fn load_hot_preserves_wait_clock_no_immediate_follow() {
        let cues = vec![
            with_follow(cue(1000, TransitionKind::Cut, 0.0), FollowMode::Wait(10.0)),
            cue(2000, TransitionKind::Cut, 0.0),
        ];
        let mut e = engine(cues.clone());
        e.go();
        tk(&mut e, 0.0); // cue 1000 active, wait 10 s depuis t=0

        tk(&mut e, 5.0);
        e.load_hot(&cues);
        let f = tk(&mut e, 5.1);
        assert!(
            !f.events.iter().any(|ev| matches!(ev, CueEvent::FollowFired { .. })),
            "pas de follow immédiat après rechargement à chaud"
        );
        assert_eq!(active(&e), Some(1000));

        let f = tk(&mut e, 10.2);
        assert!(
            f.events.iter().any(|ev| matches!(ev, CueEvent::FollowFired { .. })),
            "le wait tire à son échéance d'origine"
        );
    }

    /// Cue active supprimée par l'édition : plus rien au programme, standby
    /// repliée sur l'ancienne standby si elle existe encore.
    #[test]
    fn load_hot_active_removed_falls_back_to_standby() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0); // 1000 active, standby 2000
        e.load_hot(&[cue(2000, TransitionKind::Cut, 0.0)]);
        tk(&mut e, 1.0);
        assert_eq!(active(&e), None, "cue disparue : plus rien au programme");
        assert_eq!(standby_of(&e), Some(2000));
    }

    /// Rechargement à chaud pendant une transition : elle saute à sa fin et
    /// le snapshot de params de la cue snappée est bien posé à alpha 1.0.
    #[test]
    fn load_hot_during_transition_snaps_and_poses_params() {
        let cues = vec![
            cue(1000, TransitionKind::Cut, 0.0),
            with_param(
                with_state(cue(2000, TransitionKind::Crossfade, 4.0), 1, 7, EndMode::Hold, None),
                "slice/1/opacity",
                0.25,
            ),
        ];
        let mut e = engine(cues.clone());
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 1.0); // crossfade 1000→2000 en cours

        e.load_hot(&cues);
        let f = tk(&mut e, 1.1);
        assert_eq!(active(&e), Some(2000), "transition snappée à sa fin");
        assert!(!e.status().transition_active);
        let (map, alpha) = f.params_target.expect("snapshot de la cue snappée posé");
        assert!((alpha - 1.0).abs() < EPS);
        assert_eq!(map.get("slice/1/opacity"), Some(&ParamValue::F(0.25)));
    }

    // ------------------------------------------------------- cas limites

    #[test]
    fn empty_cuelist_commands_are_noops_with_warnings() {
        let mut e = engine(vec![]);
        e.go();
        e.back();
        e.goto(CueNumber(1000));
        let f = tk(&mut e, 0.0);
        let warnings = f
            .events
            .iter()
            .filter(|ev| matches!(ev, CueEvent::Warning { .. }))
            .count();
        assert_eq!(warnings, 3);
        assert!(f.deck_a.is_none());
        assert!(f.deck_b.is_none());
        assert_eq!(e.status().active, None);
        assert_eq!(e.status().standby, None);
    }

    #[test]
    fn goto_unknown_number_warns_and_keeps_state() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.goto(CueNumber(9999));
        let f = tk(&mut e, 1.0);
        assert!(has_warning(&f));
        assert_eq!(active(&e), Some(1000), "état inchangé");
        assert_eq!(standby_of(&e), Some(2000));
        assert!(!e.status().transition_active);
    }

    #[test]
    fn standby_unknown_number_warns_and_keeps_pointer() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.standby(CueNumber(4242));
        let f = tk(&mut e, 0.0);
        assert!(has_warning(&f));
        assert_eq!(standby_of(&e), Some(1000));
    }

    #[test]
    fn go_at_end_of_list_warns() {
        let mut e = engine(vec![cue(1000, TransitionKind::Cut, 0.0)]);
        e.go();
        tk(&mut e, 0.0);
        assert_eq!(active(&e), Some(1000));
        assert_eq!(standby_of(&e), None);
        e.go();
        let f = tk(&mut e, 1.0);
        assert!(has_warning(&f));
        assert_eq!(active(&e), Some(1000));
    }

    // ---------------------------------------------------------------- cut

    #[test]
    fn cut_go_activates_immediately() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        let f = tk(&mut e, 10.0);
        assert!(f.deck_a.is_some(), "cue 1 au programme");
        assert!((f.blend - 0.0).abs() < EPS);
        assert!(f.black.abs() < EPS);
        assert_eq!(
            f.events,
            vec![
                CueEvent::CueStarted { cue: CueNumber(1000) },
                CueEvent::TransitionFinished { cue: CueNumber(1000) },
            ]
        );
        // Alpha final 1.0 : les paramètres cibles sont posés d'un coup.
        assert_eq!(f.params_target.as_ref().map(|(_, a)| *a), Some(1.0));
        let s = e.status();
        assert_eq!(s.active, Some(CueNumber(1000)));
        assert_eq!(s.standby, Some(CueNumber(2000)));
        assert!(!s.transition_active);
    }

    #[test]
    fn cut_with_nonzero_duration_is_still_immediate() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 5.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        assert_eq!(active(&e), Some(1000));
        assert!(!e.status().transition_active);
    }

    #[test]
    fn crossfade_with_zero_duration_acts_like_cut() {
        let mut e = engine(vec![cue(1000, TransitionKind::Crossfade, 0.0)]);
        e.go();
        let f = tk(&mut e, 0.0);
        assert_eq!(active(&e), Some(1000));
        assert!(!e.status().transition_active);
        assert!((f.blend - 0.0).abs() < EPS);
    }

    // ---------------------------------------------------------- crossfade

    #[test]
    fn crossfade_blend_frame_by_frame_linear() {
        let mut e = engine(vec![
            with_state(cue(1000, TransitionKind::Cut, 0.0), 1, 10, EndMode::Loop, None),
            with_state(cue(2000, TransitionKind::Crossfade, 2.0), 1, 20, EndMode::Loop, None),
        ]);
        e.go();
        tk(&mut e, 0.0); // cue 1 au programme
        e.go();
        let f0 = tk(&mut e, 10.0);
        assert!(f0.events.contains(&CueEvent::CueStarted { cue: CueNumber(2000) }));
        assert!((f0.blend - 0.0).abs() < EPS, "départ à 0");
        // Pendant la transition : A = ancienne cue, B = cible.
        let a = f0.deck_a.expect("deck A");
        let b = f0.deck_b.expect("deck B");
        assert_eq!(a.per_slice[0].content, Content::Media(10));
        assert_eq!(b.per_slice[0].content, Content::Media(20));

        for (now, want) in [(10.5, 0.25), (11.0, 0.5), (11.5, 0.75), (11.9, 0.95)] {
            let f = tk(&mut e, now);
            assert!(
                (f.blend - want).abs() < 1e-4,
                "blend à t={now} : {} attendu {want}",
                f.blend
            );
            assert!(f.black.abs() < EPS, "pas de noir en crossfade");
            assert!(e.status().transition_active);
        }

        // Fin : B devient A, blend retombe à 0, standby avance.
        let f = tk(&mut e, 12.0);
        assert!(f.events.contains(&CueEvent::TransitionFinished { cue: CueNumber(2000) }));
        assert!((f.blend - 0.0).abs() < EPS);
        let a = f.deck_a.expect("deck A après fin");
        assert_eq!(a.per_slice[0].content, Content::Media(20));
        assert!(!e.status().transition_active);
        assert_eq!(active(&e), Some(2000));
    }

    #[test]
    fn crossfade_blend_follows_ease_in_curve() {
        let mut c2 = cue(2000, TransitionKind::Crossfade, 2.0);
        c2.transition.curve = Curve::EaseIn;
        let mut e = engine(vec![cue(1000, TransitionKind::Cut, 0.0), c2]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 0.0);
        let f = tk(&mut e, 1.0); // p = 0.5, EaseIn -> 0.25
        assert!((f.blend - 0.25).abs() < 1e-4, "blend = {}", f.blend);
    }

    // ------------------------------------------------------- through black

    #[test]
    fn through_black_dips_switches_mid_and_rises() {
        let mut e = engine(vec![
            with_state(cue(1000, TransitionKind::Cut, 0.0), 1, 10, EndMode::Loop, None),
            with_state(cue(2000, TransitionKind::ThroughBlack, 2.0), 1, 20, EndMode::Loop, None),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        // Première moitié : le noir monte, contenu A (blend 0).
        let f = tk(&mut e, 100.0);
        assert!((f.black - 0.0).abs() < EPS);
        assert!((f.blend - 0.0).abs() < EPS);
        let f = tk(&mut e, 100.5); // p = 0.25 -> noir 0.5
        assert!((f.black - 0.5).abs() < 1e-4, "black = {}", f.black);
        assert!((f.blend - 0.0).abs() < EPS, "contenu A avant mi-course");
        // Mi-course : noir plein, bascule A -> B.
        let f = tk(&mut e, 101.0); // p = 0.5
        assert!((f.black - 1.0).abs() < 1e-4, "noir plein à mi-course");
        assert!((f.blend - 1.0).abs() < EPS, "contenu B dès la mi-course");
        // Deuxième moitié : le noir redescend, contenu B.
        let f = tk(&mut e, 101.5); // p = 0.75 -> noir 0.5
        assert!((f.black - 0.5).abs() < 1e-4, "black = {}", f.black);
        assert!((f.blend - 1.0).abs() < EPS);
        // Fin : noir levé, cue 2 au programme.
        let f = tk(&mut e, 102.0);
        assert!(f.black.abs() < EPS);
        assert!((f.blend - 0.0).abs() < EPS);
        assert_eq!(active(&e), Some(2000));
    }

    // ---------------------------------------------- go pendant transition

    #[test]
    fn go_during_transition_snaps_to_end_then_starts_new() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Crossfade, 2.0),
            cue(3000, TransitionKind::Crossfade, 2.0),
        ]);
        e.go();
        tk(&mut e, 0.0); // active 1
        e.go();
        tk(&mut e, 10.0); // transition vers 2 démarrée
        tk(&mut e, 11.0); // mi-course
        e.go();
        let f = tk(&mut e, 11.5);
        // La transition vers 2 a sauté à sa fin, puis GO vers 3.
        assert!(f.events.contains(&CueEvent::TransitionFinished { cue: CueNumber(2000) }));
        assert!(f.events.contains(&CueEvent::CueStarted { cue: CueNumber(3000) }));
        assert_eq!(active(&e), Some(2000), "cue 2 posée avant le nouveau GO");
        assert!(e.status().transition_active, "transition vers 3 en cours");
        assert!((f.blend - 0.0).abs() < EPS, "nouvelle transition repart de 0");
        // La nouvelle transition vit sa vie normalement.
        let f = tk(&mut e, 12.5);
        assert!((f.blend - 0.5).abs() < 1e-4);
        tk(&mut e, 13.5);
        assert_eq!(active(&e), Some(3000));
        assert!(!e.status().transition_active);
    }

    #[test]
    fn double_go_same_tick_advances_two_cues() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
            cue(3000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        e.go();
        let f = tk(&mut e, 0.0);
        assert!(f.events.contains(&CueEvent::CueStarted { cue: CueNumber(1000) }));
        assert!(f.events.contains(&CueEvent::CueStarted { cue: CueNumber(2000) }));
        assert_eq!(active(&e), Some(2000));
        assert_eq!(standby_of(&e), Some(3000));
    }

    // -------------------------------------------------------------- follow

    #[test]
    fn wait_follow_counts_from_end_of_transition() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            with_follow(cue(2000, TransitionKind::Crossfade, 2.0), FollowMode::Wait(1.0)),
            cue(3000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 10.0); // transition 2 s -> fin à 12.0
        let f = tk(&mut e, 12.0);
        assert!(f.events.contains(&CueEvent::FollowArmed {
            cue: CueNumber(2000),
            target: CueNumber(3000)
        }));
        // Le wait démarre à la FIN de la transition : rien avant 13.0.
        let f = tk(&mut e, 12.5);
        assert!(!f.events.iter().any(|ev| matches!(ev, CueEvent::FollowFired { .. })));
        assert_eq!(active(&e), Some(2000));
        let f = tk(&mut e, 13.0);
        assert!(f.events.contains(&CueEvent::FollowFired {
            cue: CueNumber(2000),
            target: CueNumber(3000)
        }));
        assert_eq!(active(&e), Some(3000));
    }

    #[test]
    fn after_media_follow_fires_on_eof_of_follownext_slices_only() {
        let c2 = with_state(
            with_state(
                with_follow(cue(2000, TransitionKind::Cut, 0.0), FollowMode::AfterMedia),
                1,
                10,
                EndMode::Loop,
                None,
            ),
            2,
            20,
            EndMode::FollowNext,
            None,
        );
        let mut e = engine(vec![cue(1000, TransitionKind::Cut, 0.0), c2, cue(3000, TransitionKind::Cut, 0.0)]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 1.0); // cue 2 active
        assert_eq!(active(&e), Some(2000));
        // EOF du slice 1 (Loop) : ignoré.
        let f = tk_eof(&mut e, 2.0, &|s| s == 1);
        assert!(!f.events.iter().any(|ev| matches!(ev, CueEvent::FollowFired { .. })));
        assert_eq!(active(&e), Some(2000));
        // EOF du slice 2 (FollowNext) : GO automatique.
        let f = tk_eof(&mut e, 3.0, &|s| s == 2);
        assert!(f.events.contains(&CueEvent::FollowFired {
            cue: CueNumber(2000),
            target: CueNumber(3000)
        }));
        assert_eq!(active(&e), Some(3000));
    }

    #[test]
    fn follow_is_not_evaluated_during_transition() {
        let c2 = with_state(
            with_follow(cue(2000, TransitionKind::Crossfade, 2.0), FollowMode::AfterMedia),
            1,
            20,
            EndMode::FollowNext,
            None,
        );
        let mut e = engine(vec![cue(1000, TransitionKind::Cut, 0.0), c2, cue(3000, TransitionKind::Cut, 0.0)]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        // EOF vrai dès le départ, mais la transition d'entrée est en cours.
        let f = tk_eof(&mut e, 10.0, &|_| true);
        assert!(!f.events.iter().any(|ev| matches!(ev, CueEvent::FollowFired { .. })));
        let f = tk_eof(&mut e, 11.0, &|_| true);
        assert!(!f.events.iter().any(|ev| matches!(ev, CueEvent::FollowFired { .. })));
        // Transition finie : le follow peut tirer.
        let f = tk_eof(&mut e, 12.0, &|_| true);
        assert!(f.events.iter().any(|ev| matches!(ev, CueEvent::FollowFired { .. })));
    }

    #[test]
    fn wait_zero_chains_one_cue_per_tick_max() {
        let mut e = engine(vec![
            with_follow(cue(1000, TransitionKind::Cut, 0.0), FollowMode::Wait(0.0)),
            with_follow(cue(2000, TransitionKind::Cut, 0.0), FollowMode::Wait(0.0)),
            with_follow(cue(3000, TransitionKind::Cut, 0.0), FollowMode::Wait(0.0)),
        ]);
        e.go();
        // Tick 1 : GO -> cue 1, puis UN follow max -> cue 2.
        tk(&mut e, 0.0);
        assert_eq!(active(&e), Some(2000), "un seul follow par tick");
        // Tick 2 : follow -> cue 3.
        tk(&mut e, 0.016);
        assert_eq!(active(&e), Some(3000));
        // Cue 3 : follow sans standby -> warning une seule fois.
        let f = tk(&mut e, 0.032);
        assert!(has_warning(&f));
        let f = tk(&mut e, 0.048);
        assert!(!has_warning(&f), "le warning de follow ne spamme pas");
        assert_eq!(active(&e), Some(3000));
    }

    // ------------------------------------------------- boucles de section

    #[test]
    fn goto_after_loops_section() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            with_goto_after(
                with_follow(cue(2000, TransitionKind::Cut, 0.0), FollowMode::Wait(1.0)),
                1000,
            ),
            cue(3000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0); // active 1, standby 2
        e.go();
        tk(&mut e, 1.0); // active 2
        // La cible de boucle est en standby (préchargée, visible en régie).
        assert_eq!(standby_of(&e), Some(1000));
        // Fin naturelle (wait) : GO vers la cible de boucle.
        let f = tk(&mut e, 2.0);
        assert!(f.events.contains(&CueEvent::FollowFired {
            cue: CueNumber(2000),
            target: CueNumber(1000)
        }));
        assert_eq!(active(&e), Some(1000));
        assert_eq!(standby_of(&e), Some(2000), "la boucle peut repartir");
    }

    #[test]
    fn goto_after_to_missing_cue_falls_back_to_next() {
        let mut e = engine(vec![
            with_goto_after(cue(1000, TransitionKind::Cut, 0.0), 9999),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        assert_eq!(standby_of(&e), Some(2000), "repli sur la cue suivante");
    }

    // ---------------------------------------------------------- goto/back

    #[test]
    fn goto_jumps_with_target_transition() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
            cue(3000, TransitionKind::Crossfade, 2.0),
        ]);
        e.go();
        tk(&mut e, 0.0); // active 1
        e.goto(CueNumber(3000));
        let f = tk(&mut e, 5.0);
        assert!(f.events.contains(&CueEvent::CueStarted { cue: CueNumber(3000) }));
        assert!(e.status().transition_active, "transition de la cue 3 (crossfade)");
        let f = tk(&mut e, 6.0);
        assert!((f.blend - 0.5).abs() < 1e-4);
        tk(&mut e, 7.0);
        assert_eq!(active(&e), Some(3000));
        assert_eq!(standby_of(&e), None, "3 est la dernière");
    }

    #[test]
    fn back_uses_active_cue_transition() {
        let mut e = engine(vec![
            with_state(cue(1000, TransitionKind::Cut, 0.0), 1, 10, EndMode::Loop, None),
            with_state(cue(2000, TransitionKind::Crossfade, 2.0), 1, 20, EndMode::Loop, None),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 10.0);
        tk(&mut e, 12.0); // cue 2 active
        assert_eq!(active(&e), Some(2000));
        e.back();
        let f = tk(&mut e, 20.0);
        // GO inversé : transition de la cue active (crossfade 2 s de la cue 2).
        assert!(f.events.contains(&CueEvent::CueStarted { cue: CueNumber(1000) }));
        assert!(e.status().transition_active);
        let b = f.deck_b.expect("deck B = cue 1");
        assert_eq!(b.per_slice[0].content, Content::Media(10));
        let f = tk(&mut e, 21.0);
        assert!((f.blend - 0.5).abs() < 1e-4, "durée du crossfade de la cue 2");
        tk(&mut e, 22.0);
        assert_eq!(active(&e), Some(1000));
        assert_eq!(standby_of(&e), Some(2000));
    }

    #[test]
    fn back_without_previous_warns() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        // Aucune cue active.
        e.back();
        let f = tk(&mut e, 0.0);
        assert!(has_warning(&f));
        // Première cue active : pas de précédente.
        e.go();
        tk(&mut e, 1.0);
        e.back();
        let f = tk(&mut e, 2.0);
        assert!(has_warning(&f));
        assert_eq!(active(&e), Some(1000));
    }

    // --------------------------------------------------------------- panic

    #[test]
    fn panic_fades_to_black_and_keeps_conduite() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.panic(2.0);
        let f = tk(&mut e, 10.0);
        assert!(f.events.contains(&CueEvent::PanicStarted { fade_s: 2.0 }));
        assert!(f.black.abs() < EPS, "départ du fondu");
        let f = tk(&mut e, 11.0);
        assert!((f.black - 0.5).abs() < 1e-4);
        let f = tk(&mut e, 12.0);
        assert!((f.black - 1.0).abs() < EPS);
        let f = tk(&mut e, 60.0);
        assert!((f.black - 1.0).abs() < EPS, "le noir tient");
        // La conduite n'a pas bougé : active/standby inchangés, decks servis.
        let s = e.status();
        assert_eq!(s.active, Some(CueNumber(1000)));
        assert_eq!(s.standby, Some(CueNumber(2000)));
        assert!(f.deck_a.is_some());
    }

    #[test]
    fn panic_with_zero_fade_is_instant() {
        let mut e = engine(vec![cue(1000, TransitionKind::Cut, 0.0)]);
        e.go();
        tk(&mut e, 0.0);
        e.panic(0.0);
        let f = tk(&mut e, 1.0);
        assert!((f.black - 1.0).abs() < EPS);
    }

    #[test]
    fn manual_go_releases_panic() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.panic(0.0);
        tk(&mut e, 1.0);
        e.go();
        let f = tk(&mut e, 2.0);
        assert!(f.black.abs() < EPS, "GO relâche le noir d'urgence");
        assert_eq!(active(&e), Some(2000));
    }

    #[test]
    fn repanic_during_fade_continues_from_current_level() {
        let mut e = engine(vec![cue(1000, TransitionKind::Cut, 0.0)]);
        e.go();
        tk(&mut e, 0.0);
        e.panic(2.0);
        tk(&mut e, 10.0);
        tk(&mut e, 11.0); // noir à 0.5
        e.panic(1.0); // re-panic plus rapide
        let f = tk(&mut e, 11.0);
        assert!((f.black - 0.5).abs() < 1e-4, "reprend au niveau courant");
        let f = tk(&mut e, 11.5);
        assert!((f.black - 0.75).abs() < 1e-4);
        let f = tk(&mut e, 12.0);
        assert!((f.black - 1.0).abs() < EPS);
    }

    // ---------------------------------------------- préchargement/continuité

    #[test]
    fn standby_is_resolved_on_deck_b_when_idle() {
        let mut e = engine(vec![
            with_state(cue(1000, TransitionKind::Cut, 0.0), 1, 10, EndMode::Loop, None),
            with_state(cue(2000, TransitionKind::Cut, 0.0), 1, 20, EndMode::Hold, None),
        ]);
        // Avant tout GO : deck B expose déjà la première cue (préchargement).
        let f = tk(&mut e, 0.0);
        let b = f.deck_b.expect("standby résolue");
        assert_eq!(b.per_slice[0].content, Content::Media(10));
        e.go();
        let f = tk(&mut e, 1.0);
        // Au repos après GO : A = active, B = standby suivante.
        let a = f.deck_a.expect("deck A");
        let b = f.deck_b.expect("deck B");
        assert_eq!(a.per_slice[0].content, Content::Media(10));
        assert_eq!(b.per_slice[0].content, Content::Media(20));
        assert_eq!(
            b.per_slice[0].playback.as_ref().map(|p| p.end),
            Some(EndMode::Hold)
        );
    }

    #[test]
    fn continuity_info_is_identical_between_decks_for_same_content() {
        // Même média/lecture sur le slice 1 entre les deux cues : l'app doit
        // pouvoir détecter l'égalité (slice, content, playback) et garder le player.
        let mut e = engine(vec![
            with_state(cue(1000, TransitionKind::Cut, 0.0), 1, 10, EndMode::Loop, None),
            with_state(cue(2000, TransitionKind::Crossfade, 2.0), 1, 10, EndMode::Loop, None),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        let f = tk(&mut e, 1.0);
        let a = f.deck_a.expect("A");
        let b = f.deck_b.expect("B");
        assert_eq!(a.per_slice[0], b.per_slice[0], "cibles identiques ⇒ continuité");
    }

    // -------------------------------------------------------------- params

    #[test]
    fn params_target_follows_transition_curve_then_settles_at_one() {
        let c2 = with_param(
            with_state(cue(2000, TransitionKind::Crossfade, 2.0), 1, 20, EndMode::Loop, None),
            "slice/1/opacity",
            0.2,
        );
        let mut e = engine(vec![cue(1000, TransitionKind::Cut, 0.0), c2]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 10.0);
        let f = tk(&mut e, 11.0); // p = 0.5
        let (map, alpha) = f.params_target.expect("cible pendant transition");
        assert_eq!(map.get("slice/1/opacity"), Some(&ParamValue::F(0.2)));
        assert!((alpha - 0.5).abs() < 1e-4);
        // Tick de fin : alpha 1.0 pour poser les valeurs exactes.
        let f = tk(&mut e, 12.0);
        let (_, alpha) = f.params_target.expect("alpha final");
        assert!((alpha - 1.0).abs() < EPS);
        // Au repos : plus de cible.
        let f = tk(&mut e, 13.0);
        assert!(f.params_target.is_none());
    }

    // -------------------------------------------------------------- status

    #[test]
    fn status_progress_during_transition() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Crossfade, 4.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 10.0);
        tk(&mut e, 11.0);
        let s = e.status();
        assert!(s.transition_active);
        assert!((s.progress - 0.25).abs() < 1e-4);
        assert_eq!(s.remaining_s.map(|r| (r * 100.0).round() / 100.0), Some(3.0));
    }

    #[test]
    fn status_progress_for_wait_follow() {
        let mut e = engine(vec![
            with_follow(cue(1000, TransitionKind::Cut, 0.0), FollowMode::Wait(2.0)),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 100.0); // cue_start = 100
        tk(&mut e, 100.5);
        let s = e.status();
        assert!(!s.transition_active);
        assert!((s.progress - 0.25).abs() < 1e-4);
        assert!((s.remaining_s.unwrap_or(0.0) - 1.5).abs() < 1e-4);
    }

    #[test]
    fn status_progress_after_media_uses_in_out_points() {
        // IN 2 s, OUT 12 s, vitesse 2× -> durée effective 5 s.
        let mut c = with_follow(cue(1000, TransitionKind::Cut, 0.0), FollowMode::AfterMedia);
        c.states.push(SliceState {
            slice: 1,
            content: Content::Media(10),
            playback: Some(Playback {
                in_s: 2.0,
                out_s: Some(12.0),
                speed: 2.0,
                end: EndMode::FollowNext,
            }),
            params: BTreeMap::new(),
        });
        let mut e = engine(vec![c, cue(2000, TransitionKind::Cut, 0.0)]);
        e.go();
        tk(&mut e, 0.0);
        tk(&mut e, 2.5);
        let s = e.status();
        assert!((s.progress - 0.5).abs() < 1e-4, "progress = {}", s.progress);
        assert!((s.remaining_s.unwrap_or(0.0) - 2.5).abs() < 1e-4);
    }

    #[test]
    fn status_progress_unknown_without_duration() {
        let mut e = engine(vec![with_state(
            cue(1000, TransitionKind::Cut, 0.0),
            1,
            10,
            EndMode::Loop,
            None,
        )]);
        e.go();
        tk(&mut e, 0.0);
        tk(&mut e, 5.0);
        let s = e.status();
        assert!(s.progress.abs() < EPS);
        assert_eq!(s.remaining_s, None);
    }

    // ------------------------------------------------------ divers moteur

    #[test]
    fn standby_command_redirects_next_go_and_follow() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
            cue(3000, TransitionKind::Cut, 0.0),
        ]);
        e.standby(CueNumber(3000));
        e.go();
        tk(&mut e, 0.0);
        assert_eq!(active(&e), Some(3000), "GO va vers la standby choisie");
    }

    #[test]
    fn events_are_drained_each_tick() {
        let mut e = engine(vec![cue(1000, TransitionKind::Cut, 0.0)]);
        e.go();
        let f = tk(&mut e, 0.0);
        assert!(!f.events.is_empty());
        let f = tk(&mut e, 1.0);
        assert!(f.events.is_empty(), "pas de rejouage d'événements");
    }

    #[test]
    fn go_before_any_tick_then_first_tick_is_consistent() {
        // Les commandes sont latchées : le premier tick les applique sur son horloge.
        let mut e = engine(vec![
            cue(1000, TransitionKind::Crossfade, 2.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        let f = tk(&mut e, 1000.0);
        assert!(e.status().transition_active);
        assert!((f.blend - 0.0).abs() < EPS, "la transition démarre au tick, pas avant");
        tk(&mut e, 1002.0);
        assert_eq!(active(&e), Some(1000));
    }

    // ------------------------------------- panic pendant un through-black

    #[test]
    fn panic_at_through_black_midpoint_holds_full_black_forever() {
        // ThroughBlack 4 s ; PANIC fade 5 s pressé à mi-course (noir plein).
        // Le noir de panic part du noir affiché (1.0) : quand la transition
        // se termine et relâche SON noir, l'image ne doit JAMAIS réapparaître.
        let mut e = engine(vec![
            with_state(cue(1000, TransitionKind::Cut, 0.0), 1, 10, EndMode::Loop, None),
            with_state(cue(2000, TransitionKind::ThroughBlack, 4.0), 1, 20, EndMode::Loop, None),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 100.0); // transition démarre à 100
        e.panic(5.0);
        let f = tk(&mut e, 102.0); // mi-course : noir de transition = 1.0
        assert!((f.black - 1.0).abs() < EPS, "noir plein au déclenchement");
        // Frame par frame : la transition redescendrait son noir (103 -> 0.5,
        // 104 -> fin), mais le panic tient le noir à 1.0.
        for now in [103.0, 104.0, 105.0, 110.0, 200.0] {
            let f = tk(&mut e, now);
            assert!(
                (f.black - 1.0).abs() < EPS,
                "black = {} à t={now} : l'image réapparaît sous panic",
                f.black
            );
        }
        assert_eq!(active(&e), Some(2000), "la conduite continue sous le noir");
    }

    #[test]
    fn panic_during_through_black_first_half_never_drops_below_level() {
        // PANIC pressé à p=0.25 (noir de transition 0.5) : le noir de panic
        // part de 0.5 et le noir affiché ne redescend jamais sous ce niveau.
        let mut e = engine(vec![
            with_state(cue(1000, TransitionKind::Cut, 0.0), 1, 10, EndMode::Loop, None),
            with_state(cue(2000, TransitionKind::ThroughBlack, 4.0), 1, 20, EndMode::Loop, None),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 100.0);
        e.panic(4.0);
        let f = tk(&mut e, 101.0); // p = 0.25 -> noir transition 0.5
        assert!((f.black - 0.5).abs() < 1e-4, "black = {}", f.black);
        // Frame par frame : le noir affiché ne redescend JAMAIS sous le
        // niveau de déclenchement (0.5) — avant le fix, la fin de la
        // transition le laissait retomber à ~0.4 (image visible à 60 %).
        for now in [101.5, 102.0, 102.5, 103.0, 103.5, 104.0, 104.5, 105.0] {
            let f = tk(&mut e, now);
            assert!(
                f.black >= 0.5 - 1e-4,
                "black = {} à t={now} : sous le niveau de déclenchement",
                f.black
            );
        }
        // Fin du fondu panic (from 0.5, fade 4 s -> plein à t=105) : noir tenu.
        for now in [105.0, 110.0, 200.0] {
            let f = tk(&mut e, now);
            assert!((f.black - 1.0).abs() < EPS, "le panic tient le noir plein à t={now}");
        }
    }

    // ------------------------------ GO Cut pendant transition : snapshots

    #[test]
    fn go_cut_during_transition_emits_snapped_cue_params() {
        // Cue 2 (crossfade 6 s) scénarise slice/1/opacity -> 0.0. À mi-fondu,
        // GO vers la cue 3 (Cut) qui ne scénarise PAS slice/1 : le snapshot
        // alpha 1.0 doit contenir la cible de la cue 2 (opacity 0.0) ET les
        // params de la cue 3 — sinon l'opacity reste figée à mi-fondu.
        let c2 = with_param(
            with_state(cue(2000, TransitionKind::Crossfade, 6.0), 1, 20, EndMode::Loop, None),
            "slice/1/opacity",
            0.0,
        );
        let c3 = with_param(
            with_state(cue(3000, TransitionKind::Cut, 0.0), 2, 30, EndMode::Loop, None),
            "slice/2/x",
            0.7,
        );
        let mut e = engine(vec![cue(1000, TransitionKind::Cut, 0.0), c2, c3]);
        e.go();
        tk(&mut e, 0.0); // cue 1 active
        e.go();
        tk(&mut e, 10.0); // crossfade vers 2 démarre
        let f = tk(&mut e, 13.0); // mi-fondu : opacity interpole vers 0.0
        let (_, alpha) = f.params_target.expect("cible pendant transition");
        assert!((alpha - 0.5).abs() < 1e-4);
        e.go(); // GO Cut vers 3 : snap de la 2, puis activation de la 3
        let f = tk(&mut e, 13.5);
        let (map, alpha) = f.params_target.expect("snapshot de fin");
        assert!((alpha - 1.0).abs() < EPS, "alpha final 1.0");
        assert_eq!(
            map.get("slice/1/opacity"),
            Some(&ParamValue::F(0.0)),
            "la cible de la cue snappée est posée"
        );
        assert_eq!(map.get("slice/2/x"), Some(&ParamValue::F(0.7)));
        assert_eq!(active(&e), Some(3000));
        assert!(!e.status().transition_active);
    }

    #[test]
    fn double_go_cut_same_tick_merges_snapshots_last_wins() {
        let c1 = with_param(
            with_param(
                with_state(cue(1000, TransitionKind::Cut, 0.0), 1, 10, EndMode::Loop, None),
                "slice/1/opacity",
                0.25,
            ),
            "common/x",
            0.1,
        );
        let c2 = with_param(
            with_state(cue(2000, TransitionKind::Cut, 0.0), 2, 20, EndMode::Loop, None),
            "common/x",
            0.9,
        );
        let mut e = engine(vec![c1, c2]);
        e.go();
        e.go();
        let f = tk(&mut e, 0.0);
        let (map, alpha) = f.params_target.expect("snapshot fusionné");
        assert!((alpha - 1.0).abs() < EPS);
        assert_eq!(
            map.get("slice/1/opacity"),
            Some(&ParamValue::F(0.25)),
            "param de la première cue conservé"
        );
        assert_eq!(
            map.get("common/x"),
            Some(&ParamValue::F(0.9)),
            "en doublon, la dernière activation l'emporte"
        );
        assert_eq!(active(&e), Some(2000));
    }

    // --------------------------- commandes invalides pendant transition

    #[test]
    fn extra_go_during_final_transition_does_not_snap() {
        // Dernier fondu de la conduite : un GO de trop est un no-op
        // (warning), il ne doit PAS faire sauter le fondu à sa fin.
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Crossfade, 4.0),
        ]);
        e.go();
        tk(&mut e, 0.0); // active 1
        e.go();
        tk(&mut e, 10.0); // fondu final vers 2 démarre (fin à 14)
        tk(&mut e, 11.0);
        e.go(); // GO de trop
        let f = tk(&mut e, 12.0); // p = 0.5
        assert!(has_warning(&f), "GO sans standby : warning");
        assert!(
            !f.events.contains(&CueEvent::TransitionFinished { cue: CueNumber(2000) }),
            "le fondu ne saute pas à sa fin"
        );
        assert!(e.status().transition_active, "le fondu final continue");
        assert!((f.blend - 0.5).abs() < 1e-4, "blend intact = {}", f.blend);
        assert_eq!(active(&e), Some(1000), "cue 1 encore au programme");
        // Fin naturelle du fondu.
        tk(&mut e, 14.0);
        assert_eq!(active(&e), Some(2000));
        assert!(!e.status().transition_active);
    }

    #[test]
    fn back_during_entry_transition_of_first_cue_does_not_snap() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Crossfade, 4.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0); // transition d'entrée de la cue 1 en cours
        e.back(); // BACK invalide : la cue 1 est la première
        let f = tk(&mut e, 2.0); // p = 0.5
        assert!(has_warning(&f));
        assert!(e.status().transition_active, "la transition continue");
        assert!((f.blend - 0.5).abs() < 1e-4, "blend intact = {}", f.blend);
        assert_eq!(active(&e), None, "pas d'activation anticipée");
        tk(&mut e, 4.0);
        assert_eq!(active(&e), Some(1000), "fin naturelle");
    }

    // -------------------------------------------- through black : freeze_b

    #[test]
    fn through_black_freezes_deck_b_until_switch() {
        let mut e = engine(vec![
            with_state(cue(1000, TransitionKind::Cut, 0.0), 1, 10, EndMode::Loop, None),
            with_state(cue(2000, TransitionKind::ThroughBlack, 2.0), 1, 20, EndMode::Loop, None),
        ]);
        // Au repos (standby préchargée) : pas de gel.
        let f = tk(&mut e, 0.0);
        assert!(!f.freeze_b);
        e.go();
        tk(&mut e, 0.5);
        e.go();
        // Première moitié : deck B gelé (le média ne doit pas avancer).
        let f = tk(&mut e, 100.0); // p = 0
        assert!(f.freeze_b, "gelé au départ de la transition");
        let f = tk(&mut e, 100.5); // p = 0.25
        assert!(f.freeze_b, "gelé avant la bascule");
        // Bascule à mi-course : le deck B est révélé et doit tourner.
        let f = tk(&mut e, 101.0); // p = 0.5
        assert!(!f.freeze_b, "libéré à la bascule");
        assert!((f.blend - 1.0).abs() < EPS);
        let f = tk(&mut e, 101.5); // p = 0.75
        assert!(!f.freeze_b);
        let f = tk(&mut e, 102.0); // fin
        assert!(!f.freeze_b);
        assert_eq!(active(&e), Some(2000));
    }

    #[test]
    fn crossfade_never_freezes_deck_b() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Crossfade, 2.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        for now in [10.0, 10.5, 11.0, 11.5, 12.0] {
            let f = tk(&mut e, now);
            assert!(!f.freeze_b, "crossfade : deck B jamais gelé (t={now})");
        }
    }

    // ---------------------------------- compte à rebours média (AfterMedia)

    /// Cue AfterMedia : média 60 s (IN 0, OUT 60, vitesse 1), FollowNext.
    fn after_media_cue(n: u32, kind: TransitionKind, dur_s: f32) -> Cue {
        with_state(
            with_follow(cue(n, kind, dur_s), FollowMode::AfterMedia),
            1,
            20,
            EndMode::FollowNext,
            Some(60.0),
        )
    }

    #[test]
    fn after_media_countdown_starts_at_transition_begin() {
        // Crossfade d'entrée 4 s : le média du deck B avance dès le début de
        // la transition. À la fin de la transition, 4 s sont déjà consommées
        // -> remaining = 56, pas 60 (le follow tirera 56 s plus tard).
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            after_media_cue(2000, TransitionKind::Crossfade, 4.0),
            cue(3000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 100.0); // transition démarre à 100
        tk(&mut e, 104.0); // fin de transition : cue 2 active
        assert_eq!(active(&e), Some(2000));
        let s = e.status();
        assert!(
            (s.remaining_s.unwrap_or(0.0) - 56.0).abs() < 1e-3,
            "remaining = {:?} (attendu 56 : 60 - 4 s de transition)",
            s.remaining_s
        );
        assert!((s.progress - 4.0 / 60.0).abs() < 1e-4);
        // Plus tard : le décompte suit l'horloge média, pas cue_start.
        tk(&mut e, 134.0);
        let s = e.status();
        assert!((s.remaining_s.unwrap_or(0.0) - 26.0).abs() < 1e-3);
    }

    #[test]
    fn after_media_countdown_through_black_starts_at_switch() {
        // ThroughBlack 4 s : le deck B est gelé pendant la première moitié
        // (freeze_b) — le média ne démarre qu'à la bascule (mi-course).
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            after_media_cue(2000, TransitionKind::ThroughBlack, 4.0),
            cue(3000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 100.0); // transition démarre à 100, bascule à 102
        tk(&mut e, 104.0); // fin de transition
        let s = e.status();
        assert!(
            (s.remaining_s.unwrap_or(0.0) - 58.0).abs() < 1e-3,
            "remaining = {:?} (attendu 58 : média parti à la bascule)",
            s.remaining_s
        );
    }

    #[test]
    fn after_media_countdown_uses_live_speed_mult() {
        let mut e = engine(vec![
            cue(1000, TransitionKind::Cut, 0.0),
            after_media_cue(2000, TransitionKind::Crossfade, 4.0),
            cue(3000, TransitionKind::Cut, 0.0),
        ]);
        e.set_speed_mult(1, 2.0); // slice 1 lu à 2x -> durée effective 30 s
        e.go();
        tk(&mut e, 0.0);
        e.go();
        tk(&mut e, 100.0);
        tk(&mut e, 104.0);
        let s = e.status();
        assert!(
            (s.remaining_s.unwrap_or(0.0) - 26.0).abs() < 1e-3,
            "remaining = {:?} (attendu 26 : 60/2 - 4)",
            s.remaining_s
        );
        assert!((s.progress - 4.0 / 30.0).abs() < 1e-4);
        // Multiplicateur invalide : ignoré (warning), l'état ne change pas.
        e.set_speed_mult(1, 0.0);
        e.set_speed_mult(1, f32::NAN);
        let s = e.status();
        assert!((s.remaining_s.unwrap_or(0.0) - 26.0).abs() < 1e-3);
        // Retour à 1.0 : durée pleine.
        e.set_speed_mult(1, 1.0);
        let s = e.status();
        assert!((s.remaining_s.unwrap_or(0.0) - 56.0).abs() < 1e-3);
    }

    #[test]
    fn after_media_countdown_cut_starts_at_activation() {
        // Cut : pas de transition, l'horloge média part à l'activation
        // (comportement inchangé).
        let mut e = engine(vec![
            after_media_cue(1000, TransitionKind::Cut, 0.0),
            cue(2000, TransitionKind::Cut, 0.0),
        ]);
        e.go();
        tk(&mut e, 100.0);
        tk(&mut e, 110.0);
        let s = e.status();
        assert!((s.remaining_s.unwrap_or(0.0) - 50.0).abs() < 1e-3);
    }
}
