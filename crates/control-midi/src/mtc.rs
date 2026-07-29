//! Réception MIDI Time Code (MTC) — logique pure, testée.
//!
//! Deux couches, aucune IO :
//!
//! - [`MtcAssembler`] : octets bruts → [`MtcEvent`]. Assemble les
//!   quarter-frames `F1 nn` (8 pièces par valeur de timecode, détection de
//!   direction avant/arrière, compensation de la latence de transmission)
//!   et décode les full-frames SysEx `F0 7F cc 01 01 hh mm ss ff F7`
//!   (cadence dans les bits 5-6 de `hh`). Une rupture de séquence (pièces
//!   perdues, saut) émet [`MtcEvent::SignalLost`] et repart proprement.
//! - [`MtcClock`] : événements + horloge monotone (`now_s`) → temps courant.
//!   Interpole ENTRE les quarter-frames (le TC avance entre deux messages),
//!   freewheel de [`FREEWHEEL_S`] secondes sur perte de signal (le TC
//!   continue d'avancer en interne) puis unlock (temps figé, `locked =
//!   false`). L'arithmétique passe par les index de frames de
//!   [`conduite_core::TcTime`] : le drop-frame 29.97 est exact (les frames
//!   00/01 des minutes non multiples de 10 n'existent pas).
//!
//! Latence des quarter-frames : en marche AVANT, les 8 pièces couvrent
//! 2 frames — quand la pièce 7 arrive, le transport est 2 frames plus loin
//! que le temps encodé (compensé ici). En marche ARRIÈRE, les pièces sont
//! émises 7 → 0 et la pièce 0 (reçue en dernier) coïncide avec le début du
//! groupe : rien à compenser.
//!
//! Consommation : le hub route ces événements dans un canal DÉDIÉ (ce n'est
//! pas un flux de [`Command`](conduite_core::Command) mais une horloge,
//! consommée par l'app à chaque tick). Le chase agit au niveau des CUES ;
//! l'option « Timecode » du popover d'animation des paramètres reste grisée
//! (`ModKind::TimecodeChase` réservé, inchangé).

use conduite_core::{TcRate, TcTime};

/// Durée du freewheel après le dernier message MTC valide (secondes) :
/// le temps continue d'avancer, puis la position se fige et `locked = false`.
pub const FREEWHEEL_S: f64 = 2.0;

/// Événement produit par l'assembleur MTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MtcEvent {
    /// Signal acquis (ou cadence changée) : premier groupe de quarter-frames
    /// complet après un silence/une rupture. Le temps arrive avec le
    /// [`MtcEvent::Time`] suivant (2 frames plus tard).
    Locked(TcRate),
    /// Temps assemblé depuis les quarter-frames (latence déjà compensée).
    Time(TcTime),
    /// Full-frame SysEx : position envoyée à l'arrêt ou lors d'un locate —
    /// à traiter comme un SEEK (recalage), pas comme un signal qui court.
    FullFrame(TcTime),
    /// Rupture de la séquence de quarter-frames (pièces perdues, saut) :
    /// le freewheel de l'horloge prend le relais.
    SignalLost,
}

/// Cadence encodée sur 2 bits (quarter-frame pièce 7, full-frame `hh`).
fn decode_rate(code: u8) -> TcRate {
    match code & 0x03 {
        0 => TcRate::Fps24,
        1 => TcRate::Fps25,
        2 => TcRate::Fps2997Df,
        _ => TcRate::Fps30,
    }
}

/// Champs dans les bornes de la cadence (heure < 24, étiquettes drop-frame
/// existantes) — rejette les assemblages incohérents (bruit, pièces mêlées).
fn is_valid(t: TcTime, rate: TcRate) -> bool {
    let ranges = t.h < 24 && t.m < 60 && t.s < 60 && u32::from(t.f) < rate.nominal_fps();
    let dropped = rate.is_drop_frame() && t.s == 0 && t.f < 2 && !t.m.is_multiple_of(10);
    ranges && !dropped
}

/// Avance (ou recule) de `delta` frames avec wrap 24 h — drop-frame correct
/// via les index de frames de `conduite-core`.
fn add_frames(t: TcTime, rate: TcRate, delta: i64) -> TcTime {
    let day = rate.frames_per_day() as i64;
    let idx = (t.to_frames(rate) as i64 + delta).rem_euclid(day);
    TcTime::from_frames(idx as u64, rate)
}

