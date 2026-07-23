// Adapté de Lanterne (pymenvert/toolbox), MIT.
//! Homographie 4 coins (corner pinning).
//!
//! Copie de `toolbox/crates/engine/src/homography.rs`, elle-même miroir exact
//! de `tools/mapping/homography_ref.py` (implémentation de référence). Les
//! vecteurs de test en bas de ce fichier viennent de ce script — si ce module
//! et le script divergent, c'est un bug.
//!
//! Convention : quad unité (0,0)(1,0)(1,1)(0,1), ordre des coins
//! 0=TL, 1=TR, 2=BR, 3=BL (identique à Lanterne HG,HD,BD,BG), (0,0) en
//! haut-gauche. Le vertex shader reçoit la matrice DIRECTE (quad unité →
//! coins) en column-major via [`Mat3::to_gl`].

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum HomographyError {
    #[error("coins dégénérés : trois coins sont (quasi) colinéaires")]
    Degenerate,
}

/// Matrice 3x3 row-major : `m[ligne][colonne]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3(pub [[f64; 3]; 3]);

impl Mat3 {
    pub const IDENTITY: Mat3 = Mat3([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);

    pub fn det(&self) -> f64 {
        let [[a, b, c], [d, e, f], [g, h, i]] = self.0;
        a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g)
    }

    /// Inverse par cofacteurs. `None` si non inversible.
    pub fn inverse(&self) -> Option<Mat3> {
        let det = self.det();
        if det.abs() < 1e-9 {
            return None;
        }
        let [[a, b, c], [d, e, f], [g, h, i]] = self.0;
        Some(Mat3([
            [
                (e * i - f * h) / det,
                (c * h - b * i) / det,
                (b * f - c * e) / det,
            ],
            [
                (f * g - d * i) / det,
                (a * i - c * g) / det,
                (c * d - a * f) / det,
            ],
            [
                (d * h - e * g) / det,
                (b * g - a * h) / det,
                (a * e - b * d) / det,
            ],
        ]))
    }

    /// Produit matriciel `self · other` — composition de transformations :
    /// `other` est appliquée d'abord, puis `self`.
    pub fn mul(&self, other: &Mat3) -> Mat3 {
        let a = &self.0;
        let b = &other.0;
        let mut m = [[0.0f64; 3]; 3];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                *cell = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
            }
        }
        Mat3(m)
    }

    /// Applique la matrice à un point (division perspective).
    pub fn apply(&self, u: f64, v: f64) -> (f64, f64) {
        let m = &self.0;
        let w = m[2][0] * u + m[2][1] * v + m[2][2];
        (
            (m[0][0] * u + m[0][1] * v + m[0][2]) / w,
            (m[1][0] * u + m[1][1] * v + m[1][2]) / w,
        )
    }

    /// Export pour `glUniformMatrix3fv` : column-major, f32 (convention GL).
    pub fn to_gl(&self) -> [f32; 9] {
        let m = &self.0;
        [
            m[0][0] as f32,
            m[1][0] as f32,
            m[2][0] as f32,
            m[0][1] as f32,
            m[1][1] as f32,
            m[2][1] as f32,
            m[0][2] as f32,
            m[1][2] as f32,
            m[2][2] as f32,
        ]
    }
}

