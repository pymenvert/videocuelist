//! Soft-takeover (« pickup ») — logique pure, testée.
//!
//! Un fader physique n'est pas motorisé : après un changement de cue, sa
//! position ne correspond plus à la valeur logique du paramètre. Pour éviter
//! les sauts, la valeur entrante n'est **laissée passer** que lorsque le fader
//! a « croisé » la valeur logique courante (ou s'en approche à une tolérance
//! près). Avant cela, les mouvements sont ignorés et signalés à l'UI.
//!
//! La valeur logique vient d'un cache que l'app tient à jour via
//! [`Pickup::update_logical`] (cues, UI, OSC…). Les valeurs qui passent par
//! le pickup mettent aussi le cache à jour, pour rester cohérent même si
//! l'écho de l'app tarde.

use std::collections::HashMap;

/// Borne du cache logique. Les adresses arrivent de surfaces réseau non
/// authentifiées (OSC/WS → app → [`Pickup::update_logical`]) : sans borne,
/// un émetteur hostile égrenant des adresses toutes distinctes ferait
/// grossir la map sans limite (épuisement mémoire en cours de spectacle).
const MAX_LOGICAL_ENTRIES: usize = 4096;

/// Décision du pickup pour une valeur entrante.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PickupDecision {
    /// Le fader a la main : la valeur s'applique.
    Pass(f32),
    /// Fader pas encore « raccroché » : valeur ignorée. `current` est la
    /// valeur logique à rejoindre (affichage UI), `incoming` la position
    /// physique actuelle.
    Blocked { current: f32, incoming: f32 },
}

#[derive(Debug)]
struct FaderState {
    /// Adresse ciblée lors du dernier passage (un re-patch réinitialise tout).
    addr: String,
    engaged: bool,
    /// Dernière valeur physique vue (unités du paramètre).
    last: Option<f32>,
}

/// État de soft-takeover, par fader physique `(canal, cc)`.
#[derive(Debug, Default)]
pub struct Pickup {
    /// addr → valeur logique courante (unités du paramètre).
    logical: HashMap<String, f32>,
    faders: HashMap<(u8, u8), FaderState>,
}

impl Pickup {
    pub fn new() -> Self {
        Pickup::default()
    }

    /// Met à jour la valeur logique d'un paramètre (appelé par l'app quand la
    /// valeur change ailleurs : cue, UI, OSC…). Le désengagement éventuel des
    /// faders concernés est détecté paresseusement au prochain mouvement.
    pub fn update_logical(&mut self, addr: &str, value: f32) {
        self.insert_logical(addr, value);
    }

    /// Insertion bornée dans le cache logique : à saturation
    /// ([`MAX_LOGICAL_ENTRIES`]), une entrée arbitraire est évincée.
    /// Dégradation douce : pour l'adresse évincée, le soft-takeover repart en
    /// « prise immédiate » (valeur logique inconnue) — la mémoire, elle,
    /// reste bornée même sous flood d'adresses hostiles.
    fn insert_logical(&mut self, addr: &str, value: f32) {
        if !self.logical.contains_key(addr) && self.logical.len() >= MAX_LOGICAL_ENTRIES {
            if let Some(evicted) = self.logical.keys().next().cloned() {
                self.logical.remove(&evicted);
                tracing::debug!(target: "control_midi::pickup", %evicted,
                    "cache logique plein : éviction d'une entrée");
            }
        }
        self.logical.insert(addr.to_string(), value);
    }

    /// Le fader a-t-il la main ?
    pub fn is_engaged(&self, channel: u8, cc: u8) -> bool {
        self.faders
            .get(&(channel, cc))
            .map(|f| f.engaged)
            .unwrap_or(false)
    }

