//! Types de modulation (LFO, bandes audio) — owned par `core` car sérialisés
//! dans le show. La machinerie d'évaluation vit dans la crate `modulation`.

use serde::{Deserialize, Serialize};

use crate::model::ModId;

/// Forme d'onde d'un LFO.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wave {
    Sine,
    Tri,
    /// Carré avec largeur d'impulsion 0..1.
    Square { pw: f32 },
    Saw,
    /// Random sample & hold (seedé côté moteur).
    RandomSh,
    /// Dérive type Perlin (seedée côté moteur).
    Drift,
}

/// Fréquence d'un LFO : Hz fixes ou synchro BPM.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freq {
    Hz(f32),
    /// Synchro BPM : `mult` en **cycles par temps** (beat).
    /// Fréquence effective = `bpm / 60 × mult` (voir `resolve_freq` côté moteur).
    ///
    /// Convention (normative, 4/4 : 1 mesure = 4 temps) — les durées usuelles
    /// s'expriment toutes par `mult = 1 / nombre_de_temps` :
    ///
    /// | durée d'un cycle          | `mult`  |
    /// |---------------------------|---------|
    /// | 1/4 de mesure (1 temps)   | 1.0     |
    /// | 1/2 mesure (2 temps)      | 0.5     |
    /// | 1 mesure (4 temps)        | 0.25    |
    /// | 2 mesures (8 temps)       | 0.125   |
    /// | 4 mesures (16 temps)      | 0.0625  |
    ///
    /// `mult > 1` accélère : 2.0 = 2 cycles par temps (croches), 4.0 = doubles
    /// croches. Toute valeur > 0 est valide (pas de liste fermée).
    BpmSync { mult: f32 },
}

/// Nature d'un modulateur.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModKind {
    Lfo {
        wave: Wave,
        freq: Freq,
        /// Phase initiale 0..1.
        phase: f32,
    },
    /// Bande d'analyse audio (FFT d'entrée, jamais de sortie son).
    AudioBand {
        low_hz: f32,
        high_hz: f32,
        gain: f32,
        /// Plancher soustrait avant gain (réjection du bruit de fond).
        floor: f32,
        attack_ms: f32,
        release_ms: f32,
        /// AGC lent : le niveau brut de la bande est divisé par son maximum
        /// glissant (~3 s) avant plancher/gain, pour rester 0..1 quel que
        /// soit le niveau d'entrée. Absent des shows antérieurs ⇒ `true`.
        #[serde(default = "default_true")]
        normalize: bool,
    },
    /// **Réservé v2** — ANIMATION DE PARAMÈTRES pilotée timecode (MTC/LTC),
    /// cf. DECISIONS 2026-07-23. Aucune logique aujourd'hui : le moteur sort
    /// 0.0, l'option « Timecode » du popover d'animation reste grisée.
    /// NE PAS confondre avec le chase de CUES, lui bien livré :
    /// `ShowSettings::timecode_chase` + `CueTriggers::timecode` (GOTO/GO
    /// automatiques dans la crate `cue`).
    /// Les champs futurs devront porter des `serde(default)` pour rester
    /// rétro-compatibles ; les champs inconnus d'un show plus récent sont
    /// ignorés à la désérialisation (tolérance par défaut de serde).
    TimecodeChase {},
}

/// Défaut serde de `AudioBand::normalize` (shows antérieurs au champ).
fn default_true() -> bool {
    true
}

/// Un modulateur configuré (source de signal interne 0..1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModulatorCfg {
    pub id: ModId,
    pub name: String,
    pub kind: ModKind,
}

/// Mode d'application d'une route de modulation sur son paramètre cible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteMode {
    /// Valeur = base + signal × depth.
    Add,
    /// Valeur = base × (1 - depth + signal × depth).
    Mul,
    /// Valeur = signal × depth (la base est ignorée).
    Replace,
}

/// Branchement modulateur → paramètre (profondeur par défaut du show).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModRoute {
    pub id: u32,
    pub source: ModId,
    /// Adresse stable du paramètre cible (ex. `slice/1/opacity`).
    pub target_addr: String,
    pub depth: f32,
    pub mode: RouteMode,
}

/// État d'une route dans une cue : une cue peut activer/désactiver/changer
/// la profondeur d'un branchement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModRouteState {
    pub route_id: u32,
    pub depth: f32,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_band_without_normalize_defaults_to_true() {
        // Show antérieur au champ `normalize` : il doit charger tel quel.
        let json = r#"{"audio_band":{"low_hz":20.0,"high_hz":200.0,"gain":1.0,
            "floor":0.0,"attack_ms":10.0,"release_ms":80.0}}"#;
        let kind: ModKind = serde_json::from_str(json).expect("audio_band sans normalize");
        assert!(matches!(kind, ModKind::AudioBand { normalize: true, .. }));
    }

    #[test]
    fn audio_band_normalize_roundtrips() {
        let kind = ModKind::AudioBand {
            low_hz: 20.0,
            high_hz: 200.0,
            gain: 1.0,
            floor: 0.05,
            attack_ms: 10.0,
            release_ms: 80.0,
            normalize: false,
        };
        let json = serde_json::to_string(&kind).expect("sérialisation");
        let back: ModKind = serde_json::from_str(&json).expect("désérialisation");
        assert_eq!(kind, back);
    }

    #[test]
    fn timecode_chase_deserializes_tolerantly() {
        // Variant réservé v2 : vide aujourd'hui, champs futurs ignorés.
        let kind: ModKind =
            serde_json::from_str(r#"{"timecode_chase":{}}"#).expect("variant vide");
        assert_eq!(kind, ModKind::TimecodeChase {});
        let kind: ModKind = serde_json::from_str(r#"{"timecode_chase":{"fps":25,"offset_s":1.5}}"#)
            .expect("champs inconnus ignorés");
        assert_eq!(kind, ModKind::TimecodeChase {});
    }

    #[test]
    fn timecode_chase_roundtrips() {
        let json = serde_json::to_string(&ModKind::TimecodeChase {}).expect("sérialisation");
        assert_eq!(json, r#"{"timecode_chase":{}}"#);
        let back: ModKind = serde_json::from_str(&json).expect("désérialisation");
        assert_eq!(back, ModKind::TimecodeChase {});
    }
}
