//! Loom model of the daemon worktree-overlay LRU cross-thread protocol.
//!
//! Sibling of [`super::interleaving_model`] (the epoch/bitmask model). This one
//! is a *bounded, faithful* extraction of the only overlay state that crosses
//! daemon-client threads: the shared overlay LRU
//! (`BatchContext::overlays: Arc<Mutex<LruCache<PathBuf, Arc<OverlayCacheEntry>>>>`,
//! context.rs:203) and its two concurrent mutators —
//!
//! 1. **Resolver** — a daemon-client thread running
//!    [`super::view::get_overlay_via_lru`]: peek the entry **out** of the LRU
//!    under the lock, drop the lock, build (the embedder-dependent cost)
//!    OUTSIDE the lock, then re-take the lock and `put` the freshly built
//!    `Arc<Entry>` back (last-write-wins). The load-outside-lock rebuild
//!    pattern the audit taxonomy flags.
//! 2. **Invalidator** — a thread running
//!    `BatchContext::clear_cache_slots(OVERLAYS)` (context.rs:792): `try_lock`
//!    the overlay LRU and `clear()` it (operator `refresh` / forced
//!    invalidation), or defer to the sticky retry mask if the lock is held.
//!
//! ## Why this is the interleaving-auditor's job, not a unit test's
//!
//! The Part A change (#1858, PR #1900) routes scout / gather / task seed
//! search through this LRU on the daemon path — three more dispatch paths that
//! now call `ctx.overlay()` → `resolve_overlay()` → `get_overlay_via_lru`.
//! Each daemon connection is a fresh `cqs-daemon-client` thread
//! (daemon.rs:243), so **two concurrent overlay queries are two threads
//! racing on this one shared LRU.** A single-threaded test cannot express:
//!
//! - two Resolvers rebuilding the SAME worktree key concurrently (both must
//!   end with a usable overlay; an in-flight query holding a cloned
//!   `Arc<Entry>` must keep its store alive even after the other Resolver's
//!   `put` evicts the LRU's strong ref), and
//! - a Resolver's build-then-`put` racing an Invalidator's `clear()` (the
//!   post-quiesce LRU must be coherent: either the fresh build is present and
//!   matches its own fingerprint, or absent — never a torn/foreign entry).
//!
//! ## The invariants
//!
//! - **I1 — Arc keep-alive under eviction.** A Resolver that cloned an
//!   `Arc<Entry>` out of the LRU keeps reading its OWN entry's payload
//!   correctly for the whole query, regardless of any concurrent `put`
//!   (eviction) or `clear()`. (Rust's `Arc` guarantees no use-after-free
//!   statically; the model asserts the *observed value* through the held Arc
//!   is the one the resolver built/read, never another key's or a freed one.)
//! - **I2 — no wrong-CWD serve.** A Resolver for worktree key `K` only ever
//!   serves an entry whose key is `K`. (Keyed map; different keys never
//!   collide.)
//! - **I3 — invalidation is not "lost into" a torn entry.** After every
//!   interleaving quiesces, each LRU slot that is present holds a
//!   self-consistent entry (its stored `gen` matches its `key`), never a
//!   half-built or cross-key mix. A `clear()` racing a `put` resolves to
//!   last-write-wins — a valid entry or an absent one, never corruption.
//!
//! ## What this proves vs. what it can't
//!
//! Loom enumerates the interleavings of the LRU `Mutex` + the `Arc` clones
//! exhaustively over the bounded model. It PROVES the lock discipline and the
//! Arc-keep-alive shape are race-free. It does NOT model the *content* of a
//! real `build_overlay` (git delta + embed); that is the integration concern
//! the stress harness / slow-tests e2e cover. The model's `build` is a pure
//! function of the key, which is exactly the property the invariants turn on.
//!
//! ## Running
//!
//! Same harness as the sibling model — binary crate, private `cqs_loom` cfg:
//!
//! ```text
//! RUSTFLAGS="--cfg cqs_loom" CARGO_TARGET_DIR=<private-loom-dir> \
//!     cargo test --features cuda-index --bin cqs overlay_interleaving_model -- --nocapture
//! ```

use loom::sync::{Arc, Mutex};

