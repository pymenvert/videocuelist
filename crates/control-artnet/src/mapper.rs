//! Cœur pur du nœud : application du patch DMX aux trames reçues
//! ([`DmxMapper`] → `Command::ParamSet`), anti-spam par canal, suivi du champ
//! séquence ([`SequenceTracker`]) et overrides de lissage pour `params`.

use std::collections::HashMap;

use conduite_core::{Command, DmxBits, ParamValue, PatchEntry, PatchTable, Source};

/// Saut de séquence à partir duquel on journalise (trames perdues probables).
pub const SEQUENCE_GAP_WARN: u8 = 8;

/// Applique le patch Art-Net aux trames DMX et n'émet un `ParamSet` que si
/// la valeur brute du canal a changé (anti-spam : le DMX rejoue la même
/// trame ~44 fois par seconde).
#[derive(Debug, Default)]
pub struct DmxMapper {
    entries: Vec<PatchEntry>,
    /// Dernière valeur brute vue par entrée (même index). `None` = jamais vue.
    last_raw: Vec<Option<u16>>,
}

impl DmxMapper {
    pub fn new(entries: Vec<PatchEntry>) -> Self {
        let last_raw = vec![None; entries.len()];
        Self { entries, last_raw }
    }

    pub fn from_table(patch: &PatchTable) -> Self {
        Self::new(patch.artnet.clone())
    }

    /// Remplace le patch (mise à jour en cours de show). L'anti-spam est
    /// réinitialisé : la prochaine trame ré-émet toutes les valeurs.
    pub fn set_entries(&mut self, entries: Vec<PatchEntry>) {
        self.last_raw = vec![None; entries.len()];
        self.entries = entries;
    }

    pub fn entries(&self) -> &[PatchEntry] {
        &self.entries
    }

    /// Applique une trame DMX d'un univers. 8 bits : valeur canal / 255 ;
    /// 16 bits : (MSB << 8 | LSB) / 65535 (LSB sur `channel + 1`) ; puis
    /// mappage sur [min, max]. Un canal hors trame est ignoré sans bruit.
    pub fn apply(&mut self, universe: u16, data: &[u8]) -> Vec<Command> {
        let mut out = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.universe != universe {
                continue;
            }
            let Some(raw) = raw_value(entry, data) else {
                continue;
            };
            if self.last_raw[i] == Some(raw) {
                continue; // anti-spam : canal inchangé
            }
            self.last_raw[i] = Some(raw);
            let norm = match entry.bits {
                DmxBits::Eight => f32::from(raw) / 255.0,
                DmxBits::Sixteen => f32::from(raw) / 65535.0,
            };
            let value = entry.min + norm * (entry.max - entry.min);
            out.push(Command::ParamSet {
                addr: entry.addr.clone(),
                value: ParamValue::F(value),
                source: Source::ArtNet,
            });
        }
        out
    }
}

/// Valeur brute du canal d'une entrée dans la trame. `None` si le canal
/// (ou son LSB en 16 bits) n'est pas couvert, ou hors 1..=512.
fn raw_value(entry: &PatchEntry, data: &[u8]) -> Option<u16> {
    let ch = usize::from(entry.channel);
    if !(1..=512).contains(&ch) {
        return None;
    }
    match entry.bits {
        DmxBits::Eight => data.get(ch - 1).map(|&b| u16::from(b)),
        DmxBits::Sixteen => {
            let msb = *data.get(ch - 1)?;
            let lsb = *data.get(ch)?;
            Some(u16::from(msb) << 8 | u16::from(lsb))
        }
    }
}

/// Overrides de lissage à passer à `params::Registry::set_smoothing_override`
/// au chargement du show et à chaque mise à jour du patch : une paire
/// (addr, smoothing_ms) par entrée Art-Net (le lissage fin vit dans `params`,
/// pas ici — le DMX arrive à ~44 Hz, on interpole côté paramètres).
pub fn smoothing_overrides(patch: &PatchTable) -> Vec<(String, f32)> {
    patch
        .artnet
        .iter()
        .map(|e| (e.addr.clone(), e.smoothing_ms))
        .collect()
}

/// Suivi du champ séquence par univers : tolère l'absence de séquençage
/// (séquence 0) et les petits ré-ordonnancements, signale les gros sauts.
#[derive(Debug, Default)]
pub struct SequenceTracker {
    last: HashMap<u16, u8>,
}

