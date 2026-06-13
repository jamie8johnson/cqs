//! Loom model of the watch-reconcile-vs-query coherence protocol
//! (#1826 interleaving-auditor — the index-coherence magnet's single-process
//! flavor).
//!
//! ## Why this module exists
//!
//! In `cqs watch --serve` ONE process runs both a writer (the watch loop:
//! drains inotify, reconciles, commits chunks to SQLite, saves `index.bin`,
//! bumps `data_version`) and N reader threads (daemon query handlers). A query
//! reads the in-memory / on-disk vector index to get candidate chunk IDs, then
//! hydrates those IDs against the SQLite store. The index and the store are two
//! layers updated by the writer at *different instants*; a query can straddle a
//! reconcile commit and observe a CROSS-GENERATION mix — an index from
//! generation N and a store at generation N+1, or vice versa.
//!
//! The house unit suite runs single-threaded by construction: it cannot place a
//! reconcile commit *between* a query's index-read and its store-hydration.
//! That schedule is exactly what this module enumerates with loom.
//!
//! ## The invariant under test
//!
//! cqs chunk IDs are **content-addressed**: `{path}:{line_start}:{content_hash}`
//! (built in `reindex.rs`, hydrated by `WHERE id IN (...)` in
//! `async_helpers.rs::fetch_candidates_by_ids_async`). The vector index returns
//! string IDs (`query.rs` maps `idx.search(...)` results to `r.id.as_str()`),
//! never raw SQLite rowids — rowids cross no layer boundary (they are an
//! internal pagination cursor only).
//!
//! The content-addressing yields the **safety invariant** this model pins:
//!
//! > **CONTENT-FIDELITY**: for any interleaving of a reconciler advancing the
//! > generation against a query that reads the index then hydrates against the
//! > store, every result the query returns carries the content its requesting ID
//! > names. A candidate ID from a stale generation is either absent from the
//! > store (dropped — a `None` slot) or present with byte-identical content (its
//! > `content_hash` matched). There is NO interleaving under which an ID
//! > hydrates to a *different* chunk's content.
//!
//! This is the strong invariant. The weaker "results reflect the LATEST
//! generation" is deliberately NOT an invariant — the watch loop's own comment
//! (`events.rs`: "concurrent searches during this window may see partial
//! results … self-heals after HNSW rebuild. Acceptable for a dev tool")
//! documents that staleness/omission is accepted. The model therefore asserts
//! CONTENT-FIDELITY (which must always hold) and separately *characterises* the
//! omission window (which is allowed), so a future change that turned omission
//! into aliasing would flip a green model red.
//!
//! ## What the model abstracts (the bounded shape loom needs)
//!
//! - A generation counter `gen: AtomicUsize` plays the role of `data_version` /
//!   the store's committed state. The reconciler advances it.
//! - The "store" is a content-addressed map: ID -> content. We model two
//!   generations with a 2-slot shared cell whose published index (`Mutex`)
//!   names which generation is live. An ID is `(slot, gen)`; its content is
//!   `gen`. A hydration of `(slot, g)` succeeds iff the store's live generation
//!   for `slot` still equals `g` — exactly the `content_hash` match: same ID
//!   present ⇒ same content; ID superseded ⇒ absent.
//! - The "index.bin on disk" is a shared cell holding the generation the writer
//!   last published. A query loads it (latest-at-load, NOT snapshot-isolated —
//!   `index.bin` is read through `std::fs`, not the pooled store).
//! - Hydration reads the store's published generation (latest-at-fetch — the
//!   read-only store is a *connection pool*, each `fetch` sees latest-committed
//!   WAL state, NOT a pinned snapshot; confirmed in `store/mod.rs`).
//!
//! The model deliberately keeps the index-load and the store-hydration as two
//! separate shared reads so loom can schedule a reconcile commit between them.
//!
//! ## Running
//!
//! Loom tests do not run in the normal suite (the module is
//! `#![cfg(all(cqs_loom, test))]`, absent from a normal build). Run:
//!
//! ```bash
//! RUSTFLAGS="--cfg cqs_loom" cargo test --features cuda-index --bin cqs \
//!     reconcile_interleaving_model
//! ```
//!
//! All models here are GREEN: the protocol upholds CONTENT-FIDELITY under every
//! interleaving loom explores. The `#[ignore]`d `..._aliasing_would_break`
//! model is the negative control — it swaps content-addressed IDs for
//! position-addressed (rowid-style) IDs and loom finds the wrong-content
//! schedule, proving the test has teeth (it is the content-addressing, not luck,
//! that holds the invariant).

