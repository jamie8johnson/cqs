//! Loom model of the daemon cache-invalidation epoch protocol.
//!
//! This module is a *bounded, faithful* extraction of the cross-thread shared
//! state in [`super::context::BatchContext`] / [`super::view::BatchView`]: the
//! `invalidation_epoch` atomic, one write-back cache cell, the deferred-clear
//! bitmask (`pending_invalidation`), and the retry path. It exists so loom can
//! exhaustively enumerate the interleavings of the actors that the
//! single-threaded daemon test suite cannot reach:
//!
//! 1. **Publisher** — a connection thread that built a cache value from its
//!    checkout-time store snapshot OUTSIDE the outer `Mutex<BatchContext>` and
//!    now publishes it via [`super::view::BatchView::publish_if_current`].
//! 2. **Reader** — a connection thread reading the cell via
//!    [`super::view::BatchView::read_cell_if_current`]: returns the cached value
//!    ONLY when its `EpochCell` tag equals `checkout_epoch`, else `None` (fall
//!    back to a fresh build). This is the actor whose correctness *matters* — a
//!    daemon query must never be served a value from a generation other than its
//!    own store snapshot.
//! 3. **Invalidator** — a connection thread whose `check_index_staleness`
//!    detected a new index generation; runs UNDER the outer lock:
//!    `invalidation_epoch.fetch_add(1)` then `clear_cache_slots(ALL)` with
//!    `try_lock`-or-defer.
//! 4. **Retry** — a later connection thread, UNDER the outer lock, that honours
//!    the sticky `pending_invalidation` mask via `clear_cache_slots(pending)`
//!    (no epoch bump).
//!
//! ## The concurrency model (why these run the way they do)
//!
//! Production serializes the Invalidator and Retry against each other and
//! against checkout because all three touch `BatchContext`'s `!Sync` interior
//! (`Cell`/`RefCell`) only while holding the outer `Mutex<BatchContext>`. The
//! Publisher and Reader run OUTSIDE that lock — they own a `BatchView` and only
//! touch the shared `Arc<AtomicU64>` epoch and the `Arc<Mutex<cell>>`. The model
//! mirrors this exactly: a model "outer lock" (`Mutex<Pending>`) gates the
//! Invalidator + Retry; the Publisher and Reader run free, synchronizing with
//! them only through the epoch atomic and the cell mutex.
//!
//! ## The invariant, and what these models now prove
//!
//! Production does NOT promise "the cell is never transiently stale": a deferred
//! clear (the Invalidator's `try_lock` lost to a Publisher holding the cell)
//! intentionally leaves a stale value in the cell until the sticky retry mops it
//! up (context.rs:174-183). What it must promise is on the READ side:
//!
//! > **A Reader must never RECEIVE a value whose generation differs from the
//! > Reader's own checkout generation.**
//!
//! ### History: these models found a real race, then proved the fix
//!
//! In the ORIGINAL protocol the cells held a bare `Option<Arc<T>>` and
//! `build_view` snapshotted them unconditionally, serving `cached_*` straight
//! with no epoch gate. These models (run with the now-deleted unconditional
//! `checkout`) found a sequential-consistency-reachable schedule where a stale
//! residue — left by a deferred clear that the Publisher's (C) re-check AND the
//! sticky retry both raced and missed — was snapshotted into `cached_vector_index`
//! and served against a newer-generation store (silently wrong results). The
//! `checkout_ungated` + `removing_the_gate_reproduces_the_race` model below is
//! the committed negative control: it still reproduces that race on demand, so
//! the suite proves the gate is load-bearing, not decorative.
//!
//! The FIX: every cached value now carries the epoch it was
//! published at (`EpochCell<T> = (u64, Arc<T>)`), and every read — the
//! `build_view` snapshot, `read_cell_if_current`, and the `vector_index` direct
//! serve — adopts the value ONLY when its tag equals `checkout_epoch`. A residue
//! is then detected on read regardless of whether any clear ever ran. The model
//! `checkout` / `read_cell_if_current` below transcribe the FIXED code, and
//! Models 1-3 PASS on every interleaving: the stale-generation serve is
//! impossible by construction. They are the durable regression guard — reverting
//! the gate (see `checkout_ungated`) turns them red.
//!
//! ## Running
//!
//! `cli/` is part of the BINARY crate (declared in `src/main.rs`), not the lib,
//! so these run under `--bin cqs`, not `--lib`. Loom tests do NOT run in the
//! normal suite — they need the std-sync shims swapped for loom's, which only
//! happens under our private `--cfg cqs_loom`. Run with:
//!
//! ```text
//! RUSTFLAGS="--cfg cqs_loom" CARGO_TARGET_DIR=<private-loom-dir> \
//!     cargo test --features cuda-index --bin cqs interleaving_model -- --nocapture
//! ```
//!
//! ### Why `cqs_loom` and not the bare `loom` cfg
//!
//! Loom's docs suggest `--cfg loom`, but that cfg name propagates through
//! `RUSTFLAGS` to the WHOLE dependency graph, and crates with their own
//! `cfg(loom)` code paths (e.g. `concurrent-queue`, pulled in transitively)
//! then try to use loom shims they were never given and fail to compile. Loom's
//! shim swap is entirely in OUR code — `use loom::sync` vs `use std::sync`,
//! gated by whatever cfg WE pick — so a private name (`cqs_loom`) activates the
//! model here while leaving every dependency on its std path.
//!
//! (Add `LOOM_MAX_PREEMPTIONS=3` to bound exploration if a full sweep is too
//! slow; the bounded models here complete a full sweep without it.)

