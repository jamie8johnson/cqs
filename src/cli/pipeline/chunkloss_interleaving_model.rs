//! Loom model of the index-pipeline chunk-loss protocol — the GPU/CPU
//! work-steal race that fed two producers into one `store_stage` consumer and
//! lost chunks non-deterministically on a full-corpus build (#1891, the
//! interleaving-auditor validation lane).
//!
//! ## Why this module exists
//!
//! `run_index_pipeline` (`pipeline/mod.rs`) runs THREE threads over a file:
//!
//!   * a GPU embed stage and a CPU embed stage that BOTH work-steal `parse_rx`
//!     and BOTH produce to a cloned `embed_tx` (`embedding.rs`);
//!   * a single `store_stage` consumer that drains `embed_rx`, accumulates each
//!     file's chunks per-origin, and flushes a file (writing its chunks +
//!     pruning stale rows + stamping its reconcile fingerprint, one fused tx)
//!     the moment that file's fingerprint arrives (`upsert.rs`).
//!
//! A file's fingerprint rides the batch carrying its LAST chunk. The flush
//! PRUNES to the file's live id set. So if a file's chunks are scattered across
//! two embed batches processed by the two stages at different speeds, the
//! fingerprint-bearing batch (the file's "last" chunk) could reach `store_stage`
//! BEFORE the file's earlier chunks — firing a flush+prune on a PARTIAL
//! accumulator, then stranding the late-arriving chunks in a fresh accum that
//! never flushes. That is the confirmed silent, non-deterministic chunk loss.
//!
//! The house unit suite runs single-threaded by construction: it cannot place
//! the two producers' sends in the adversarial order. The lane's deterministic
//! `store_stage_out_of_order_*` tests feed ONE channel in a fixed bad order;
//! they assert the consumer survives that ONE schedule. This module goes
//! further: it runs the actual two-producer race (GPU + CPU) as loom threads,
//! captures the order their messages land on `embed_tx`, drains that order
//! through the real `store_stage` flush logic, and asserts the no-loss invariant
//! after EVERY interleaving loom explores — the structural null the
//! deterministic test cannot reach.
//!
//! ## The fix under validation (two layers)
//!
//!   1. **Parser file-alignment** (`parsing.rs` `file_aligned_take`): a file's
//!      whole contiguous chunk run rides exactly ONE `ParsedBatch`, so a file is
//!      processed by exactly ONE embed stage. Its chunks + fingerprint then
//!      travel together (or, on a GPU-failure split, cached-then-requeued in
//!      FIFO order on the single `embed_tx`). This makes completion
//!      order-independent BY CONSTRUCTION.
//!   2. **store_stage additive-reflush net** (`upsert.rs` `flushed` map): a late
//!      batch for an already-flushed file triggers an ADDITIVE re-flush whose
//!      prune live set is the UNION of prior-committed ids and the new chunks —
//!      so nothing the file produced is pruned away even if a chunk arrives
//!      after the flush.
//!
//! ## The invariant under test
//!
//! > **NO-LOSS**: after the pipeline drains for a file, the store holds EXACTLY
//! > the set of chunk ids the parser produced for that file — none lost, none
//! > duplicated — AND the file is stamped (its fingerprint committed) iff every
//! > one of its chunks committed. There is no interleaving of the two producers
//! > under which a parsed chunk is absent from the store at quiesce.
//!
//! ## What the model abstracts (the bounded shape loom needs)
//!
//! - A file `F` with three chunk ids `{a, b, c}`. `c` is the file's LAST chunk,
//!   so the fingerprint rides `c`'s batch (the production stamping rule).
//! - `embed_tx` / `fail_tx` are modelled as `Mutex<Vec<Msg>>` FIFO queues (the
//!   reconcile-model house style: all shared state behind `Mutex`, no loom mpsc
//!   — whose `Receiver::drop` re-enters the runtime and aborts a model that
//!   panics, masking the real assertion). A `Mutex` push gives the same
//!   release/acquire happens-before a real channel send does, which is exactly
//!   the edge the cached-before-requeued ordering rests on.
//! - The contended thing is the ORDER messages land on `embed_tx`, set by the
//!   two producer threads + the `fail_tx -> fail_rx` handoff. The two producers
//!   are loom threads; loom explores their interleaving. The single `store_stage`
//!   consumer then drains the resulting `embed_tx` order on the main thread (it
//!   is the SOLE consumer — no consumer/consumer race exists to model), so its
//!   logic runs against every producer interleaving loom found.
//! - The GPU-failure split is modelled to the letter of `flush_to_cpu`: the
//!   cached half goes to `embed_tx` with F's fingerprint DROPPED (F has chunks
//!   in `to_embed`), then the requeued half goes to `fail_tx` carrying F's
//!   fingerprint; the CPU stage pops it from `fail_tx` and forwards it to
//!   `embed_tx`. The cached-before-requeued happens-before is therefore a real
//!   lock-ordering edge, not an assumption — loom explores a reordering if one
//!   were possible.
//!
//! ## Running
//!
//! Loom tests do not run in the normal suite (this module is
//! `#![cfg(all(cqs_loom, test))]`, absent from a normal build). Run:
//!
//! ```bash
//! RUSTFLAGS="--cfg cqs_loom" CARGO_TARGET_DIR=<private-loom-dir> \
//!     cargo test --features cuda-index --bin cqs chunkloss_interleaving_model
//! ```
//!
//! All safety models here are GREEN: the protocol upholds NO-LOSS under every
//! interleaving loom explores. The `#[ignore]`d `..._would_lose_a_chunk` model
//! is the negative control — it reverts BOTH fix layers (flush+prune on
//! fingerprint with a partial live set, no additive net) and the FIFO ordering
//! edge, and loom finds the schedule that drops chunk `c`'s earlier siblings,
//! proving the test has teeth.

#![cfg(all(cqs_loom, test))]

use std::collections::HashSet;

use loom::sync::{Arc, Mutex};
use loom::thread;

/// A parsed chunk id. In production this is `{path}:{line}:{byte}:{hash8}`; here
/// a single char stands for each of file F's three chunks.
type ChunkId = char;

/// File F's full parsed chunk set, in parse order. `c` is the file's LAST chunk,
/// so the fingerprint rides `c`'s batch (the production stamping rule).
const F_CHUNKS: [ChunkId; 3] = ['a', 'b', 'c'];
const F_LAST: ChunkId = 'c';

