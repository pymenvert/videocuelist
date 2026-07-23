//! Bruit déterministe seedé (xorshift) pour `RandomSh` et `Drift`.
//!
//! Le bruit est adressé par cellule : même `(seed, index)` ⇒ même valeur.
//! Pas d'état à stocker, donc pas de dérive quand la fréquence change et
//! comportement reproductible d'une exécution à l'autre.

/// Un pas de xorshift32 (Marsaglia).
#[inline]
fn xorshift32(mut x: u32) -> u32 {
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    x
}

/// Graine non nulle dérivée d'un id de modulateur (dispersion de Knuth).
pub fn seed_from_id(id: u32) -> u32 {
    id.wrapping_mul(0x9E37_79B9) | 1
}

/// Valeur pseudo-aléatoire déterministe dans [-1, 1] pour la cellule `index`.
pub fn cell_noise(seed: u32, index: i64) -> f32 {
    let ix = index as u64;
    let mixed = seed
        ^ (ix as u32).wrapping_mul(0x85EB_CA6B)
        ^ ((ix >> 32) as u32).wrapping_mul(0xC2B2_AE35);
    let mut x = mixed | 1; // jamais zéro (point fixe du xorshift)
    x = xorshift32(x);
    x = xorshift32(x);
    x = xorshift32(x);
    // 24 bits de mantisse → [0, 1) → [-1, 1].
    let unit = (x >> 8) as f32 / (1u32 << 24) as f32;
    unit * 2.0 - 1.0
}

/// Interpolation smoothstep (dérivée nulle aux bornes, sortie 0..1).
pub fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_noise_is_deterministic_and_bounded() {
        let seed = seed_from_id(3);
        for i in -100i64..100 {
            let a = cell_noise(seed, i);
            let b = cell_noise(seed, i);
            assert_eq!(a, b, "même cellule ⇒ même valeur");
            assert!((-1.0..=1.0).contains(&a), "hors bornes à {i}: {a}");
        }
    }

    #[test]
    fn different_seeds_give_different_streams() {
        let s1 = seed_from_id(1);
        let s2 = seed_from_id(2);
        assert_ne!(s1, s2);
        let differs = (0i64..16).any(|i| cell_noise(s1, i) != cell_noise(s2, i));
        assert!(differs, "deux modulateurs distincts doivent diverger");
    }

    #[test]
    fn cell_noise_varies_across_cells() {
        let seed = seed_from_id(7);
        let first = cell_noise(seed, 0);
        let differs = (1i64..16).any(|i| cell_noise(seed, i) != first);
        assert!(differs, "le bruit ne doit pas être constant");
    }

    #[test]
    fn smoothstep_endpoints_and_midpoint() {
        assert_eq!(smoothstep(0.0), 0.0);
        assert_eq!(smoothstep(1.0), 1.0);
        assert!((smoothstep(0.5) - 0.5).abs() < 1e-6);
        // Clamp hors bornes.
        assert_eq!(smoothstep(-1.0), 0.0);
        assert_eq!(smoothstep(2.0), 1.0);
        // Départ doux : pente quasi nulle près de 0.
        assert!(smoothstep(0.01) < 0.001);
    }
}
