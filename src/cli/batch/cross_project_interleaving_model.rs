//! Loom model of the daemon **cross-project** cache-cell protocol.
//!
//! This is the sibling of [`super::interleaving_model`] (which models the
//! `EpochCell<T> = (u64, Arc<T>)` write-back cells: `hnsw`, `file_set`,
//! `notes_cache`, `splade_index`). It exists because the cross-project cell was
//! the ONE mutable write-back cache that did **not** use the `EpochCell` tag: it
//! stored `Option<CachedCrossProject>` where `CachedCrossProject` carried no
//! publish-epoch tag, and its freshness gate in
//! [`super::view::BatchView::cross_project`] was a **bare counter comparison**:
//!
//! ```ignore
//! let epoch_ok = self.invalidation_epoch.load(SeqCst) == self.checkout_epoch;
//! let fingerprint_ok = cached.fingerprint == current_fingerprint;
//! let dbs_fresh = !cached.ctx.lock().is_stale();
//! if epoch_ok && fingerprint_ok && dbs_fresh { return Ok(cached.ctx); }
//! ```
//!
//! That `epoch_ok` was the SAME bare-counter shape the sibling model's negative
//! control (`checkout_ungated` / `removing_the_gate_reproduces_the_race`) proves
//! is racy: it reads the LIVE shared counter, not a tag baked into the cell
//! value, so a deferred-clear residue that coexists with the current epoch is
//! invisible to it. The sibling model FIXED that race for the tagged cells by
//! storing the epoch IN the value; the cross-project cell now gets the same fix:
//! `CachedCrossProject` carries a `published_epoch` tag and the read gates on it
//! (`published_epoch == checkout_epoch`), mirroring `EpochCell`.
//!
//! ## The two refutations to clear, and how this model clears them
//!
//! A reviewer's instinct is that the cross-project cell has TWO extra guards the
//! tagged cells don't, which might refute the residue race:
//!
//! 1. **`fingerprint`** — refutes only a *config* edit. A local reindex leaves
//!    `references` unchanged, so the residue and a fresh build share a
//!    fingerprint. Modelled as a constant (`FINGERPRINT`): `fingerprint_ok` is
//!    always `true` here, exactly as on the reindex path. Does NOT refute.
//!
//! 2. **`is_stale()`** — stats each store's `index.db` `(mtime, size)`. This
//!    DOES refute the residue race **when the invalidation came from an
//!    `index.db` identity change** (a `cqs index --force` rename-over): the
//!    residue captured the OLD `(mtime, size)` at open, the file changed, so
//!    `is_stale()` returns `true` and the residue is rejected. But it does NOT
//!    refute when the invalidation came from a **WAL `data_version` bump**
//!    (the watch loop's incremental reindex commits to `index.db-wal`, leaving
//!    the main file's `(mtime, size)` untouched until checkpoint —
//!    `invalidate_mutable_caches` fires on `data_version` *without* an identity
//!    change). On that path the residue's captured identity equals the live
//!    identity, so `is_stale()` returns `false`. The model parameterises this as
//!    `stale_visible`: with `stale_visible = false` (the WAL path), `dbs_fresh`
//!    is always `true` and the only thing standing between the reader and the
//!    stale residue is the generation gate — bare counter (racy) or tag (safe).
//!
//! ## The invariant (identical to the sibling, applied to this cell)
//!
//! > A reader (`cross_project()`) must never RECEIVE a `CachedCrossProject`
//! > whose generation differs from the reader's own `checkout_epoch`.
//!
//! Serving a gen-N context against a gen-(N+1) store snapshot mixes generations:
//! the merged call graph folds in the local project's edges, and a reindex
//! reassigns chunk rowids / rebuilds the graph, so a stale cross-project context
//! returns silently-wrong callers/callees/impact/test-map/trace results.
//!
//! ## What this model proves
//!
//! The fix is now in production: the cross-project cell carries a
//! `published_epoch` tag and the read gates on it (`published_epoch ==
//! checkout_epoch`), mirroring `EpochCell`. The model proves both halves:
//!
//! - `wal_residue_schedule_is_coherent_under_tagged_gate` exercises the
//!   PRODUCTION (tagged) gate on the exact WAL-path schedule that reproduced the
//!   finding and PASSES on every interleaving — the durable guard. This is the
//!   `#[should_panic]` reproduction, FLIPPED to an assertion now that the fix
//!   ships.
//! - `tagged_cell_is_coherent_on_wal_path` and
//!   `tagged_cell_coherent_with_second_wave_publisher` cover the tagged gate
//!   under the plain and second-wave-publisher schedules.
//! - `identity_change_path_is_refuted_by_is_stale` documents the refutation that
//!   *does* hold for the (now removed) bare gate (the rename-over path) so the
//!   finding's scope is honest.
//!
//! ## Calibration (reverting the fix must break a test)
//!
//! The bare-counter actors (`*_bare`) are the transcription of the *removed*
//! production gate that DID reproduce the race; they are retained so the fix is
//! falsifiable. To verify the guard bites, degenerate the tagged reader's
//! `tag_ok` (`cached.tag == checkout_epoch`) back into the bare-counter load
//! (`shared.epoch.load() == checkout_epoch`) — equivalently, point
//! `wal_residue_schedule_is_coherent_under_tagged_gate` at the `*_bare` actors —
//! and it FAILS with "STALE CROSS-PROJECT SERVE". The map proved the race; this
//! proves the reduce.
//!
//! ## Running
//!
//! Same harness as the sibling models — binary crate, private `cqs_loom` cfg:
//!
//! ```text
//! RUSTFLAGS="--cfg cqs_loom" CARGO_TARGET_DIR=<private-loom-dir> \
//!     cargo test --features cuda-index --bin cqs cross_project_interleaving_model -- --nocapture
//! ```