/// A message on `embed_tx` / `fail_tx`, the faithful `EmbeddedBatch` /
/// `ParsedBatch` shape reduced to what the no-loss invariant depends on: the
/// chunk ids it carries and whether it carries F's fingerprint (the completion
/// signal). `has_fp` true ⇔ F's last chunk's batch (or the requeued half that
/// holds the fingerprint across a GPU split).
#[derive(Clone, Debug)]
struct Msg {
    chunks: Vec<ChunkId>,
    has_fp: bool,
}

/// A FIFO queue behind a `Mutex` — the house-style stand-in for a crossbeam
/// channel. A push under the lock and a later pop under the lock are ordered by
/// the lock's release/acquire, the same happens-before a channel send/recv
/// gives. Single-consumer (matches `embed_rx` / `fail_rx`).
#[derive(Default)]
struct Queue {
    items: Vec<Msg>,
}

impl Queue {
    fn push(q: &Mutex<Queue>, msg: Msg) {
        q.lock().unwrap().items.push(msg);
    }
    /// Pop the front item if present (FIFO). Returns `None` when empty.
    fn pop(q: &Mutex<Queue>) -> Option<Msg> {
        let mut g = q.lock().unwrap();
        if g.items.is_empty() {
            None
        } else {
            Some(g.items.remove(0))
        }
    }
    fn drain(q: &Mutex<Queue>) -> Vec<Msg> {
        std::mem::take(&mut q.lock().unwrap().items)
    }
}

/// The committed store state for file F. `committed` is the set of chunk ids
/// written; `stamped` is whether F's fingerprint was committed.
#[derive(Default)]
struct Store {
    committed: HashSet<ChunkId>,
    stamped: bool,
}

/// The store_stage consumer's per-run scratch: the in-flight accumulator (chunks
/// arrived for F but not yet flushed) and the "flushed" net (F's fingerprint
/// arrived once, with the cumulative committed id set — the additive-reflush
/// safety net).
#[derive(Default)]
struct Consumer {
    accum: Vec<ChunkId>,
    /// `Some(prior_ids)` once F has been flushed at least once this run. Mirrors
    /// `store_stage`'s `flushed: HashMap<PathBuf, (fp, HashSet<id>)>`.
    flushed: Option<HashSet<ChunkId>>,
}

/// THE FIXED store_stage flush: write the accum's chunks AND prune to the UNION
/// of (this flush's ids) ∪ (prior-committed ids). The union prune is the
/// additive net — it never deletes a chunk an earlier (out-of-order) flush
/// committed. Faithful to `flush_file`'s `live_ids = own_ids ∪ extra_live_ids`.
fn flush_fixed(store: &mut Store, consumer: &mut Consumer, stamp: bool) {
    let own: HashSet<ChunkId> = consumer.accum.drain(..).collect();
    let prior = consumer.flushed.clone().unwrap_or_default();
    let live: HashSet<ChunkId> = own.union(&prior).copied().collect();

    // Prune: keep only `live` (delete anything for F not in the union).
    store.committed.retain(|id| live.contains(id));
    // Write this flush's own chunks.
    store.committed.extend(own.iter().copied());
    if stamp {
        store.stamped = true;
    }

    // Record the cumulative committed set so a later additive flush unions in.
    let entry = consumer.flushed.get_or_insert_with(HashSet::new);
    entry.extend(own);
}

/// The store_stage consumer body (FIXED). Drains the captured `embed_tx` message
/// order, accumulating chunks per the production rules and flushing on the
/// fingerprint signal (plus the additive late-arrival re-flush + residual pass).
fn store_stage_fixed(store: &mut Store, msgs: Vec<Msg>) {
    let mut consumer = Consumer::default();
    for msg in msgs {
        consumer.accum.extend(msg.chunks.iter().copied());
        if msg.has_fp {
            // Fingerprint arrived → flush (complete in the in-order path;
            // out-of-order it may be partial, but the additive net + the
            // late/residual passes catch the rest).
            flush_fixed(store, &mut consumer, true);
        } else if consumer.flushed.is_some() && !consumer.accum.is_empty() {
            // LATE-ARRIVAL additive flush: F already flushed, more chunks
            // arrived. Re-flush additively (no new stamp). Mirrors the per-batch
            // late pass in `store_stage`.
            flush_fixed(store, &mut consumer, false);
        }
    }
    // Residual / end-of-stream additive flush: leftover accum for a file that
    // WAS flushed. Mirrors the belt-and-suspenders residual pass.
    if consumer.flushed.is_some() && !consumer.accum.is_empty() {
        flush_fixed(store, &mut consumer, false);
    }
    // An accum with chunks but NO prior flush is an incomplete file (fingerprint
    // never arrived): left unwritten by design. Under file-alignment the
    // fingerprint always arrives WITH the file's chunks, so this never strands a
    // chunk the parser produced — the models below confirm it.
}

/// THE BUGGY store_stage flush (negative control): prune to ONLY this flush's
/// ids — no union with prior-committed ids, no additive net. This is the
/// pre-fix shape: a fingerprint flush prunes the file to the partial set it can
/// see, deleting earlier-arriving chunks, and a fresh accum that fills AFTER the
/// fingerprint was consumed never flushes.
fn store_stage_buggy(store: &mut Store, msgs: Vec<Msg>) {
    let mut accum: Vec<ChunkId> = Vec::new();
    for msg in msgs {
        accum.extend(msg.chunks.iter().copied());
        if msg.has_fp {
            let own: HashSet<ChunkId> = accum.drain(..).collect();
            // BUG: prune to ONLY own ids — no union with prior commits.
            store.committed.retain(|id| own.contains(id));
            store.committed.extend(own.iter().copied());
            store.stamped = true;
            // BUG: no `flushed` record → a later batch for F is treated as a
            // brand-new file whose accum is never flushed on a non-fp message.
        }
        // No additive late-arrival pass — chunks after the fingerprint sit in a
        // fresh accum that never flushes.
    }
    // No residual additive flush.
}