/// Assembleur de quarter-frames + full-frames MTC. Pur : nourri d'octets
/// bruts par [`crate::MidiEngine`], il émet au plus un événement par message.
#[derive(Debug)]
pub struct MtcAssembler {
    /// Valeur 4 bits de chaque pièce (0..=7) du groupe en cours.
    data: [u8; 8],
    /// Pièces déjà reçues dans le groupe en cours (bit n = pièce n).
    mask: u8,
    /// Dernière pièce reçue (détection de direction et de rupture).
    prev: Option<u8>,
    /// Direction courante du transport (true = avant).
    forward: bool,
    /// Un groupe complet a déjà été assemblé sans rupture depuis.
    locked: bool,
    /// Dernière cadence décodée (pièce 7 ou full-frame).
    rate: Option<TcRate>,
}

impl Default for MtcAssembler {
    fn default() -> Self {
        MtcAssembler::new()
    }
}

impl MtcAssembler {
    pub fn new() -> Self {
        MtcAssembler {
            data: [0; 8],
            mask: 0,
            prev: None,
            forward: true,
            locked: false,
            rate: None,
        }
    }

    /// Vrai si la trame est du MTC (quarter-frame `F1` ou full-frame SysEx) —
    /// permet au moteur de router ces octets ici sans toucher au reste.
    pub fn is_mtc(bytes: &[u8]) -> bool {
        matches!(
            bytes,
            [0xF1, ..] | [0xF0, 0x7F, _, 0x01, 0x01, _, _, _, _, 0xF7]
        )
    }

    /// Dernière cadence vue (quarter-frame pièce 7 ou full-frame).
    pub fn rate(&self) -> Option<TcRate> {
        self.rate
    }

    /// Traite une trame MIDI brute. `None` : trame non-MTC, groupe encore
    /// incomplet, ou assemblage incohérent (ignoré).
    pub fn push(&mut self, bytes: &[u8]) -> Option<MtcEvent> {
        match bytes {
            [0xF1, d, ..] => self.quarter_frame(d & 0x7F),
            [0xF0, 0x7F, _dev, 0x01, 0x01, h, m, s, f, 0xF7] => self.full_frame(*h, *m, *s, *f),
            _ => None,
        }
    }

    fn quarter_frame(&mut self, d: u8) -> Option<MtcEvent> {
        let piece = d >> 4; // 0..=7
        let val = d & 0x0F;
        let mut lost = false;
        if let Some(p) = self.prev {
            if piece == (p + 1) % 8 {
                // Pas en avant ; un demi-tour vide la collecte en cours.
                if !self.forward {
                    self.forward = true;
                    self.mask = 0;
                }
            } else if piece == (p + 7) % 8 {
                if self.forward {
                    self.forward = false;
                    self.mask = 0;
                }
            } else {
                // Rupture : pièces perdues ou saut de position.
                lost = self.locked;
                self.locked = false;
                self.mask = 0;
            }
        }
        // Début de groupe (pièce 0 en avant, 7 en arrière) : collecte neuve —
        // les pièces précédentes appartenaient au groupe d'avant.
        if (self.forward && piece == 0) || (!self.forward && piece == 7) {
            self.mask = 0;
        }
        self.data[piece as usize] = val;
        self.mask |= 1 << piece;
        self.prev = Some(piece);
        if lost {
            return Some(MtcEvent::SignalLost);
        }
        let complete =
            self.mask == 0xFF && ((self.forward && piece == 7) || (!self.forward && piece == 0));
        if complete {
            self.assemble()
        } else {
            None
        }
    }

    /// Groupe complet : décode, valide, compense la latence, verrouille.
    fn assemble(&mut self) -> Option<MtcEvent> {
        let t = TcTime::new(
            self.data[6] | ((self.data[7] & 0x01) << 4),
            self.data[4] | ((self.data[5] & 0x03) << 4),
            self.data[2] | ((self.data[3] & 0x03) << 4),
            self.data[0] | ((self.data[1] & 0x01) << 4),
        );
        let rate = decode_rate(self.data[7] >> 1);
        if !is_valid(t, rate) {
            // Assemblage incohérent (bruit) : on repart sans événement.
            self.locked = false;
            self.mask = 0;
            return None;
        }
        // Latence : +2 frames en marche avant, rien en arrière (cf. doc module).
        let t = if self.forward { add_frames(t, rate, 2) } else { t };
        let newly = !self.locked || self.rate != Some(rate);
        self.locked = true;
        self.rate = Some(rate);
        Some(if newly {
            MtcEvent::Locked(rate)
        } else {
            MtcEvent::Time(t)
        })
    }

