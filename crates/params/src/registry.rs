//! Le registre des paramètres : valeurs courantes, lissage exponentiel,
//! fondus de cue stables, overrides live et modulation non persistante.

use std::collections::{BTreeMap, HashMap};

use conduite_core::{ParamValue, Source};
use tracing::{debug, trace, warn};

use crate::{ParamKind, ParamSpec};

/// Nombre maximal de composantes scalaires d'une valeur (Color = RGBA).
const MAX_COMPS: usize = 4;

/// Représentation scalaire interne — f64 pour la précision des `Int`.
type Comps = [f64; MAX_COMPS];

const ZERO: Comps = [0.0; MAX_COMPS];

/// Marge de détection d'un alpha qui recule (⇒ nouvelle transition).
const ALPHA_BACKSTEP: f32 = 1e-4;

/// État runtime d'un paramètre. Tout est pré-alloué (tableaux fixes) :
/// `tick()` ne fait aucune allocation.
#[derive(Debug)]
struct ParamState {
    spec: ParamSpec,
    /// Cible vers laquelle le lissage converge.
    target: Comps,
    /// Valeur lissée courante (sans les offsets de modulation).
    current: Comps,
    /// Point de départ mémorisé au début du blend courant (fondu stable).
    blend_from: Comps,
    /// Cible du blend courant — sert à détecter l'arrivée d'une nouvelle cible.
    blend_to: Comps,
    /// Un blend est en cours pour cette adresse (départ mémorisé valide).
    blend_active: bool,
    /// Dernier alpha reçu — un alpha qui recule signale une nouvelle transition.
    last_alpha: f32,
    /// Fader « live » : plus affecté par `blend_toward` ni par un set de cue.
    live_override: bool,
    /// Lissage forcé (ms), ex. réception DMX ; sinon `spec.smoothing_ms`.
    smoothing_override: Option<f32>,
    /// Offsets additifs de modulation, réécrits à chaque `apply_modulation`.
    mod_offset: Comps,
}

impl ParamState {
    fn new(spec: ParamSpec, initial: Comps) -> Self {
        Self {
            spec,
            target: initial,
            current: initial,
            blend_from: initial,
            blend_to: initial,
            blend_active: false,
            last_alpha: 0.0,
            live_override: false,
            smoothing_override: None,
            mod_offset: ZERO,
        }
    }

    /// Lissage effectif (ms), override compris.
    fn smoothing_ms(&self) -> f32 {
        self.smoothing_override.unwrap_or(self.spec.smoothing_ms)
    }

    /// La valeur se cale immédiatement : kinds discrets, ou lissage nul.
    fn snaps_immediately(&self) -> bool {
        is_discrete(&self.spec.kind) || self.smoothing_ms() <= 0.0
    }
}

/// Registre central des paramètres. Lookups en `HashMap<String, _>`,
/// aucune allocation dans [`Registry::tick`].
#[derive(Debug, Default)]
pub struct Registry {
    params: HashMap<String, ParamState>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Déclare (ou re-déclare) un paramètre. Si l'adresse existe déjà avec un
    /// `kind` strictement identique, la valeur courante est conservée
    /// (hot-reload ISF sans à-coup) ; sinon l'état repart du défaut.
    pub fn register(&mut self, spec: ParamSpec) {
        let mut initial = match to_comps(&spec.kind, &spec.default) {
            Some(c) => c,
            None => {
                warn!(
                    target: "params",
                    addr = %spec.addr,
                    default = ?spec.default,
                    "défaut incompatible avec le kind, repli sur zéro"
                );
                ZERO
            }
        };
        clamp(&spec.kind, &mut initial);

        match self.params.get_mut(&spec.addr) {
            Some(state) if state.spec.kind == spec.kind => {
                // Même forme : on garde la valeur, on rafraîchit la spec.
                state.spec = spec;
            }
            Some(state) => {
                debug!(target: "params", addr = %spec.addr, "kind changé, retour au défaut");
                *state = ParamState::new(spec, initial);
            }
            None => {
                let addr = spec.addr.clone();
                self.params.insert(addr, ParamState::new(spec, initial));
            }
        }
    }

    /// Retire toutes les adresses commençant par `prefix`
    /// (ex. `slice/3/` au retrait d'un slice).
    pub fn unregister_prefix(&mut self, prefix: &str) {
        let before = self.params.len();
        self.params.retain(|addr, _| !addr.starts_with(prefix));
        let removed = before - self.params.len();
        if removed > 0 {
            debug!(target: "params", prefix, removed, "paramètres retirés");
        }
    }

