//! A swappable, thread-shared embedder cell.
//!
//! Replaces the old `Arc<OnceLock<Arc<Embedder>>>` that both the watch loop and
//! the daemon `BatchContext` used to hold the resident embedder. A `OnceLock`
//! can be initialized but never *replaced* — which is exactly what a
//! foreign-model slot promote needs: a new model means a new ONNX session and
//! (usually) a different embedding dimension, so the resident embedder must be
//! swapped out, not merely lazily set once.
//!
//! # Coherence contract
//!
//! The resident `Arc<Embedder>` self-describes its model
//! ([`Embedder::model_config`]). Every consumer resolves against the model IT
//! needs — derived from its own store's recorded model, the single source of
//! truth — rather than blindly reusing whatever the cell currently holds. So a
//! swap by one holder can never make another holder pair a mismatched embedder
//! with its store snapshot: a reader that asks for model M either gets an
//! embedder for M or builds one, never an embedder for some other model.

use std::sync::{Arc, RwLock};

use super::{Embedder, EmbedderError, ModelConfig};

/// Swappable shared embedder. See the module docs for the coherence contract.
///
/// `RwLock` (not `ArcSwapOption`) because the read path — a checkout-time
/// snapshot or a per-reindex-cycle resolve — takes an uncontended read lock and
/// clones an `Arc`; the write path (a rare model swap) is the only exclusive
/// hold. The arc-swap crate is not a dependency, and the read frequency here
/// (once per query checkout, not once per embedding call) does not justify
/// pulling one in.
#[derive(Default)]
pub struct SharedEmbedder {
    inner: RwLock<Option<Arc<Embedder>>>,
}

impl SharedEmbedder {
    /// An empty cell (no resident embedder yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the resident embedder regardless of model. Cheap read-lock
    /// clone; `None` when nothing has been installed yet. Used by the idle
    /// sweep (to `clear_session` whatever is resident) and by the daemon's
    /// startup seed of the query-side context.
    pub fn get(&self) -> Option<Arc<Embedder>> {
        self.read().clone()
    }

    /// Snapshot the resident embedder only when its model matches `want`. The
    /// checkout-time pairing read: a `Some` result is guaranteed coherent with a
    /// store whose recorded model is `want`.
    pub fn get_matching(&self, want: &ModelConfig) -> Option<Arc<Embedder>> {
        self.read()
            .as_ref()
            .filter(|e| e.model_config() == want)
            .map(Arc::clone)
    }

    /// Return an embedder for `want`, building one if the resident is absent or
    /// a different model.
    ///
    /// Installs a freshly built embedder into the cell ONLY when the cell is
    /// empty (or another thread installed a matching one meanwhile) — it never
    /// clobbers a resident of a *different* model, because that resident may be
    /// a newer model a concurrent [`Self::swap`] just installed. A reader on the
    /// mismatch path therefore gets a private (unshared) embedder for `want`
    /// rather than overwriting the newer model. This is the non-clobbering lazy
    /// path used by the daemon query side, where several connection threads may
    /// resolve concurrently; the deliberate model change goes through
    /// [`Self::swap`].
    pub fn resolve_matching(&self, want: &ModelConfig) -> Result<Arc<Embedder>, EmbedderError> {
        if let Some(e) = self.get_matching(want) {
            return Ok(e);
        }
        // Build outside the read lock (heavy — an ONNX-backed session on first
        // embed). `Embedder::new` is lazy (no download at construction), but the
        // model swap it represents is still the rare path.
        let built = Arc::new(Embedder::new(want.clone())?);
        let mut guard = self.write();
        match guard.as_ref() {
            // A concurrent installer already put a matching embedder in place —
            // prefer it so the daemon threads still share one session.
            Some(existing) if existing.model_config() == want => Ok(Arc::clone(existing)),
            // Resident is a DIFFERENT model — do not overwrite it (it may be a
            // newer model a `swap` just installed). Hand back the private build.
            Some(_different) => Ok(built),
            None => {
                *guard = Some(Arc::clone(&built));
                Ok(built)
            }
        }
    }

    /// Unconditionally install `embedder` as the resident, replacing any prior
    /// model. The deliberate-swap path: a detected model change on store reopen
    /// (query side) or a foreign-model slot rebind (watch side), where the new
    /// model is authoritative and the old resident is now garbage.
    pub fn swap(&self, embedder: Arc<Embedder>) {
        *self.write() = Some(embedder);
    }

    /// Evict the resident, leaving the cell empty. Used when a deliberate model
    /// change swapped `model_config` but the new embedder could NOT be built:
    /// leaving the old-model resident would strand `resolve_matching` on the
    /// non-clobber branch (it would rebuild a private embedder every query
    /// without ever installing). Clearing lets the next `resolve_matching`
    /// install the new model on its first successful build — the cell converges.
    pub fn clear(&self) {
        *self.write() = None;
    }

    /// Install `embedder` iff the cell is currently empty. Returns `true` when
    /// this call installed it, `false` when a resident was already present
    /// (mirrors `OnceLock::set(..).is_ok()`). The lazy-warm / adopt path — never
    /// replaces an existing model.
    pub fn set_if_empty(&self, embedder: Arc<Embedder>) -> bool {
        let mut guard = self.write();
        if guard.is_some() {
            false
        } else {
            *guard = Some(embedder);
            true
        }
    }

