//! Undo/redo d'édition : pile de snapshots complets du [`Show`], bornée en
//! NOMBRE (cap 100) et en OCTETS (64 Mo, taille estimée par sérialisation
//! JSON — cible Raspberry Pi : l'historique ne peut pas épuiser la RAM).
//!
//! Le coalescing des édits continus (un drag de coin = un seul snapshot)
//! vit côté session (`Session::apply_edit`), pas ici.

use conduite_core::Show;

/// Capacité maximale de la pile d'annulation (en entrées).
pub const UNDO_CAP: usize = 100;
/// Plafond mémoire de l'historique complet (undo + redo), en octets estimés.
pub const UNDO_BYTE_CAP: usize = 64 * 1024 * 1024;

/// Un snapshot et sa taille estimée (octets JSON).
#[derive(Debug)]
struct Entry {
    show: Show,
    bytes: usize,
}

/// Taille JSON estimée d'un show. Échec de sérialisation (jamais observé
/// pour un Show valide) : estimation forfaitaire prudente.
fn estimate_bytes(show: &Show) -> usize {
    match serde_json::to_vec(show) {
        Ok(v) => v.len(),
        Err(_) => 64 * 1024,
    }
}

/// Pile d'annulation : snapshot pris AVANT chaque édition.
#[derive(Debug, Default)]
pub struct UndoStack {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Octets totaux (undo + redo) actuellement retenus.
    fn total_bytes(&self) -> usize {
        self.undo.iter().map(|e| e.bytes).sum::<usize>()
            + self.redo.iter().map(|e| e.bytes).sum::<usize>()
    }

    /// Évince les entrées les plus anciennes de la pile d'undo tant que le
    /// plafond en octets est dépassé (au moins l'entrée la plus récente est
    /// toujours conservée).
    fn enforce_byte_cap(&mut self) {
        while self.undo.len() > 1 && self.total_bytes() > UNDO_BYTE_CAP {
            self.undo.remove(0);
        }
    }

    /// À appeler avant d'appliquer une édition : mémorise l'état courant
    /// et invalide la pile de redo.
    pub fn push(&mut self, before: Show) {
        if self.undo.len() >= UNDO_CAP {
            self.undo.remove(0);
        }
        let bytes = estimate_bytes(&before);
        self.undo.push(Entry { show: before, bytes });
        self.redo.clear();
        self.enforce_byte_cap();
    }

    /// Annule : rend l'état précédent, en mémorisant `current` pour redo.
    pub fn undo(&mut self, current: &Show) -> Option<Show> {
        let prev = self.undo.pop()?;
        let bytes = estimate_bytes(current);
        self.redo.push(Entry { show: current.clone(), bytes });
        Some(prev.show)
    }

    /// Rétablit : rend l'état annulé, en re-mémorisant `current` pour undo.
    pub fn redo(&mut self, current: &Show) -> Option<Show> {
        let next = self.redo.pop()?;
        let bytes = estimate_bytes(current);
        self.undo.push(Entry { show: current.clone(), bytes });
        Some(next.show)
    }

    /// Vide les deux piles (chargement d'un autre show).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduite_core::demo_show;

    #[test]
    fn undo_then_redo_roundtrip() {
        let mut stack = UndoStack::new();
        let v1 = Show::new("v1");
        let mut v2 = demo_show();
        v2.name = "v2".into();

        stack.push(v1.clone()); // avant l'édition qui produit v2
        let back = stack.undo(&v2).expect("undo");
        assert_eq!(back.name, "v1");
        let fwd = stack.redo(&back).expect("redo");
        assert_eq!(fwd.name, "v2");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut stack = UndoStack::new();
        let v1 = Show::new("v1");
        let v2 = Show::new("v2");
        stack.push(v1);
        let _ = stack.undo(&v2).expect("undo");
        stack.push(Show::new("v1bis"));
        assert!(stack.redo(&v2).is_none(), "redo invalidé par une édition");
    }

    #[test]
    fn capped_at_100() {
        let mut stack = UndoStack::new();
        for i in 0..150 {
            stack.push(Show::new(format!("v{i}")));
        }
        assert_eq!(stack.undo.len(), UNDO_CAP);
        // Le plus récent est bien le dernier poussé.
        assert_eq!(
            stack.undo.last().map(|e| e.show.name.clone()),
            Some("v149".into())
        );
    }

    #[test]
    fn empty_stacks_are_none() {
        let mut stack = UndoStack::new();
        let cur = Show::new("x");
        assert!(stack.undo(&cur).is_none());
        assert!(stack.redo(&cur).is_none());
    }

    /// Plafond en octets : les entrées les plus anciennes sont évincées, la
    /// plus récente survit toujours (même si elle dépasse à elle seule).
    #[test]
    fn byte_cap_evicts_oldest() {
        let mut stack = UndoStack::new();
        // Show artificiellement gros : ~1,4 Mo de notes par cue.
        let big = |name: &str| {
            let mut s = demo_show();
            s.name = name.to_string();
            for c in &mut s.cues {
                c.notes = "x".repeat(300_000);
            }
            s
        };
        // 5 cues * 300 Ko ≈ 1,5 Mo par snapshot ; 64 Mo / 1,5 Mo ≈ 42.
        for i in 0..60 {
            stack.push(big(&format!("v{i}")));
        }
        assert!(stack.undo.len() < 60, "des snapshots ont été évincés");
        assert!(
            stack.total_bytes() <= UNDO_BYTE_CAP,
            "total {} > cap {}",
            stack.total_bytes(),
            UNDO_BYTE_CAP
        );
        assert_eq!(
            stack.undo.last().map(|e| e.show.name.clone()),
            Some("v59".into()),
            "le plus récent survit"
        );
    }
}