    /// Pose la cible d'un paramètre (le lissage fait le reste).
    /// Une adresse en override live ignore les sets de source `Cue`.
    pub fn set(&mut self, addr: &str, v: ParamValue, source: Source) {
        let Some(state) = self.params.get_mut(addr) else {
            warn!(target: "params", addr, ?source, "set sur une adresse inconnue");
            return;
        };
        if state.live_override && source == Source::Cue {
            trace!(target: "params", addr, "override live : set de cue ignoré");
            return;
        }
        let Some(mut comps) = to_comps(&state.spec.kind, &v) else {
            warn!(
                target: "params",
                addr,
                value = ?v,
                kind = ?state.spec.kind,
                "valeur incompatible avec le kind, ignorée"
            );
            return;
        };
        clamp(&state.spec.kind, &mut comps);
        state.target = comps;
        // Un set direct interrompt le suivi du blend : le prochain
        // blend_toward repartira de la valeur courante.
        state.blend_active = false;
        if state.snaps_immediately() {
            state.current = state.target;
        }
    }

    /// Active/désactive le mode « live » : l'adresse n'est plus affectée par
    /// [`Registry::blend_toward`] (ni par un set de source `Cue`).
    pub fn set_live_override(&mut self, addr: &str, on: bool) {
        match self.params.get_mut(addr) {
            Some(state) => {
                state.live_override = on;
                // Repartir proprement au prochain blend (pas de départ périmé).
                state.blend_active = false;
            }
            None => warn!(target: "params", addr, "override live sur une adresse inconnue"),
        }
    }

    /// Valeur lissée courante + offsets de modulation, clampée au kind.
    pub fn value(&self, addr: &str) -> Option<ParamValue> {
        let state = self.params.get(addr)?;
        Some(to_value(&state.spec.kind, &modulated(state)))
    }

    /// Composante scalaire de la valeur (r pour Color, x pour Point2,
    /// 0/1 pour Bool, index pour Enum). `0.0` si l'adresse est inconnue.
    pub fn value_f32(&self, addr: &str) -> f32 {
        match self.params.get(addr) {
            Some(state) => modulated(state)[0] as f32,
            None => {
                warn!(target: "params", addr, "value_f32 sur une adresse inconnue");
                0.0
            }
        }
    }

    /// Snapshot des paramètres scénarisables : adresse → **cible** (valeur
    /// posée, pas l'état mi-lissé, sans modulation) — ce qu'on enregistre
    /// dans une cue.
    pub fn snapshot_scripted(&self) -> BTreeMap<String, ParamValue> {
        self.params
            .iter()
            .filter(|(_, state)| state.spec.scriptable)
            .map(|(addr, state)| (addr.clone(), to_value(&state.spec.kind, &state.target)))
            .collect()
    }

    /// Fondu vers un snapshot cible, `alpha` ∈ 0..=1 (appelé chaque frame par
    /// le moteur de cues). L'interpolation part des valeurs **au début du
    /// blend** : quand une nouvelle cible arrive (ou que l'alpha repart en
    /// arrière), le point de départ est mémorisé une fois — le fondu est
    /// stable, sans re-départ compound frame après frame.
    /// Les adresses en override live sont ignorées.
    pub fn blend_toward(&mut self, target: &BTreeMap<String, ParamValue>, alpha: f32) {
        let alpha = alpha.clamp(0.0, 1.0);
        for (addr, want) in target {
            let Some(state) = self.params.get_mut(addr) else {
                trace!(target: "params", addr = %addr, "blend vers une adresse inconnue");
                continue;
            };
            if state.live_override {
                continue;
            }
            let Some(mut to) = to_comps(&state.spec.kind, want) else {
                warn!(
                    target: "params",
                    addr = %addr,
                    value = ?want,
                    "cible de blend incompatible avec le kind, ignorée"
                );
                continue;
            };
            clamp(&state.spec.kind, &mut to);

            let restarted = alpha + ALPHA_BACKSTEP < state.last_alpha;
            if !state.blend_active || state.blend_to != to || restarted {
                // Nouvelle cible : le départ est la valeur courante, une fois.
                state.blend_from = state.current;
                state.blend_to = to;
                state.blend_active = true;
            }
            state.last_alpha = alpha;
            state.target = interp(&state.spec.kind, &state.blend_from, &to, alpha);
            if alpha >= 1.0 {
                // Blend terminé : une future transition vers la même valeur
                // repartira de la valeur courante (pas de creux fantôme).
                state.blend_active = false;
            }
            if state.snaps_immediately() {
                state.current = state.target;
            }
        }
    }