/// NO-LOSS assertion: the store holds EXACTLY F's parsed chunk set, and F is
/// stamped (it completed). Run after both producers join and the consumer drains.
fn assert_no_loss(store: &Store) {
    for id in F_CHUNKS {
        assert!(
            store.committed.contains(&id),
            "NO-LOSS VIOLATED: chunk {id:?} is absent from the store at quiesce — \
             a producer interleaving stranded or pruned it (committed = {:?})",
            store.committed,
        );
    }
    assert_eq!(
        store.committed.len(),
        F_CHUNKS.len(),
        "NO-LOSS VIOLATED: store holds {} ids, expected {} (duplicate or foreign id): {:?}",
        store.committed.len(),
        F_CHUNKS.len(),
        store.committed,
    );
    assert!(
        store.stamped,
        "NO-LOSS VIOLATED: F completed (all chunks present) but was never stamped — \
         the next run would needlessly re-index it, and a real partial flush would \
         have stamped it AHEAD of its data",
    );
}

// ===========================================================================
// MODEL 1 (safety, GREEN): the GPU-failure split under file-alignment.
//
// File F rides ONE ParsedBatch (file-alignment). The GPU stage grabs it, splits
// it into a cached half {a, b} and a to_embed half {c} (the LAST chunk — so the
// fingerprint would ride c). The GPU embed FAILS, so `flush_to_cpu` runs:
//   * cached half {a, b} -> embed_tx with F's fingerprint DROPPED (F has chunks
//     in to_embed);
//   * requeued half {c}  -> fail_tx carrying F's fingerprint.
// The CPU stage pops {c}+fp from fail_tx, re-embeds, pushes it to embed_tx.
//
// Loom explores every interleaving of the GPU and CPU threads. The load-bearing
// question: can {c}+fp ever land on embed_tx BEFORE {a, b}? It cannot — the lock
// edges (GPU pushes cached to embed_tx THEN to fail_tx; CPU pops fail_tx THEN
// pushes to embed_tx) force cached-before-requeued. The model captures embed_tx's
// final order, drains it through the FIXED store_stage, and asserts NO-LOSS after
// every schedule.
// ===========================================================================
#[test]
fn gpu_failure_split_loses_nothing_under_file_alignment() {
    loom::model(|| {
        let embed_tx = Arc::new(Mutex::new(Queue::default()));
        let fail_tx = Arc::new(Mutex::new(Queue::default()));

        // GPU stage: cached half to embed_tx (fp dropped), requeued to fail_tx.
        let embed_gpu = Arc::clone(&embed_tx);
        let fail_gpu = Arc::clone(&fail_tx);
        let gpu = thread::spawn(move || {
            // cached half {a, b}: F has chunks in to_embed, so its fingerprint is
            // DROPPED here (faithful to flush_to_cpu's cached_fps filter).
            Queue::push(
                &embed_gpu,
                Msg {
                    chunks: vec!['a', 'b'],
                    has_fp: false,
                },
            );
            // requeued half {c}: carries F's fingerprint (lands last).
            Queue::push(
                &fail_gpu,
                Msg {
                    chunks: vec![F_LAST],
                    has_fp: true,
                },
            );
        });

        // CPU stage: pop fail_tx, forward to embed_tx. The pop happens-after the
        // GPU's push to fail_tx; the GPU's push to embed_tx happens-before its
        // push to fail_tx; so the forwarded msg's embed_tx push is ordered AFTER
        // the cached half's. (If fail_tx is momentarily empty the CPU yields and
        // retries — loom bounds this since the GPU push is the only producer.)
        let embed_cpu = Arc::clone(&embed_tx);
        let fail_cpu = Arc::clone(&fail_tx);
        let cpu = thread::spawn(move || loop {
            match Queue::pop(&fail_cpu) {
                Some(requeued) => {
                    Queue::push(&embed_cpu, requeued);
                    break;
                }
                None => thread::yield_now(),
            }
        });

        gpu.join().unwrap();
        cpu.join().unwrap();

        // store_stage is the SOLE consumer; drain embed_tx's final order through
        // the real flush logic. (No consumer/consumer race exists to model.)
        let msgs = Queue::drain(&embed_tx);
        let mut store = Store::default();
        store_stage_fixed(&mut store, msgs);
        assert_no_loss(&store);
    });
}

// ===========================================================================
// MODEL 2 (safety, GREEN): two producers racing into the shared embed_tx on
// DISTINCT in-order files.
//
// File-alignment guarantees a file rides one stage in order, but the GPU and CPU
// stages run CONCURRENTLY on different files. This models F (all chunks + fp in
// one in-order batch) sent by the GPU stage, racing a wholly independent file
// 'z' sent by the CPU stage, both into the shared embed_tx. The two producers'
// pushes interleave arbitrarily (loom explores both orders), yet F's single
// batch carries its chunks AND fingerprint together, so F flushes atomically
// regardless of where 'z' lands relative to it. Cross-file independence
// (store_stage keys accums by origin) is pinned: 'z' never lands in F's set.
// ===========================================================================
#[test]
fn in_order_file_unaffected_by_concurrent_producer() {
    loom::model(|| {
        let embed_tx = Arc::new(Mutex::new(Queue::default()));

        // GPU stage: F's whole run + fingerprint in ONE batch (file-aligned,
        // in-order — the common production path).
        let embed_gpu = Arc::clone(&embed_tx);
        let gpu = thread::spawn(move || {
            Queue::push(
                &embed_gpu,
                Msg {
                    chunks: F_CHUNKS.to_vec(),
                    has_fp: true,
                },
            );
        });

        // CPU stage: a concurrent, independent batch for a DIFFERENT file ('z').
        let embed_cpu = Arc::clone(&embed_tx);
        let cpu = thread::spawn(move || {
            Queue::push(
                &embed_cpu,
                Msg {
                    chunks: vec!['z'],
                    has_fp: true,
                },
            );
        });

        gpu.join().unwrap();
        cpu.join().unwrap();

        // Drain through a consumer scoped to F (a foreign origin has its own
        // accum/flush path — cross-file independence).
        let msgs = Queue::drain(&embed_tx);
        let mut store = Store::default();
        let mut consumer = Consumer::default();
        for msg in msgs {
            let f_chunks: Vec<ChunkId> = msg
                .chunks
                .iter()
                .copied()
                .filter(|id| F_CHUNKS.contains(id))
                .collect();
            let is_f = !f_chunks.is_empty();
            consumer.accum.extend(f_chunks);
            if msg.has_fp && is_f {
                flush_fixed(&mut store, &mut consumer, true);
            }
        }
        assert_no_loss(&store);
    });
}