impl SequenceTracker {
    /// Observe la séquence d'une trame. Retourne le nombre de trames sautées
    /// quand le saut atteint [`SEQUENCE_GAP_WARN`] (à journaliser par
    /// l'appelant) — jamais de rejet : on tolère tout.
    pub fn observe(&mut self, universe: u16, sequence: u8) -> Option<u8> {
        if sequence == 0 {
            return None; // séquençage désactivé par l'émetteur
        }
        let last = self.last.insert(universe, sequence)?;
        if last == 0 {
            return None;
        }
        // La séquence court 1..=255 puis reboucle à 1 (0 exclu).
        let expected = if last == 255 { 1 } else { last + 1 };
        let gap = if sequence >= expected {
            sequence - expected
        } else {
            255 - expected + sequence
        };
        (gap >= SEQUENCE_GAP_WARN).then_some(gap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(universe: u16, channel: u16, bits: DmxBits, addr: &str, min: f32, max: f32) -> PatchEntry {
        PatchEntry {
            universe,
            channel,
            bits,
            addr: addr.into(),
            min,
            max,
            smoothing_ms: 80.0,
        }
    }

    fn param_set(cmd: &Command) -> (&str, f32) {
        match cmd {
            Command::ParamSet {
                addr,
                value: ParamValue::F(v),
                source: Source::ArtNet,
            } => (addr.as_str(), *v),
            other => panic!("attendu ParamSet(F, ArtNet), obtenu {other:?}"),
        }
    }

    #[test]
    fn eight_bits_maps_channel_over_255_to_min_max() {
        let mut m = DmxMapper::new(vec![
            entry(0, 1, DmxBits::Eight, "master/intensity", 0.0, 1.0),
            entry(0, 4, DmxBits::Eight, "slice/1/media/speed", 0.25, 4.0),
        ]);
        let cmds = m.apply(0, &[255, 0, 0, 128]);
        assert_eq!(cmds.len(), 2);
        let (addr, v) = param_set(&cmds[0]);
        assert_eq!(addr, "master/intensity");
        assert!((v - 1.0).abs() < 1e-6);
        let (addr, v) = param_set(&cmds[1]);
        assert_eq!(addr, "slice/1/media/speed");
        let want = 0.25 + (128.0 / 255.0) * (4.0 - 0.25);
        assert!((v - want).abs() < 1e-5, "vitesse {v} != {want}");
    }

    #[test]
    fn sixteen_bits_combines_msb_lsb_over_65535() {
        let mut m = DmxMapper::new(vec![entry(
            0,
            5,
            DmxBits::Sixteen,
            "slice/1/media/position",
            0.0,
            1.0,
        )]);
        // Canal 5 = MSB, canal 6 = LSB.
        let mut data = [0u8; 6];
        data[4] = 0x12;
        data[5] = 0x34;
        let cmds = m.apply(0, &data);
        assert_eq!(cmds.len(), 1);
        let (_, v) = param_set(&cmds[0]);
        let want = f32::from(0x1234u16) / 65535.0;
        assert!((v - want).abs() < 1e-7);

        // Pleine échelle : exactement max.
        m.set_entries(vec![entry(0, 1, DmxBits::Sixteen, "a", 2.0, 10.0)]);
        let cmds = m.apply(0, &[255, 255]);
        let (_, v) = param_set(&cmds[0]);
        assert!((v - 10.0).abs() < 1e-5);
    }

    /// L'anti-spam : la même trame rejouée (DMX à ~44 Hz) n'émet rien ;
    /// seul le canal qui change ré-émet.
    #[test]
    fn antispam_emits_only_on_channel_change() {
        let mut m = DmxMapper::new(vec![
            entry(0, 1, DmxBits::Eight, "a", 0.0, 1.0),
            entry(0, 2, DmxBits::Eight, "b", 0.0, 1.0),
        ]);
        assert_eq!(m.apply(0, &[10, 20]).len(), 2, "première trame : tout sort");
        assert!(m.apply(0, &[10, 20]).is_empty(), "trame identique : silence");
        assert!(m.apply(0, &[10, 20]).is_empty());

        let cmds = m.apply(0, &[10, 21]);
        assert_eq!(cmds.len(), 1, "seul le canal 2 a changé");
        assert_eq!(param_set(&cmds[0]).0, "b");

        // En 16 bits, un LSB qui bouge suffit à ré-émettre.
        m.set_entries(vec![entry(0, 1, DmxBits::Sixteen, "fine", 0.0, 1.0)]);
        assert_eq!(m.apply(0, &[8, 0]).len(), 1);
        assert!(m.apply(0, &[8, 0]).is_empty());
        assert_eq!(m.apply(0, &[8, 1]).len(), 1, "LSB changé");
    }

    #[test]
    fn set_entries_resets_antispam() {
        let mut m = DmxMapper::new(vec![entry(0, 1, DmxBits::Eight, "a", 0.0, 1.0)]);
        assert_eq!(m.apply(0, &[10]).len(), 1);
        m.set_entries(vec![entry(0, 1, DmxBits::Eight, "a", 0.0, 1.0)]);
        assert_eq!(m.apply(0, &[10]).len(), 1, "patch rechargé : ré-émission");
    }

    #[test]
    fn other_universe_is_ignored() {
        let mut m = DmxMapper::new(vec![entry(5, 1, DmxBits::Eight, "a", 0.0, 1.0)]);
        assert!(m.apply(0, &[255]).is_empty(), "univers 0 ≠ univers patché 5");
        assert_eq!(m.apply(5, &[255]).len(), 1);
    }

    /// Canal hors trame ou hors bornes : ignoré sans panique (une console
    /// peut émettre une trame courte, un patch peut être erroné).
    #[test]
    fn out_of_frame_channels_are_skipped() {
        let mut m = DmxMapper::new(vec![
            entry(0, 10, DmxBits::Eight, "a", 0.0, 1.0),   // trame trop courte
            entry(0, 4, DmxBits::Sixteen, "b", 0.0, 1.0),  // LSB (canal 5) absent
            entry(0, 0, DmxBits::Eight, "c", 0.0, 1.0),    // canal 0 invalide
            entry(0, 513, DmxBits::Eight, "d", 0.0, 1.0),  // canal > 512
            entry(0, 2, DmxBits::Eight, "ok", 0.0, 1.0),
        ]);
        let cmds = m.apply(0, &[1, 2, 3, 4]);
        assert_eq!(cmds.len(), 1);
        assert_eq!(param_set(&cmds[0]).0, "ok");
    }

    /// min > max inverse simplement la course du fader (voulu).
    #[test]
    fn inverted_range_maps_downward() {
        let mut m = DmxMapper::new(vec![entry(0, 1, DmxBits::Eight, "a", 1.0, 0.0)]);
        let (_, v) = param_set(&m.apply(0, &[0])[0]);
        assert!((v - 1.0).abs() < 1e-6);
        let (_, v) = param_set(&m.apply(0, &[255])[0]);
        assert!(v.abs() < 1e-6);
    }

    #[test]
    fn smoothing_overrides_come_from_patch_table() {
        let patch = PatchTable {
            artnet: vec![
                entry(0, 1, DmxBits::Eight, "master/intensity", 0.0, 1.0),
                PatchEntry {
                    smoothing_ms: 120.0,
                    ..entry(0, 2, DmxBits::Sixteen, "slice/1/opacity", 0.0, 1.0)
                },
            ],
            ..PatchTable::default()
        };
        let overrides = smoothing_overrides(&patch);
        assert_eq!(
            overrides,
            vec![
                ("master/intensity".to_string(), 80.0),
                ("slice/1/opacity".to_string(), 120.0),
            ]
        );
    }

    #[test]
    fn sequence_tracker_tolerates_normal_flow() {
        let mut t = SequenceTracker::default();
        // Séquençage désactivé : jamais de signalement.
        assert_eq!(t.observe(0, 0), None);
        assert_eq!(t.observe(0, 0), None);
        // Première trame séquencée puis flux nominal.
        assert_eq!(t.observe(0, 1), None);
        assert_eq!(t.observe(0, 2), None);
        assert_eq!(t.observe(0, 3), None);
        // Rebouclage 255 → 1 (0 exclu) : normal.
        let mut t = SequenceTracker::default();
        assert_eq!(t.observe(0, 255), None);
        assert_eq!(t.observe(0, 1), None);
        // Petit saut (< seuil) : toléré en silence.
        assert_eq!(t.observe(0, 5), None, "3 trames perdues < seuil");
    }

    #[test]
    fn sequence_tracker_reports_big_gaps_per_universe() {
        let mut t = SequenceTracker::default();
        assert_eq!(t.observe(0, 10), None);
        // 11..=19 perdues : saut de 9 ≥ seuil.
        assert_eq!(t.observe(0, 20), Some(9));
        // L'univers 1 est suivi indépendamment.
        assert_eq!(t.observe(1, 20), None);
        assert_eq!(t.observe(1, 21), None);
        // Gros saut à travers le rebouclage : 251..=255,1..=3 = 8 perdues.
        let mut t = SequenceTracker::default();
        assert_eq!(t.observe(0, 250), None);
        assert_eq!(t.observe(0, 4), Some(8));
    }
}