    /// Full-frame SysEx : position + cadence (bits 5-6 de `hh`). Vide la
    /// collecte de quarter-frames — le prochain flux QF réémettra `Locked`.
    fn full_frame(&mut self, hh: u8, m: u8, s: u8, f: u8) -> Option<MtcEvent> {
        let rate = decode_rate((hh >> 5) & 0x03);
        let t = TcTime::new(hh & 0x1F, m & 0x7F, s & 0x7F, f & 0x7F);
        if !is_valid(t, rate) {
            return None;
        }
        self.mask = 0;
        self.prev = None;
        self.locked = false;
        self.rate = Some(rate);
        Some(MtcEvent::FullFrame(t))
    }
}

/// Base d'interpolation de l'horloge.
#[derive(Debug, Clone, Copy)]
struct Base {
    time: TcTime,
    at_s: f64,
    /// Le signal court (quarter-frames) : le temps avance entre deux
    /// messages. Faux après un full-frame (position figée d'un locate).
    running: bool,
}

/// Horloge MTC : consomme les [`MtcEvent`] et fournit le temps courant
/// interpolé à n'importe quel instant `now_s` (secondes monotones, même
/// origine que les `now_s` passés à [`MtcClock::feed`]).
#[derive(Debug)]
pub struct MtcClock {
    rate: TcRate,
    base: Option<Base>,
    /// Instant du dernier message MTC reçu (départ du freewheel).
    last_signal_s: f64,
}

impl Default for MtcClock {
    fn default() -> Self {
        MtcClock::new()
    }
}

impl MtcClock {
    pub fn new() -> Self {
        MtcClock {
            rate: TcRate::Fps25,
            base: None,
            last_signal_s: f64::NEG_INFINITY,
        }
    }

    /// Cadence courante (25 tant que rien n'a été reçu).
    pub fn rate(&self) -> TcRate {
        self.rate
    }

    /// Intègre un événement de l'assembleur, daté `now_s`.
    pub fn feed(&mut self, event: MtcEvent, now_s: f64) {
        match event {
            MtcEvent::Locked(rate) => {
                // Re-verrouillage après un trou : la position freewheelée est
                // figée (pas de saut) — le Time suivant recale, comme un seek.
                if let Some((t, false)) = self.current(now_s) {
                    self.base = Some(Base {
                        time: t,
                        at_s: now_s,
                        running: false,
                    });
                }
                self.rate = rate;
                self.last_signal_s = now_s;
            }
            MtcEvent::Time(t) => {
                self.base = Some(Base {
                    time: t,
                    at_s: now_s,
                    running: true,
                });
                self.last_signal_s = now_s;
            }
            MtcEvent::FullFrame(t) => {
                self.base = Some(Base {
                    time: t,
                    at_s: now_s,
                    running: false,
                });
                self.last_signal_s = now_s;
            }
            MtcEvent::SignalLost => {
                // Rupture : on ne fige rien tout de suite — le freewheel court
                // depuis le dernier message, l'unlock viendra du timeout.
            }
        }
    }