    /// Install `embedder` iff the cell is empty, then return the resident
    /// (either the just-installed one or the pre-existing winner of a race).
    /// The startup-seed path: `get_or_set` mirrors `OnceLock::get_or_init`.
    pub fn get_or_set(&self, embedder: Arc<Embedder>) -> Arc<Embedder> {
        let mut guard = self.write();
        match guard.as_ref() {
            Some(existing) => Arc::clone(existing),
            None => {
                *guard = Some(Arc::clone(&embedder));
                embedder
            }
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Option<Arc<Embedder>>> {
        self.inner.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Option<Arc<Embedder>>> {
        self.inner.write().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A distinct-identity clone of a preset: same underlying files, a different
    /// `name`, so `get_matching` treats it as a different model without needing
    /// a second real model download. `Embedder::new` is lazy, so building one is
    /// cheap (no ONNX load until first embed).
    fn renamed(base: &ModelConfig, name: &str) -> ModelConfig {
        let mut m = base.clone();
        m.name = name.to_string();
        m
    }

    #[test]
    fn empty_cell_reads_none() {
        let cell = SharedEmbedder::new();
        assert!(cell.get().is_none());
        assert!(cell.get_matching(&ModelConfig::e5_base()).is_none());
    }

    #[test]
    fn get_matching_gates_on_model_identity() {
        let cell = SharedEmbedder::new();
        let m1 = ModelConfig::e5_base();
        let e1 = Arc::new(Embedder::new_cpu(m1.clone()).unwrap());
        cell.swap(e1);

        // Same model → hit.
        assert!(cell.get_matching(&m1).is_some());
        // Different identity (renamed) → miss, even though files match.
        assert!(cell.get_matching(&renamed(&m1, "not-e5")).is_none());
        // Different real model → miss.
        assert!(cell.get_matching(&ModelConfig::bge_large()).is_none());
    }

    #[test]
    fn swap_replaces_resident() {
        let cell = SharedEmbedder::new();
        let m1 = ModelConfig::e5_base();
        let m2 = ModelConfig::bge_large();
        cell.swap(Arc::new(Embedder::new_cpu(m1.clone()).unwrap()));
        assert!(cell.get_matching(&m1).is_some());
        cell.swap(Arc::new(Embedder::new_cpu(m2.clone()).unwrap()));
        // The old model is gone; the new one is resident.
        assert!(cell.get_matching(&m1).is_none());
        assert!(cell.get_matching(&m2).is_some());
    }

    #[test]
    fn set_if_empty_only_installs_once() {
        let cell = SharedEmbedder::new();
        let m1 = ModelConfig::e5_base();
        assert!(cell.set_if_empty(Arc::new(Embedder::new_cpu(m1.clone()).unwrap())));
        // Second attempt (a different model) must NOT replace the resident.
        assert!(!cell.set_if_empty(Arc::new(
            Embedder::new_cpu(ModelConfig::bge_large()).unwrap()
        )));
        assert!(cell.get_matching(&m1).is_some());
    }

    #[test]
    fn clear_evicts_then_resolve_matching_reinstalls() {
        let cell = SharedEmbedder::new();
        let m1 = ModelConfig::e5_base();
        let m2 = ModelConfig::bge_large();
        // Resident is the OLD model; clear evicts it (the reconcile build-failure
        // path, where model_config already moved to m2).
        cell.swap(Arc::new(Embedder::new_cpu(m1.clone()).unwrap()));
        cell.clear();
        assert!(cell.get().is_none(), "clear must leave the cell empty");
        // With the stale resident gone, resolve_matching for the NEW model now
        // installs (the convergence the eviction guarantees).
        let got = cell.resolve_matching(&m2).unwrap();
        assert_eq!(got.model_config(), &m2);
        assert!(cell.get_matching(&m2).is_some());
    }

    #[test]
    fn resolve_matching_builds_on_empty_and_installs() {
        let cell = SharedEmbedder::new();
        let m1 = ModelConfig::e5_base();
        let got = cell.resolve_matching(&m1).unwrap();
        assert_eq!(got.model_config(), &m1);
        // Installed back into the cell for sharing.
        assert!(cell.get_matching(&m1).is_some());
    }

    #[test]
    fn resolve_matching_does_not_clobber_a_different_model() {
        let cell = SharedEmbedder::new();
        let m1 = ModelConfig::e5_base();
        let m2 = ModelConfig::bge_large();
        cell.swap(Arc::new(Embedder::new_cpu(m1.clone()).unwrap()));

        // Resolving a DIFFERENT model returns a build for it but must leave the
        // resident (m1) untouched — otherwise a lazy reader would clobber a
        // model a concurrent swap just installed.
        let got = cell.resolve_matching(&m2).unwrap();
        assert_eq!(got.model_config(), &m2);
        assert!(
            cell.get_matching(&m1).is_some(),
            "resolve_matching must not overwrite a different resident model"
        );
        assert!(
            cell.get_matching(&m2).is_none(),
            "the private build for the mismatched model must not be installed"
        );
    }

    #[test]
    fn get_or_set_returns_race_winner() {
        let cell = SharedEmbedder::new();
        let m1 = ModelConfig::e5_base();
        let first = Arc::new(Embedder::new_cpu(m1.clone()).unwrap());
        let winner = cell.get_or_set(Arc::clone(&first));
        assert!(Arc::ptr_eq(&winner, &first));
        // A later get_or_set with a different model returns the existing
        // resident, not the new arg.
        let second = Arc::new(Embedder::new_cpu(ModelConfig::bge_large()).unwrap());
        let still = cell.get_or_set(second);
        assert!(Arc::ptr_eq(&still, &first));
    }
}