use loom::sync::atomic::{AtomicU64, Ordering};
use loom::sync::{Arc, Mutex};

/// The references-config fingerprint. Constant across a reindex (the config is
/// unchanged), so `fingerprint_ok` is always `true` on the path this model
/// targets — `fingerprint` only catches a `.cqs.toml` edit, not a reindex.
const FINGERPRINT: u64 = 0xC0FFEE;

/// A modelled pre-fix `CachedCrossProject`:
/// `{ ctx: Arc<Mutex<CrossProjectContext>>, fingerprint: u64 }` — note the
/// absence of any epoch tag. The `gen` here is the generation the context was
/// BUILT for (the `index.db` generation `from_config` opened). It was NOT stored
/// in the pre-fix cell; it lives here only so the invariant can be checked.
/// The bare-counter gate cannot see it — that is the whole point of the finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cached {
    /// The generation this context's stores were opened at.
    gen: u64,
    fingerprint: u64,
}

/// The tagged variant — the production fix. Mirrors `EpochCell<T> = (u64,
/// Arc<T>)` and the production `CachedCrossProject { ..., published_epoch }`: the
/// publish-epoch rides WITH the value, so a read can prove the value's
/// generation instead of trusting the live counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TaggedCached {
    /// The publish-epoch tag (`published_epoch` == `checkout_epoch` of the
    /// publisher).
    tag: u64,
    /// The generation the context's stores were opened at (== tag for a
    /// self-consistent publish, since the publisher builds from its own
    /// checkout generation's store snapshot).
    gen: u64,
    fingerprint: u64,
}

/// Outer-lock-protected interior — production's `Cell<u16>` pending mask, only
/// ever touched under `Mutex<BatchContext>`. Mirrors the sibling model.
struct Pending {
    mask: u16,
    index_gen: u64,
}

const SLOT_BIT: u16 = 1 << 0;

/// The shared world: the epoch atomic, the cross-project cell, and the
/// outer-lock interior.
struct Shared {
    epoch: AtomicU64,
    /// Pre-fix cell: `Arc<Mutex<Option<CachedCrossProject>>>` without a tag.
    cell: Mutex<Option<Cached>>,
    /// The production (tagged-fix) variant of the cell.
    tagged_cell: Mutex<Option<TaggedCached>>,
    outer: Mutex<Pending>,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: AtomicU64::new(0),
            cell: Mutex::new(None),
            tagged_cell: Mutex::new(None),
            outer: Mutex::new(Pending {
                mask: 0,
                index_gen: 0,
            }),
        })
    }
}

// ─── is_stale() ──────────────────────────────────────────────────────────────

/// `is_stale()` for a residue built at `cached_gen`, given the current disk
/// generation `index_gen` and whether identity-level staleness is *visible*.
///
/// - `stale_visible == true` → identity change path (rename-over): a residue
///   built at an older gen has a DIFFERENT captured `(mtime, size)`, so
///   `is_stale()` returns `true`. This is the refutation that holds.
/// - `stale_visible == false` → WAL `data_version` path: the main `index.db`
///   `(mtime, size)` is UNCHANGED across the generation bump (the commit lives
///   in `index.db-wal`), so `is_stale()` returns `false` even for a stale
///   residue. This is the hole.
fn is_stale(cached_gen: u64, index_gen: u64, stale_visible: bool) -> bool {
    stale_visible && cached_gen != index_gen
}

// ─── The BARE-COUNTER actors (negative control: the *removed* gate) ──────────

