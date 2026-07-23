// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Traduction PURE message OSC → [`Command`] — schéma normatif
//! d'INTERFACES.md. Tolérance d'arguments pensée Chataigne/TouchOSC :
//! int, float, double, bool et string numérique sont interchangeables
//! quand c'est sans ambiguïté.

use std::str::FromStr;

use conduite_core::{Command, CueNumber, ParamValue, Source};
use rosc::OscType;
use tracing::warn;

/// Adresse du paramètre piloté par `/conduite/master`.
const MASTER_ADDR: &str = "master/intensity";

/// Traduit une adresse + arguments OSC en [`Command`]. Pure et totale :
/// un message inconnu ou mal formé retourne `None` (tracé en warn, visible
/// dans le journal) — l'OSC ne plante jamais la régie.
pub fn map_message(addr: &str, args: &[OscType]) -> Option<Command> {
    match addr {
        "/conduite/cue/go" => Some(Command::CueGo),
        "/conduite/cue/back" => Some(Command::CueBack),
        "/conduite/cue/goto" => match cue_number_arg(args, 0) {
            Some(cue) => Some(Command::CueGoto { cue }),
            None => reject(addr, "attendu : numéro de cue (float 12.5 ou string \"12.5\")"),
        },
        // /conduite/bpm/tap avant /conduite/bpm : adresses exactes, pas de préfixe.
        "/conduite/bpm/tap" => Some(Command::TapTempo),
        "/conduite/bpm" => match float_arg(args, 0) {
            Some(bpm) => Some(Command::BpmSet { bpm }),
            None => reject(addr, "attendu : BPM (float)"),
        },
        "/conduite/master" => match float_arg(args, 0) {
            Some(v) => Some(Command::ParamSet {
                addr: MASTER_ADDR.to_string(),
                value: ParamValue::F(v),
                source: Source::Osc,
            }),
            None => reject(addr, "attendu : intensité (float 0..1)"),
        },
        "/conduite/dbo" => match float_arg(args, 0) {
            Some(fade_s) => Some(Command::Dbo { fade_s }),
            None => reject(addr, "attendu : temps de fondu en secondes (float)"),
        },
        // Extension : lever le DBO par OSC (INTERFACES ne définit que la pose).
        "/conduite/dbo/release" => Some(Command::DboRelease),
        other => {
            if let Some(param) = other.strip_prefix("/conduite/param/") {
                if param.is_empty() {
                    return reject(addr, "adresse de paramètre vide");
                }
                return match float_arg(args, 0) {
                    Some(v) => Some(Command::ParamSet {
                        addr: param.to_string(),
                        value: ParamValue::F(v),
                        source: Source::Osc,
                    }),
                    None => reject(addr, "attendu : valeur (float)"),
                };
            }
            warn!(%addr, "adresse OSC inconnue : message ignoré");
            None
        }
    }
}

/// Trace le rejet d'un message mal formé et retourne `None`.
fn reject(addr: &str, detail: &str) -> Option<Command> {
    warn!(%addr, detail, "message OSC ignoré : arguments invalides");
    None
}

/// Numéro de cue tolérant : string "12.5", float/double 12.5, ou entier 12.
fn cue_number_arg(args: &[OscType], index: usize) -> Option<CueNumber> {
    match args.get(index)? {
        OscType::String(s) => CueNumber::from_str(s).ok(),
        OscType::Int(i) => cue_from_f64(f64::from(*i)),
        OscType::Long(l) => cue_from_f64(*l as f64),
        OscType::Float(f) => cue_from_f64(f64::from(*f)),
        OscType::Double(d) => cue_from_f64(*d),
        _ => None,
    }
}

/// Convertit un nombre en millièmes de cue, arrondi au millième le plus
/// proche (12.34 en float vaut 12.34000015… : l'arrondi retombe sur 12340).
fn cue_from_f64(v: f64) -> Option<CueNumber> {
    let thousandths = (v * 1000.0).round();
    if !thousandths.is_finite() || thousandths < 0.0 || thousandths > f64::from(u32::MAX) {
        return None;
    }
    Some(CueNumber(thousandths as u32))
}

