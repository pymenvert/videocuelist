//! Undo/redo d'édition : pile de snapshots complets du [`Show`]
//! (cap 100). Simple, robuste — un show reste petit (JSON de quelques Ko).

use conduite_core::Show;

/// Capacité maximale de la pile d'annulation.
pub const UNDO_CAP: usize = 100;

/// Pile d'annulation : snapshot pris AVANT chaque édition.
#[derive(Debug, Default)]
pub struct UndoStack {
    undo: Vec<Show>,
    redo: Vec<Show>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// À appeler avant d'appliquer une édition : mémorise l'état courant
    /// et invalide la pile de redo.
    pub fn push(&mut self, before: Show) {
        if self.undo.len() >= UNDO_CAP {
            self.undo.remove(0);
        }
        self.undo.push(before);
        self.redo.clear();
    }

    /// Annule : rend l'état précédent, en mémorisant `current` pour redo.
    pub fn undo(&mut self, current: &Show) -> Option<Show> {
        let prev = self.undo.pop()?;
        self.redo.push(current.clone());
        Some(prev)
    }

    /// Rétablit : rend l'état annulé, en re-mémorisant `current` pour undo.
    pub fn redo(&mut self, current: &Show) -> Option<Show> {
        let next = self.redo.pop()?;
        self.undo.push(current.clone());
        Some(next)
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
        assert_eq!(stack.undo.last().map(|s| s.name.clone()), Some("v149".into()));
    }

    #[test]
    fn empty_stacks_are_none() {
        let mut stack = UndoStack::new();
        let cur = Show::new("x");
        assert!(stack.undo(&cur).is_none());
        assert!(stack.redo(&cur).is_none());
    }
}