    /// Offsets de modulation **additifs et non persistants** : chaque appel
    /// remplace intégralement les offsets de la frame précédente ; ils ne
    /// touchent jamais la cible ni l'état lissé, seulement la lecture
    /// (`value`/`value_f32`), clampée au kind. Deux routes sur la même
    /// adresse s'additionnent. Ignoré sur `Bool`/`Enum`.
    pub fn apply_modulation(&mut self, offsets: &[(String, f32)]) {
        for state in self.params.values_mut() {
            state.mod_offset = ZERO;
        }
        for (addr, offset) in offsets {
            let Some(state) = self.params.get_mut(addr) else {
                trace!(target: "params", addr = %addr, "modulation vers une adresse inconnue");
                continue;
            };
            let off = f64::from(*offset);
            match state.spec.kind {
                ParamKind::Float { .. } | ParamKind::Int { .. } => state.mod_offset[0] += off,
                // Couleur : RGB modulé, alpha préservé.
                ParamKind::Color => {
                    for c in &mut state.mod_offset[..3] {
                        *c += off;
                    }
                }
                ParamKind::Point2 => {
                    for c in &mut state.mod_offset[..2] {
                        *c += off;
                    }
                }
                ParamKind::Bool | ParamKind::Enum(_) => {
                    trace!(target: "params", addr = %addr, "modulation ignorée (kind discret)");
                }
            }
        }
    }

    /// Lissage exponentiel de toutes les valeurs vers leur cible, constante
    /// de temps `smoothing_ms` (override compris). Kinds discrets : bascule
    /// immédiate. **Aucune allocation ici** (chemin de rendu).
    pub fn tick(&mut self, dt_s: f32) {
        let dt = f64::from(dt_s.max(0.0));
        for state in self.params.values_mut() {
            if state.snaps_immediately() {
                state.current = state.target;
                continue;
            }
            let tau = f64::from(state.smoothing_ms()) / 1000.0;
            let k = 1.0 - (-dt / tau).exp();
            for i in 0..MAX_COMPS {
                let delta = state.target[i] - state.current[i];
                // Calage exact quand on est assez près : convergence finie.
                if delta.abs() <= 1e-6 * state.target[i].abs().max(1.0) {
                    state.current[i] = state.target[i];
                } else {
                    state.current[i] += delta * k;
                }
            }
        }
    }

    /// Force (ou relâche, avec `None`) la constante de lissage d'une adresse —
    /// ex. patch DMX qui impose son propre lissage de réception.
    pub fn set_smoothing_override(&mut self, addr: &str, ms: Option<f32>) {
        match self.params.get_mut(addr) {
            Some(state) => state.smoothing_override = ms,
            None => warn!(target: "params", addr, "override de lissage sur une adresse inconnue"),
        }
    }

    /// Toutes les specs enregistrées (ordre non spécifié).
    pub fn specs(&self) -> impl Iterator<Item = &ParamSpec> {
        self.params.values().map(|state| &state.spec)
    }
}

// ------------------------------------------------------------------ helpers

/// Valeur lue : état lissé + offsets de modulation, clampée.
fn modulated(state: &ParamState) -> Comps {
    let mut comps = state.current;
    for (comp, off) in comps.iter_mut().zip(&state.mod_offset) {
        *comp += off;
    }
    clamp(&state.spec.kind, &mut comps);
    comps
}

/// Kinds qui basculent au lieu d'interpoler (et ne se lissent pas).
fn is_discrete(kind: &ParamKind) -> bool {
    matches!(kind, ParamKind::Bool | ParamKind::Enum(_))
}

/// Conversion `ParamValue` → composantes scalaires, avec coercitions sûres
/// (F↔I, F→B, S→index d'enum). `None` si incompatible.
fn to_comps(kind: &ParamKind, v: &ParamValue) -> Option<Comps> {
    let mut c = ZERO;
    match (kind, v) {
        (ParamKind::Float { .. }, ParamValue::F(x)) => c[0] = f64::from(*x),
        (ParamKind::Float { .. }, ParamValue::I(x)) => c[0] = *x as f64,
        (ParamKind::Int { .. }, ParamValue::I(x)) => c[0] = *x as f64,
        (ParamKind::Int { .. }, ParamValue::F(x)) => c[0] = f64::from(*x).round(),
        (ParamKind::Bool, ParamValue::B(b)) => c[0] = f64::from(u8::from(*b)),
        (ParamKind::Bool, ParamValue::F(x)) => c[0] = f64::from(u8::from(*x >= 0.5)),
        (ParamKind::Bool, ParamValue::I(x)) => c[0] = f64::from(u8::from(*x != 0)),
        (ParamKind::Color, ParamValue::Color(rgba)) => {
            for (dst, src) in c.iter_mut().zip(rgba) {
                *dst = f64::from(*src);
            }
        }
        (ParamKind::Point2, ParamValue::P2(p)) => {
            c[0] = f64::from(p[0]);
            c[1] = f64::from(p[1]);
        }
        (ParamKind::Enum(_), ParamValue::I(x)) => c[0] = *x as f64,
        (ParamKind::Enum(_), ParamValue::F(x)) => c[0] = f64::from(x.round()),
        (ParamKind::Enum(labels), ParamValue::S(s)) => {
            c[0] = labels.iter().position(|l| l == s)? as f64;
        }
        _ => return None,
    }
    Some(c)
}