// ===========================================================================
// MODEL 3 (safety, GREEN): the ADVERSARIAL out-of-order arrival the additive net
// defends — fingerprint-bearing chunk c lands FIRST, THEN earlier chunks a, b
// arrive late. This is the schedule a FUTURE regression (re-introducing a file
// straddle across the work-steal so the two halves race with NO ordering edge)
// would produce. Two producer threads push the two halves into embed_tx with NO
// causal edge between them, so loom explores BOTH landing orders. The fix's
// additive-reflush net must keep a and b regardless of order.
//
// This is the loom-generalised form of the lane's deterministic
// `store_stage_out_of_order_fingerprint_before_chunks_loses_nothing` test: that
// test pins ONE order; here loom explores the order AND the (single-consumer)
// drain handles both.
// ===========================================================================
#[test]
fn out_of_order_halves_keep_every_chunk_either_order() {
    loom::model(|| {
        let embed_tx = Arc::new(Mutex::new(Queue::default()));

        // Producer 1: the fingerprint-bearing "last chunk" c.
        let e1 = Arc::clone(&embed_tx);
        let p1 = thread::spawn(move || {
            Queue::push(
                &e1,
                Msg {
                    chunks: vec![F_LAST],
                    has_fp: true,
                },
            );
        });
        // Producer 2: the earlier chunks a, b (no fingerprint). NO edge to p1,
        // so loom runs them in either order.
        let e2 = Arc::clone(&embed_tx);
        let p2 = thread::spawn(move || {
            Queue::push(
                &e2,
                Msg {
                    chunks: vec!['a', 'b'],
                    has_fp: false,
                },
            );
        });

        p1.join().unwrap();
        p2.join().unwrap();

        let msgs = Queue::drain(&embed_tx);
        let mut store = Store::default();
        store_stage_fixed(&mut store, msgs);
        assert_no_loss(&store);
    });
}

// ===========================================================================
// MODEL 4 (safety, GREEN): the DUPLICATE-FINGERPRINT hazard a multi-file
// GPU-failure split introduces, and that two individually-correct ops compose
// into.
//
// `flush_to_cpu` (`embedding.rs`) sends the cached half with fingerprints DROPPED
// for files present in `to_embed`, but sends the requeued half with the FULL
// `prepared.file_fingerprints` set. So when a batch packs file F (straddling
// cached+to_embed) AND file G (fully in the cached half), G's fingerprint rides
// BOTH halves: the cached half (kept, since G ∉ to_embed) AND the requeued half
// (the full-set send). G's fingerprint therefore reaches `store_stage` TWICE.
//
//   * Op 1 — `flush_to_cpu` sending the full fp set on the requeued half: correct
//     in isolation (the requeued half is the completion signal for the straddling
//     files, and reusing the full map is the simplest faithful carrier).
//   * Op 2 — `store_stage` flushing-and-pruning on a fingerprint: correct in
//     isolation (a fingerprint means the file is complete; prune drops stale
//     rows).
//
// Their COMPOSITION must not let G's SECOND fingerprint re-prune G to nothing.
// In production the second fp hits the `None` arm (chunk-bearing fingerprint with
// no accumulated chunks — G's accum was consumed by the first flush): it
// re-stamps the in-memory fp WITHOUT a prune. The model reproduces both files,
// both fp arrivals for G, the cached-before-requeued ordering, and asserts BOTH F
// and G survive intact.
//
// The load-bearing safety: the second (empty-accum, already-flushed) flush unions
// `own={}` with G's prior committed ids, so the prune is a no-op. A regression
// that made the `None` arm prune to the (empty) accum would wipe G — this model
// would catch it.
// ===========================================================================
#[test]
fn multi_file_split_duplicate_fingerprint_does_not_reprune() {
    // G's chunks (fully cached). F is the straddling file (a,b cached; c requeued).
    const G_CHUNKS: [ChunkId; 2] = ['x', 'y'];

    loom::model(|| {
        let embed_tx = Arc::new(Mutex::new(Queue::default()));
        let fail_tx = Arc::new(Mutex::new(Queue::default()));

        // GPU stage on a batch packing F (straddle) + G (fully cached), GPU embed
        // fails → flush_to_cpu:
        //   cached half: {a, b} (F) + {x, y} (G); fp KEPT for G (G ∉ to_embed),
        //               DROPPED for F (F ∈ to_embed).
        //   requeued half: {c} (F); fp set = FULL {F, G} (the full-map send).
        let embed_gpu = Arc::clone(&embed_tx);
        let fail_gpu = Arc::clone(&fail_tx);
        let gpu = thread::spawn(move || {
            Queue::push(
                &embed_gpu,
                Msg {
                    // cached half: F's a,b + G's x,y. `has_fp` is F-scoped in the
                    // shared `Msg`; G's fingerprint on this half is derived in the
                    // drain below from the presence of G's chunks (`contains('x')`),
                    // faithful to `cached_fps` keeping G (G ∉ to_embed).
                    chunks: vec!['a', 'b', 'x', 'y'],
                    has_fp: false,
                },
            );
            // requeued half: F's c, fp set = FULL {F, G}.
            Queue::push(
                &fail_gpu,
                Msg {
                    chunks: vec![F_LAST],
                    has_fp: true,
                },
            );
        });

        let embed_cpu = Arc::clone(&embed_tx);
        let fail_cpu = Arc::clone(&fail_tx);
        let cpu = thread::spawn(move || loop {
            match Queue::pop(&fail_cpu) {
                Some(requeued) => {
                    Queue::push(&embed_cpu, requeued);
                    break;
                }
                None => thread::yield_now(),
            }
        });

        gpu.join().unwrap();
        cpu.join().unwrap();

        // Per-file drain, faithful to store_stage's per-origin keying. We model the
        // fingerprint carriage explicitly per the flush_to_cpu split:
        //   - cached half (msg 0): carries G's fingerprint (G fully cached).
        //   - requeued half (msg 1, has_fp): carries F's AND G's fingerprints
        //     (the full-set send) — G's is the DUPLICATE.
        let msgs = Queue::drain(&embed_tx);
        let mut store_f = Store::default();
        let mut store_g = Store::default();
        let mut cons_f = Consumer::default();
        let mut cons_g = Consumer::default();

        for msg in &msgs {
            // Accumulate per origin.
            for &id in &msg.chunks {
                if F_CHUNKS.contains(&id) {
                    cons_f.accum.push(id);
                } else if G_CHUNKS.contains(&id) {
                    cons_g.accum.push(id);
                }
            }
            // Which files' fingerprints does THIS message carry?
            //   cached half (the !has_fp one that holds G's chunks): G's fp.
            //   requeued half (has_fp): F's fp + G's fp (full-set DUPLICATE).
            let carries_g_fp = msg.chunks.contains(&'x') || msg.has_fp;
            let carries_f_fp = msg.has_fp;

            // G flush: if G's accum has chunks → flush; if G already flushed and
            // this is the duplicate fp with empty accum → the None-arm no-op
            // (flush_fixed unions own={} with prior, pruning nothing).
            if carries_g_fp && (!cons_g.accum.is_empty() || cons_g.flushed.is_some()) {
                flush_fixed(&mut store_g, &mut cons_g, true);
            }
            if carries_f_fp {
                flush_fixed(&mut store_f, &mut cons_f, true);
            }
        }

        // BOTH files intact: G survived the duplicate fingerprint (no re-prune),
        // F survived the straddle split.
        assert_no_loss(&store_f);
        // G's own no-loss check (G_CHUNKS, stamped).
        for id in G_CHUNKS {
            assert!(
                store_g.committed.contains(&id),
                "NO-LOSS VIOLATED: G's chunk {id:?} was re-pruned by its DUPLICATE \
                 fingerprint on the requeued half (committed = {:?})",
                store_g.committed,
            );
        }
        assert_eq!(
            store_g.committed.len(),
            G_CHUNKS.len(),
            "NO-LOSS VIOLATED: G holds {} ids, expected {}: {:?}",
            store_g.committed.len(),
            G_CHUNKS.len(),
            store_g.committed,
        );
        assert!(
            store_g.stamped,
            "G must remain stamped after the duplicate fp"
        );
    });
}