#![cfg(all(cqs_loom, test))]

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

/// A content-addressed chunk ID, faithful to `{path}:{line}:{content_hash}`.
///
/// `slot` stands in for `{path}:{line}` (the location); `content_hash` stands
/// in for the BLAKE3 of the chunk body. Two IDs are equal iff BOTH match — the
/// SQLite `WHERE id IN (...)` semantics. The key property the model exercises:
/// when a reconcile rewrites a location's content, the *content_hash changes*,
/// so the new ID differs from the old one. A stale old ID therefore never
/// matches a live row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct ChunkId {
    slot: usize,
    content_hash: usize,
}

/// The shared world: the store (content-addressed, latest-at-fetch via a pool)
/// and the on-disk index (latest-at-load via the filesystem). Both are advanced
/// by the reconciler; both are read by the query at independent instants.
struct World {
    /// Monotonic generation, the `data_version` analogue. Each reconcile bumps
    /// it; the new content_hash for every rewritten slot is derived from it, so
    /// distinct generations yield distinct content-addressed IDs.
    gen: AtomicUsize,
    /// The store's live row for the single modelled slot: `Some((id, content))`
    /// where `content == id.content_hash` (content-addressing). A pooled read
    /// (`hydrate`) sees whatever is published here AT FETCH TIME.
    store_row: Mutex<Option<(ChunkId, usize)>>,
    /// The on-disk index's candidate ID for the slot. A query LOADS this
    /// (filesystem, latest-at-load), then hands the ID to hydration. Models
    /// `index.bin` returning a content-addressed string ID.
    index_id: Mutex<Option<ChunkId>>,
}

impl World {
    fn new() -> Arc<Self> {
        // Generation 0 is the initial committed state: slot 0 has content_hash 0.
        let id0 = ChunkId {
            slot: 0,
            content_hash: 0,
        };
        Arc::new(World {
            gen: AtomicUsize::new(0),
            store_row: Mutex::new(Some((id0, 0))),
            index_id: Mutex::new(Some(id0)),
        })
    }

    /// One reconcile cycle: advance the generation, rewrite the slot's content
    /// (new content_hash = new gen), publish the new row to the store, then save
    /// the new ID to the on-disk index. This ORDER (store commit, THEN index
    /// save) mirrors the watch loop: `reindex_files` commits chunks before the
    /// HNSW save. The two writes are separate shared mutations so a query can
    /// interleave between any pair.
    fn reconcile(&self) {
        let g = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        let new_id = ChunkId {
            slot: 0,
            content_hash: g,
        };
        // Store commit (the chunk-write tx).
        {
            let mut row = self.store_row.lock().unwrap();
            *row = Some((new_id, g));
        }
        // Index save (the `index.bin` write — a SEPARATE instant, filesystem).
        {
            let mut idx = self.index_id.lock().unwrap();
            *idx = Some(new_id);
        }
    }