/// Clamp typé, sans panic même si une spec est mal formée (min > max).
fn clamp(kind: &ParamKind, c: &mut Comps) {
    match kind {
        ParamKind::Float { min, max } => {
            c[0] = c[0].max(f64::from(*min)).min(f64::from(*max));
        }
        ParamKind::Int { min, max } => {
            c[0] = c[0].max(*min as f64).min(*max as f64);
        }
        ParamKind::Bool => c[0] = f64::from(u8::from(c[0] >= 0.5)),
        ParamKind::Color => {
            for comp in c.iter_mut() {
                *comp = comp.clamp(0.0, 1.0);
            }
        }
        ParamKind::Point2 => {} // espace libre (un coin peut sortir de l'écran)
        ParamKind::Enum(labels) => {
            let last = labels.len().saturating_sub(1) as f64;
            c[0] = c[0].round().max(0.0).min(last);
        }
    }
}

/// Composantes → `ParamValue` canonique (Enum ⇒ `I(index)`).
fn to_value(kind: &ParamKind, c: &Comps) -> ParamValue {
    match kind {
        ParamKind::Float { .. } => ParamValue::F(c[0] as f32),
        ParamKind::Int { .. } => ParamValue::I(c[0].round() as i64),
        ParamKind::Bool => ParamValue::B(c[0] >= 0.5),
        ParamKind::Color => {
            ParamValue::Color([c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32])
        }
        ParamKind::Point2 => ParamValue::P2([c[0] as f32, c[1] as f32]),
        ParamKind::Enum(_) => ParamValue::I(c[0].round() as i64),
    }
}