// ===========================================================================
// CALL-GRAPH DIMENSION — validating the LATE-LANDING P2 FIX (the additive
// re-flush's wholesale-DELETE call-graph wipe). This fix was committed AFTER the
// chunk-loss models above; it lives in the SAME additive-net region this lane
// audits, so the producer-interleaving validation belongs here too.
//
// `write_function_calls_in_tx` (`store/calls/crud.rs`) does an UNCONDITIONAL
// `DELETE FROM function_calls WHERE file = ?` then inserts the supplied set. So a
// flush that supplies an EMPTY relationship set deletes the file's whole call
// graph and inserts nothing. The additive re-flush (chunk-loss net layer 2)
// originally flushed a fresh accumulator whose `function_calls` were empty
// (relationships ride the first batch, drained into the accum the FIRST flush
// consumed) — so a late additive flush WIPED the call graph the first flush
// wrote: chunks survived (union-prune), the call graph silently vanished.
//
// The fix: the `flushed` state carries the CUMULATIVE relationships, re-supplied
// on EVERY flush, so the wholesale DELETE-then-INSERT reconstructs the graph.
//
// > **CALL-GRAPH-FIDELITY**: after any producer interleaving, the file's
// > committed call graph equals the cumulative call set the parser produced — as a
// > MULTISET. NO-WIPE (never deleted to empty by a later additive flush) and
// > NO-ORPHAN join NO-DUP: each parser edge committed EXACTLY as many times as the
// > parser produced it, no extra rows. The multiset framing matters because
// > `function_calls` has no UNIQUE constraint (`id ... AUTOINCREMENT` + plain
// > `INSERT`): a regression that supplies an edge twice commits two rows, and a
// > set-valued model store would silently collapse that. The `CallStore::edges`
// > `Vec` + per-edge cardinality assertion is the guard that expresses the dup.
// ===========================================================================

/// A call edge `(caller, callee)` — the `function_calls` row reduced to what the
/// fidelity invariant depends on.
type Edge = (char, char);

/// A relationship-aware message: chunk ids + whether it carries the fingerprint
/// + the call edges riding THIS batch (relationships ride the first batch in
/// production; the GPU-split requeued half carries the full set).
#[derive(Clone, Debug)]
struct RelMsg {
    chunks: Vec<ChunkId>,
    has_fp: bool,
    edges: Vec<Edge>,
}

/// A relationship FIFO queue behind a `Mutex` (house-style channel stand-in).
#[derive(Default)]
struct RelQueue {
    items: Vec<RelMsg>,
}

impl RelQueue {
    fn push(q: &Mutex<RelQueue>, msg: RelMsg) {
        q.lock().unwrap().items.push(msg);
    }
    fn pop(q: &Mutex<RelQueue>) -> Option<RelMsg> {
        let mut g = q.lock().unwrap();
        if g.items.is_empty() {
            None
        } else {
            Some(g.items.remove(0))
        }
    }
    fn drain(q: &Mutex<RelQueue>) -> Vec<RelMsg> {
        std::mem::take(&mut q.lock().unwrap().items)
    }
}

/// The store's committed call graph for file F. The wholesale write REPLACES it
/// with the supplied set (DELETE-then-INSERT), so a flush with an empty supplied
/// set leaves it empty.
///
/// `edges` is a **multiset** (`Vec`), NOT a `HashSet`: production `function_calls`
/// is `id INTEGER PRIMARY KEY AUTOINCREMENT` with a plain `INSERT` and no UNIQUE
/// constraint, so supplying an edge twice produces TWO rows. A `HashSet` would
/// collapse that duplicate and the model could not express a duplicate-edge
/// regression — the structural gap this multiset closes. The CALL-GRAPH-FIDELITY
/// cardinality assertion below bites a dup that a set-valued store would hide.
#[derive(Default)]
struct CallStore {
    chunks: HashSet<ChunkId>,
    edges: Vec<Edge>,
    stamped: bool,
}

/// Consumer scratch for the call-graph dimension: chunk accum + the cumulative
/// `flushed` state (committed ids AND cumulative edges — the P2-fix carry-forward).
/// The cumulative edge carrier is a `Vec` multiset for the same row-semantics
/// reason as `CallStore::edges`: a duplicated supplied edge must survive the
/// carry-forward to the wholesale write as two entries, not be silently deduped.
#[derive(Default)]
struct CallConsumer {
    accum: Vec<ChunkId>,
    accum_edges: Vec<Edge>,
    flushed: Option<(HashSet<ChunkId>, Vec<Edge>)>,
}