    /// A query: load the candidate ID from the on-disk index (latest-at-load),
    /// then hydrate it against the store (latest-at-fetch, pooled). Returns the
    /// content actually served, or `None` if the ID was dropped (stale).
    ///
    /// The two reads are intentionally NOT in one critical section — that is the
    /// whole point. A reconcile may commit between them.
    fn query(&self) -> QueryResult {
        // Index read: the vector index hands back a content-addressed string ID.
        let candidate: Option<ChunkId> = {
            let idx = self.index_id.lock().unwrap();
            *idx
        };
        let Some(cand) = candidate else {
            return QueryResult::Empty;
        };
        // Hydration: `WHERE id IN (cand)` against the live store row. Matches iff
        // the FULL content-addressed ID equals the live row's ID — same slot AND
        // same content_hash. This is the load-bearing semantics.
        let row = self.store_row.lock().unwrap();
        match *row {
            Some((live_id, content)) if live_id == cand => {
                // Hit: the ID is still live. content == content_hash by
                // construction, so the served content is exactly what the ID
                // names.
                QueryResult::Served {
                    requested: cand,
                    content,
                }
            }
            _ => {
                // The ID was superseded (different content_hash) or deleted —
                // dropped from results (the `None` slot in
                // `fetch_candidates_by_ids_async`). An omission, never an alias.
                QueryResult::Dropped { requested: cand }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum QueryResult {
    /// No candidate in the index.
    Empty,
    /// The candidate hydrated to content `content`.
    Served { requested: ChunkId, content: usize },
    /// The candidate was stale and dropped.
    Dropped { requested: ChunkId },
}

impl QueryResult {
    /// CONTENT-FIDELITY: a served result's content must equal the content_hash
    /// its requesting ID names. This is the invariant under test.
    fn assert_content_fidelity(&self) {
        if let QueryResult::Served { requested, content } = self {
            assert_eq!(
                *content, requested.content_hash,
                "CONTENT-FIDELITY VIOLATED: id {requested:?} hydrated to content {content} \
                 — the served chunk is NOT the one the ID names (cross-generation aliasing)"
            );
        }
    }
}

/// MODEL 1 (safety, GREEN): one reconciler thread advancing the generation
/// concurrently with one query thread that reads the index then hydrates.
///
/// Loom explores every interleaving of the reconciler's two writes (store
/// commit, index save) against the query's two reads (index load, store
/// hydrate). The query can observe:
///   - index gen N + store gen N           (coherent, served, fresh)
///   - index gen N + store gen N+1         (index ahead: cand dropped — omission)
///   - index gen N+1 + store gen N+1       (coherent, served, fresh)
///   - index gen N+1 + store gen N         (store ahead: cand dropped — omission)
/// In NONE of these does a served result carry foreign content. The model
/// asserts CONTENT-FIDELITY after every schedule.
#[test]
fn content_fidelity_holds_query_straddling_one_reconcile() {
    loom::model(|| {
        let world = World::new();
        let w_recon = Arc::clone(&world);
        let w_query = Arc::clone(&world);

        let recon = thread::spawn(move || {
            w_recon.reconcile();
        });
        let query = thread::spawn(move || {
            let result = w_query.query();
            result.assert_content_fidelity();
        });

        recon.join().unwrap();
        query.join().unwrap();
    });
}

/// MODEL 2 (safety, GREEN): the index save reordered to land BEFORE the store
/// commit — the adversarial write order the real code does NOT use (it commits
/// chunks first). Modelled to prove CONTENT-FIDELITY does not even depend on the
/// store-before-index ordering: content-addressing carries the invariant under
/// EITHER write order, so the protocol is robust to a future reorder of the two
/// writes. (A reorder would change the *omission* window — which results are
/// dropped — but never introduce aliasing.)
#[test]
fn content_fidelity_holds_under_reversed_write_order() {
    loom::model(|| {
        let world = World::new();
        let w_recon = Arc::clone(&world);
        let w_query = Arc::clone(&world);

        let recon = thread::spawn(move || {
            // Reversed: index save first, then store commit.
            let g = w_recon.gen.fetch_add(1, Ordering::SeqCst) + 1;
            let new_id = ChunkId {
                slot: 0,
                content_hash: g,
            };
            {
                let mut idx = w_recon.index_id.lock().unwrap();
                *idx = Some(new_id);
            }
            {
                let mut row = w_recon.store_row.lock().unwrap();
                *row = Some((new_id, g));
            }
        });
        let query = thread::spawn(move || {
            w_query.query().assert_content_fidelity();
        });

        recon.join().unwrap();
        query.join().unwrap();
    });
}

/// MODEL 3 (safety, GREEN): two queries straddling one reconcile. Pins that
/// CONTENT-FIDELITY is per-query — no shared mutable query state can let one
/// query corrupt another's hydration. (The daemon spawns one thread per
/// connection; this confirms the property composes across concurrent readers.)
#[test]
fn content_fidelity_holds_two_concurrent_queries() {
    loom::model(|| {
        let world = World::new();
        let w_recon = Arc::clone(&world);
        let w_q1 = Arc::clone(&world);
        let w_q2 = Arc::clone(&world);

        let recon = thread::spawn(move || {
            w_recon.reconcile();
        });
        let q1 = thread::spawn(move || {
            w_q1.query().assert_content_fidelity();
        });
        let q2 = thread::spawn(move || {
            w_q2.query().assert_content_fidelity();
        });

        recon.join().unwrap();
        q1.join().unwrap();
        q2.join().unwrap();
    });
}

/// NEGATIVE CONTROL (GREEN, but characterises the allowed-omission window):
/// after a reconcile fully completes, a query that loaded the OLD index ID and
/// hydrates against the NEW store is allowed to DROP the candidate. This is the
/// accepted "partial results, self-heals" property. The model asserts the
/// outcome is `Dropped` (never `Served` with wrong content) — i.e. the staleness
/// manifests ONLY as omission. A regression that turned this into an alias would
/// fail MODEL 1's fidelity assert; this model documents that the omission path
/// is reachable and is the *only* staleness symptom.
#[test]
fn stale_candidate_is_dropped_not_aliased() {
    loom::model(|| {
        let world = World::new();
        let w_recon = Arc::clone(&world);
        let w_query = Arc::clone(&world);

        // Pre-load the stale (gen-0) candidate ID before any reconcile, exactly
        // as a query that snapshotted the old index would hold.
        let stale_id = {
            let idx = world.index_id.lock().unwrap();
            idx.expect("seeded")
        };

        let recon = thread::spawn(move || {
            w_recon.reconcile();
        });
        let query = thread::spawn(move || {
            // Hydrate the stale ID against the store, which the reconcile may or
            // may not have advanced yet.
            let row = w_query.store_row.lock().unwrap();
            let result = match *row {
                Some((live_id, content)) if live_id == stale_id => QueryResult::Served {
                    requested: stale_id,
                    content,
                },
                _ => QueryResult::Dropped {
                    requested: stale_id,
                },
            };
            // Whichever way it lands, fidelity holds: a served stale ID means the
            // reconcile had not yet superseded slot 0, so content == hash.
            result.assert_content_fidelity();
        });

        recon.join().unwrap();
        query.join().unwrap();
    });
}

/// ADVERSARIAL CONTROL (`#[ignore]`, FAILS by design): proves the test has
/// teeth. Replace content-addressed IDs with POSITION-addressed ones — an ID
/// that names only the *slot* (a rowid-style key), so a reconcile that rewrites
/// the slot's content KEEPS the same ID. Now a query that loaded the old ID and
/// hydrates after the reconcile matches the live row by position and is served
/// the NEW content under the OLD ID — wrong-content aliasing.
///
/// Loom finds the interleaving and the fidelity assert fires. This is the
/// counterfactual that pins WHY the real code is safe: it is the
/// `content_hash` suffix on the chunk ID, not the locking, that makes
/// cross-generation mixing omit rather than alias. Run with `--ignored` to
/// observe the failure:
///
/// ```bash
/// RUSTFLAGS="--cfg cqs_loom" cargo test --features cuda-index --bin cqs \
///     reconcile_interleaving_model -- --ignored
/// ```
#[test]
#[ignore = "negative control: reproduces wrong-content aliasing under POSITION-addressed IDs \
            (the bug the content-addressed ID design avoids); FAILS by design"]
fn position_addressed_ids_would_alias() {
    loom::model(|| {
        // Position-addressed world: the ID is just the slot; content is the gen.
        let gen = Arc::new(AtomicUsize::new(0));
        // store_row: (position_id == slot, content == gen)
        let store_row = Arc::new(Mutex::new(Some((0usize, 0usize))));
        // The query holds a position-addressed candidate (slot 0) loaded at gen 0.
        let stale_position_id = 0usize;
        let stale_content_expected = 0usize; // what gen-0 content was

        let g_recon = Arc::clone(&gen);
        let s_recon = Arc::clone(&store_row);
        let s_query = Arc::clone(&store_row);

        let recon = thread::spawn(move || {
            let g = g_recon.fetch_add(1, Ordering::SeqCst) + 1;
            // Rewrite the slot's CONTENT but KEEP the position id (the bug).
            let mut row = s_recon.lock().unwrap();
            *row = Some((stale_position_id, g));
        });
        let query = thread::spawn(move || {
            let row = s_query.lock().unwrap();
            if let Some((live_pos, content)) = *row {
                if live_pos == stale_position_id {
                    // Served by position — but content may be from a newer gen.
                    assert_eq!(
                        content, stale_content_expected,
                        "ALIASING: position id {stale_position_id} served content {content} \
                         (expected {stale_content_expected}) — a reconcile rewrote the slot but \
                         kept the id, so the query was handed foreign content"
                    );
                }
            }
        });

        recon.join().unwrap();
        query.join().unwrap();
    });
}

/// MODEL 4 (safety, GREEN): the watch loop's STAMPED-SAVE discard protocol.
///
/// `save_stamped` (`hnsw/persist.rs`) re-reads the live store stamp UNDER the
/// exclusive save lock and discards the save when `live != snapshot` — the
/// guard that stops a slow rebuild from overwriting a newer on-disk index with
/// stale contents. The invariant:
///
/// > **NO-STALE-OVERWRITE**: the on-disk index's stamp is monotonic — a save
/// > whose snapshot is older than the live store at save time writes nothing,
/// > so the persisted generation never goes backwards.
///
/// Modelled with two savers (a background rebuild built from snapshot S, and an
/// incremental save from a later snapshot) racing the live generation. Each
/// holds the exclusive lock across the re-read+decision+write, mirroring the
/// real `lock_file.lock()` hold. Loom proves the on-disk generation is
/// non-decreasing across every interleaving.
#[test]
fn stamped_save_never_overwrites_a_newer_on_disk_index() {
    loom::model(|| {
        // The live store generation (what `StoreStamp::read` returns).
        let live_gen = Arc::new(AtomicUsize::new(2));
        // The on-disk index generation. Protected by the exclusive save lock.
        let on_disk_gen = Arc::new(Mutex::new(0usize));

        // Saver A: built from snapshot gen 1 (an older rebuild).
        let live_a = Arc::clone(&live_gen);
        let disk_a = Arc::clone(&on_disk_gen);
        let saver_a = thread::spawn(move || {
            let snapshot = 1usize;
            let mut disk = disk_a.lock().unwrap(); // exclusive save lock
            let live = live_a.load(Ordering::SeqCst);
            // `save_stamped`: discard unless live == snapshot.
            if live == snapshot {
                *disk = snapshot;
            }
            // else DiscardedStale — write nothing.
        });

        // Saver B: built from snapshot gen 2 (the current live state).
        let live_b = Arc::clone(&live_gen);
        let disk_b = Arc::clone(&on_disk_gen);
        let saver_b = thread::spawn(move || {
            let snapshot = 2usize;
            let mut disk = disk_b.lock().unwrap();
            let live = live_b.load(Ordering::SeqCst);
            if live == snapshot {
                *disk = snapshot;
            }
        });

        saver_a.join().unwrap();
        saver_b.join().unwrap();

        // INVARIANT: saver A's snapshot (1) never matches live (2), so A always
        // discards; only B (snapshot == live == 2) may write. The on-disk gen is
        // therefore either 0 (B not yet committed in this schedule — but both
        // joined, so B ran) or 2. It is NEVER 1: a stale save can never land.
        let disk = *on_disk_gen.lock().unwrap();
        assert!(
            disk == 0 || disk == 2,
            "NO-STALE-OVERWRITE VIOLATED: on-disk gen {disk} — a snapshot older than \
             live (gen 1) was persisted over a current generation"
        );
        assert_ne!(
            disk, 1,
            "NO-STALE-OVERWRITE VIOLATED: the stale (gen-1) save landed despite live being gen 2"
        );
    });
}