/// A modelled overlay cache entry. Production's `OverlayCacheEntry` holds the
/// built `Arc<WorktreeOverlay>` (which owns its in-memory `Store` by value) +
/// a `Mutex<Instant>` debounce stamp. The only property the invariants turn on
/// is *identity*: which worktree key this entry belongs to, and that the entry
/// is internally consistent (the `gen` was built FOR this `key`). The real
/// store payload is dropped — the `gen` IS the fingerprint stand-in.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    /// Which worktree key this entry was built for. I2 turns on this.
    key: u8,
    /// The "fingerprint generation" the build stamped. A self-consistent entry
    /// has `gen` derived from `key` (here: `gen == key as u64`), so a torn /
    /// cross-key entry is detectable. I1 + I3 turn on this.
    gen: u64,
}

impl Entry {
    /// The faithful, deterministic stand-in for `build_overlay(key)`: a build
    /// for worktree `key` always produces an entry tagged `(key, key)`. Two
    /// concurrent builds of the same key therefore produce IDENTICAL entries —
    /// exactly production's "last-write-wins is idempotent (same delta)"
    /// property (view.rs:353).
    fn build(key: u8) -> Self {
        Entry {
            key,
            gen: key as u64,
        }
    }

    /// Self-consistency check for I1/I3: the entry's `gen` was built for its
    /// own `key`. A torn write or a cross-key mix would break this.
    fn is_consistent(&self) -> bool {
        self.gen == self.key as u64
    }
}

/// A 1-slot "LRU" — production's `LruCache<PathBuf, Arc<Entry>>`. One slot is
/// enough to exercise eviction (a `put` of a different key evicts the
/// resident; a `put` of the same key replaces it) and the `clear()` race; the
/// keys are independent so more slots add states without new shapes. Modelled
/// as `Option<(key, Arc<Entry>)>` so the resident key is observable.
struct Lru {
    slot: Option<(u8, Arc<Entry>)>,
}

impl Lru {
    fn new() -> Self {
        Lru { slot: None }
    }

    /// `LruCache::get(key).map(Arc::clone)` — peek + clone the Arc out under
    /// the caller's lock. Returns `None` on a miss or a different resident key.
    fn get(&self, key: u8) -> Option<Arc<Entry>> {
        match &self.slot {
            Some((k, e)) if *k == key => Some(Arc::clone(e)),
            _ => None,
        }
    }

    /// `LruCache::put(key, entry)` — insert/replace, evicting any other
    /// resident (1-slot model). The evicted `Arc` strong ref is dropped here;
    /// any thread that already cloned it keeps its copy alive (I1).
    fn put(&mut self, key: u8, entry: Arc<Entry>) {
        self.slot = Some((key, entry));
    }

    /// `LruCache::clear()` — drop every entry. Production's
    /// `clear_cache_slots(OVERLAYS)` calls this under a successful `try_lock`.
    fn clear(&mut self) {
        self.slot = None;
    }
}

/// The whole shared world: the overlay LRU behind its `Mutex`, exactly the
/// `Arc<Mutex<LruCache>>` shape every `BatchView` clones in (view.rs:1545).
struct Shared {
    overlays: Mutex<Lru>,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            overlays: Mutex::new(Lru::new()),
        })
    }
}

// ─── The actors, transcribed from production ─────────────────────────────────

/// Resolver — the faithful core of [`super::view::get_overlay_via_lru`] for
/// worktree `key`:
///
/// ```ignore
/// // 1. peek the entry out of the LRU + clone the Arc, drop the lock.
/// let cached = { let mut c = overlays.lock(); c.get(key).map(Arc::clone) };
/// // (debounce / fingerprint re-validation elided — see note below)
/// // 2. build OUTSIDE the lock (the embedder cost).
/// let built = Arc::new(build(key));
/// // 3. re-take the lock and put (last-write-wins).
/// overlays.lock().put(key, built);
/// ```
///
/// The fingerprint-debounce HIT path (return the cloned cached Arc without
/// rebuilding) is modelled as the `cached.is_some()` early branch: when a
/// resident entry for `key` exists, the resolver MAY serve it straight (a
/// cache hit within the debounce window). Either way the entry the resolver
/// ends up serving is captured + asserted for I1/I2.
///
/// Returns the entry this resolver would serve to its query (always for its
/// OWN key). The caller asserts the invariants on it AFTER all threads join,
/// so the held `Arc` outliving any concurrent eviction is exercised.
fn resolve(shared: &Shared, key: u8, force_rebuild: bool) -> Arc<Entry> {
    // 1. Peek + clone out under the lock, then drop it.
    let cached: Option<Arc<Entry>> = {
        let cache = shared.overlays.lock().unwrap();
        cache.get(key)
    };

    // Hit path: a resident entry for our key is reused straight (the
    // within-debounce branch). The held Arc must stay valid + ours even if a
    // concurrent put/clear changes the LRU after we cloned it (I1).
    if let Some(entry) = cached {
        if !force_rebuild {
            return entry;
        }
        // force_rebuild models the "past the debounce, fingerprint changed"
        // arm that falls through to a rebuild even on a hit.
    }

    // 2. Build OUTSIDE the lock — the window a concurrent Resolver or the
    //    Invalidator runs in.
    let built = Arc::new(Entry::build(key));

    // 3. Re-take the lock and put (last-write-wins). A concurrent builder may
    //    have put an identical-fingerprint entry meanwhile; overwriting is
    //    idempotent (same delta) — view.rs:353.
    {
        let mut cache = shared.overlays.lock().unwrap();
        cache.put(key, Arc::clone(&built));
    }
    built
}