/// Calcule H telle que H · quad_unité = coins du slice (`[[x, y]; 4]`,
/// ordre TL,TR,BR,BL, espace sortie normalisé 0..1).
///
/// Résolution directe du système 8x8 (DLT, h33=1) par élimination de Gauss
/// avec pivot partiel — pas de dépendance d'algèbre linéaire pour 8 équations.
pub fn from_corners(corners: &[[f32; 2]; 4]) -> Result<Mat3, HomographyError> {
    const UNIT: [(f64, f64); 4] = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];

    let mut a = [[0.0f64; 9]; 8]; // colonne 8 = second membre (matrice augmentée)
    for (k, ((u, v), corner)) in UNIT.iter().zip(corners.iter()).enumerate() {
        let (x, y) = (f64::from(corner[0]), f64::from(corner[1]));
        a[2 * k] = [*u, *v, 1.0, 0.0, 0.0, 0.0, -u * x, -v * x, x];
        a[2 * k + 1] = [0.0, 0.0, 0.0, *u, *v, 1.0, -u * y, -v * y, y];
    }

    // Élimination de Gauss, pivot partiel.
    for col in 0..8 {
        let pivot = (col..8)
            .max_by(|&r1, &r2| {
                a[r1][col]
                    .abs()
                    .partial_cmp(&a[r2][col].abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(col);
        if a[pivot][col].abs() < 1e-12 {
            return Err(HomographyError::Degenerate);
        }
        a.swap(col, pivot);
        // Élimination sous le pivot. La ligne pivot est copiée ([f64; 9], Copy)
        // pour itérer proprement sans conflit d'emprunts.
        let (top, bottom) = a.split_at_mut(col + 1);
        let pivot_row = top[col];
        for row in bottom.iter_mut() {
            let f = row[col] / pivot_row[col];
            for (c, cell) in row.iter_mut().enumerate().skip(col) {
                *cell -= f * pivot_row[c];
            }
        }
    }
    let mut h = [0.0f64; 8];
    for r in (0..8).rev() {
        let sum: f64 = ((r + 1)..8).map(|c| a[r][c] * h[c]).sum();
        h[r] = (a[r][8] - sum) / a[r][r];
    }

    let m = Mat3([[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], 1.0]]);
    // Comme dans la référence Python : le système peut se résoudre pour des
    // coins dégénérés, la dégénérescence se voit au déterminant.
    if m.det().abs() < 1e-9 {
        return Err(HomographyError::Degenerate);
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    fn close(p: (f64, f64), q: (f64, f64)) -> bool {
        (p.0 - q.0).abs() < EPS && (p.1 - q.1).abs() < EPS
    }

    fn corners(c: [(f32, f32); 4]) -> [[f32; 2]; 4] {
        c.map(|(x, y)| [x, y])
    }

    const UNIT_CORNERS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

    #[test]
    fn unit_quad_gives_identity() {
        let h = from_corners(&UNIT_CORNERS).expect("identity");
        for r in 0..3 {
            for c in 0..3 {
                let expected = if r == c { 1.0 } else { 0.0 };
                assert!((h.0[r][c] - expected).abs() < EPS, "m[{r}][{c}]");
            }
        }
    }

    /// Vecteurs générés par tools/mapping/homography_ref.py (Lanterne) — ne pas
    /// modifier à la main : relancer le script si la convention change.
    ///
    /// Tolérance 1e-6 (et non 1e-9) : les coins sont des f32, donc
    /// 0.08_f32 ≈ 0.079999998... — l'écart se propage en ~1e-8 dans H alors
    /// que la référence Python calcule sur des f64 exacts.
    #[test]
    fn matches_python_reference() {
        const REF_EPS: f64 = 1e-6;
        let m = corners([(0.08, 0.05), (0.97, 0.02), (1.0, 0.93), (0.03, 0.98)]);
        let h = from_corners(&m).expect("homography");
        let expected = [
            [0.906894367790, -0.052490386790, 0.080000000000],
            [-0.029651662520, 0.848647364850, 0.050000000000],
            [0.017416874010, -0.083012893011, 1.000000000000],
        ];
        for (r, row) in expected.iter().enumerate() {
            for (c, &want) in row.iter().enumerate() {
                assert!(
                    (h.0[r][c] - want).abs() < REF_EPS,
                    "m[{r}][{c}] = {} != {want}",
                    h.0[r][c]
                );
            }
        }
        let (x, y) = h.apply(0.5, 0.5);
        assert!((x - 0.524401309635).abs() < REF_EPS && (y - 0.475079513564).abs() < REF_EPS);
    }

    #[test]
    fn corners_map_exactly() {
        let cs = corners([(0.08, 0.05), (0.97, 0.02), (1.0, 0.93), (0.03, 0.98)]);
        let h = from_corners(&cs).expect("homography");
        let unit = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        for ((u, v), c) in unit.iter().zip(cs.iter()) {
            assert!(close(h.apply(*u, *v), (f64::from(c[0]), f64::from(c[1]))));
        }
    }

    #[test]
    fn inverse_roundtrip() {
        let h = from_corners(&corners([
            (0.08, 0.05),
            (0.97, 0.02),
            (1.0, 0.93),
            (0.03, 0.98),
        ]))
        .expect("homography");
        let inv = h.inverse().expect("inverse");
        for p in [(0.5, 0.5), (0.25, 0.75), (0.1, 0.9), (0.999, 0.001)] {
            let (x, y) = h.apply(p.0, p.1);
            assert!(close(inv.apply(x, y), p));
        }
    }

    #[test]
    fn mul_composes_transforms() {
        // Translation-like (affine) puis échelle : vérifie l'ordre de
        // composition (other d'abord, self ensuite).
        let scale = Mat3([[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 1.0]]);
        let shift = Mat3([[1.0, 0.0, 0.5], [0.0, 1.0, 0.25], [0.0, 0.0, 1.0]]);
        // self=scale, other=shift : p' = scale(shift(p)).
        let composed = scale.mul(&shift);
        assert!(close(composed.apply(0.0, 0.0), (1.0, 0.5)));
        // Produit par l'identité : inchangé.
        let same = Mat3::IDENTITY.mul(&scale);
        assert_eq!(same, scale);
        // A · A⁻¹ = I.
        let h = from_corners(&corners([
            (0.08, 0.05),
            (0.97, 0.02),
            (1.0, 0.93),
            (0.03, 0.98),
        ]))
        .expect("homography");
        let inv = h.inverse().expect("inverse");
        let identity = h.mul(&inv);
        for (got, want) in identity.to_gl().iter().zip(Mat3::IDENTITY.to_gl().iter()) {
            assert!((got - want).abs() < 1e-6);
        }
    }

    #[test]
    fn degenerate_corners_rejected() {
        // Trois coins colinéaires (mêmes valeurs que la référence Python).
        let res = from_corners(&corners([(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (0.0, 1.0)]));
        assert_eq!(res, Err(HomographyError::Degenerate));
    }

    #[test]
    fn gl_export_is_column_major() {
        let h = Mat3([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]]);
        assert_eq!(h.to_gl(), [1.0, 4.0, 7.0, 2.0, 5.0, 8.0, 3.0, 6.0, 9.0]);
    }
}