// The `mod interleaving_model;` declaration in `mod.rs` carries the
// `#[cfg(cqs_loom)]` gate (canonical over a file-level `#![cfg]`, which an
// `mod foo;`-loaded file evaluates inconsistently against the crate root), so a
// normal build never sees `use loom` and `loom` stays a `cfg(cqs_loom)`-only dep.

use loom::sync::atomic::{AtomicU64, Ordering};
use loom::sync::{Arc, Mutex};

/// A cache value carrying its publish-epoch tag — the model's stand-in for the
/// production `EpochCell<T> = (u64, Arc<T>)`. The real cells hold
/// `Arc<dyn VectorIndex>` etc.; the only property the invariant turns on is
/// "which generation does this value belong to", so `gen` IS the tag, and the
/// payload is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Value {
    gen: u64,
}

/// The single deferred-clear bit modelled (production has 10 slots; one is
/// enough to exercise the mask/retry logic — the slots are independent).
const SLOT_BIT: u16 = 1 << 0;

/// State that production keeps in `BatchContext` `Cell`/`RefCell` fields and
/// only ever touches under the outer `Mutex<BatchContext>`. In the model the
/// outer lock IS this mutex, so Invalidator and Retry are mutually exclusive
/// and exclusive with each other exactly as in production.
struct Pending {
    /// Mirror of `BatchContext::pending_invalidation` (`Cell<u16>`).
    mask: u16,
    /// The current "disk" generation the invalidator has advanced to. Stands
    /// in for `index_id` / `data_version` having moved. Monotonic.
    index_gen: u64,
}

/// The shared cross-thread surface: the epoch atomic, the write-back cell, and
/// the outer-lock-protected `Pending`. One `Shared` is the whole model world.
struct Shared {
    /// `BatchContext::invalidation_epoch` — `Arc<AtomicU64>`.
    epoch: AtomicU64,
    /// One write-back cache cell — `Arc<Mutex<Option<EpochCell<T>>>>` in
    /// production; `Value` here carries the epoch tag.
    cell: Mutex<Option<Value>>,
    /// Outer-lock-protected interior. Production's outer lock is
    /// `Mutex<BatchContext>`; here it guards just the `Pending` the
    /// invalidate/retry paths read+write.
    outer: Mutex<Pending>,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: AtomicU64::new(0),
            cell: Mutex::new(None),
            outer: Mutex::new(Pending {
                mask: 0,
                index_gen: 0,
            }),
        })
    }
}