/// THE FIXED flush: re-supply the CUMULATIVE edge set on every flush (the
/// wholesale write replaces the store's edges with it), and union-prune chunks.
fn call_flush_fixed(store: &mut CallStore, c: &mut CallConsumer, stamp: bool) {
    let own_ids: HashSet<ChunkId> = c.accum.drain(..).collect();
    let new_edges: Vec<Edge> = c.accum_edges.drain(..).collect();

    let (prior_ids, mut cum_edges) = c.flushed.take().unwrap_or_default();
    // Cumulative edges = prior ∪ this batch's edges (the flushed-state merge).
    cum_edges.extend(new_edges.iter().copied());
    let live: HashSet<ChunkId> = own_ids.union(&prior_ids).copied().collect();

    // Wholesale call-graph write: REPLACE the store's edges with the cumulative
    // set (DELETE-then-INSERT). Because cum_edges carries forward, this rebuilds
    // — never wipes.
    store.edges = cum_edges.clone();
    // Union-prune chunks.
    store.chunks.retain(|id| live.contains(id));
    store.chunks.extend(own_ids.iter().copied());
    if stamp {
        store.stamped = true;
    }

    let mut next_ids = prior_ids;
    next_ids.extend(own_ids);
    c.flushed = Some((next_ids, cum_edges));
}

/// THE BUGGY flush (negative control): supply ONLY this batch's edges to the
/// wholesale write (no carry-forward). A late additive flush whose batch carried
/// NO edges therefore supplies an empty set → the DELETE wipes the graph.
fn call_flush_buggy(store: &mut CallStore, c: &mut CallConsumer, stamp: bool) {
    let own_ids: HashSet<ChunkId> = c.accum.drain(..).collect();
    let new_edges: Vec<Edge> = c.accum_edges.drain(..).collect();
    let (prior_ids, _) = c.flushed.take().unwrap_or_default();
    let live: HashSet<ChunkId> = own_ids.union(&prior_ids).copied().collect();

    // BUG: wholesale write with ONLY this batch's edges (no cumulative). When the
    // late additive batch has no edges, this DELETE-then-INSERT wipes the graph.
    store.edges = new_edges;
    store.chunks.retain(|id| live.contains(id));
    store.chunks.extend(own_ids.iter().copied());
    if stamp {
        store.stamped = true;
    }
    let mut next_ids = prior_ids;
    next_ids.extend(own_ids);
    // Note: still records ids (so chunks survive) but DROPS edges — the precise
    // pre-P2-fix shape (chunks ok, call graph wiped).
    c.flushed = Some((next_ids, Vec::new()));
}

/// THE DUPLICATE-EDGE flush (negative control for NO-DUP): the carry-forward is
/// correct (no wipe) but each supplied edge is committed TWICE to the wholesale
/// write — the shape of a future regression that double-`INSERT`s a row (the
/// parser yields an edge twice, or a re-flush re-supplies an already-committed edge
/// without dedup). Production `function_calls` has no UNIQUE constraint, so this
/// commits two rows for one parser edge. A `HashSet`-valued store would collapse
/// the dup and `assert_call_graph_intact` would still pass; the `Vec` store + the
/// per-edge cardinality assertion is what catches it.
fn call_flush_duplicating(store: &mut CallStore, c: &mut CallConsumer, stamp: bool) {
    let own_ids: HashSet<ChunkId> = c.accum.drain(..).collect();
    let new_edges: Vec<Edge> = c.accum_edges.drain(..).collect();

    let (prior_ids, mut cum_edges) = c.flushed.take().unwrap_or_default();
    // BUG: each new edge is appended TWICE (the double-INSERT regression shape).
    for e in &new_edges {
        cum_edges.push(*e);
        cum_edges.push(*e);
    }
    let live: HashSet<ChunkId> = own_ids.union(&prior_ids).copied().collect();

    store.edges = cum_edges.clone();
    store.chunks.retain(|id| live.contains(id));
    store.chunks.extend(own_ids.iter().copied());
    if stamp {
        store.stamped = true;
    }
    let mut next_ids = prior_ids;
    next_ids.extend(own_ids);
    c.flushed = Some((next_ids, cum_edges));
}

/// The expected cumulative call graph the parser produced for F.
const F_EDGES: [Edge; 1] = [('a', 'v')]; // chunk `a` (caller) calls `v` (victim)

fn assert_call_graph_intact(store: &CallStore) {
    // NO-WIPE / NO-ORPHAN: every parser-produced edge present (the contains check
    // the HashSet store already enforced).
    for &e in &F_EDGES {
        assert!(
            store.edges.contains(&e),
            "CALL-GRAPH-FIDELITY VIOLATED (NO-WIPE): edge {e:?} absent — an additive \
             re-flush supplied an empty relationship set and the wholesale DELETE \
             wiped the call graph (edges = {:?})",
            store.edges,
        );
    }
    // NO-DUP / CARDINALITY: the committed call graph equals the parser's cumulative
    // call set as a MULTISET, not just a set. Production `function_calls` has no
    // UNIQUE constraint and a plain `INSERT`, so a regression that supplies an edge
    // twice commits TWO rows. A `HashSet`-valued store would collapse that to one
    // and pass `contains` silently; this `Vec` store + per-edge cardinality check
    // is the guard that BITES the duplicate. Total-length equality alone could be
    // fooled by one missing + one duplicate cancelling out, so assert the count of
    // EACH parser edge matches AND the total carries no foreign/extra rows.
    for &e in &F_EDGES {
        let want = F_EDGES.iter().filter(|&&x| x == e).count();
        let got = store.edges.iter().filter(|&&x| x == e).count();
        assert_eq!(
            got, want,
            "CALL-GRAPH-FIDELITY VIOLATED (NO-DUP): edge {e:?} committed {got}× but the \
             parser produced it {want}× — a duplicate `INSERT` made spurious rows \
             (no UNIQUE constraint on function_calls); edges = {:?}",
            store.edges,
        );
    }
    assert_eq!(
        store.edges.len(),
        F_EDGES.len(),
        "CALL-GRAPH-FIDELITY VIOLATED (CARDINALITY): store holds {} edge rows, the \
         parser produced {} — a duplicate or foreign edge row is present: {:?}",
        store.edges.len(),
        F_EDGES.len(),
        store.edges,
    );
    // Chunks must also survive (the union-prune); a wiped graph with surviving
    // chunks is the exact pre-fix asymmetry.
    assert!(
        store.chunks.contains(&'a'),
        "the caller chunk must survive alongside its edge (chunks = {:?})",
        store.chunks,
    );
}