/// Reader fast-path, NEGATIVE CONTROL: the transcription of the *removed*
/// bare-counter `BatchView::cross_project()` fast path. The gate is the bare
/// counter. Returns `Some(Cached)` when it would SERVE the cached context (the
/// dangerous outcome if the value is stale), else `None` (rebuild).
/// `stale_visible` selects the invalidation path.
fn cross_project_read_bare(
    shared: &Shared,
    checkout_epoch: u64,
    index_gen_for_is_stale: u64,
    stale_visible: bool,
) -> Option<Cached> {
    let guard = shared.cell.lock().unwrap();
    if let Some(cached) = *guard {
        let epoch_ok = shared.epoch.load(Ordering::SeqCst) == checkout_epoch;
        let fingerprint_ok = cached.fingerprint == FINGERPRINT;
        let dbs_fresh = !is_stale(cached.gen, index_gen_for_is_stale, stale_visible);
        if epoch_ok && fingerprint_ok && dbs_fresh {
            return Some(cached);
        }
    }
    None
}

/// Publisher, NEGATIVE CONTROL: the transcription of the *removed* bare
/// `BatchView::cross_project()` publish-back. One pre-write epoch check under
/// the cell lock, then write. NO post-write re-check, NO epoch tag on the value
/// — the two halves of the sibling fix this cell had been missing.
fn cross_project_publish_bare(shared: &Shared, checkout_epoch: u64, built_gen: u64) {
    let mut guard = shared.cell.lock().unwrap();
    if shared.epoch.load(Ordering::SeqCst) == checkout_epoch {
        *guard = Some(Cached {
            gen: built_gen,
            fingerprint: FINGERPRINT,
        });
    }
}

/// Invalidator (bare-cell): `invalidate_mutable_caches` + `clear_cache_slots`.
/// Bumps the epoch FIRST (before any cell lock), advances the disk generation,
/// then `try_lock`-clears the cross-project cell or records a deferral.
/// Identical shape to the sibling model's `invalidate`.
fn invalidate_bare(shared: &Shared) {
    let mut pending = shared.outer.lock().unwrap();
    shared.epoch.fetch_add(1, Ordering::SeqCst);
    pending.index_gen += 1;
    clear_cross_project_slot_bare(shared, &mut pending, SLOT_BIT);
}

/// `clear_cache_slots(mask)` restricted to the cross-project slot
/// (`try_clear_cell!`). `try_lock`; on failure record the deferral bit (the
/// sticky `pending_invalidation` mask).
fn clear_cross_project_slot_bare(shared: &Shared, pending: &mut Pending, mask: u16) {
    let mut deferred: u16 = 0;
    if mask & SLOT_BIT != 0 {
        match shared.cell.try_lock() {
            Ok(mut g) => *g = None,
            Err(_) => deferred |= SLOT_BIT,
        }
    }
    pending.mask = deferred;
}

/// Checkout (bare-cell): the relevant slice of `build_view` for the
/// cross-project path. Unlike the tagged cells, `build_view` takes NO
/// checkout-time snapshot of the cross-project cell (the view holds only
/// `cross_project_cell` + `checkout_epoch`); the cell is read live by
/// `cross_project()`. So checkout here just runs the sticky retry under the
/// outer lock and captures `checkout_epoch`.
fn checkout_bare(shared: &Shared) -> u64 {
    let mut pending = shared.outer.lock().unwrap();
    let mask = pending.mask;
    if mask != 0 {
        clear_cross_project_slot_bare(shared, &mut pending, mask);
    }
    shared.epoch.load(Ordering::SeqCst)
}

// ─── The PRODUCTION (tagged cell) actors ─────────────────────────────────────

/// PRODUCTION reader (the fix): gate on the value's TAG (`published_epoch`), not
/// the live counter — mirrors `read_cell_if_current`. The `is_stale` /
/// fingerprint guards remain (they catch the ref-update and config-edit paths
/// the epoch can't), but the generation guard is now a TAG comparison, so a
/// residue is caught regardless of the live counter.
fn cross_project_read_tagged(
    shared: &Shared,
    checkout_epoch: u64,
    index_gen_for_is_stale: u64,
    stale_visible: bool,
) -> Option<TaggedCached> {
    let guard = shared.tagged_cell.lock().unwrap();
    if let Some(cached) = *guard {
        let tag_ok = cached.tag == checkout_epoch; // THE FIX
        let fingerprint_ok = cached.fingerprint == FINGERPRINT;
        let dbs_fresh = !is_stale(cached.gen, index_gen_for_is_stale, stale_visible);
        if tag_ok && fingerprint_ok && dbs_fresh {
            return Some(cached);
        }
    }
    None
}