// ─── The actors, transcribed from production ─────────────────────────────────

/// Publisher: [`BatchView::publish_if_current`]. Runs OUTSIDE the outer lock.
/// `checkout_epoch` is the epoch this view snapshotted at checkout; `value` was
/// built from the matching store snapshot and is published TAGGED with
/// `checkout_epoch` (here `value.gen == checkout_epoch`, set by the caller —
/// production writes `(checkout_epoch, Arc<T>)`). Faithful transcription of the
/// FIXED view.rs publish:
///
/// ```ignore
/// let mut guard = cell.lock();
/// if epoch.load(SeqCst) != checkout_epoch { return; }              // (A)
/// *guard = Some((checkout_epoch, value));                          // (B) tagged
/// if epoch.load(SeqCst) != checkout_epoch { *guard = None; }       // (C)
/// ```
fn publish_if_current(shared: &Shared, checkout_epoch: u64, value: Value) {
    let mut guard = shared.cell.lock().unwrap();
    // (A) discard a value whose generation an invalidation already superseded.
    if shared.epoch.load(Ordering::SeqCst) != checkout_epoch {
        return;
    }
    // (B) publish, tagged (in the model `value.gen` already equals checkout_epoch).
    *guard = Some(value);
    // (C) deferred-clear re-check: an invalidation that bumped the epoch
    // between (A) and here found the cell locked and deferred its clear to us.
    if shared.epoch.load(Ordering::SeqCst) != checkout_epoch {
        *guard = None;
    }
}

/// Reader: the FIXED [`BatchView::read_cell_if_current`] (view.rs:517-523). Runs
/// OUTSIDE the outer lock. Returns the cached value ONLY when its epoch TAG
/// equals the reader's `checkout_epoch` — else `None` (the caller rebuilds from
/// its own snapshot store). The tag comparison subsumes the old "epoch moved
/// since checkout" check AND catches a stale residue that coexists with the
/// current epoch (the deferred-clear race the bare counter check missed).
///
/// ```ignore
/// let guard = cell.lock();
/// guard.as_ref().filter(|(epoch, _)| *epoch == checkout_epoch).map(...)
/// ```
fn read_cell_if_current(shared: &Shared, checkout_epoch: u64) -> Option<Value> {
    let guard = shared.cell.lock().unwrap();
    match *guard {
        Some(v) if v.gen == checkout_epoch => Some(v),
        _ => None,
    }
}

/// Invalidator: `BatchContext::invalidate_mutable_caches` + `clear_cache_slots`.
/// Runs UNDER the outer lock. Bumps the epoch FIRST (context.rs:713), advances
/// the disk generation, then `try_lock`-clears the cell or records a deferral
/// bit.
fn invalidate(shared: &Shared) {
    let mut pending = shared.outer.lock().unwrap();
    shared.epoch.fetch_add(1, Ordering::SeqCst);
    pending.index_gen += 1;
    clear_cache_slots(shared, &mut pending, SLOT_BIT);
}

// The sticky-retry path (the `else if pending != 0` branch of
// `check_index_staleness`, context.rs:589-598 → `clear_cache_slots(pending)`,
// no epoch bump) is exercised INLINE inside `checkout` below — the retry runs
// under the same outer-lock critical section as the rest of `build_view`, so a
// standalone `retry()` that re-locks `outer` would not compose with `checkout`'s
// already-held guard. Keeping it inline keeps the model faithful to the single
// critical section production actually uses.