// MODEL 5 (safety, GREEN): the GPU-failure split carries relationships on the
// requeued half (the full set), with the cached half carrying NONE (faithful to
// flush_to_cpu sending `RelationshipData::default()` on the cached half when
// to_embed is non-empty). The additive interactions must leave F's call graph
// intact. Loom explores the GPU/CPU producer interleaving; the fixed
// carry-forward flush keeps the edge under every schedule.
#[test]
fn gpu_split_relationships_survive_call_graph_intact() {
    loom::model(|| {
        let embed_tx = Arc::new(Mutex::new(RelQueue::default()));
        let fail_tx = Arc::new(Mutex::new(RelQueue::default()));

        // GPU stage (embed fails → flush_to_cpu):
        //   cached half: {a} (the caller chunk), NO fp, NO edges (cached half
        //     sends RelationshipData::default() when to_embed is non-empty).
        //   requeued half: {c} (F's last chunk), fp, FULL edges {(a,v)}.
        let embed_gpu = Arc::clone(&embed_tx);
        let fail_gpu = Arc::clone(&fail_tx);
        let gpu = thread::spawn(move || {
            RelQueue::push(
                &embed_gpu,
                RelMsg {
                    chunks: vec!['a'],
                    has_fp: false,
                    edges: vec![],
                },
            );
            RelQueue::push(
                &fail_gpu,
                RelMsg {
                    chunks: vec![F_LAST],
                    has_fp: true,
                    edges: vec![('a', 'v')],
                },
            );
        });

        let embed_cpu = Arc::clone(&embed_tx);
        let fail_cpu = Arc::clone(&fail_tx);
        let cpu = thread::spawn(move || loop {
            match RelQueue::pop(&fail_cpu) {
                Some(requeued) => {
                    RelQueue::push(&embed_cpu, requeued);
                    break;
                }
                None => thread::yield_now(),
            }
        });

        gpu.join().unwrap();
        cpu.join().unwrap();

        let msgs = RelQueue::drain(&embed_tx);
        let mut store = CallStore::default();
        let mut c = CallConsumer::default();
        for msg in msgs {
            c.accum.extend(msg.chunks.iter().copied());
            c.accum_edges.extend(msg.edges.iter().copied());
            if msg.has_fp {
                call_flush_fixed(&mut store, &mut c, true);
            } else if c.flushed.is_some() && !c.accum.is_empty() {
                call_flush_fixed(&mut store, &mut c, false);
            }
        }
        if c.flushed.is_some() && !c.accum.is_empty() {
            call_flush_fixed(&mut store, &mut c, false);
        }
        assert_call_graph_intact(&store);
    });
}

// MODEL 6 (safety, GREEN): relationships arrive in a SEPARATE batch AFTER the
// file's fingerprint+chunks flushed (the relationship-after-fingerprint
// ordering). Two producers, no edge → loom explores both orders. The fixed
// carry-forward + relationship-only re-flush must write the edge whichever order
// lands, and must NOT prune the already-committed caller chunk.
#[test]
fn relationships_after_flush_write_edge_either_order() {
    loom::model(|| {
        let embed_tx = Arc::new(Mutex::new(RelQueue::default()));

        // Producer 1: fp + the caller chunk {a}, NO edges (file flushes complete).
        let e1 = Arc::clone(&embed_tx);
        let p1 = thread::spawn(move || {
            RelQueue::push(
                &e1,
                RelMsg {
                    chunks: vec!['a'],
                    has_fp: true,
                    edges: vec![],
                },
            );
        });
        // Producer 2: the relationships arrive late, NO new chunks, NO fp.
        let e2 = Arc::clone(&embed_tx);
        let p2 = thread::spawn(move || {
            RelQueue::push(
                &e2,
                RelMsg {
                    chunks: vec![],
                    has_fp: false,
                    edges: vec![('a', 'v')],
                },
            );
        });

        p1.join().unwrap();
        p2.join().unwrap();

        // Faithful consumer: chunk-flush on fp; a NO-chunk NO-fp batch with edges
        // for an already-flushed file is the RELATIONSHIP-ONLY re-flush (keeps the
        // committed chunks via the cumulative live ids; re-supplies cumulative
        // edges). When relationships arrive BEFORE the fingerprint, they buffer in
        // the accum and ride the flush.
        let msgs = RelQueue::drain(&embed_tx);
        let mut store = CallStore::default();
        let mut c = CallConsumer::default();
        for msg in msgs {
            c.accum.extend(msg.chunks.iter().copied());
            c.accum_edges.extend(msg.edges.iter().copied());
            if msg.has_fp {
                call_flush_fixed(&mut store, &mut c, true);
            } else if c.flushed.is_some() {
                // already-flushed file got a no-fp batch → late chunk pass and/or
                // relationship-only re-flush. Either way re-supply cumulative.
                call_flush_fixed(&mut store, &mut c, false);
            }
            // A no-fp batch BEFORE any flush just buffers (accum holds edges).
        }
        if c.flushed.is_some() && (!c.accum.is_empty() || !c.accum_edges.is_empty()) {
            call_flush_fixed(&mut store, &mut c, false);
        }
        assert_call_graph_intact(&store);
    });
}

// NEGATIVE CONTROL (`#[ignore]`, FAILS by design): the pre-P2-fix shape. The
// additive re-flush supplies ONLY the late batch's edges (no carry-forward); a
// late batch with no edges hands an empty set to the wholesale DELETE-then-INSERT
// and the call graph is WIPED. Loom finds the relationship-after-fingerprint
// schedule and the fidelity assert fires.
//
// Run with `--ignored`:
//   RUSTFLAGS="--cfg cqs_loom" cargo test --features cuda-index --bin cqs \
//       call_graph_wiped_by_empty_additive_reflush -- --ignored
#[test]
#[ignore = "negative control: reproduces the call-graph wipe with the carry-forward reverted \
            (additive re-flush supplies an empty relationship set); FAILS by design"]