    /// Filtre une valeur entrante (déjà mise à l'échelle du paramètre) pour le
    /// fader `(channel, cc)` visant `addr`. `tolerance` : écart considéré
    /// comme « à niveau » (typiquement un pas de CC 7 bits).
    pub fn filter(
        &mut self,
        channel: u8,
        cc: u8,
        addr: &str,
        incoming: f32,
        tolerance: f32,
    ) -> PickupDecision {
        let fs = self
            .faders
            .entry((channel, cc))
            .or_insert_with(|| FaderState {
                addr: addr.to_string(),
                engaged: false,
                last: None,
            });
        if fs.addr != addr {
            // Re-patch du fader : on repart de zéro.
            fs.addr = addr.to_string();
            fs.engaged = false;
            fs.last = None;
        }

        match self.logical.get(addr).copied() {
            // Valeur logique inconnue : rien à protéger, on prend la main.
            None => fs.engaged = true,
            Some(current) => {
                // La valeur a-t-elle bougé ailleurs depuis notre dernier
                // passage ? Si oui, on lâche la main (détection paresseuse).
                if fs.engaged {
                    if let Some(last) = fs.last {
                        if (current - last).abs() > tolerance {
                            fs.engaged = false;
                        }
                    }
                }
                if !fs.engaged {
                    let near = (incoming - current).abs() <= tolerance;
                    let crossed = fs.last.is_some_and(|last| {
                        (last <= current && incoming >= current)
                            || (last >= current && incoming <= current)
                    });
                    if near || crossed {
                        fs.engaged = true;
                    }
                }
            }
        }

        let engaged = fs.engaged;
        fs.last = Some(incoming);
        if engaged {
            self.insert_logical(addr, incoming);
            PickupDecision::Pass(incoming)
        } else {
            // `engaged == false` implique une valeur logique connue.
            let current = self.logical.get(addr).copied().unwrap_or(incoming);
            PickupDecision::Blocked { current, incoming }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOL: f32 = 1.0 / 127.0;

    fn blocked(current: f32, incoming: f32) -> PickupDecision {
        PickupDecision::Blocked { current, incoming }
    }

    #[test]
    fn crossing_from_below_engages() {
        let mut p = Pickup::new();
        p.update_logical("slice/1/opacity", 0.5);
        assert_eq!(p.filter(0, 7, "slice/1/opacity", 0.2, TOL), blocked(0.5, 0.2));
        assert_eq!(p.filter(0, 7, "slice/1/opacity", 0.4, TOL), blocked(0.5, 0.4));
        // 0.4 → 0.6 : croise 0.5 → prise de main.
        assert_eq!(
            p.filter(0, 7, "slice/1/opacity", 0.6, TOL),
            PickupDecision::Pass(0.6)
        );
        assert!(p.is_engaged(0, 7));
        // Ensuite tout passe.
        assert_eq!(
            p.filter(0, 7, "slice/1/opacity", 0.7, TOL),
            PickupDecision::Pass(0.7)
        );
    }

    #[test]
    fn crossing_from_above_engages() {
        let mut p = Pickup::new();
        p.update_logical("master/intensity", 0.5);
        assert_eq!(
            p.filter(0, 1, "master/intensity", 0.9, TOL),
            blocked(0.5, 0.9)
        );
        // 0.9 → 0.3 : croise 0.5 par le haut.
        assert_eq!(
            p.filter(0, 1, "master/intensity", 0.3, TOL),
            PickupDecision::Pass(0.3)
        );
    }

    #[test]
    fn near_value_engages_without_crossing() {
        let mut p = Pickup::new();
        p.update_logical("a", 0.5);
        assert_eq!(
            p.filter(0, 7, "a", 0.5 + TOL / 2.0, TOL),
            PickupDecision::Pass(0.5 + TOL / 2.0)
        );
    }

    #[test]
    fn unknown_logical_value_engages_immediately() {
        let mut p = Pickup::new();
        assert_eq!(p.filter(0, 7, "a", 0.42, TOL), PickupDecision::Pass(0.42));
        // Et alimente le cache logique.
        assert_eq!(p.filter(0, 7, "a", 0.43, TOL), PickupDecision::Pass(0.43));
    }

    #[test]
    fn external_change_disengages_then_recross() {
        let mut p = Pickup::new();
        p.update_logical("a", 0.5);
        let _ = p.filter(0, 7, "a", 0.5, TOL); // engagé (à niveau)
        let _ = p.filter(0, 7, "a", 0.7, TOL); // suit
        assert!(p.is_engaged(0, 7));
        // Une cue pose la valeur à 0.2 : le fader (à 0.7) doit lâcher.
        p.update_logical("a", 0.2);
        assert_eq!(p.filter(0, 7, "a", 0.72, TOL), blocked(0.2, 0.72));
        assert!(!p.is_engaged(0, 7));
        // Descente sous 0.2 : croisement → reprise.
        assert_eq!(p.filter(0, 7, "a", 0.1, TOL), PickupDecision::Pass(0.1));
    }

    #[test]
    fn faders_are_independent() {
        let mut p = Pickup::new();
        p.update_logical("a", 0.5);
        p.update_logical("b", 0.5);
        assert_eq!(p.filter(0, 7, "a", 0.4, TOL), blocked(0.5, 0.4));
        // L'autre fader n'est pas affecté par l'état du premier.
        assert_eq!(p.filter(0, 8, "b", 0.5, TOL), PickupDecision::Pass(0.5));
        assert!(!p.is_engaged(0, 7));
        assert!(p.is_engaged(0, 8));
    }

    #[test]
    fn logical_cache_is_bounded_under_hostile_flood() {
        let mut p = Pickup::new();
        // Un émetteur hostile égrène des adresses toutes distinctes
        // (« /conduite/param/<compteur> ») : la map doit rester bornée.
        for i in 0..(MAX_LOGICAL_ENTRIES + 1000) {
            p.update_logical(&format!("hostile/{i}"), 0.5);
        }
        assert!(
            p.logical.len() <= MAX_LOGICAL_ENTRIES,
            "cache logique non borné : {} entrées",
            p.logical.len()
        );
        // Le pickup reste pleinement fonctionnel après le flood.
        p.update_logical("master/intensity", 0.5);
        assert_eq!(
            p.filter(0, 7, "master/intensity", 0.2, TOL),
            blocked(0.5, 0.2)
        );
        assert_eq!(
            p.filter(0, 7, "master/intensity", 0.6, TOL),
            PickupDecision::Pass(0.6)
        );
        assert!(p.logical.len() <= MAX_LOGICAL_ENTRIES);
    }

    #[test]
    fn engaged_fader_insert_path_is_bounded_too() {
        let mut p = Pickup::new();
        for i in 0..MAX_LOGICAL_ENTRIES {
            p.update_logical(&format!("bourrage/{i}"), 0.0);
        }
        // Valeur logique inconnue ⇒ prise immédiate : le Pass insère dans le
        // cache via le chemin `filter`, qui doit lui aussi rester borné.
        assert_eq!(p.filter(0, 7, "nouvelle/addr", 0.3, TOL), PickupDecision::Pass(0.3));
        assert!(p.logical.len() <= MAX_LOGICAL_ENTRIES);
    }

    #[test]
    fn retargeting_fader_resets_state() {
        let mut p = Pickup::new();
        p.update_logical("a", 0.5);
        p.update_logical("b", 0.9);
        let _ = p.filter(0, 7, "a", 0.5, TOL); // engagé sur a
        assert!(p.is_engaged(0, 7));
        // Le fader est re-patché sur b : plus engagé, doit re-croiser.
        assert_eq!(p.filter(0, 7, "b", 0.5, TOL), blocked(0.9, 0.5));
    }
}