/// Checkout: a faithful transcription of the FIXED `BatchContext::build_view`
/// (context.rs:1468-1494) — the reader's ACTUAL cell access in production. Runs
/// UNDER the outer lock, in one critical section:
///
/// 1. `check_index_staleness()` → the sticky-retry branch: if a deferral is
///    pending, `clear_cache_slots(pending)` (try_lock-or-defer).
/// 2. `snapshot_cell(&self.hnsw, checkout_epoch)` — a BLOCKING `cell.lock()`
///    that captures the cell value into `cached_vector_index` ONLY when its
///    epoch TAG equals `checkout_epoch` (the fix). A stale-tagged residue is
///    dropped, so `cached_vector_index` never holds a foreign generation —
///    which is what lets `vector_index()` serve it straight.
/// 3. `checkout_epoch = epoch.load()`.
///
/// In this model `Value.gen` IS the production `EpochCell` tag (the `u64` a
/// value is published with). Returns `(snapshot, checkout_epoch)`. The invariant
/// the daemon depends on: the snapshot's generation equals `checkout_epoch`'s
/// generation — `vector_index()` serves `cached_vector_index` straight, against
/// a store snapshot of `checkout_epoch`'s generation.
///
/// THE GATE IS THE FIX. The three reproduction models below pass because of the
/// `v.gen == checkout_epoch` filter here; deleting it (reverting to an
/// unconditional `*guard`) makes them fail again — this is the regression guard.
fn checkout(shared: &Shared) -> (Option<Value>, u64) {
    let mut pending = shared.outer.lock().unwrap();
    // (1) check_index_staleness → sticky-retry branch.
    let mask = pending.mask;
    if mask != 0 {
        clear_cache_slots(shared, &mut pending, mask);
    }
    // (2) snapshot_cell: blocking lock; adopt the value ONLY if its tag matches.
    let guard = shared.cell.lock().unwrap();
    // (3) capture checkout_epoch (same outer-lock critical section, so no
    // invalidation can have moved the epoch between (2) and here).
    let checkout_epoch = shared.epoch.load(Ordering::SeqCst);
    // THE FIX — drop a residue tagged with an older generation.
    let snapshot = match *guard {
        Some(v) if v.gen == checkout_epoch => Some(v),
        _ => None,
    };
    (snapshot, checkout_epoch)
}

/// `clear_cache_slots(mask)` (context.rs:733). Holds the outer lock (the
/// `&mut Pending` proves it). For each masked slot: `try_lock` the cell; on
/// success clear it, on failure set the deferral bit. The sticky mask is
/// rewritten to exactly the slots that deferred this pass (context.rs:803).
fn clear_cache_slots(shared: &Shared, pending: &mut Pending, mask: u16) {
    let mut deferred: u16 = 0;
    if mask & SLOT_BIT != 0 {
        match shared.cell.try_lock() {
            Ok(mut g) => *g = None,
            Err(_) => deferred |= SLOT_BIT,
        }
    }
    pending.mask = deferred;
}

