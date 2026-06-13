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