/// Invalidator — `BatchContext::clear_cache_slots(OVERLAYS)` (context.rs:792):
/// `try_lock` the overlay LRU and `clear()`, or defer. The model uses a
/// blocking `lock()` rather than `try_lock` because the *deferred* branch is
/// already exhaustively covered by the sibling bitmask model
/// ([`super::interleaving_model`]); here we want the WORST case for the LRU
/// coherence invariant — the clear actually fires and races the Resolver's
/// `put`. Returns nothing; its effect is the cleared (or last-write-wins)
/// slot the post-quiesce assertion inspects.
fn invalidate(shared: &Shared) {
    let mut cache = shared.overlays.lock().unwrap();
    cache.clear();
}

// ─── Models ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert an entry the resolver served honors I1 (consistent payload) and
    /// I2 (it is the resolver's OWN key).
    fn assert_served_ok(entry: &Entry, expected_key: u8) {
        assert_eq!(
            entry.key, expected_key,
            "I2 VIOLATED: resolver for key {expected_key} served an entry for key {} \
             — a wrong-CWD overlay leaked across threads",
            entry.key
        );
        assert!(
            entry.is_consistent(),
            "I1 VIOLATED: served entry {entry:?} is internally torn \
             (gen != f(key)) — a half-built overlay was observed through a live Arc"
        );
    }

    /// Assert the LRU's post-quiesce resident (if any) is self-consistent (I3):
    /// no torn or cross-key entry survives any interleaving.
    fn assert_lru_coherent(shared: &Shared) {
        let cache = shared.overlays.lock().unwrap();
        if let Some((k, e)) = &cache.slot {
            assert_eq!(
                *k, e.key,
                "I3 VIOLATED: LRU slot keyed {k} holds an entry built for key {} \
                 — a cross-key torn write survived",
                e.key
            );
            assert!(
                e.is_consistent(),
                "I3 VIOLATED: surviving LRU entry {e:?} is torn (gen != f(key))"
            );
        }
    }

    /// Model 1 — two Resolvers rebuilding the SAME worktree key concurrently.
    /// The headline Part-A schedule: two daemon connections from the same
    /// worktree race `get_overlay_via_lru` for one key. Loom enumerates every
    /// interleaving of (peek, build, put) ‖ (peek, build, put).
    ///
    /// Invariant: each resolver serves a consistent entry for ITS OWN key
    /// (I1+I2), and the LRU quiesces coherent (I3). The held `Arc`s must stay
    /// valid through the other thread's `put` (eviction of the LRU's strong
    /// ref leaves each resolver's clone alive).
    #[test]
    fn concurrent_same_key_rebuilds_are_coherent() {
        loom::model(|| {
            let shared = Shared::new();

            let s0 = Arc::clone(&shared);
            let r0 = loom::thread::spawn(move || resolve(&s0, 1, true));
            let s1 = Arc::clone(&shared);
            let r1 = loom::thread::spawn(move || resolve(&s1, 1, true));

            let e0 = r0.join().unwrap();
            let e1 = r1.join().unwrap();

            // I1 + I2: each resolver served a consistent entry for key 1, read
            // through an Arc that outlived the other's put.
            assert_served_ok(&e0, 1);
            assert_served_ok(&e1, 1);
            // Same-key idempotent builds => identical entries.
            assert_eq!(*e0, *e1, "idempotent same-key builds must agree");
            // I3.
            assert_lru_coherent(&shared);
        });
    }

    /// Model 2 — two Resolvers rebuilding DIFFERENT worktree keys
    /// concurrently. Two daemon connections from two different worktrees race
    /// the SAME 1-slot LRU: each `put` evicts the other's resident. The
    /// audit's wrong-CWD-serve question (Q3): can a resolver for key A be
    /// served key B's overlay because B's `put` landed between A's peek and
    /// serve?
    ///
    /// Invariant: NO. A resolver only ever returns an entry for its own key
    /// (I2); eviction of A's LRU slot by B's `put` does not change the Arc A
    /// already cloned/built (I1). The LRU quiesces holding exactly one
    /// self-consistent entry (I3).
    #[test]
    fn concurrent_distinct_key_rebuilds_no_cross_serve() {
        loom::model(|| {
            let shared = Shared::new();

            let s0 = Arc::clone(&shared);
            let ra = loom::thread::spawn(move || resolve(&s0, 1, true));
            let s1 = Arc::clone(&shared);
            let rb = loom::thread::spawn(move || resolve(&s1, 2, true));

            let ea = ra.join().unwrap();
            let eb = rb.join().unwrap();

            assert_served_ok(&ea, 1); // resolver A served key 1, never key 2.
            assert_served_ok(&eb, 2); // resolver B served key 2, never key 1.
            assert_lru_coherent(&shared);
        });
    }

    /// Model 3 — a Resolver's build-then-`put` racing an Invalidator's
    /// `clear()`. The operator `refresh` / forced invalidation drops the whole
    /// overlay LRU while a query is mid-rebuild. Loom enumerates (peek, build,
    /// put) ‖ (clear).
    ///
    /// Invariant: the resolver still serves its own consistent entry — its
    /// held/built Arc is independent of the LRU slot the clear wiped (I1+I2).
    /// The post-quiesce LRU is coherent (I3): either the resolver's `put`
    /// landed after the clear (slot holds the fresh build), or the clear
    /// landed after the put (slot empty) — never a torn entry, and never an
    /// invalidation "lost into" a corrupt slot.
    #[test]
    fn rebuild_racing_clear_serves_consistent_overlay() {
        loom::model(|| {
            let shared = Shared::new();

            let s0 = Arc::clone(&shared);
            let resolver = loom::thread::spawn(move || resolve(&s0, 1, true));
            let s1 = Arc::clone(&shared);
            let invalidator = loom::thread::spawn(move || invalidate(&s1));

            let served = resolver.join().unwrap();
            invalidator.join().unwrap();

            // I1 + I2: the served overlay is the resolver's own, intact, even
            // though clear() may have wiped the LRU between its put and now.
            assert_served_ok(&served, 1);
            // I3: whatever survived is coherent.
            assert_lru_coherent(&shared);
        });
    }

    /// Model 4 — a cache-HIT Resolver (within-debounce reuse) racing an
    /// Invalidator that clears the very entry it is reading. Pre-seeds the LRU
    /// with a valid entry, then a Resolver serves it via the hit path
    /// (`force_rebuild = false`) WHILE an Invalidator clears the slot.
    ///
    /// This is the Arc-keep-alive headline (I1): the resolver clones the Arc
    /// out under the lock, drops the lock, and the invalidator then clears the
    /// slot — dropping the LRU's strong ref. The resolver's clone must keep
    /// the entry alive and correct for the rest of its query. (In production
    /// this is the in-flight query holding `Arc<WorktreeOverlay>` whose
    /// `Store` an eviction must not free mid-read.)
    #[test]
    fn cache_hit_arc_outlives_concurrent_clear() {
        loom::model(|| {
            let shared = Shared::new();
            // Pre-seed: a prior build left a valid entry for key 1.
            {
                let mut cache = shared.overlays.lock().unwrap();
                cache.put(1, Arc::new(Entry::build(1)));
            }

            let s0 = Arc::clone(&shared);
            // force_rebuild=false → the resolver takes the hit path: clone the
            // resident Arc out and serve it straight.
            let resolver = loom::thread::spawn(move || resolve(&s0, 1, false));
            let s1 = Arc::clone(&shared);
            let invalidator = loom::thread::spawn(move || invalidate(&s1));

            let served = resolver.join().unwrap();
            invalidator.join().unwrap();

            // I1: the entry read through the cloned Arc is intact + ours, even
            // if clear() dropped the LRU's ref to it first.
            assert_served_ok(&served, 1);
            assert_lru_coherent(&shared);
        });
    }
}