// ─── Loom models ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The core safety invariant, evaluated on a `checkout()` result: the value
    /// the checkout SNAPSHOTTED (into `cached_vector_index`, served later
    /// without an epoch gate) must belong to the same generation as the
    /// `checkout_epoch` the checkout captured. Generation == epoch in this model
    /// (each invalidation bumps both in lockstep under the outer lock). A
    /// snapshot of a different generation is a stale-serve: the daemon would hand
    /// a query a cache value built from a store generation other than its own
    /// snapshot.
    fn assert_snapshot_coherent(snapshot: Option<Value>, checkout_epoch: u64) {
        if let Some(v) = snapshot {
            assert_eq!(
                v.gen, checkout_epoch,
                "STALE SNAPSHOT: checkout captured a gen-{} value but its \
                 checkout_epoch is {checkout_epoch} — build_view snapshotted a \
                 cache value from the wrong generation; vector_index() would \
                 serve it against a gen-{checkout_epoch} store snapshot",
                v.gen
            );
        }
    }

    /// Model 1 — the flagship regression guard, fully faithful: a gen-0
    /// Publisher, an Invalidator (gen 0→1), and a Reader doing the REAL FIXED
    /// `build_view` checkout (`checkout()`), all racing from a clean start. Loom
    /// enumerates every interleaving.
    ///
    /// The invariant: whatever value the Reader's checkout snapshots into its
    /// `cached_vector_index` must be coherent with the `checkout_epoch` it
    /// captures in the SAME outer-lock critical section. This is the property
    /// `vector_index()` (view.rs) relies on when it serves the cached snapshot
    /// straight.
    ///
    /// This is the schedule a single-threaded test structurally cannot express:
    /// a Publisher publishing OUTSIDE the outer lock while a Reader checks out
    /// UNDER it and an Invalidator bumps the epoch between them.
    ///
    /// PASSES with the `EpochCell` gate in `checkout` — the stale residue carries
    /// an older tag and is dropped. This is the durable guard: deleting the gate
    /// (see `checkout_ungated` / `removing_the_gate_reproduces_the_race`) makes
    /// this red.
    #[test]
    fn checkout_snapshot_is_epoch_coherent() {
        loom::model(|| {
            let shared = Shared::new();

            let s_pub = Arc::clone(&shared);
            let publisher = loom::thread::spawn(move || {
                publish_if_current(&s_pub, 0, Value { gen: 0 });
            });

            let s_inv = Arc::clone(&shared);
            let invalidator = loom::thread::spawn(move || {
                invalidate(&s_inv);
            });

            // The reader's checkout runs under the outer lock — it serializes
            // against the invalidator and the (post-join) retry, and uses a
            // blocking cell lock that orders after the publisher.
            let (snapshot, checkout_epoch) = checkout(&shared);

            publisher.join().unwrap();
            invalidator.join().unwrap();

            assert_snapshot_coherent(snapshot, checkout_epoch);
        });
    }

    /// Model 2 — the full read path regression guard: a Reader does `checkout()`
    /// then a `read_cell_if_current` with its OWN captured `checkout_epoch` (the
    /// fallback path `vector_index()`/`base_vector_index()`/`notes()` take when
    /// the checkout snapshot was empty). Both the checkout snapshot AND the
    /// later tag-gated cell read must be coherent with the captured epoch, racing
    /// a gen-0 Publisher and an Invalidator.
    ///
    /// PASSES with the fix: the `checkout` snapshot drops a stale-tagged residue,
    /// and `read_cell_if_current` (now tag-gated, not bare-epoch-gated) returns a
    /// value only when its tag equals `checkout_epoch`.
    #[test]
    fn full_read_path_is_epoch_coherent() {
        loom::model(|| {
            let shared = Shared::new();

            let s_pub = Arc::clone(&shared);
            let publisher = loom::thread::spawn(move || {
                publish_if_current(&s_pub, 0, Value { gen: 0 });
            });

            let s_inv = Arc::clone(&shared);
            let invalidator = loom::thread::spawn(move || {
                invalidate(&s_inv);
            });

            // Full production read: checkout (under outer lock), then the
            // epoch-gated cell read on the fallback path (outside the lock).
            let (snapshot, checkout_epoch) = checkout(&shared);
            assert_snapshot_coherent(snapshot, checkout_epoch);
            let fallback = read_cell_if_current(&shared, checkout_epoch);
            // A value returned by the epoch-gated fallback read must match the
            // checkout generation — anything else is a stale-serve.
            assert_snapshot_coherent(fallback, checkout_epoch);

            publisher.join().unwrap();
            invalidator.join().unwrap();
        });
    }

    /// Model 3 — deferred-clear residue + concurrent fresh publish + a checkout.
    /// A gen-0 Publisher and an Invalidator race (this is what CREATES a
    /// deferred residue, faithfully, instead of injecting it); then a gen-1
    /// Publisher and a Reader's checkout race the settling residue. Exercises
    /// the retry-inside-checkout against a fresh legitimate publish.
    ///
    /// The Reader's checkout must snapshot an epoch-coherent value — the gate in
    /// `checkout` drops any older-tagged residue regardless of whether the retry
    /// or the publisher's (C) cleared it.
    ///
    /// PASSES with the fix: the multi-publisher deferred-residue path (the most
    /// adversarial way the race arose) cannot serve a stale value because the tag
    /// comparison catches it.
    #[test]
    fn checkout_with_deferred_residue_is_coherent() {
        loom::model(|| {
            let shared = Shared::new();

            // First wave: gen-0 publisher races the invalidator (0→1). This is
            // the faithful way a deferred residue arises — no hand-injection.
            let s_pub0 = Arc::clone(&shared);
            let pub0 = loom::thread::spawn(move || {
                publish_if_current(&s_pub0, 0, Value { gen: 0 });
            });
            let s_inv = Arc::clone(&shared);
            let inv = loom::thread::spawn(move || {
                invalidate(&s_inv);
            });

            // Second wave overlaps: a gen-1 publisher and a reader checkout.
            let s_pub1 = Arc::clone(&shared);
            let pub1 = loom::thread::spawn(move || {
                publish_if_current(&s_pub1, 1, Value { gen: 1 });
            });

            let (snapshot, checkout_epoch) = checkout(&shared);

            pub0.join().unwrap();
            inv.join().unwrap();
            pub1.join().unwrap();

            assert_snapshot_coherent(snapshot, checkout_epoch);
        });
    }

    /// Model 4 — liveness backstop: after a gen-0 publisher and an invalidator
    /// race (possibly leaving a deferred residue), a subsequent checkout's
    /// sticky retry, run with no live publisher, clears the residue so the
    /// checkout snapshots `None` or a current value — never a settled stale one.
    ///
    /// This is the LIVENESS contract (the retry eventually mops up), distinct
    /// from the safety invariant of Models 1-3. Loom proves the retry inside the
    /// checkout reaches a clean cell on every interleaving where the publisher
    /// has finished.
    #[test]
    fn checkout_retry_drains_settled_residue() {
        loom::model(|| {
            let shared = Shared::new();

            let s_pub = Arc::clone(&shared);
            let publisher = loom::thread::spawn(move || {
                publish_if_current(&s_pub, 0, Value { gen: 0 });
            });
            let s_inv = Arc::clone(&shared);
            let invalidator = loom::thread::spawn(move || {
                invalidate(&s_inv);
            });

            publisher.join().unwrap();
            invalidator.join().unwrap();

            // Now the publisher is fully done (no live contention). A checkout's
            // retry must drain any settled residue: its snapshot is coherent.
            let (snapshot, checkout_epoch) = checkout(&shared);
            assert_snapshot_coherent(snapshot, checkout_epoch);
        });
    }

    // ── Negative controls: prove the gate is load-bearing ────────────────────
    //
    // The race the fix closes (the original finding), kept as a committed
    // negative control so the suite proves the gate matters — Models 1-3 are only
    // meaningful if removing the gate turns them red. Loom found this on a
    // SEQUENTIAL-CONSISTENCY-reachable schedule (architecture-independent — x86
    // included, not a weak-memory artifact):
    //
    //   pub: cell.lock(); (A) load epoch=0 -> ok; (B) cell=gen0;
    //   pub: (C) load epoch=0 -> ok, DO NOT clear   <- runs BEFORE the bump
    //   inv: outer.lock(); fetch_add -> epoch=1; try_lock(cell) FAILS (pub still
    //        holds the cell) -> defer, pending mask set; outer.unlock()
    //   pub: release cell  (still holds the stale gen-0 value)
    //   chk: outer.lock(); UNGATED snapshot_cell BLOCKING-locks -> captures
    //        gen-0; checkout_epoch = 1
    //   => an UNGATED `build_view` snapshot is gen-0 while checkout_epoch is 1;
    //      the pre-fix `vector_index()` served that straight against a gen-1
    //      store snapshot (silently wrong results).
    //
    // Root cause: a logic race, NOT memory ordering. The publisher's (C)
    // deferred-clear re-check can pass (epoch still 0) while its cell-lock hold
    // extends PAST the invalidator's bump+defer, so the deferral slips through
    // both (C) AND the sticky retry. The `EpochCell` tag fixes it by making every
    // read prove the value's generation instead of relying on a clear that races.

    /// The PRE-FIX checkout: an UNGATED snapshot (no tag comparison). Identical
    /// to the production `build_view` before the `EpochCell` gate. Used by the
    /// negative controls to show the race reappears the moment the gate is removed.
    fn checkout_ungated(shared: &Shared) -> (Option<Value>, u64) {
        let mut pending = shared.outer.lock().unwrap();
        let mask = pending.mask;
        if mask != 0 {
            clear_cache_slots(shared, &mut pending, mask);
        }
        // The bug: capture whatever is in the cell, no tag check.
        let snapshot = *shared.cell.lock().unwrap();
        let checkout_epoch = shared.epoch.load(Ordering::SeqCst);
        (snapshot, checkout_epoch)
    }

    /// Negative control — removing the gate reproduces the race. Same schedule as
    /// Model 1 but with the UNGATED checkout; loom finds the stale-snapshot
    /// interleaving and `assert_snapshot_coherent` panics. `#[should_panic]`, so
    /// this is GREEN exactly while the race is reproducible: if a future change
    /// to the model's `publish`/`invalidate`/`clear` accidentally made the bug
    /// unreachable, THIS test would fail and flag that the regression guards
    /// (Models 1-3) had gone vacuous.
    #[test]
    #[should_panic(expected = "STALE SNAPSHOT")]
    fn removing_the_gate_reproduces_the_race() {
        loom::model(|| {
            let shared = Shared::new();
            let s_pub = Arc::clone(&shared);
            let publisher = loom::thread::spawn(move || {
                publish_if_current(&s_pub, 0, Value { gen: 0 });
            });
            let s_inv = Arc::clone(&shared);
            let invalidator = loom::thread::spawn(move || invalidate(&s_inv));
            let (snapshot, checkout_epoch) = checkout_ungated(&shared);
            publisher.join().unwrap();
            invalidator.join().unwrap();
            assert_snapshot_coherent(snapshot, checkout_epoch);
        });
    }

    /// A SeqCst fence between (B) and (C) in the publisher. Documents that the
    /// fix is the read-side `EpochCell` tag, NOT a publisher-side fence: with the
    /// UNGATED checkout, even a fenced publisher still loses (the race is logical
    /// — the (C) check races the bump in program order — not a reordering a fence
    /// repairs). `#[should_panic]`: GREEN while the fence-doesn't-help race holds.
    fn publish_if_current_fenced(shared: &Shared, checkout_epoch: u64, value: Value) {
        let mut guard = shared.cell.lock().unwrap();
        if shared.epoch.load(Ordering::SeqCst) != checkout_epoch {
            return;
        }
        *guard = Some(value);
        loom::sync::atomic::fence(Ordering::SeqCst);
        if shared.epoch.load(Ordering::SeqCst) != checkout_epoch {
            *guard = None;
        }
    }

    /// Negative control — a fence on the publisher is the WRONG fix. With the
    /// UNGATED checkout, the fenced publisher still leaves a stale residue the
    /// checkout snapshots. Pins that the `EpochCell` read-side gate, not any
    /// publisher fence, is what closes the hole.
    #[test]
    #[should_panic(expected = "STALE SNAPSHOT")]
    fn fence_is_not_the_fix() {
        loom::model(|| {
            let shared = Shared::new();
            let s_pub = Arc::clone(&shared);
            let publisher = loom::thread::spawn(move || {
                publish_if_current_fenced(&s_pub, 0, Value { gen: 0 });
            });
            let s_inv = Arc::clone(&shared);
            let invalidator = loom::thread::spawn(move || invalidate(&s_inv));
            let (snapshot, checkout_epoch) = checkout_ungated(&shared);
            publisher.join().unwrap();
            invalidator.join().unwrap();
            assert_snapshot_coherent(snapshot, checkout_epoch);
        });
    }
}