    /// Temps courant. `None` tant qu'aucune position n'a jamais été reçue ;
    /// sinon `(temps, locked)` — pendant le freewheel le temps avance encore
    /// (`locked` reste vrai), après [`FREEWHEEL_S`] il se fige et `locked`
    /// passe faux. Les cues actives CONTINUENT : l'unlock ne coupe rien.
    pub fn current(&self, now_s: f64) -> Option<(TcTime, bool)> {
        let base = self.base?;
        let locked = now_s - self.last_signal_s <= FREEWHEEL_S;
        if !base.running {
            return Some((base.time, locked));
        }
        // Le temps avance jusqu'à `now`, ou jusqu'à la fin du freewheel.
        let until = if locked {
            now_s
        } else {
            self.last_signal_s + FREEWHEEL_S
        };
        let elapsed = (until - base.at_s).max(0.0);
        let frames = (elapsed * self.rate.fps()).floor() as i64;
        Some((add_frames(base.time, self.rate, frames), locked))
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Les 8 trames quarter-frame encodant `t` (cadence `rate_code` 0..=3),
    /// dans l'ordre des pièces 0..=7.
    fn qf_group(t: TcTime, rate_code: u8) -> [[u8; 2]; 8] {
        let pieces = [
            t.f & 0x0F,
            (t.f >> 4) & 0x01,
            t.s & 0x0F,
            (t.s >> 4) & 0x03,
            t.m & 0x0F,
            (t.m >> 4) & 0x03,
            t.h & 0x0F,
            ((t.h >> 4) & 0x01) | ((rate_code & 0x03) << 1),
        ];
        std::array::from_fn(|i| [0xF1, ((i as u8) << 4) | pieces[i]])
    }

    /// Pousse un groupe complet en marche avant, retourne les événements.
    fn push_fwd(a: &mut MtcAssembler, t: TcTime, rate_code: u8) -> Vec<MtcEvent> {
        qf_group(t, rate_code)
            .iter()
            .filter_map(|b| a.push(b))
            .collect()
    }

    /// Pousse un groupe complet en marche arrière (pièces 7 → 0).
    fn push_bwd(a: &mut MtcAssembler, t: TcTime, rate_code: u8) -> Vec<MtcEvent> {
        qf_group(t, rate_code)
            .iter()
            .rev()
            .filter_map(|b| a.push(b))
            .collect()
    }

    fn full_frame_bytes(t: TcTime, rate_code: u8) -> [u8; 10] {
        [
            0xF0,
            0x7F,
            0x7F,
            0x01,
            0x01,
            ((rate_code & 0x03) << 5) | (t.h & 0x1F),
            t.m,
            t.s,
            t.f,
            0xF7,
        ]
    }

    // ------------------------------------------------------------ Assembleur

    #[test]
    fn forward_assembly_locks_then_times_with_2_frame_compensation() {
        let mut a = MtcAssembler::new();
        // Premier groupe complet : acquisition (le temps vient au suivant).
        let evs = push_fwd(&mut a, TcTime::new(1, 0, 0, 0), 1);
        assert_eq!(evs, vec![MtcEvent::Locked(TcRate::Fps25)]);
        assert_eq!(a.rate(), Some(TcRate::Fps25));
        // Groupe suivant (+2 frames dans le flux réel) : temps compensé +2.
        let evs = push_fwd(&mut a, TcTime::new(1, 0, 0, 2), 1);
        assert_eq!(evs, vec![MtcEvent::Time(TcTime::new(1, 0, 0, 4))]);
        // La compensation traverse aussi les secondes.
        let evs = push_fwd(&mut a, TcTime::new(1, 0, 0, 24), 1);
        assert_eq!(evs, vec![MtcEvent::Time(TcTime::new(1, 0, 1, 1))]);
    }

    #[test]
    fn forward_compensation_respects_drop_frame() {
        let mut a = MtcAssembler::new();
        assert_eq!(
            push_fwd(&mut a, TcTime::new(0, 0, 59, 26), 2),
            vec![MtcEvent::Locked(TcRate::Fps2997Df)]
        );
        // 00:00:59:28 + 2 frames : 29 puis saut des étiquettes 00/01.
        assert_eq!(
            push_fwd(&mut a, TcTime::new(0, 0, 59, 28), 2),
            vec![MtcEvent::Time(TcTime::new(0, 1, 0, 2))]
        );
    }

    #[test]
    fn backward_assembly_locks_and_does_not_compensate() {
        let mut a = MtcAssembler::new();
        // Premier groupe arrière : la direction ne se détecte qu'à la 2e
        // pièce, le groupe est incomplet → rien.
        assert_eq!(push_bwd(&mut a, TcTime::new(10, 0, 0, 10), 1), vec![]);
        // Deuxième groupe : complet → acquisition.
        assert_eq!(
            push_bwd(&mut a, TcTime::new(10, 0, 0, 8), 1),
            vec![MtcEvent::Locked(TcRate::Fps25)]
        );
        // Troisième : temps EXACT (la pièce 0 arrive au début du groupe).
        assert_eq!(
            push_bwd(&mut a, TcTime::new(10, 0, 0, 6), 1),
            vec![MtcEvent::Time(TcTime::new(10, 0, 0, 6))]
        );
    }

    #[test]
    fn partial_group_then_full_groups() {
        let mut a = MtcAssembler::new();
        // Prise en cours de route : pièces 3..=7 seulement → incomplet.
        for b in &qf_group(TcTime::new(0, 0, 1, 0), 0)[3..] {
            assert_eq!(a.push(b), None);
        }
        // Le groupe suivant, complet, verrouille.
        assert_eq!(
            push_fwd(&mut a, TcTime::new(0, 0, 1, 2), 0),
            vec![MtcEvent::Locked(TcRate::Fps24)]
        );
    }

    #[test]
    fn sequence_break_emits_signal_lost_then_relocks() {
        let mut a = MtcAssembler::new();
        push_fwd(&mut a, TcTime::new(0, 0, 1, 0), 1);
        push_fwd(&mut a, TcTime::new(0, 0, 1, 2), 1);
        // Pièce 5 alors qu'on attend 0 : rupture (saut de position).
        assert_eq!(a.push(&[0xF1, 0x50]), Some(MtcEvent::SignalLost));
        // Une seule rupture signalée, puis silence jusqu'au recalage.
        assert_eq!(a.push(&[0xF1, 0x02]), None);
        // Deux groupes frais : re-verrouillage puis temps.
        assert_eq!(
            push_fwd(&mut a, TcTime::new(0, 2, 0, 0), 1),
            vec![MtcEvent::Locked(TcRate::Fps25)]
        );
        assert_eq!(
            push_fwd(&mut a, TcTime::new(0, 2, 0, 2), 1),
            vec![MtcEvent::Time(TcTime::new(0, 2, 0, 4))]
        );
    }

    #[test]
    fn full_frame_decodes_time_and_rate() {
        let mut a = MtcAssembler::new();
        let t = TcTime::new(5, 4, 3, 2);
        assert_eq!(
            a.push(&full_frame_bytes(t, 3)),
            Some(MtcEvent::FullFrame(t))
        );
        assert_eq!(a.rate(), Some(TcRate::Fps30));
        // Cadence 29.97 DF dans les bits 5-6 de hh.
        let t = TcTime::new(23, 59, 0, 29);
        assert_eq!(
            a.push(&full_frame_bytes(t, 2)),
            Some(MtcEvent::FullFrame(t))
        );
        assert_eq!(a.rate(), Some(TcRate::Fps2997Df));
        // Après un full-frame, le flux QF ré-annonce l'acquisition.
        assert_eq!(
            push_fwd(&mut a, TcTime::new(23, 59, 1, 0), 2),
            vec![MtcEvent::Locked(TcRate::Fps2997Df)]
        );
    }

    #[test]
    fn garbage_is_ignored() {
        let mut a = MtcAssembler::new();
        // Trames non-MTC.
        assert!(!MtcAssembler::is_mtc(&[0x90, 60, 100]));
        assert!(!MtcAssembler::is_mtc(&[0xF0, 0x7F, 0x01, 0x02, 0x7F, 0x01, 0xF7]));
        assert!(MtcAssembler::is_mtc(&[0xF1, 0x00]));
        assert_eq!(a.push(&[0x90, 60, 100]), None);
        assert_eq!(a.push(&[0xF1]), None); // quarter-frame tronqué
        // Full-frame avec des secondes impossibles : ignoré.
        let mut bad = full_frame_bytes(TcTime::new(1, 0, 0, 0), 1);
        bad[7] = 61;
        assert_eq!(a.push(&bad), None);
        // Groupe QF incohérent (secondes = 61) : aucun événement.
        let mut t = TcTime::new(0, 0, 0, 0);
        t.s = 61;
        assert_eq!(push_fwd(&mut a, t, 1), vec![]);
        // Étiquette drop-frame inexistante (00:01:00:00 en 29.97 DF).
        let mut a = MtcAssembler::new();
        assert_eq!(push_fwd(&mut a, TcTime::new(0, 1, 0, 0), 2), vec![]);
    }

    // --------------------------------------------------------------- Horloge

    #[test]
    fn clock_interpolates_between_quarter_frames() {
        let mut c = MtcClock::new();
        assert_eq!(c.current(0.0), None);
        c.feed(MtcEvent::Locked(TcRate::Fps25), 0.0);
        c.feed(MtcEvent::Time(TcTime::new(1, 0, 0, 0)), 0.0);
        // Entre deux messages (80 ms à 25 fps), le temps avance frame à frame.
        assert_eq!(c.current(0.02), Some((TcTime::new(1, 0, 0, 0), true)));
        assert_eq!(c.current(0.05), Some((TcTime::new(1, 0, 0, 1), true)));
        assert_eq!(c.current(0.081), Some((TcTime::new(1, 0, 0, 2), true)));
    }

    #[test]
    fn clock_interpolation_is_drop_frame_correct() {
        let mut c = MtcClock::new();
        c.feed(MtcEvent::Locked(TcRate::Fps2997Df), 0.0);
        c.feed(MtcEvent::Time(TcTime::new(0, 0, 59, 29)), 0.0);
        // Une frame plus tard : les étiquettes 00/01 de la minute 1 n'existent
        // pas — le contrat : 00:00:59:29 → 00:01:00:02.
        let one_frame = 1.001 / 30.0;
        assert_eq!(
            c.current(one_frame + 0.001),
            Some((TcTime::new(0, 1, 0, 2), true))
        );
    }

    #[test]
    fn clock_freewheels_then_unlocks_frozen() {
        let mut c = MtcClock::new();
        c.feed(MtcEvent::Locked(TcRate::Fps25), 10.0);
        c.feed(MtcEvent::Time(TcTime::new(0, 5, 0, 0)), 10.0);
        c.feed(MtcEvent::SignalLost, 10.04);
        // Pendant le freewheel : le temps AVANCE encore, locked reste vrai.
        assert_eq!(c.current(11.5), Some((TcTime::new(0, 5, 1, 12), true)));
        // Après 2 s sans message : unlock, temps figé à la fin du freewheel.
        assert_eq!(c.current(12.5), Some((TcTime::new(0, 5, 2, 0), false)));
        assert_eq!(c.current(60.0), Some((TcTime::new(0, 5, 2, 0), false)));
    }

    #[test]
    fn clock_relocks_without_jumping_then_reseats_on_time() {
        let mut c = MtcClock::new();
        c.feed(MtcEvent::Locked(TcRate::Fps25), 0.0);
        c.feed(MtcEvent::Time(TcTime::new(0, 5, 0, 0)), 0.0);
        // Signal perdu à 0.0 : figé à 0:05:02:00 après le freewheel.
        assert_eq!(c.current(30.0), Some((TcTime::new(0, 5, 2, 0), false)));
        // Le signal revient : re-verrouillage SANS saut (position figée)…
        c.feed(MtcEvent::Locked(TcRate::Fps25), 30.0);
        assert_eq!(c.current(30.05), Some((TcTime::new(0, 5, 2, 0), true)));
        // … puis le premier Time recale, comme un seek.
        c.feed(MtcEvent::Time(TcTime::new(2, 0, 0, 0)), 30.08);
        assert_eq!(c.current(30.13), Some((TcTime::new(2, 0, 0, 1), true)));
    }

    #[test]
    fn clock_full_frame_is_a_frozen_seek() {
        let mut c = MtcClock::new();
        let t = TcTime::new(3, 0, 0, 0);
        c.feed(MtcEvent::FullFrame(t), 0.0);
        // Position tenue sans avancer (transport à l'arrêt).
        assert_eq!(c.current(0.5), Some((t, true)));
        // Plus de messages : la position reste, mais le lock tombe.
        assert_eq!(c.current(3.0), Some((t, false)));
        // Un full-frame pendant la lecture = seek : la position saute.
        c.feed(MtcEvent::Locked(TcRate::Fps25), 4.0);
        c.feed(MtcEvent::Time(TcTime::new(3, 0, 1, 0)), 4.0);
        c.feed(MtcEvent::FullFrame(TcTime::new(9, 0, 0, 0)), 4.5);
        assert_eq!(c.current(4.6), Some((TcTime::new(9, 0, 0, 0), true)));
    }

    #[test]
    fn clock_wraps_over_midnight() {
        let mut c = MtcClock::new();
        c.feed(MtcEvent::Locked(TcRate::Fps25), 0.0);
        c.feed(MtcEvent::Time(TcTime::new(23, 59, 59, 24)), 0.0);
        assert_eq!(c.current(0.05), Some((TcTime::new(0, 0, 0, 0), true)));
    }
}