fn call_graph_wiped_by_empty_additive_reflush() {
    loom::model(|| {
        let embed_tx = Arc::new(Mutex::new(RelQueue::default()));

        // Producer 1: fp + caller chunk {a} + the edge {(a,v)} (the FIRST flush
        // writes the edge).
        let e1 = Arc::clone(&embed_tx);
        let p1 = thread::spawn(move || {
            RelQueue::push(
                &e1,
                RelMsg {
                    chunks: vec!['a'],
                    has_fp: true,
                    edges: vec![('a', 'v')],
                },
            );
        });
        // Producer 2: a LATE additive batch — a straddling tail chunk {c}, NO
        // edges. With the buggy (non-cumulative) flush this re-flush supplies an
        // empty edge set and wipes the graph.
        let e2 = Arc::clone(&embed_tx);
        let p2 = thread::spawn(move || {
            RelQueue::push(
                &e2,
                RelMsg {
                    chunks: vec![F_LAST],
                    has_fp: false,
                    edges: vec![],
                },
            );
        });

        p1.join().unwrap();
        p2.join().unwrap();

        let msgs = RelQueue::drain(&embed_tx);
        let mut store = CallStore::default();
        let mut c = CallConsumer::default();
        for msg in msgs {
            c.accum.extend(msg.chunks.iter().copied());
            c.accum_edges.extend(msg.edges.iter().copied());
            if msg.has_fp {
                call_flush_buggy(&mut store, &mut c, true);
            } else if c.flushed.is_some() && !c.accum.is_empty() {
                // late additive flush with the BUGGY (empty-set) write.
                call_flush_buggy(&mut store, &mut c, false);
            }
        }
        if c.flushed.is_some() && !c.accum.is_empty() {
            call_flush_buggy(&mut store, &mut c, false);
        }
        // FAILS on the schedule where p1 flushes (edge written), then p2's late
        // chunk triggers an additive flush that supplies an empty edge set and the
        // wholesale DELETE wipes the edge.
        assert_call_graph_intact(&store);
    });
}

// NEGATIVE CONTROL for NO-DUP (`#[ignore]`, FAILS by design): the carry-forward is
// intact (NO-WIPE holds — the edge is present), but the flush commits the edge
// TWICE — the double-`INSERT` regression shape `function_calls`'s missing UNIQUE
// constraint permits. This is the EXACT class the old `HashSet`-valued store could
// not express: a set collapses the two rows to one and `contains` passes. The
// `Vec` store + per-edge cardinality assertion fires here.
//
// Calibration (the deliverable's red-without/green-with):
//   * green-with: the dup-free fixed path (`call_flush_fixed`) commits `(a,v)`
//     once → cardinality 1 == F_EDGES → `gpu_split_relationships_survive_*` and
//     `relationships_after_flush_*` PASS.
//   * red-without: this test commits `(a,v)` twice → cardinality 2 != 1 → the
//     NO-DUP assert fires. Run with `--ignored` to observe.
//
//   RUSTFLAGS="--cfg cqs_loom" cargo test --features cuda-index --bin cqs \
//       call_graph_duplicate_edge_makes_spurious_row -- --ignored
#[test]
#[ignore = "negative control: commits an edge twice (the double-INSERT shape function_calls' \
            missing UNIQUE constraint permits); the NO-DUP cardinality assert FAILS by design — \
            the regression class a HashSet-valued store could not express"]
fn call_graph_duplicate_edge_makes_spurious_row() {
    loom::model(|| {
        let embed_tx = Arc::new(Mutex::new(RelQueue::default()));

        // A single in-order batch: F's caller chunk {a}, fp, the edge {(a,v)}.
        // The dup is introduced by the BUGGY (duplicating) flush, NOT by the
        // producer — this isolates the model-store-vocabulary gap from any
        // interleaving (loom still explores the trivial single-producer schedule).
        let e1 = Arc::clone(&embed_tx);
        let p1 = thread::spawn(move || {
            RelQueue::push(
                &e1,
                RelMsg {
                    chunks: vec!['a'],
                    has_fp: true,
                    edges: vec![('a', 'v')],
                },
            );
        });

        p1.join().unwrap();

        let msgs = RelQueue::drain(&embed_tx);
        let mut store = CallStore::default();
        let mut c = CallConsumer::default();
        for msg in msgs {
            c.accum.extend(msg.chunks.iter().copied());
            c.accum_edges.extend(msg.edges.iter().copied());
            if msg.has_fp {
                // BUGGY: commits each supplied edge twice.
                call_flush_duplicating(&mut store, &mut c, true);
            }
        }
        // FAILS: store.edges == [(a,v), (a,v)] — cardinality 2, parser produced 1.
        // The NO-WIPE `contains` check would PASS (the edge IS present); only the
        // NO-DUP cardinality assertion catches the spurious row.
        assert_call_graph_intact(&store);
    });
}

// ===========================================================================
// NEGATIVE CONTROL (`#[ignore]`, FAILS by design): proves the model has teeth.
// Revert BOTH fix layers: (1) the producers straddle F across two batches with
// NO ordering edge so its fingerprint-bearing chunk c can race ahead, and (2)
// the consumer flush-prunes on the fingerprint with ONLY the chunks-so-far (no
// union, no additive net). Loom finds the schedule where c lands first, the
// consumer flushes and prunes the store to {c}, then a,b land in a fresh accum
// that never flushes — a and b are LOST.
//
// Run with `--ignored` to observe the failure:
//   RUSTFLAGS="--cfg cqs_loom" cargo test --features cuda-index --bin cqs \
//       chunkloss_interleaving_model -- --ignored
// ===========================================================================
#[test]
#[ignore = "negative control: reproduces the #1891 chunk-loss schedule with both fix layers \
            reverted (file straddle + non-additive flush); FAILS by design"]
fn straddle_with_nonadditive_flush_would_lose_a_chunk() {
    loom::model(|| {
        let embed_tx = Arc::new(Mutex::new(Queue::default()));

        // Producer A: the fingerprint-bearing LAST chunk c (the straddle's
        // second half), able to race ahead of producer B (no ordering edge).
        let ea = Arc::clone(&embed_tx);
        let pa = thread::spawn(move || {
            Queue::push(
                &ea,
                Msg {
                    chunks: vec![F_LAST],
                    has_fp: true,
                },
            );
        });
        // Producer B: the earlier chunks a, b (no fingerprint), the straddle's
        // first half.
        let eb = Arc::clone(&embed_tx);
        let pb = thread::spawn(move || {
            Queue::push(
                &eb,
                Msg {
                    chunks: vec!['a', 'b'],
                    has_fp: false,
                },
            );
        });

        pa.join().unwrap();
        pb.join().unwrap();

        let msgs = Queue::drain(&embed_tx);
        let mut store = Store::default();
        // BUGGY consumer: non-additive flush, no net.
        store_stage_buggy(&mut store, msgs);
        // FAILS on the schedule where A's {c}+fp is consumed and flushed (prune
        // to {c}) before B's {a, b} arrive — a and b are stranded/pruned.
        assert_no_loss(&store);
    });
}
