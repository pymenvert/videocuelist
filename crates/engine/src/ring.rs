//! Ring buffer borné de frames (SPSC), mutex + condvar.
//!
//! Le thread lecteur (producteur) **bloque** quand le buffer est plein :
//! c'est la backpressure naturelle — ffmpeg se retrouve bloqué sur son pipe
//! et arrête de décoder. La pause consiste simplement à ne plus consommer.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::pacing::{Decision, Pacer};
use crate::FrameRgba;

/// Capacité du ring (frames décodées en avance).
pub const RING_CAPACITY: usize = 4;

#[derive(Debug, Default)]
struct State {
    frames: VecDeque<FrameRgba>,
    closed: bool,
}

#[derive(Debug, Default)]
struct Inner {
    state: Mutex<State>,
    not_full: Condvar,
}

/// File bornée partagée entre le thread lecteur et `poll_frame`.
#[derive(Debug, Clone, Default)]
pub struct FrameRing {
    inner: Arc<Inner>,
}

impl FrameRing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Verrouille en récupérant un éventuel poison (jamais de panic).
    fn lock(&self) -> MutexGuard<'_, State> {
        match self.inner.state.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Producteur : pousse une frame, bloque tant que le buffer est plein.
    /// Retourne `false` si le ring a été fermé (le producteur doit s'arrêter).
    pub fn push(&self, frame: FrameRgba) -> bool {
        let mut st = self.lock();
        while st.frames.len() >= RING_CAPACITY && !st.closed {
            st = match self.inner.not_full.wait(st) {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
        if st.closed {
            return false;
        }
        st.frames.push_back(frame);
        true
    }

    /// Ferme le ring : débloque le producteur, `push` renverra `false`.
    /// Les frames déjà en file restent consommables.
    pub fn close(&self) {
        let mut st = self.lock();
        st.closed = true;
        self.inner.not_full.notify_all();
    }

    /// Réouvre le ring après [`FrameRing::close`] pour le producteur suivant
    /// (cycle de boucle, seek). Les frames en file restent consommables.
    /// SPSC : à n'appeler qu'une fois l'ancien producteur terminé.
    pub fn reopen(&self) {
        let mut st = self.lock();
        st.closed = false;
    }

    /// Jette toutes les frames en attente (seek : elles n'ont plus cours).
    pub fn clear(&self) {
        let mut st = self.lock();
        st.frames.clear();
        self.inner.not_full.notify_all();
    }

    /// `true` si le producteur a terminé ET que tout a été consommé.
    pub fn is_drained(&self) -> bool {
        let st = self.lock();
        st.closed && st.frames.is_empty()
    }

    /// Nombre de frames en attente.
    pub fn len(&self) -> usize {
        self.lock().frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Consommateur : applique la décision de pacing pour `media_time_s`.
    /// Jette les frames sautées, retourne la frame choisie, ou `None` (dup).
    /// Ne bloque jamais.
    pub fn poll(&self, pacer: &mut Pacer, media_time_s: f64) -> Option<FrameRgba> {
        let mut st = self.lock();
        // Pts disponibles, sans allocation dans le cas nominal (cap 4).
        let mut pts = [0.0f64; RING_CAPACITY];
        let n = st.frames.len().min(RING_CAPACITY);
        for (i, f) in st.frames.iter().take(n).enumerate() {
            pts[i] = f.pts_s;
        }
        match pacer.choose(&pts[..n], media_time_s) {
            Decision::Hold => None,
            Decision::Take { index, skipped } => {
                if skipped > 0 {
                    tracing::trace!(target: "engine::ring", skipped, "frames sautées (retard)");
                }
                let mut chosen = None;
                for _ in 0..=index {
                    chosen = st.frames.pop_front();
                }
                self.inner.not_full.notify_all();
                chosen
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn frame(pts: f64) -> FrameRgba {
        FrameRgba { width: 2, height: 2, data: vec![0; 16].into(), pts_s: pts }
    }

    #[test]
    fn push_puis_poll_fifo() {
        let ring = FrameRing::new();
        assert!(ring.push(frame(0.0)));
        assert!(ring.push(frame(1.0 / 30.0)));
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        let f = ring.poll(&mut pacer, 0.0).unwrap();
        assert!((f.pts_s - 0.0).abs() < 1e-9);
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn poll_saute_les_frames_en_retard() {
        let ring = FrameRing::new();
        for i in 0..4 {
            assert!(ring.push(frame(i as f64 / 30.0)));
        }
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        // Horloge à la frame 3 → frames 0..2 jetées.
        let f = ring.poll(&mut pacer, 3.0 / 30.0).unwrap();
        assert!((f.pts_s - 3.0 / 30.0).abs() < 1e-9);
        assert!(ring.is_empty());
    }

    #[test]
    fn poll_hold_quand_en_avance() {
        let ring = FrameRing::new();
        assert!(ring.push(frame(0.5)));
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        assert!(ring.poll(&mut pacer, 0.0).is_none());
        assert_eq!(ring.len(), 1); // rien consommé
    }

    #[test]
    fn push_bloque_quand_plein_et_reprend_apres_poll() {
        let ring = FrameRing::new();
        for i in 0..RING_CAPACITY {
            assert!(ring.push(frame(i as f64 / 30.0)));
        }
        let ring2 = ring.clone();
        let handle = std::thread::spawn(move || ring2.push(frame(4.0 / 30.0)));
        // Le producteur doit être bloqué : laisse-lui une chance de finir s'il ne l'était pas.
        std::thread::sleep(Duration::from_millis(50));
        assert!(!handle.is_finished(), "push aurait dû bloquer sur ring plein");
        // Consommer une frame débloque le producteur.
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        assert!(ring.poll(&mut pacer, 0.0).is_some());
        assert!(handle.join().unwrap());
        assert_eq!(ring.len(), RING_CAPACITY);
    }

    #[test]
    fn close_debloque_le_producteur() {
        let ring = FrameRing::new();
        for i in 0..RING_CAPACITY {
            assert!(ring.push(frame(i as f64 / 30.0)));
        }
        let ring2 = ring.clone();
        let handle = std::thread::spawn(move || ring2.push(frame(9.9)));
        std::thread::sleep(Duration::from_millis(20));
        ring.close();
        assert!(!handle.join().unwrap(), "push doit renvoyer false après close");
    }

    #[test]
    fn reopen_permet_un_nouveau_cycle_de_production() {
        let ring = FrameRing::new();
        assert!(ring.push(frame(0.0)));
        ring.close();
        assert!(!ring.push(frame(1.0)), "fermé : push refusé");
        ring.reopen();
        assert!(ring.push(frame(1.0)), "réouvert : push accepté");
        assert_eq!(ring.len(), 2, "les frames d'avant la fermeture restent");
        assert!(!ring.is_drained());
    }

    #[test]
    fn clear_jette_les_frames_en_attente() {
        let ring = FrameRing::new();
        assert!(ring.push(frame(0.0)));
        assert!(ring.push(frame(1.0)));
        ring.clear();
        assert!(ring.is_empty());
        // Le ring reste utilisable après un clear.
        assert!(ring.push(frame(2.0)));
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn is_drained_apres_close_et_consommation() {
        let ring = FrameRing::new();
        assert!(ring.push(frame(0.0)));
        ring.close();
        assert!(!ring.is_drained(), "il reste une frame");
        let mut pacer = Pacer::new(30.0, 0.0, 1.0);
        assert!(ring.poll(&mut pacer, 0.0).is_some());
        assert!(ring.is_drained());
    }
}