/// PRODUCTION publisher (the fix): tag the value with `checkout_epoch` (so a
/// later read can prove its generation) AND add the `(C)` deferred-clear
/// re-check after the write — the full sibling fix applied to this cell.
fn cross_project_publish_tagged(shared: &Shared, checkout_epoch: u64, built_gen: u64) {
    let mut guard = shared.tagged_cell.lock().unwrap();
    if shared.epoch.load(Ordering::SeqCst) != checkout_epoch {
        return; // (A)
    }
    *guard = Some(TaggedCached {
        tag: checkout_epoch,
        gen: built_gen,
        fingerprint: FINGERPRINT,
    }); // (B) tagged
    if shared.epoch.load(Ordering::SeqCst) != checkout_epoch {
        *guard = None; // (C) deferred-clear re-check
    }
}

fn invalidate_tagged(shared: &Shared) {
    let mut pending = shared.outer.lock().unwrap();
    shared.epoch.fetch_add(1, Ordering::SeqCst);
    pending.index_gen += 1;
    let mut deferred: u16 = 0;
    match shared.tagged_cell.try_lock() {
        Ok(mut g) => *g = None,
        Err(_) => deferred |= SLOT_BIT,
    }
    pending.mask = deferred;
}

fn checkout_tagged(shared: &Shared) -> u64 {
    let mut pending = shared.outer.lock().unwrap();
    let mask = pending.mask;
    if mask != 0 {
        match shared.tagged_cell.try_lock() {
            Ok(mut g) => *g = None,
            Err(_) => pending.mask = SLOT_BIT,
        }
    }
    shared.epoch.load(Ordering::SeqCst)
}

