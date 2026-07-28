//! Fonte bitmap 5×7 des mires (pur, sans GL).
//!
//! Étend l'affichage « chiffres 7 segments » de la mire Ident : table de
//! glyphes majuscules + chiffres + tiret + « × », encodée en bits et injectée
//! dans le fragment shader sous forme de tableaux constants GLSL
//! (`GLYPH_LO`/`GLYPH_HI`). Le libellé (« nom de la sortie — résolution »)
//! est passé au shader en indices de glyphes via l'uniform `u_ident_text`.
//!
//! Encodage d'un glyphe : bit `row * 5 + col` (ligne 0 en haut, colonne 0 à
//! gauche) ; `lo` porte les lignes 0..5 (30 bits), `hi` la ligne 6 (5 bits) —
//! tout tient dans des `int` GLSL 32 bits, signe jamais touché.

/// Largeur d'un glyphe en « pixels fonte ».
pub(crate) const GLYPH_W: usize = 5;
/// Hauteur d'un glyphe en « pixels fonte ».
pub(crate) const GLYPH_H: usize = 7;
/// Longueur maximale du libellé Ident (glyphes) — taille de l'uniform GLSL.
pub const IDENT_TEXT_MAX: usize = 32;

/// Table des glyphes : caractère → 7 lignes de 5 colonnes (`#` = allumé).
/// L'ORDRE fait foi : l'index dans ce tableau est l'index envoyé au shader.
#[rustfmt::skip]
const GLYPHS: [(char, [&str; GLYPH_H]); 40] = [
    (' ', [".....", ".....", ".....", ".....", ".....", ".....", "....."]),
    ('A', [".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"]),
    ('B', ["####.", "#...#", "#...#", "####.", "#...#", "#...#", "####."]),
    ('C', [".###.", "#...#", "#....", "#....", "#....", "#...#", ".###."]),
    ('D', ["####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."]),
    ('E', ["#####", "#....", "#....", "####.", "#....", "#....", "#####"]),
    ('F', ["#####", "#....", "#....", "####.", "#....", "#....", "#...."]),
    ('G', [".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".###."]),
    ('H', ["#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"]),
    ('I', [".###.", "..#..", "..#..", "..#..", "..#..", "..#..", ".###."]),
    ('J', ["..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##.."]),
    ('K', ["#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#"]),
    ('L', ["#....", "#....", "#....", "#....", "#....", "#....", "#####"]),
    ('M', ["#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#"]),
    ('N', ["#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#"]),
    ('O', [".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."]),
    ('P', ["####.", "#...#", "#...#", "####.", "#....", "#....", "#...."]),
    ('Q', [".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#"]),
    ('R', ["####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#"]),
    ('S', [".####", "#....", "#....", ".###.", "....#", "....#", "####."]),
    ('T', ["#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#.."]),
    ('U', ["#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."]),
    ('V', ["#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#.."]),
    ('W', ["#...#", "#...#", "#...#", "#.#.#", "#.#.#", "##.##", "#...#"]),
    ('X', ["#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#"]),
    ('Y', ["#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#.."]),
    ('Z', ["#####", "....#", "...#.", "..#..", ".#...", "#....", "#####"]),
    ('0', [".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."]),
    ('1', ["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."]),
    ('2', [".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####"]),
    ('3', ["####.", "....#", "....#", ".###.", "....#", "....#", "####."]),
    ('4', ["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."]),
    ('5', ["#####", "#....", "#....", "####.", "....#", "....#", "####."]),
    ('6', [".###.", "#....", "#....", "####.", "#...#", "#...#", ".###."]),
    ('7', ["#####", "....#", "...#.", "..#..", "..#..", "..#..", "..#.."]),
    ('8', [".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."]),
    ('9', [".###.", "#...#", "#...#", ".####", "....#", "....#", ".###."]),
    ('-', [".....", ".....", ".....", "#####", ".....", ".....", "....."]),
    ('×', [".....", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "....."]),
    ('.', [".....", ".....", ".....", ".....", ".....", ".##..", ".##.."]),
];

/// Empaquette un glyphe en `(lo, hi)` : bit `row * 5 + col`, `lo` = lignes
/// 0..5 (bits 0..29), `hi` = ligne 6 (bits 0..4).
pub(crate) fn pack_glyph(rows: &[&str; GLYPH_H]) -> (i32, i32) {
    let mut lo: i32 = 0;
    let mut hi: i32 = 0;
    for (r, row) in rows.iter().enumerate() {
        for (c, ch) in row.chars().enumerate() {
            if ch != '#' {
                continue;
            }
            let idx = r * GLYPH_W + c;
            if idx < 30 {
                lo |= 1 << idx;
            } else {
                hi |= 1 << (idx - 30);
            }
        }
    }
    (lo, hi)
}

/// Index de glyphe d'un caractère : majuscules forcées, accents français
/// repliés sur la lettre nue, tirets typographiques repliés sur `-`,
/// inconnu → espace.
pub(crate) fn glyph_index(c: char) -> i32 {
    let folded = match c {
        'a'..='z' => c.to_ascii_uppercase(),
        'à' | 'â' | 'ä' | 'À' | 'Â' | 'Ä' => 'A',
        'ç' | 'Ç' => 'C',
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'î' | 'ï' | 'Î' | 'Ï' => 'I',
        'ô' | 'ö' | 'Ô' | 'Ö' => 'O',
        'ù' | 'û' | 'ü' | 'Ù' | 'Û' | 'Ü' => 'U',
        '—' | '–' | '_' => '-',
        other => other,
    };
    GLYPHS
        .iter()
        .position(|(g, _)| *g == folded)
        .unwrap_or(0) as i32
}

/// Encode « NOM - LARGEUR×HAUTEUR » en indices de glyphes dans `out`
/// (ex. `PRINCIPAL - 1920×1080`). Rend le nombre de glyphes écrits.
/// Le nom est tronqué si nécessaire : la résolution est toujours intacte.
/// Nom vide → seulement la résolution.
pub fn encode_output_ident(name: &str, w: u32, h: u32, out: &mut [i32; IDENT_TEXT_MAX]) -> i32 {
    // Partie résolution : "1920×1080" (+" - " si un nom précède).
    let mut res: [i32; 16] = [0; 16];
    let mut res_len = 0usize;
    for c in digits(w) {
        res[res_len] = glyph_index(c);
        res_len += 1;
    }
    res[res_len] = glyph_index('×');
    res_len += 1;
    for c in digits(h) {
        res[res_len] = glyph_index(c);
        res_len += 1;
    }

    let name = name.trim();
    let mut len = 0usize;
    if !name.is_empty() {
        let sep = 3; // " - "
        let name_max = IDENT_TEXT_MAX.saturating_sub(res_len + sep);
        for c in name.chars().take(name_max) {
            out[len] = glyph_index(c);
            len += 1;
        }
        for c in [' ', '-', ' '] {
            out[len] = glyph_index(c);
            len += 1;
        }
    }
    for &g in &res[..res_len.min(IDENT_TEXT_MAX - len)] {
        out[len] = g;
        len += 1;
    }
    len as i32
}

/// Chiffres décimaux d'un entier, poids fort en tête.
fn digits(mut v: u32) -> impl Iterator<Item = char> {
    let mut buf = ['0'; 10];
    let mut n = 0usize;
    loop {
        buf[9 - n] = char::from(b'0' + (v % 10) as u8);
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    (0..n).map(move |i| buf[10 - n + i])
}

/// Génère la table de glyphes en GLSL : constantes `IDENT_MAX`,
/// `GLYPH_COUNT`, tableaux `GLYPH_LO`/`GLYPH_HI` et la fonction
/// `glyph_bit(g, px, py)`. Injecté AVANT le corps du fragment shader.
pub(crate) fn glyph_table_glsl() -> String {
    let n = GLYPHS.len();
    let mut lo = String::with_capacity(n * 12);
    let mut hi = String::with_capacity(n * 6);
    for (i, (_, rows)) in GLYPHS.iter().enumerate() {
        let (l, h) = pack_glyph(rows);
        if i > 0 {
            lo.push_str(", ");
            hi.push_str(", ");
        }
        lo.push_str(&l.to_string());
        hi.push_str(&h.to_string());
    }
    format!(
        "const int IDENT_MAX = {IDENT_TEXT_MAX};\n\
         const int GLYPH_COUNT = {n};\n\
         const int GLYPH_LO[{n}] = int[{n}]({lo});\n\
         const int GLYPH_HI[{n}] = int[{n}]({hi});\n\
         // 1.0 si le pixel fonte (px, py) du glyphe g est allumé (fonte 5x7).\n\
         float glyph_bit(int g, int px, int py) {{\n\
             if (g < 0 || g >= GLYPH_COUNT) {{ return 0.0; }}\n\
             int idx = py * 5 + px;\n\
             int bit = (idx < 30) ? ((GLYPH_LO[g] >> idx) & 1)\n\
                                  : ((GLYPH_HI[g] >> (idx - 30)) & 1);\n\
             return float(bit);\n\
         }}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_bien_formee() {
        for (c, rows) in GLYPHS.iter() {
            for row in rows {
                assert_eq!(row.chars().count(), GLYPH_W, "glyphe {c:?} : ligne de largeur 5");
                assert!(
                    row.chars().all(|p| p == '#' || p == '.'),
                    "glyphe {c:?} : seulement '#' et '.'"
                );
            }
        }
        // Pas de doublon de caractère.
        for (i, (a, _)) in GLYPHS.iter().enumerate() {
            for (b, _) in GLYPHS.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn pack_glyph_encode_les_bits_attendus() {
        // Le tiret : ligne 3 pleine → bits 15..19.
        let dash = &GLYPHS[glyph_index('-') as usize].1;
        let (lo, hi) = pack_glyph(dash);
        assert_eq!(lo, 0b11111 << 15);
        assert_eq!(hi, 0);

        // Le point : lignes 5 et 6, colonnes 1-2 → lo bits 26,27 ; hi bits 1,2.
        let dot = &GLYPHS[glyph_index('.') as usize].1;
        let (lo, hi) = pack_glyph(dot);
        assert_eq!(lo, (1 << 26) | (1 << 27));
        assert_eq!(hi, (1 << 1) | (1 << 2));

        // L'espace : rien.
        assert_eq!(pack_glyph(&GLYPHS[0].1), (0, 0));

        // Tous les glyphes restent dans des int positifs (bit 31 jamais posé).
        for (_, rows) in GLYPHS.iter() {
            let (lo, hi) = pack_glyph(rows);
            assert!(lo >= 0 && hi >= 0);
            assert!(hi < 32, "hi ne porte que 5 bits");
        }
    }

    #[test]
    fn glyph_index_replis() {
        assert_eq!(glyph_index(' '), 0);
        assert_eq!(glyph_index('A'), 1);
        assert_eq!(glyph_index('Z'), 26);
        assert_eq!(glyph_index('0'), 27);
        assert_eq!(glyph_index('9'), 36);
        // Minuscules et accents repliés.
        assert_eq!(glyph_index('a'), glyph_index('A'));
        assert_eq!(glyph_index('é'), glyph_index('E'));
        assert_eq!(glyph_index('Ô'), glyph_index('O'));
        assert_eq!(glyph_index('ç'), glyph_index('C'));
        // Tirets typographiques → '-'.
        assert_eq!(glyph_index('—'), glyph_index('-'));
        assert_eq!(glyph_index('_'), glyph_index('-'));
        // '×' a son glyphe propre, distinct de 'X'.
        assert_ne!(glyph_index('×'), glyph_index('X'));
        assert_ne!(glyph_index('×'), 0);
        // Inconnu → espace.
        assert_eq!(glyph_index('@'), 0);
        assert_eq!(glyph_index('!'), 0);
    }

    #[test]
    fn encode_nominal() {
        let mut out = [0i32; IDENT_TEXT_MAX];
        let len = encode_output_ident("Principal", 1920, 1080, &mut out);
        // "PRINCIPAL - 1920×1080" = 9 + 3 + 9 = 21 glyphes.
        assert_eq!(len, 21);
        assert_eq!(out[0], glyph_index('P'));
        assert_eq!(out[8], glyph_index('L'));
        assert_eq!(out[9], 0, "espace avant le tiret");
        assert_eq!(out[10], glyph_index('-'));
        assert_eq!(out[11], 0, "espace après le tiret");
        assert_eq!(out[12], glyph_index('1'));
        assert_eq!(out[16], glyph_index('×'));
        assert_eq!(out[17], glyph_index('1'));
        assert_eq!(out[20], glyph_index('0'));
    }

    #[test]
    fn encode_nom_vide_donne_la_resolution_seule() {
        let mut out = [0i32; IDENT_TEXT_MAX];
        let len = encode_output_ident("", 1280, 720, &mut out);
        assert_eq!(len, 8); // "1280×720"
        assert_eq!(out[0], glyph_index('1'));
        assert_eq!(out[4], glyph_index('×'));
        // Idem avec un nom fait d'espaces.
        let len = encode_output_ident("   ", 1280, 720, &mut out);
        assert_eq!(len, 8);
    }

    #[test]
    fn encode_tronque_le_nom_jamais_la_resolution() {
        let mut out = [0i32; IDENT_TEXT_MAX];
        let len = encode_output_ident(
            "UN NOM DE SORTIE VRAIMENT TRES LONG",
            3840,
            2160,
            &mut out,
        );
        assert_eq!(len as usize, IDENT_TEXT_MAX);
        // La fin est toujours "3840×2160".
        let tail: Vec<i32> = "3840×2160".chars().map(glyph_index).collect();
        assert_eq!(&out[IDENT_TEXT_MAX - tail.len()..], &tail[..]);
    }

    #[test]
    fn encode_accents_et_casse() {
        let mut out = [0i32; IDENT_TEXT_MAX];
        let len = encode_output_ident("Côté jardin", 800, 600, &mut out);
        assert!(len > 0);
        assert_eq!(out[0], glyph_index('C'));
        assert_eq!(out[1], glyph_index('O'));
        assert_eq!(out[2], glyph_index('T'));
        assert_eq!(out[3], glyph_index('E'));
        assert_eq!(out[4], 0); // espace
        assert_eq!(out[5], glyph_index('J'));
    }

    #[test]
    fn digits_poids_fort_en_tete() {
        let s: String = digits(1920).collect();
        assert_eq!(s, "1920");
        let s: String = digits(0).collect();
        assert_eq!(s, "0");
        let s: String = digits(7).collect();
        assert_eq!(s, "7");
        let s: String = digits(4294967295).collect();
        assert_eq!(s, "4294967295");
    }

    #[test]
    fn glsl_genere_coherent() {
        let glsl = glyph_table_glsl();
        assert!(glsl.contains(&format!("const int IDENT_MAX = {IDENT_TEXT_MAX};")));
        assert!(glsl.contains(&format!("const int GLYPH_COUNT = {};", GLYPHS.len())));
        assert!(glsl.contains(&format!("GLYPH_LO[{}]", GLYPHS.len())));
        assert!(glsl.contains(&format!("GLYPH_HI[{}]", GLYPHS.len())));
        assert!(glsl.contains("float glyph_bit(int g, int px, int py)"));
        // La valeur du tiret apparaît telle quelle dans la table générée.
        let (dash_lo, _) = pack_glyph(&GLYPHS[glyph_index('-') as usize].1);
        assert!(glsl.contains(&dash_lo.to_string()));
    }
}
