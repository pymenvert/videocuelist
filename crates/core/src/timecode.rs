//! Timecode SMPTE — types du contrat (`docs/INTERFACES.md`, § core).
//!
//! [`TcTime`] est la position (heures/minutes/secondes/frames) et [`TcRate`]
//! la cadence. L'arithmétique frames ↔ TcTime couvre le drop-frame 29.97
//! (convention SMPTE : les frames 0 et 1 sont sautées chaque minute, sauf
//! les minutes multiples de 10).
//!
//! Sérialisation : `TcTime` s'écrit `"HH:MM:SS:FF"` (JSON lisible — même
//! forme que `Display`/`FromStr`), `TcRate` en snake_case (`"fps25"`…).
//! Le chase lui-même vit dans la crate `cue` ; l'option « Timecode » du
//! popover d'animation des paramètres reste grisée
//! ([`crate::ModKind::TimecodeChase`] inchangé, réservé).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// Cadence de timecode supportée (MTC/LTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TcRate {
    /// 24 images/s (cinéma).
    Fps24,
    /// 25 images/s (PAL/EBU — défaut spectacle en Europe).
    Fps25,
    /// 29.97 images/s drop-frame (NTSC).
    Fps2997Df,
    /// 30 images/s non-drop.
    Fps30,
}

impl TcRate {
    /// Base de comptage HH:MM:SS:FF (30 pour le drop-frame : les étiquettes
    /// vont de 00 à 29 même si des numéros sont sautés).
    pub fn nominal_fps(self) -> u32 {
        match self {
            TcRate::Fps24 => 24,
            TcRate::Fps25 => 25,
            TcRate::Fps2997Df | TcRate::Fps30 => 30,
        }
    }

    /// Cadence réelle en images par seconde (29.97 = 30000/1001).
    pub fn fps(self) -> f64 {
        match self {
            TcRate::Fps2997Df => 30_000.0 / 1_001.0,
            other => f64::from(other.nominal_fps()),
        }
    }

    /// Drop-frame ? (seul 29.97 DF l'est.)
    pub fn is_drop_frame(self) -> bool {
        matches!(self, TcRate::Fps2997Df)
    }

    /// Nombre de frames dans 24 h à cette cadence (borne de wrap).
    pub fn frames_per_day(self) -> u64 {
        match self {
            // 24 × (108000 − 108) : 2 frames sautées par minute sauf les
            // minutes multiples de 10 → 2 × 54 = 108 par heure.
            TcRate::Fps2997Df => 2_589_408,
            other => 86_400 * u64::from(other.nominal_fps()),
        }
    }
}

impl fmt::Display for TcRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TcRate::Fps24 => write!(f, "24"),
            TcRate::Fps25 => write!(f, "25"),
            TcRate::Fps2997Df => write!(f, "29.97DF"),
            TcRate::Fps30 => write!(f, "30"),
        }
    }
}

/// Position de timecode `HH:MM:SS:FF`. Sérialisée en JSON sous cette même
/// forme texte (contrat : `Cue.triggers.timecode`, `runtime.timecode.time`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct TcTime {
    pub h: u8,
    pub m: u8,
    pub s: u8,
    pub f: u8,
}

impl TcTime {
    /// Constructeur simple (aucune validation : réservé aux valeurs sûres —
    /// passer par [`FromStr`] pour du texte utilisateur).
    pub fn new(h: u8, m: u8, s: u8, f: u8) -> Self {
        TcTime { h, m, s, f }
    }

    /// Nombre de frames écoulées depuis 00:00:00:00 à la cadence donnée.
    /// Drop-frame : les étiquettes sautées sont retranchées (deux par
    /// minute, sauf les minutes multiples de 10).
    pub fn to_frames(self, rate: TcRate) -> u64 {
        let h = u64::from(self.h);
        let m = u64::from(self.m);
        let s = u64::from(self.s);
        let f = u64::from(self.f);
        let nominal = u64::from(rate.nominal_fps());
        let base = (h * 3600 + m * 60 + s) * nominal + f;
        if rate.is_drop_frame() {
            let total_min = h * 60 + m;
            base - 2 * (total_min - total_min / 10)
        } else {
            base
        }
    }