// ─── Loom models ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The safety invariant: a SERVED cross-project context must belong to the
    /// reader's checkout generation. `gen` is the generation the context's
    /// stores were opened at; `checkout_epoch` is the reader's generation. A
    /// mismatch is a stale-generation serve.
    fn assert_served_coherent(served_gen: Option<u64>, checkout_epoch: u64) {
        if let Some(g) = served_gen {
            assert_eq!(
                g, checkout_epoch,
                "STALE CROSS-PROJECT SERVE: served a gen-{g} context to a \
                 gen-{checkout_epoch} reader — the merged call graph belongs to \
                 a superseded index generation; callers/callees/impact/test-map/\
                 trace results are silently wrong"
            );
        }
    }

    /// THE FINDING'S SCHEDULE, now an ASSERTION against the SHIPPED fix
    /// (originally `#[should_panic]` against the bare gate; flipped here because
    /// the production gate is now the tagged one). The gen-0 publisher races the
    /// WAL `data_version` invalidator (`stale_visible = false`, so `is_stale()`
    /// can't see the generation bump) and a gen-1 reader. The schedule is the
    /// one a single-threaded test cannot express: a publisher writing OUTSIDE
    /// the outer lock, an invalidator bumping + deferring (its `try_lock` lost to
    /// the publisher holding the cell), and a reader reading the residue before
    /// the sticky retry mops it up.
    ///
    /// Under the bare-counter gate this served a gen-0 residue to the gen-1
    /// reader: `fingerprint_ok` always true (config unchanged), the bare
    /// `epoch_ok` read the live counter (now 1) == the reader's `checkout_epoch`
    /// (1) → passed → the gen-0 residue was served. Under the PRODUCTION tagged
    /// gate the residue carries the older `tag` (0) and is rejected on read
    /// regardless of the live counter, so the reader never receives a
    /// stale-generation context. PASSES on every interleaving.
    ///
    /// Calibration: degenerating `cross_project_read_tagged`'s `tag_ok` back
    /// into a bare-counter load (the *removed* gate, transcribed in the `*_bare`
    /// actors) makes THIS test and `tagged_cell_is_coherent_on_wal_path` fail
    /// with "STALE CROSS-PROJECT SERVE".
    #[test]
    fn wal_residue_schedule_is_coherent_under_tagged_gate() {
        loom::model(|| {
            let shared = Shared::new();

            // gen-0 publisher (built from the gen-0 store snapshot), OUTSIDE the
            // outer lock — the production publish-back, now tagged.
            let s_pub = Arc::clone(&shared);
            let publisher = loom::thread::spawn(move || {
                cross_project_publish_tagged(&s_pub, 0, 0);
            });

            // Invalidator: WAL data_version bump 0→1, UNDER the outer lock,
            // try_lock-or-defer the cross-project cell.
            let s_inv = Arc::clone(&shared);
            let invalidator = loom::thread::spawn(move || {
                invalidate_tagged(&s_inv);
            });

            // Reader: checkout (captures checkout_epoch under the outer lock,
            // runs the sticky retry), then the tagged cross_project() read. WAL
            // path → stale_visible = false, so the tag is the only thing
            // standing between the reader and a residue.
            let checkout_epoch = checkout_tagged(&shared);
            let served = cross_project_read_tagged(&shared, checkout_epoch, 1, false);

            publisher.join().unwrap();
            invalidator.join().unwrap();

            assert_served_coherent(served.map(|c| c.gen), checkout_epoch);
        });
    }

    /// The refutation that DOES hold, pinned so the finding's scope is honest:
    /// on the identity-change path (`stale_visible = true`, a `cqs index
    /// --force` rename-over), `is_stale()` rejects the gen-0 residue because its
    /// captured `(mtime, size)` differs from the live file. So even the (now
    /// removed) bare counter was safe on THAT path. PASSES — the residue is
    /// never served because `dbs_fresh` is false for it.
    ///
    /// This is why the finding is scoped to the WAL / `data_version`
    /// invalidation path specifically, not "all cross-project reads". Run
    /// against the `*_bare` actors deliberately: it proves the bare gate's only
    /// safe path, the complement of the race the tagged gate closes everywhere.
    #[test]
    fn identity_change_path_is_refuted_by_is_stale() {
        loom::model(|| {
            let shared = Shared::new();

            let s_pub = Arc::clone(&shared);
            let publisher = loom::thread::spawn(move || {
                cross_project_publish_bare(&s_pub, 0, 0);
            });
            let s_inv = Arc::clone(&shared);
            let invalidator = loom::thread::spawn(move || {
                invalidate_bare(&s_inv);
            });

            let checkout_epoch = checkout_bare(&shared);
            // Identity-change path: stale_visible = true, live disk gen = 1. A
            // gen-0 residue is is_stale (0 != 1) → not served.
            let served = cross_project_read_bare(&shared, checkout_epoch, 1, true);

            publisher.join().unwrap();
            invalidator.join().unwrap();

            assert_served_coherent(served.map(|c| c.gen), checkout_epoch);
        });
    }

    /// THE FIX, the plain WAL schedule: tag the cell value with its
    /// publish-epoch (mirroring `EpochCell`) and gate the read on the TAG, plus
    /// the `(C)` post-publish re-check. PASSES on every interleaving even on the
    /// WAL path (`stale_visible = false`) — the residue carries an older tag and
    /// is dropped on read regardless of the live counter or `is_stale()`. This
    /// is the durable regression guard the fix installs.
    #[test]
    fn tagged_cell_is_coherent_on_wal_path() {
        loom::model(|| {
            let shared = Shared::new();

            let s_pub = Arc::clone(&shared);
            let publisher = loom::thread::spawn(move || {
                cross_project_publish_tagged(&s_pub, 0, 0);
            });
            let s_inv = Arc::clone(&shared);
            let invalidator = loom::thread::spawn(move || {
                invalidate_tagged(&s_inv);
            });

            let checkout_epoch = checkout_tagged(&shared);
            let served = cross_project_read_tagged(&shared, checkout_epoch, 1, false);

            publisher.join().unwrap();
            invalidator.join().unwrap();

            assert_served_coherent(served.map(|c| c.gen), checkout_epoch);
        });
    }

    /// The fix also holds with a fresh gen-1 publisher racing the settling
    /// residue (the most adversarial way the residue arises — a second-wave
    /// legitimate publish overlapping the retry). PASSES: the tag comparison
    /// catches the gen-0 residue and admits the gen-1 value only when it matches
    /// the reader's checkout generation.
    #[test]
    fn tagged_cell_coherent_with_second_wave_publisher() {
        loom::model(|| {
            let shared = Shared::new();

            let s_pub0 = Arc::clone(&shared);
            let pub0 = loom::thread::spawn(move || {
                cross_project_publish_tagged(&s_pub0, 0, 0);
            });
            let s_inv = Arc::clone(&shared);
            let inv = loom::thread::spawn(move || {
                invalidate_tagged(&s_inv);
            });
            let s_pub1 = Arc::clone(&shared);
            let pub1 = loom::thread::spawn(move || {
                cross_project_publish_tagged(&s_pub1, 1, 1);
            });

            let checkout_epoch = checkout_tagged(&shared);
            let served = cross_project_read_tagged(&shared, checkout_epoch, 1, false);

            pub0.join().unwrap();
            inv.join().unwrap();
            pub1.join().unwrap();

            assert_served_coherent(served.map(|c| c.gen), checkout_epoch);
        });
    }
}
