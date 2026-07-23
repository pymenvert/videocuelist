//! Cache de programmes ISF par clé matériau — logique pure, testée sans GL.
//!
//! Politique : un échec de compilation ne détruit JAMAIS l'entrée précédente
//! (le slice continue d'afficher l'ancien programme, l'erreur remonte à l'UI).
//! Le hot-reload passe par [`ProgramCache::invalidate`] avant recompilation.

use std::collections::HashMap;

use conduite_core::MaterialId;

/// Cache clé → programme compilé. `P` est le programme GL réel en prod,
/// un type quelconque dans les tests.
pub struct ProgramCache<P> {
    map: HashMap<MaterialId, P>,
}

impl<P> Default for ProgramCache<P> {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
}

impl<P> ProgramCache<P> {
    /// Programme en cache pour cette clé, s'il existe.
    pub fn get(&self, key: MaterialId) -> Option<&P> {
        self.map.get(&key)
    }

    /// Garantit qu'un programme existe pour la clé : compile via `build`
    /// seulement si absent du cache. En cas d'échec de `build`, le cache est
    /// inchangé (aucune entrée créée) et l'erreur remonte telle quelle.
    pub fn ensure<E>(
        &mut self,
        key: MaterialId,
        build: impl FnOnce() -> Result<P, E>,
    ) -> Result<&P, E> {
        use std::collections::hash_map::Entry;
        match self.map.entry(key) {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(v) => Ok(v.insert(build()?)),
        }
    }

    /// Recompile inconditionnellement (hot-reload). En cas d'échec, l'entrée
    /// PRÉCÉDENTE est conservée — le show continue avec l'ancien shader.
    /// Retourne le programme remplacé pour libération des ressources GL.
    pub fn recompile<E>(
        &mut self,
        key: MaterialId,
        build: impl FnOnce() -> Result<P, E>,
    ) -> Result<Option<P>, E> {
        let program = build()?;
        Ok(self.map.insert(key, program))
    }

    /// Retire l'entrée (le prochain `ensure` recompilera). Retourne le
    /// programme évincé pour que l'appelant libère les ressources GL.
    pub fn invalidate(&mut self, key: MaterialId) -> Option<P> {
        self.map.remove(&key)
    }

    /// Vrai si un programme est déjà en cache pour cette clé.
    pub fn contains(&self, key: MaterialId) -> bool {
        self.map.contains_key(&key)
    }

    /// Ne conserve que les clés listées dans `keep` (matériaux encore
    /// présents dans le show). Retourne les programmes évincés pour que
    /// l'appelant libère leurs ressources GL — sans cet appel à chaque
    /// changement de show, les programmes des matériaux disparus restent
    /// en VRAM à vie.
    pub fn retain(&mut self, keep: &[MaterialId]) -> Vec<P> {
        crate::keys_not_kept(self.map.keys().copied(), keep)
            .into_iter()
            .filter_map(|k| self.map.remove(&k))
            .collect()
    }

    /// Nombre de programmes en cache.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Vrai si le cache est vide.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_compiles_once_per_key() {
        let mut cache: ProgramCache<u32> = ProgramCache::default();
        let mut builds = 0u32;
        for _ in 0..3 {
            let p = cache
                .ensure(7, || -> Result<u32, String> {
                    builds += 1;
                    Ok(42)
                })
                .expect("build ok");
            assert_eq!(*p, 42);
        }
        assert_eq!(builds, 1, "une seule compilation pour la même clé");
        assert_eq!(cache.len(), 1);

        // Une autre clé compile indépendamment.
        cache
            .ensure(8, || -> Result<u32, String> {
                builds += 1;
                Ok(43)
            })
            .expect("build ok");
        assert_eq!(builds, 2);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn ensure_failure_leaves_cache_unchanged() {
        let mut cache: ProgramCache<u32> = ProgramCache::default();
        let err = cache
            .ensure(1, || Err::<u32, _>("log GLSL complet".to_string()))
            .expect_err("échec attendu");
        assert_eq!(err, "log GLSL complet");
        assert!(cache.is_empty(), "pas d'entrée fantôme après échec");
        assert!(cache.get(1).is_none());
    }

    #[test]
    fn recompile_failure_keeps_previous_program() {
        let mut cache: ProgramCache<u32> = ProgramCache::default();
        cache
            .ensure(5, || Ok::<_, String>(100))
            .expect("premier build");

        // Hot-reload raté : le shader précédent reste servi.
        let err = cache
            .recompile(5, || Err::<u32, _>("erreur de syntaxe ligne 12".to_string()))
            .expect_err("échec attendu");
        assert!(err.contains("ligne 12"));
        assert_eq!(cache.get(5), Some(&100), "ancien programme conservé");

        // Hot-reload réussi : remplacement, l'ancien est retourné.
        let old = cache
            .recompile(5, || Ok::<_, String>(200))
            .expect("recompile ok");
        assert_eq!(old, Some(100));
        assert_eq!(cache.get(5), Some(&200));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn retain_evicts_absent_keys_and_returns_programs() {
        let mut cache: ProgramCache<u32> = ProgramCache::default();
        cache.ensure(1, || Ok::<_, String>(10)).expect("build");
        cache.ensure(2, || Ok::<_, String>(20)).expect("build");
        cache.ensure(3, || Ok::<_, String>(30)).expect("build");

        let mut evicted = cache.retain(&[2]);
        evicted.sort_unstable();
        assert_eq!(evicted, [10, 30], "programmes évincés retournés pour libération GL");
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(2));
        assert!(!cache.contains(1));
        assert!(!cache.contains(3));

        // Le prochain ensure d'une clé évincée recompile.
        let mut builds = 0;
        cache
            .ensure(1, || -> Result<u32, String> {
                builds += 1;
                Ok(11)
            })
            .expect("rebuild");
        assert_eq!(builds, 1, "recompilé après éviction");

        // `keep` vide : tout est purgé (changement de show sans matériaux).
        let evicted = cache.retain(&[]);
        assert_eq!(evicted.len(), 2);
        assert!(cache.is_empty());

        // retain sur cache vide : no-op.
        assert!(cache.retain(&[1, 2]).is_empty());
    }

    #[test]
    fn retain_with_all_keys_kept_is_a_noop() {
        let mut cache: ProgramCache<u32> = ProgramCache::default();
        cache.ensure(7, || Ok::<_, String>(70)).expect("build");
        cache.ensure(8, || Ok::<_, String>(80)).expect("build");
        // Clés surnuméraires dans keep : sans effet.
        assert!(cache.retain(&[7, 8, 99]).is_empty());
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(7), Some(&70));
        assert_eq!(cache.get(8), Some(&80));
    }

    #[test]
    fn invalidate_forces_rebuild_and_returns_old() {
        let mut cache: ProgramCache<u32> = ProgramCache::default();
        cache.ensure(3, || Ok::<_, String>(1)).expect("build");
        assert_eq!(cache.invalidate(3), Some(1), "programme évincé retourné");
        assert_eq!(cache.invalidate(3), None, "déjà retiré");

        let mut builds = 0;
        cache
            .ensure(3, || -> Result<u32, String> {
                builds += 1;
                Ok(2)
            })
            .expect("rebuild");
        assert_eq!(builds, 1, "recompilé après invalidation");
        assert_eq!(cache.get(3), Some(&2));
    }
}
