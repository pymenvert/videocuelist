//! Pool de buffers de frames : recycle les `Vec<u8>` (~8 Mo en 1080p RGBA)
//! au lieu d'allouer/zéroter/libérer un buffer PAR frame.
//!
//! Le thread lecteur prend un buffer via [`BufferPool::take`] ; la frame
//! traverse le ring puis l'app ; au drop de [`FrameData`], le buffer revient
//! automatiquement au pool (borné), quel que soit le thread. Aucune règle de
//! restitution côté appelant : le `Drop` s'en charge.
//!
//! Le zérotage n'a lieu qu'à la première allocation d'un buffer : un buffer
//! recyclé garde sa longueur pleine (il a été intégralement écrit) et est
//! réécrit en entier par la lecture du pipe.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::PixelOrder;

/// État partagé du pool (réserve bornée de buffers).
struct PoolInner {
    /// Taille exacte (octets) des buffers gérés : `width * height * 4`.
    frame_size: usize,
    /// Réserve maximale : au-delà, les retours sont simplement libérés.
    max_spares: usize,
    spares: Mutex<Vec<Vec<u8>>>,
}

/// Verrouille en récupérant un éventuel poison (jamais de panic).
fn lock_spares(inner: &PoolInner) -> MutexGuard<'_, Vec<Vec<u8>>> {
    match inner.spares.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Pool borné de buffers de frames, clonable et thread-safe.
#[derive(Clone)]
pub struct BufferPool {
    inner: Arc<PoolInner>,
}

impl BufferPool {
    /// Pool de buffers de `frame_size` octets, gardant au plus `max_spares`
    /// buffers en réserve.
    pub fn new(frame_size: usize, max_spares: usize) -> Self {
        BufferPool {
            inner: Arc::new(PoolInner {
                frame_size,
                max_spares,
                spares: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Buffer de `frame_size` octets prêt à être écrit : recyclé si possible,
    /// sinon fraîchement alloué (seul cas où un zérotage a lieu).
    pub fn take(&self) -> FrameData {
        let recycled = lock_spares(&self.inner).pop();
        let data = match recycled {
            Some(v) if v.len() == self.inner.frame_size => v,
            _ => vec![0u8; self.inner.frame_size],
        };
        FrameData {
            data,
            pool: Some(Arc::downgrade(&self.inner)),
            order: PixelOrder::Rgba,
        }
    }

    /// Nombre de buffers actuellement en réserve.
    #[cfg(test)]
    fn spare_count(&self) -> usize {
        lock_spares(&self.inner).len()
    }
}

/// Octets d'une frame, éventuellement adossés à un [`BufferPool`] : au drop,
/// le buffer retourne au pool (sans libération mémoire). Un `Vec<u8>` s'y
/// convertit via `From` pour les frames hors pool (mire de test, frame unie…).
pub struct FrameData {
    data: Vec<u8>,
    /// `None` = buffer hors pool (libéré normalement au drop).
    pool: Option<Weak<PoolInner>>,
    /// Ordre des canaux du contenu (RGBA par défaut, BGRA si le décodage
    /// BGRA est actif — posé par le thread lecteur ffmpeg).
    order: PixelOrder,
}

impl FrameData {
    /// Ordre des canaux du contenu.
    pub fn pixel_order(&self) -> PixelOrder {
        self.order
    }

    /// Pose l'ordre des canaux (thread lecteur, après écriture du contenu).
    pub(crate) fn set_pixel_order(&mut self, order: PixelOrder) {
        self.order = order;
    }
}

impl Drop for FrameData {
    fn drop(&mut self) {
        let Some(weak) = self.pool.take() else { return };
        let Some(inner) = weak.upgrade() else { return };
        if self.data.len() != inner.frame_size {
            return; // buffer inattendu : libération normale
        }
        let mut spares = lock_spares(&inner);
        if spares.len() < inner.max_spares {
            spares.push(std::mem::take(&mut self.data));
        }
    }
}

impl Deref for FrameData {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl DerefMut for FrameData {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl From<Vec<u8>> for FrameData {
    /// Buffer hors pool (contenu RGBA) : le drop libère normalement.
    fn from(data: Vec<u8>) -> Self {
        FrameData { data, pool: None, order: PixelOrder::Rgba }
    }
}

impl Clone for FrameData {
    /// Copie profonde, HORS pool : un buffer ne doit être rendu qu'une fois.
    fn clone(&self) -> Self {
        FrameData {
            data: self.data.clone(),
            pool: None,
            order: self.order,
        }
    }
}

impl std::fmt::Debug for FrameData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameData")
            .field("len", &self.data.len())
            .field("pooled", &self.pool.is_some())
            .field("order", &self.order)
            .finish()
    }
}

impl PartialEq for FrameData {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_rend_un_buffer_zerote_de_la_bonne_taille() {
        let pool = BufferPool::new(16, 2);
        let buf = pool.take();
        assert_eq!(buf.len(), 16);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn drop_restitue_et_take_recycle_la_meme_allocation() {
        let pool = BufferPool::new(16, 2);
        let buf = pool.take();
        let ptr = buf.as_ptr();
        drop(buf);
        assert_eq!(pool.spare_count(), 1);
        let again = pool.take();
        assert_eq!(again.as_ptr(), ptr, "le buffer doit être recyclé, pas réalloué");
        assert_eq!(pool.spare_count(), 0);
    }

    #[test]
    fn le_contenu_recycle_est_reutilisable_sans_zerotage() {
        // Un buffer recyclé garde sa longueur pleine : il est réécrit en
        // entier par le lecteur, le zérotage serait du travail perdu.
        let pool = BufferPool::new(4, 2);
        let mut buf = pool.take();
        buf.copy_from_slice(&[1, 2, 3, 4]);
        drop(buf);
        let again = pool.take();
        assert_eq!(again.len(), 4, "longueur pleine conservée");
    }

    #[test]
    fn la_reserve_est_bornee() {
        let pool = BufferPool::new(8, 1);
        let a = pool.take();
        let b = pool.take();
        let c = pool.take();
        drop(a);
        drop(b);
        drop(c);
        assert_eq!(pool.spare_count(), 1, "au-delà de max_spares, on libère");
    }

    #[test]
    fn from_vec_est_hors_pool() {
        let pool = BufferPool::new(4, 4);
        let unpooled: FrameData = vec![9u8; 4].into();
        drop(unpooled);
        assert_eq!(pool.spare_count(), 0);
    }

    #[test]
    fn clone_est_profond_et_hors_pool() {
        let pool = BufferPool::new(4, 4);
        let original = pool.take();
        let cloned = original.clone();
        assert_eq!(original, cloned);
        drop(cloned);
        assert_eq!(pool.spare_count(), 0, "le clone ne restitue pas");
        drop(original);
        assert_eq!(pool.spare_count(), 1, "l'original restitue");
    }

    #[test]
    fn drop_apres_disparition_du_pool_ne_panique_pas() {
        let pool = BufferPool::new(4, 4);
        let buf = pool.take();
        drop(pool);
        drop(buf); // Weak mort : libération normale, sans panic
    }
}