/// Float tolérant : Float, Double, Int, Long, Bool (0/1), string numérique
/// (TouchOSC envoie parfois "0.5" en texte).
fn float_arg(args: &[OscType], index: usize) -> Option<f32> {
    match args.get(index)? {
        OscType::Float(f) => Some(*f),
        OscType::Double(d) => Some(*d as f32),
        OscType::Int(i) => Some(*i as f32),
        OscType::Long(l) => Some(*l as f32),
        OscType::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        OscType::String(s) => s.trim().parse::<f32>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param_set(addr: &str, v: f32) -> Command {
        Command::ParamSet {
            addr: addr.into(),
            value: ParamValue::F(v),
            source: Source::Osc,
        }
    }

    #[test]
    fn transport_addresses_map() {
        assert_eq!(map_message("/conduite/cue/go", &[]), Some(Command::CueGo));
        assert_eq!(map_message("/conduite/cue/back", &[]), Some(Command::CueBack));
        // Les arguments superflus sont tolérés (TouchOSC envoie souvent 1.0).
        assert_eq!(
            map_message("/conduite/cue/go", &[OscType::Float(1.0)]),
            Some(Command::CueGo)
        );
    }

    #[test]
    fn goto_accepts_float_string_and_int() {
        // Float 12.5 → cue 12.5 (millièmes = 12500).
        assert_eq!(
            map_message("/conduite/cue/goto", &[OscType::Float(12.5)]),
            Some(Command::CueGoto { cue: CueNumber(12500) })
        );
        // String "12.5" → même cue.
        assert_eq!(
            map_message("/conduite/cue/goto", &[OscType::String("12.5".into())]),
            Some(Command::CueGoto { cue: CueNumber(12500) })
        );
        // Int 7 → cue 7.
        assert_eq!(
            map_message("/conduite/cue/goto", &[OscType::Int(7)]),
            Some(Command::CueGoto { cue: CueNumber(7000) })
        );
        // Double et Long tolérés aussi.
        assert_eq!(
            map_message("/conduite/cue/goto", &[OscType::Double(2.05)]),
            Some(Command::CueGoto { cue: CueNumber(2050) })
        );
        assert_eq!(
            map_message("/conduite/cue/goto", &[OscType::Long(3)]),
            Some(Command::CueGoto { cue: CueNumber(3000) })
        );
        // 12.34 en float 32 bits vaut 12.34000015… : l'arrondi doit retomber
        // sur la cue 12.34 exacte.
        assert_eq!(
            map_message("/conduite/cue/goto", &[OscType::Float(12.34)]),
            Some(Command::CueGoto { cue: CueNumber(12340) })
        );
    }

    #[test]
    fn goto_rejects_garbage() {
        assert_eq!(map_message("/conduite/cue/goto", &[]), None);
        assert_eq!(
            map_message("/conduite/cue/goto", &[OscType::String("douze".into())]),
            None
        );
        assert_eq!(
            map_message("/conduite/cue/goto", &[OscType::Float(-1.0)]),
            None
        );
        assert_eq!(
            map_message("/conduite/cue/goto", &[OscType::Float(f32::NAN)]),
            None
        );
        assert_eq!(
            map_message("/conduite/cue/goto", &[OscType::Double(1e12)]),
            None
        );
    }

    #[test]
    fn param_addresses_carry_the_stable_addr() {
        assert_eq!(
            map_message("/conduite/param/slice/1/opacity", &[OscType::Float(0.5)]),
            Some(param_set("slice/1/opacity", 0.5))
        );
        assert_eq!(
            map_message("/conduite/param/slice/2/gain/r", &[OscType::Float(1.2)]),
            Some(param_set("slice/2/gain/r", 1.2))
        );
        assert_eq!(
            map_message("/conduite/param/mod/1/depth", &[OscType::Float(0.8)]),
            Some(param_set("mod/1/depth", 0.8))
        );
        // Adresse vide ou argument manquant : rejet propre.
        assert_eq!(map_message("/conduite/param/", &[OscType::Float(0.5)]), None);
        assert_eq!(map_message("/conduite/param/slice/1/opacity", &[]), None);
    }

    #[test]
    fn param_arguments_are_tolerant() {
        // Chataigne envoie volontiers des ints pour 0/1, TouchOSC des strings.
        assert_eq!(
            map_message("/conduite/param/slice/1/opacity", &[OscType::Int(1)]),
            Some(param_set("slice/1/opacity", 1.0))
        );
        assert_eq!(
            map_message("/conduite/param/slice/1/opacity", &[OscType::Double(0.25)]),
            Some(param_set("slice/1/opacity", 0.25))
        );
        assert_eq!(
            map_message("/conduite/param/slice/1/opacity", &[OscType::Bool(true)]),
            Some(param_set("slice/1/opacity", 1.0))
        );
        assert_eq!(
            map_message(
                "/conduite/param/slice/1/opacity",
                &[OscType::String("0.75".into())]
            ),
            Some(param_set("slice/1/opacity", 0.75))
        );
        assert_eq!(
            map_message(
                "/conduite/param/slice/1/opacity",
                &[OscType::String("beaucoup".into())]
            ),
            None
        );
    }

    #[test]
    fn master_maps_to_master_intensity() {
        assert_eq!(
            map_message("/conduite/master", &[OscType::Float(0.8)]),
            Some(param_set("master/intensity", 0.8))
        );
        assert_eq!(
            map_message("/conduite/master", &[OscType::Int(0)]),
            Some(param_set("master/intensity", 0.0))
        );
        assert_eq!(map_message("/conduite/master", &[]), None);
    }

    #[test]
    fn dbo_maps_with_fade_and_release() {
        assert_eq!(
            map_message("/conduite/dbo", &[OscType::Float(2.0)]),
            Some(Command::Dbo { fade_s: 2.0 })
        );
        assert_eq!(
            map_message("/conduite/dbo", &[OscType::Int(0)]),
            Some(Command::Dbo { fade_s: 0.0 })
        );
        assert_eq!(map_message("/conduite/dbo", &[]), None);
        assert_eq!(
            map_message("/conduite/dbo/release", &[]),
            Some(Command::DboRelease)
        );
    }

    #[test]
    fn bpm_and_tap_map() {
        assert_eq!(
            map_message("/conduite/bpm", &[OscType::Float(128.0)]),
            Some(Command::BpmSet { bpm: 128.0 })
        );
        assert_eq!(
            map_message("/conduite/bpm", &[OscType::Int(90)]),
            Some(Command::BpmSet { bpm: 90.0 })
        );
        assert_eq!(map_message("/conduite/bpm", &[]), None);
        assert_eq!(map_message("/conduite/bpm/tap", &[]), Some(Command::TapTempo));
        // Un pad TouchOSC envoie 1.0 avec le tap : toléré.
        assert_eq!(
            map_message("/conduite/bpm/tap", &[OscType::Float(1.0)]),
            Some(Command::TapTempo)
        );
    }

    #[test]
    fn unknown_addresses_return_none() {
        for addr in [
            "/conduite/self/destruct",
            "/conduite",
            "/conduite/",
            "/cue/go",           // sans préfixe /conduite
            "/conduite/cue",     // incomplet
            "/conduite/cue/goto/extra",
            "/autre/param/x",
            "",
        ] {
            assert_eq!(map_message(addr, &[OscType::Float(1.0)]), None, "adresse {addr:?}");
        }
    }
}