/// Interpolation typée : Float/Color/Point2 lerpent, Int lerpe puis arrondit,
/// Bool/Enum basculent à `alpha >= 0.5`.
fn interp(kind: &ParamKind, from: &Comps, to: &Comps, alpha: f32) -> Comps {
    let a = f64::from(alpha.clamp(0.0, 1.0));
    let mut out = *from;
    match kind {
        ParamKind::Float { .. } | ParamKind::Color | ParamKind::Point2 => {
            for i in 0..MAX_COMPS {
                out[i] = from[i] + (to[i] - from[i]) * a;
            }
        }
        ParamKind::Int { .. } => {
            out[0] = (from[0] + (to[0] - from[0]) * a).round();
        }
        ParamKind::Bool | ParamKind::Enum(_) => {
            if alpha >= 0.5 {
                out = *to;
            }
        }
    }
    out
}

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_f(addr: &str, default: f32, smoothing_ms: f32) -> ParamSpec {
        ParamSpec {
            addr: addr.into(),
            label: addr.into(),
            kind: ParamKind::Float { min: 0.0, max: 1.0 },
            default: ParamValue::F(default),
            smoothing_ms,
            scriptable: true,
        }
    }

    fn spec_kind(addr: &str, kind: ParamKind, default: ParamValue) -> ParamSpec {
        ParamSpec {
            addr: addr.into(),
            label: addr.into(),
            kind,
            default,
            smoothing_ms: 0.0,
            scriptable: true,
        }
    }

    fn blend_map(pairs: &[(&str, ParamValue)]) -> BTreeMap<String, ParamValue> {
        pairs
            .iter()
            .map(|(a, v)| (a.to_string(), v.clone()))
            .collect()
    }

    // -------------------------------------------------------- enregistrement

    #[test]
    fn register_exposes_default_and_specs() {
        let mut reg = Registry::new();
        reg.register(spec_f("master/intensity", 1.0, 0.0));
        reg.register(spec_kind(
            "slice/1/blendmode",
            ParamKind::Enum(vec!["normal".into(), "add".into()]),
            ParamValue::I(0),
        ));
        assert_eq!(reg.value("master/intensity"), Some(ParamValue::F(1.0)));
        assert_eq!(reg.value("slice/1/blendmode"), Some(ParamValue::I(0)));
        assert_eq!(reg.specs().count(), 2);
        assert!(reg.specs().any(|s| s.addr == "master/intensity"));
        // Défaut hors plage : clampé à l'enregistrement.
        reg.register(spec_f("hors/plage", 5.0, 0.0));
        assert_eq!(reg.value("hors/plage"), Some(ParamValue::F(1.0)));
        // Défaut incompatible : repli sur zéro (jamais d'échec).
        reg.register(spec_kind("mauvais/defaut", ParamKind::Bool, ParamValue::S("x".into())));
        assert_eq!(reg.value("mauvais/defaut"), Some(ParamValue::B(false)));
    }

    #[test]
    fn reregister_same_kind_keeps_value_different_kind_resets() {
        let mut reg = Registry::new();
        reg.register(spec_f("p", 0.2, 0.0));
        reg.set("p", ParamValue::F(0.8), Source::Ui);
        // Même kind : la valeur survit (hot-reload sans à-coup).
        reg.register(spec_f("p", 0.2, 0.0));
        assert_eq!(reg.value("p"), Some(ParamValue::F(0.8)));
        // Kind différent : retour au défaut.
        reg.register(spec_kind(
            "p",
            ParamKind::Int { min: 0, max: 10 },
            ParamValue::I(3),
        ));
        assert_eq!(reg.value("p"), Some(ParamValue::I(3)));
    }

    #[test]
    fn unregister_prefix_removes_only_matching() {
        let mut reg = Registry::new();
        reg.register(spec_f("slice/3/opacity", 1.0, 0.0));
        reg.register(spec_f("slice/3/gamma", 1.0, 0.0));
        reg.register(spec_f("slice/30/opacity", 1.0, 0.0));
        reg.register(spec_f("master/intensity", 1.0, 0.0));
        reg.unregister_prefix("slice/3/");
        let left: Vec<&str> = reg.specs().map(|s| s.addr.as_str()).collect();
        assert_eq!(left.len(), 2);
        assert!(left.contains(&"slice/30/opacity"), "slice/30 préservé");
        assert!(left.contains(&"master/intensity"));
        assert!(reg.value("slice/3/opacity").is_none());
    }

    // ------------------------------------------------------------ set/value

    #[test]
    fn set_clamps_to_kind() {
        let mut reg = Registry::new();
        reg.register(spec_f("f", 0.0, 0.0));
        reg.register(spec_kind("i", ParamKind::Int { min: -5, max: 5 }, ParamValue::I(0)));
        reg.register(spec_kind("c", ParamKind::Color, ParamValue::Color([0.0; 4])));
        reg.register(spec_kind(
            "e",
            ParamKind::Enum(vec!["a".into(), "b".into(), "c".into()]),
            ParamValue::I(0),
        ));

        reg.set("f", ParamValue::F(7.5), Source::Ui);
        assert_eq!(reg.value("f"), Some(ParamValue::F(1.0)));
        reg.set("f", ParamValue::F(-3.0), Source::Ui);
        assert_eq!(reg.value("f"), Some(ParamValue::F(0.0)));

        reg.set("i", ParamValue::I(99), Source::Ui);
        assert_eq!(reg.value("i"), Some(ParamValue::I(5)));
        // Coercition F→I : arrondi puis clamp.
        reg.set("i", ParamValue::F(-7.6), Source::Ui);
        assert_eq!(reg.value("i"), Some(ParamValue::I(-5)));

        reg.set("c", ParamValue::Color([2.0, -1.0, 0.5, 1.5]), Source::Ui);
        assert_eq!(reg.value("c"), Some(ParamValue::Color([1.0, 0.0, 0.5, 1.0])));

        reg.set("e", ParamValue::I(42), Source::Ui);
        assert_eq!(reg.value("e"), Some(ParamValue::I(2)), "index clampé au dernier");
        reg.set("e", ParamValue::S("b".into()), Source::Ui);
        assert_eq!(reg.value("e"), Some(ParamValue::I(1)), "label accepté");
    }

    #[test]
    fn set_with_wrong_type_is_ignored() {
        let mut reg = Registry::new();
        reg.register(spec_f("f", 0.5, 0.0));
        reg.set("f", ParamValue::Color([1.0; 4]), Source::Osc);
        reg.set("f", ParamValue::S("boom".into()), Source::Osc);
        assert_eq!(reg.value("f"), Some(ParamValue::F(0.5)), "valeur intacte");
        // Adresse inconnue : pas de panic.
        reg.set("inconnu", ParamValue::F(1.0), Source::Osc);
    }

    #[test]
    fn value_returns_canonical_variants_and_value_f32_scalar() {
        let mut reg = Registry::new();
        reg.register(spec_kind("b", ParamKind::Bool, ParamValue::B(false)));
        reg.register(spec_kind("c", ParamKind::Color, ParamValue::Color([0.3, 0.4, 0.5, 1.0])));
        reg.register(spec_kind("p", ParamKind::Point2, ParamValue::P2([0.25, 0.75])));

        // Coercition F→B au set.
        reg.set("b", ParamValue::F(0.9), Source::Midi);
        assert_eq!(reg.value("b"), Some(ParamValue::B(true)));
        assert_eq!(reg.value_f32("b"), 1.0);
        assert_eq!(reg.value_f32("c"), 0.3, "Color → composante r");
        assert_eq!(reg.value_f32("p"), 0.25, "Point2 → x");
    }

    #[test]
    fn value_f32_missing_returns_zero() {
        let reg = Registry::new();
        assert_eq!(reg.value_f32("nulle/part"), 0.0);
        assert_eq!(reg.value("nulle/part"), None);
    }

    // -------------------------------------------------------------- lissage

    #[test]
    fn smoothing_converges_exponentially() {
        let mut reg = Registry::new();
        reg.register(spec_f("p", 0.0, 100.0)); // tau = 100 ms
        reg.set("p", ParamValue::F(1.0), Source::Ui);
        // Avant tout tick : la valeur lissée n'a pas bougé.
        assert_eq!(reg.value_f32("p"), 0.0);

        // Un pas de dt = tau : k = 1 - e^-1 ≈ 0.632.
        reg.tick(0.1);
        assert!((reg.value_f32("p") - 0.632_12).abs() < 1e-3, "{}", reg.value_f32("p"));

        // Convergence monotone puis calage exact.
        let mut prev = reg.value_f32("p");
        for _ in 0..200 {
            reg.tick(0.016);
            let v = reg.value_f32("p");
            assert!(v >= prev - 1e-6, "lissage non monotone");
            prev = v;
        }
        assert_eq!(reg.value_f32("p"), 1.0, "calage exact sur la cible");
    }

    #[test]
    fn smoothing_zero_snaps_immediately() {
        let mut reg = Registry::new();
        reg.register(spec_f("p", 0.0, 0.0));
        reg.set("p", ParamValue::F(0.7), Source::Ui);
        assert_eq!(reg.value_f32("p"), 0.7, "sans lissage, effet immédiat");
    }

    #[test]
    fn discrete_kinds_never_smooth() {
        let mut reg = Registry::new();
        let mut spec = spec_kind("e", ParamKind::Enum(vec!["a".into(), "b".into()]), ParamValue::I(0));
        spec.smoothing_ms = 500.0; // ignoré : kind discret
        reg.register(spec);
        reg.set("e", ParamValue::I(1), Source::Ui);
        assert_eq!(reg.value("e"), Some(ParamValue::I(1)), "bascule immédiate");
    }

    #[test]
    fn int_smoothing_rounds_at_read_and_converges() {
        let mut reg = Registry::new();
        reg.register(ParamSpec {
            addr: "i".into(),
            label: "i".into(),
            kind: ParamKind::Int { min: 0, max: 100 },
            default: ParamValue::I(0),
            smoothing_ms: 100.0,
            scriptable: true,
        });
        reg.set("i", ParamValue::I(10), Source::Ui);
        reg.tick(0.05); // k ≈ 0.393 → interne ≈ 3.93 → lu 4
        assert_eq!(reg.value("i"), Some(ParamValue::I(4)));
        for _ in 0..200 {
            reg.tick(0.05);
        }
        assert_eq!(reg.value("i"), Some(ParamValue::I(10)), "converge sur l'entier cible");
    }

    #[test]
    fn smoothing_override_wins_and_reverts() {
        let mut reg = Registry::new();
        reg.register(spec_f("p", 0.0, 1000.0));
        reg.set_smoothing_override("p", Some(0.0));
        reg.set("p", ParamValue::F(1.0), Source::ArtNet);
        assert_eq!(reg.value_f32("p"), 1.0, "override 0 ms : immédiat");

        reg.set_smoothing_override("p", None);
        reg.set("p", ParamValue::F(0.0), Source::ArtNet);
        reg.tick(0.016);
        let v = reg.value_f32("p");
        assert!(v > 0.9, "retour au lissage lent de la spec, obtenu {v}");
        // Adresse inconnue : pas de panic.
        reg.set_smoothing_override("x", Some(1.0));
    }

    // ---------------------------------------------------------------- blend

    #[test]
    fn blend_uses_start_values_not_compounding() {
        let mut reg = Registry::new();
        reg.register(spec_f("p", 0.0, 0.0));
        let target = blend_map(&[("p", ParamValue::F(1.0))]);

        reg.blend_toward(&target, 0.25);
        assert_eq!(reg.value_f32("p"), 0.25);
        // Un blend naïf « depuis la valeur courante » donnerait 0.25+0.75*0.5=0.625.
        reg.blend_toward(&target, 0.5);
        assert_eq!(reg.value_f32("p"), 0.5, "interpole depuis le DÉBUT du blend");
        reg.blend_toward(&target, 1.0);
        assert_eq!(reg.value_f32("p"), 1.0);
    }

    #[test]
    fn blend_restarts_when_target_changes() {
        let mut reg = Registry::new();
        reg.register(spec_f("p", 0.0, 0.0));
        reg.blend_toward(&blend_map(&[("p", ParamValue::F(1.0))]), 0.5);
        assert_eq!(reg.value_f32("p"), 0.5);
        // Nouvelle cible en cours de route : nouveau départ = valeur courante.
        let t2 = blend_map(&[("p", ParamValue::F(0.0))]);
        reg.blend_toward(&t2, 0.5);
        assert_eq!(reg.value_f32("p"), 0.25, "0.5 → 0.0 à mi-chemin");
        reg.blend_toward(&t2, 1.0);
        assert_eq!(reg.value_f32("p"), 0.0);
    }

    #[test]
    fn blend_finished_then_same_target_stays_stable() {
        let mut reg = Registry::new();
        reg.register(spec_f("p", 0.0, 0.0));
        let target = blend_map(&[("p", ParamValue::F(1.0))]);
        // Transition 1 : 0 → 1, menée à terme.
        reg.blend_toward(&target, 0.5);
        reg.blend_toward(&target, 1.0);
        assert_eq!(reg.value_f32("p"), 1.0);
        // Transition 2 vers la MÊME valeur : aucun creux vers l'ancien départ.
        for alpha in [0.0, 0.1, 0.5, 0.9, 1.0] {
            reg.blend_toward(&target, alpha);
            assert_eq!(reg.value_f32("p"), 1.0, "creux fantôme à alpha={alpha}");
        }
    }

    #[test]
    fn blend_alpha_reset_restarts_from_current() {
        let mut reg = Registry::new();
        reg.register(spec_f("p", 0.0, 0.0));
        let target = blend_map(&[("p", ParamValue::F(1.0))]);
        // Transition interrompue à 60 % (jamais alpha=1).
        reg.blend_toward(&target, 0.6);
        assert_eq!(reg.value_f32("p"), 0.6);
        // Nouvelle transition (alpha repart en arrière), même cible :
        // départ re-mémorisé sur la valeur courante.
        reg.blend_toward(&target, 0.1);
        assert!((reg.value_f32("p") - 0.64).abs() < 1e-6, "0.6 + 0.4×0.1");
    }

    #[test]
    fn blend_typed_interpolation() {
        let mut reg = Registry::new();
        reg.register(spec_kind("i", ParamKind::Int { min: 0, max: 10 }, ParamValue::I(0)));
        reg.register(spec_kind("b", ParamKind::Bool, ParamValue::B(false)));
        reg.register(spec_kind(
            "e",
            ParamKind::Enum(vec!["a".into(), "b".into(), "c".into()]),
            ParamValue::I(0),
        ));
        reg.register(spec_kind("c", ParamKind::Color, ParamValue::Color([0.0; 4])));
        reg.register(spec_kind("p", ParamKind::Point2, ParamValue::P2([0.0, 0.0])));

        let target = blend_map(&[
            ("i", ParamValue::I(10)),
            ("b", ParamValue::B(true)),
            ("e", ParamValue::I(2)),
            ("c", ParamValue::Color([1.0, 0.5, 0.0, 1.0])),
            ("p", ParamValue::P2([1.0, -1.0])),
        ]);

        reg.blend_toward(&target, 0.24);
        assert_eq!(reg.value("i"), Some(ParamValue::I(2)), "10×0.24 = 2.4 → 2");
        assert_eq!(reg.value("b"), Some(ParamValue::B(false)), "pas encore basculé");
        assert_eq!(reg.value("e"), Some(ParamValue::I(0)));

        reg.blend_toward(&target, 0.5);
        assert_eq!(reg.value("i"), Some(ParamValue::I(5)));
        assert_eq!(reg.value("b"), Some(ParamValue::B(true)), "bascule à 0.5");
        assert_eq!(reg.value("e"), Some(ParamValue::I(2)), "bascule directe vers la cible");
        assert_eq!(reg.value("c"), Some(ParamValue::Color([0.5, 0.25, 0.0, 0.5])));
        assert_eq!(reg.value("p"), Some(ParamValue::P2([0.5, -0.5])), "P2 non clampé");
    }

    #[test]
    fn blend_enum_accepts_string_labels_and_unknown_addrs() {
        let mut reg = Registry::new();
        reg.register(spec_kind(
            "slice/1/blendmode",
            ParamKind::Enum(vec!["normal".into(), "add".into()]),
            ParamValue::I(0),
        ));
        let target = blend_map(&[
            ("slice/1/blendmode", ParamValue::S("add".into())),
            ("slice/99/opacity", ParamValue::F(1.0)), // inconnue : ignorée sans panic
        ]);
        reg.blend_toward(&target, 1.0);
        assert_eq!(reg.value("slice/1/blendmode"), Some(ParamValue::I(1)));
    }

    #[test]
    fn manual_set_rebases_the_next_blend() {
        let mut reg = Registry::new();
        reg.register(spec_f("p", 0.0, 0.0));
        let target = blend_map(&[("p", ParamValue::F(1.0))]);
        reg.blend_toward(&target, 0.5);
        // Reprise en main directe (pas d'override live) : dernière action gagne…
        reg.set("p", ParamValue::F(0.2), Source::Ui);
        assert_eq!(reg.value_f32("p"), 0.2);
        // …et le blend suivant repart de là (départ re-mémorisé).
        reg.blend_toward(&target, 0.5);
        assert!((reg.value_f32("p") - 0.6).abs() < 1e-6, "0.2 + 0.8×0.5");
    }

    // -------------------------------------------------------- override live

    #[test]
    fn live_override_shields_from_blend_and_cue_set() {
        let mut reg = Registry::new();
        reg.register(spec_f("fader", 0.0, 0.0));
        reg.set_live_override("fader", true);
        reg.set("fader", ParamValue::F(0.3), Source::Midi);
        assert_eq!(reg.value_f32("fader"), 0.3);

        // Ni le blend des cues ni un set de source Cue ne l'écrasent.
        reg.blend_toward(&blend_map(&[("fader", ParamValue::F(1.0))]), 1.0);
        assert_eq!(reg.value_f32("fader"), 0.3, "blend ignoré en live");
        reg.set("fader", ParamValue::F(0.9), Source::Cue);
        assert_eq!(reg.value_f32("fader"), 0.3, "set de cue ignoré en live");

        // Les sources directes gardent la main.
        reg.set("fader", ParamValue::F(0.6), Source::Osc);
        assert_eq!(reg.value_f32("fader"), 0.6);

        // Relâché : les cues reprennent le contrôle.
        reg.set_live_override("fader", false);
        reg.blend_toward(&blend_map(&[("fader", ParamValue::F(1.0))]), 1.0);
        assert_eq!(reg.value_f32("fader"), 1.0);
        // Adresse inconnue : pas de panic.
        reg.set_live_override("inconnu", true);
    }

    // ----------------------------------------------------------- modulation

    #[test]
    fn modulation_is_additive_clamped_and_non_persistent() {
        let mut reg = Registry::new();
        reg.register(spec_f("p", 0.5, 0.0));

        reg.apply_modulation(&[("p".into(), 0.2)]);
        assert!((reg.value_f32("p") - 0.7).abs() < 1e-6);

        // Deux routes sur la même adresse : additives.
        reg.apply_modulation(&[("p".into(), 0.2), ("p".into(), 0.1)]);
        assert!((reg.value_f32("p") - 0.8).abs() < 1e-6);

        // Clamp au kind (0.5 + 0.9 → 1.0 ; 0.5 - 0.9 → 0.0).
        reg.apply_modulation(&[("p".into(), 0.9)]);
        assert_eq!(reg.value_f32("p"), 1.0);
        reg.apply_modulation(&[("p".into(), -0.9)]);
        assert_eq!(reg.value_f32("p"), 0.0);

        // Non persistant : la frame suivante remplace tout ; la base est intacte.
        reg.apply_modulation(&[]);
        assert_eq!(reg.value_f32("p"), 0.5);
        assert_eq!(
            reg.snapshot_scripted().get("p"),
            Some(&ParamValue::F(0.5)),
            "la modulation ne pollue jamais la valeur scénarisée"
        );
        // Le lissage n'absorbe pas la modulation.
        reg.apply_modulation(&[("p".into(), 0.3)]);
        reg.tick(0.016);
        reg.apply_modulation(&[]);
        assert_eq!(reg.value_f32("p"), 0.5);
        // Adresse inconnue : ignorée sans panic.
        reg.apply_modulation(&[("fantome".into(), 1.0)]);
    }

    #[test]
    fn modulation_per_kind_targets() {
        let mut reg = Registry::new();
        reg.register(spec_kind("b", ParamKind::Bool, ParamValue::B(true)));
        reg.register(spec_kind(
            "e",
            ParamKind::Enum(vec!["a".into(), "b".into()]),
            ParamValue::I(0),
        ));
        reg.register(spec_kind(
            "c",
            ParamKind::Color,
            ParamValue::Color([0.2, 0.2, 0.2, 0.5]),
        ));
        reg.register(spec_kind("p2", ParamKind::Point2, ParamValue::P2([0.5, 0.5])));

        reg.apply_modulation(&[
            ("b".into(), -1.0),
            ("e".into(), 1.0),
            ("c".into(), 0.3),
            ("p2".into(), 0.25),
        ]);
        assert_eq!(reg.value("b"), Some(ParamValue::B(true)), "Bool insensible");
        assert_eq!(reg.value("e"), Some(ParamValue::I(0)), "Enum insensible");
        assert_eq!(
            reg.value("c"),
            Some(ParamValue::Color([0.5, 0.5, 0.5, 0.5])),
            "Color : RGB modulé, alpha préservé"
        );
        assert_eq!(reg.value("p2"), Some(ParamValue::P2([0.75, 0.75])));
    }

    // ------------------------------------------------------------- snapshot

    #[test]
    fn snapshot_scripted_filters_and_uses_targets() {
        let mut reg = Registry::new();
        reg.register(spec_f("scripted", 0.0, 1000.0));
        let mut hidden = spec_f("interne", 0.0, 0.0);
        hidden.scriptable = false;
        reg.register(hidden);

        reg.set("scripted", ParamValue::F(0.8), Source::Ui);
        // Pas de tick : la valeur lissée est encore à 0, mais le snapshot
        // capture la CIBLE posée (ce qu'on veut enregistrer en cue).
        let snap = reg.snapshot_scripted();
        assert_eq!(snap.len(), 1, "seuls les scriptables sont capturés");
        assert_eq!(snap.get("scripted"), Some(&ParamValue::F(0.8)));
        assert!(!snap.contains_key("interne"));
    }
}