    /// Position correspondant à un nombre de frames à la cadence donnée
    /// (wrap sur 24 h). Inverse exact de [`Self::to_frames`].
    pub fn from_frames(frames: u64, rate: TcRate) -> Self {
        let frames = frames % rate.frames_per_day();
        let nominal = u64::from(rate.nominal_fps());
        let frames = if rate.is_drop_frame() {
            // Réinjecte les étiquettes sautées : 17982 frames par bloc de
            // 10 minutes, 1798 par minute hors première minute du bloc.
            let d = frames / 17_982;
            let m = frames % 17_982;
            let extra = if m > 1 { 2 * ((m - 2) / 1_798) } else { 0 };
            frames + 18 * d + extra
        } else {
            frames
        };
        TcTime {
            h: (frames / (nominal * 3600)) as u8,
            m: ((frames / (nominal * 60)) % 60) as u8,
            s: ((frames / nominal) % 60) as u8,
            f: (frames % nominal) as u8,
        }
    }
}

impl fmt::Display for TcTime {
    fn fmt(&self, fm: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fm, "{:02}:{:02}:{:02}:{:02}", self.h, self.m, self.s, self.f)
    }
}

impl FromStr for TcTime {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || CoreError::InvalidTimecode(s.to_string());
        let raw = s.trim();
        // Tolérance d'entrée : le séparateur drop-frame ';' est accepté.
        let parts: Vec<&str> = raw.split([':', ';']).collect();
        let [h, m, sec, f] = parts.as_slice() else {
            return Err(err());
        };
        let field = |p: &str, max: u8| -> Result<u8, CoreError> {
            if p.is_empty() || p.len() > 2 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(err());
            }
            let v: u8 = p.parse().map_err(|_| err())?;
            if v > max {
                return Err(err());
            }
            Ok(v)
        };
        Ok(TcTime {
            h: field(h, 23)?,
            m: field(m, 59)?,
            s: field(sec, 59)?,
            f: field(f, 59)?,
        })
    }
}

impl From<TcTime> for String {
    fn from(t: TcTime) -> Self {
        t.to_string()
    }
}

impl TryFrom<String> for TcTime {
    type Error = CoreError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_parse_roundtrip() {
        for (s, t) in [
            ("00:00:00:00", TcTime::new(0, 0, 0, 0)),
            ("01:02:03:04", TcTime::new(1, 2, 3, 4)),
            ("23:59:59:29", TcTime::new(23, 59, 59, 29)),
        ] {
            assert_eq!(t.to_string(), s);
            assert_eq!(TcTime::from_str(s).expect("parse"), t);
        }
        // Tolérances : espaces, séparateur ';' drop-frame, un seul chiffre.
        assert_eq!(
            TcTime::from_str(" 10:00:05;12 ").expect("parse"),
            TcTime::new(10, 0, 5, 12)
        );
        assert_eq!(TcTime::from_str("1:2:3:4").expect("parse"), TcTime::new(1, 2, 3, 4));
        for bad in [
            "", "10:00:00", "10:00:00:00:00", "24:00:00:00", "10:60:00:00",
            "10:00:60:00", "10:00:00:60", "aa:00:00:00", "-1:00:00:00",
            "10:00:00:001", "+1:00:00:00",
        ] {
            assert!(TcTime::from_str(bad).is_err(), "aurait dû refuser {bad:?}");
        }
    }

