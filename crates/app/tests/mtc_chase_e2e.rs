//! Intégration MTC de bout en bout — quarter-frames synthétiques →
//! [`MtcAssembler`] → [`MtcClock`] → [`CueEngine`] (chase de cues).
//!
//! C'est exactement la chaîne de production (`protocols::drain_mtc` →
//! `session::update_timecode` → `EngineTick::tc`), sans IO MIDI : on
//! vérifie que des octets MTC bruts déclenchent bien des cues.

use conduite_control_midi::{MtcAssembler, MtcClock};
use conduite_core::{Cue, CueNumber, CueTriggers, FollowMode, TcRate, TcTime, Transition};
use conduite_cue::{CueEngine, CueEvent, EngineTick, TcState};

/// Les 8 trames quarter-frame (pièces 0..=7) encodant `t` à 25 fps.
fn qf_group(t: TcTime) -> [[u8; 2]; 8] {
    let rate_code = 1u8; // 25 fps
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

/// Full-frame SysEx (position d'un locate) à 25 fps.
fn full_frame(t: TcTime) -> [u8; 10] {
    let rate_code = 1u8;
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

/// Cue minimale déclenchée par timecode (transition Cut, follow Manual).
fn cue_at(n: u32, tc: TcTime) -> Cue {
    Cue {
        number: CueNumber(n),
        name: format!("cue {n}"),
        color: None,
        notes: String::new(),
        armed: true,
        transition: Transition::default(),
        follow: FollowMode::Manual,
        goto_after: None,
        states: Vec::new(),
        mod_routes: Vec::new(),
        triggers: CueTriggers {
            timecode: Some(tc),
            ..CueTriggers::default()
        },
    }
}

/// Banc d'essai : la chaîne complète, datée par une horloge simulée.
struct Bench {
    asm: MtcAssembler,
    clock: MtcClock,
    engine: CueEngine,
    now_s: f64,
    started: Vec<CueNumber>,
}

impl Bench {
    fn new(cues: Vec<Cue>) -> Self {
        let mut engine = CueEngine::new();
        engine.load(&cues);
        Bench {
            asm: MtcAssembler::new(),
            clock: MtcClock::new(),
            engine,
            now_s: 100.0,
            started: Vec::new(),
        }
    }

    /// Injecte une trame MIDI brute dans l'assembleur, datée `now_s`.
    fn push_bytes(&mut self, bytes: &[u8]) {
        if let Some(ev) = self.asm.push(bytes) {
            self.clock.feed(ev, self.now_s);
        }
    }

    /// Un tick moteur : l'horloge fournit `EngineTick::tc`, comme
    /// `session::update_timecode` en production.
    fn tick(&mut self) {
        let tc = self.clock.current(self.now_s).map(|(time, locked)| TcState {
            time,
            rate: self.clock.rate(),
            locked,
        });
        let frame = self.engine.tick(EngineTick {
            now_s: self.now_s,
            media_eof: &|_| false,
            tc,
        });
        for ev in &frame.events {
            if let CueEvent::CueStarted { cue } = ev {
                self.started.push(*cue);
            }
        }
    }

    /// Un groupe complet de quarter-frames encodant `t` (8 pièces à 10 ms
    /// d'intervalle — cadence réelle du MTC à 25 fps), puis un tick.
    fn feed_group_and_tick(&mut self, t: TcTime) {
        for bytes in qf_group(t) {
            self.push_bytes(&bytes);
            self.now_s += 0.010;
        }
        self.tick();
    }

    fn active(&self) -> Option<u32> {
        self.engine.status().active.map(|n| n.0)
    }
}

/// Lecture normale : le transport court depuis 01:00:00:00 et passe les
/// triggers 01:00:00:10 puis 01:00:01:00 — les deux cues partent, dans
/// l'ordre, par GO automatique.
#[test]
fn quarter_frames_drive_cues_end_to_end() {
    let mut b = Bench::new(vec![
        cue_at(1000, TcTime::new(1, 0, 0, 10)),
        cue_at(2000, TcTime::new(1, 0, 1, 0)),
    ]);
    // 14 groupes = 28 frames de transport : 01:00:00:00 → 01:00:01:03.
    let t0 = TcTime::new(1, 0, 0, 0);
    for k in 0..14u64 {
        let t = TcTime::from_frames(t0.to_frames(TcRate::Fps25) + 2 * k, TcRate::Fps25);
        b.feed_group_and_tick(t);
    }
    assert_eq!(
        b.started,
        vec![CueNumber(1000), CueNumber(2000)],
        "chaque trigger passé déclenche sa cue, dans l'ordre"
    );
    assert_eq!(b.active(), Some(2000));
}

/// Locate (full-frame) après acquisition : SAUT en avant → calage GOTO sur
/// la DERNIÈRE cue dont le trigger est passé, sans jouer les précédentes.
/// Puis perte de signal : après la roue libre, rien n'est coupé.
#[test]
fn full_frame_seek_then_signal_loss_end_to_end() {
    let mut b = Bench::new(vec![
        cue_at(1000, TcTime::new(1, 0, 0, 10)),
        cue_at(2000, TcTime::new(1, 0, 1, 0)),
    ]);
    // Acquisition avant les triggers : 3 groupes depuis 01:00:00:00.
    let t0 = TcTime::new(1, 0, 0, 0);
    for k in 0..3u64 {
        let t = TcTime::from_frames(t0.to_frames(TcRate::Fps25) + 2 * k, TcRate::Fps25);
        b.feed_group_and_tick(t);
    }
    assert_eq!(b.active(), None, "aucun trigger encore passé");
    assert_eq!(b.started, vec![]);

    // Locate à 02:00:00:00 : les deux triggers sont derrière — on se cale
    // sur la dernière (2000), la 1000 n'est PAS rejouée.
    b.push_bytes(&full_frame(TcTime::new(2, 0, 0, 0)));
    b.now_s += 0.010;
    b.tick();
    assert_eq!(b.active(), Some(2000), "seek : GOTO de la dernière cue passée");
    assert_eq!(b.started, vec![CueNumber(2000)]);

    // Silence MTC : roue libre puis suspension — la cue active CONTINUE.
    b.now_s += 5.0;
    b.tick();
    let (_, locked) = b.clock.current(b.now_s).expect("position connue");
    assert!(!locked, "après 2 s sans message le lock tombe");
    assert_eq!(b.active(), Some(2000), "perte de signal : rien n'est coupé");
}