    #[test]
    fn serde_is_display_string() {
        let t = TcTime::new(10, 0, 5, 12);
        let json = serde_json::to_string(&t).expect("ser");
        assert_eq!(json, r#""10:00:05:12""#);
        let back: TcTime = serde_json::from_str(&json).expect("de");
        assert_eq!(back, t);
        // Une chaîne invalide est refusée à la désérialisation.
        let res: Result<TcTime, _> = serde_json::from_str(r#""99:00:00:00""#);
        assert!(res.is_err());
    }

    #[test]
    fn rate_serde_tokens_are_stable() {
        for (rate, want) in [
            (TcRate::Fps24, r#""fps24""#),
            (TcRate::Fps25, r#""fps25""#),
            (TcRate::Fps2997Df, r#""fps2997_df""#),
            (TcRate::Fps30, r#""fps30""#),
        ] {
            assert_eq!(serde_json::to_string(&rate).expect("ser"), want);
            let back: TcRate = serde_json::from_str(want).expect("de");
            assert_eq!(back, rate);
        }
    }

    #[test]
    fn frames_conversion_non_drop() {
        for rate in [TcRate::Fps24, TcRate::Fps25, TcRate::Fps30] {
            let n = u64::from(rate.nominal_fps());
            assert_eq!(TcTime::new(0, 0, 0, 0).to_frames(rate), 0);
            assert_eq!(TcTime::new(0, 0, 1, 0).to_frames(rate), n);
            assert_eq!(TcTime::new(0, 1, 0, 0).to_frames(rate), 60 * n);
            assert_eq!(TcTime::new(1, 0, 0, 0).to_frames(rate), 3600 * n);
            assert_eq!(
                TcTime::new(10, 20, 30, 4).to_frames(rate),
                (10 * 3600 + 20 * 60 + 30) * n + 4
            );
        }
    }

    #[test]
    fn frames_conversion_drop_frame_smpte() {
        let df = TcRate::Fps2997Df;
        // Les frames 0 et 1 de 00:01 n'existent pas : 00:01:00:02 suit
        // immédiatement 00:00:59:29.
        assert_eq!(TcTime::new(0, 0, 59, 29).to_frames(df), 1799);
        assert_eq!(TcTime::new(0, 1, 0, 2).to_frames(df), 1800);
        assert_eq!(TcTime::from_frames(1800, df), TcTime::new(0, 1, 0, 2));
        // Les minutes multiples de 10 ne sautent rien : 00:10:00:00 existe.
        assert_eq!(TcTime::new(0, 10, 0, 0).to_frames(df), 17_982);
        assert_eq!(TcTime::from_frames(17_982, df), TcTime::new(0, 10, 0, 0));
        // Une heure drop-frame = 107892 frames ; dérive < 1 frame vs horloge.
        assert_eq!(TcTime::new(1, 0, 0, 0).to_frames(df), 107_892);
        let wall = 107_892.0 / df.fps();
        assert!((wall - 3600.0).abs() < 0.15, "dérive DF : {wall}");
    }

    #[test]
    fn frames_roundtrip_all_rates() {
        let rates = [TcRate::Fps24, TcRate::Fps25, TcRate::Fps2997Df, TcRate::Fps30];
        for rate in rates {
            // Balayage dense au début (là où vivent les drop frames) puis
            // échantillonné sur 24 h.
            for frames in (0..40_000u64).chain((0..2_000).map(|i| i * 1_193)) {
                let frames = frames % rate.frames_per_day();
                let t = TcTime::from_frames(frames, rate);
                assert_eq!(
                    t.to_frames(rate),
                    frames,
                    "roundtrip frames raté : {frames} @ {rate}"
                );
                assert!(t.f < rate.nominal_fps() as u8);
                // Drop-frame : jamais d'étiquette inexistante (f ∈ {0,1}
                // d'une minute non multiple de 10).
                if rate.is_drop_frame() && t.f < 2 && t.s == 0 {
                    assert_eq!(t.m % 10, 0, "étiquette drop-frame invalide : {t}");
                }
            }
            // Wrap 24 h.
            assert_eq!(
                TcTime::from_frames(rate.frames_per_day(), rate),
                TcTime::new(0, 0, 0, 0)
            );
        }
    }

    #[test]
    fn timecode_order_follows_frames() {
        let a = TcTime::new(0, 0, 10, 0);
        let b = TcTime::new(0, 0, 10, 1);
        let c = TcTime::new(0, 1, 0, 2);
        assert!(a < b && b < c);
        for rate in [TcRate::Fps25, TcRate::Fps2997Df] {
            assert!(a.to_frames(rate) < b.to_frames(rate));
            assert!(b.to_frames(rate) < c.to_frames(rate));
        }
    }
}
