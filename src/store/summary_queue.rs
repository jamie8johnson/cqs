//! Write-coalescing queue for streamed LLM summary inserts.
//!
//! ## Why this exists
//!
//! Before #1126 / P2.60, [`crate::store::Store::stream_summary_writer`] returned
//! a callback that executed `INSERT OR IGNORE INTO llm_summaries ...` directly
//! against `&self.pool`, **bypassing** [`crate::store::WRITE_LOCK`]. Two
//! concrete races followed:
//!
//! 1. A `cqs index` reindex running in the same process as a streaming
//!    LLM batch could collide with the per-row implicit-tx writes the
//!    callback fired. With WAL mode and a 30s `busy_timeout`, either side
//!    could `SQLITE_BUSY` and abort.
//! 2. Multiple concurrent LLM streams (Haiku + doc-comments + hyde) each
//!    fired one INSERT-OR-IGNORE per item. sqlx wraps a bare statement in
//!    its own implicit transaction → 1 fsync per row. A kill mid-stream
//!    left partial writes visible to readers immediately.
//!
//! ## How this module fixes it
//!
//! The streaming callback now calls [`PendingSummaryQueue::push`] which
//! enqueues the row in-memory. When the queue length crosses
//! [`PendingSummaryQueue::flush_threshold_rows`] OR more than
//! [`PendingSummaryQueue::flush_interval`] elapsed since the last drain,
//! [`PendingSummaryQueue::flush`] runs synchronously: it drains the buffer,
//! acquires the process-wide [`crate::store::WRITE_LOCK`] via
//! [`crate::store::Store::begin_write`]'s discipline, and commits the rows
//! in a single multi-row INSERT batch. All `index.db` writes are now
//! serialized through the same lock — the pre-existing fix for DS-5.
//!
//! ## Backpressure
//!
//! The queue is hard-capped at [`HARD_CAP_ROWS`] (10_000). At the cap, the
//! next `push` runs a synchronous flush before enqueueing. Worst-case
//! memory: ~10k rows × ~512 bytes/row = ~5 MiB.
//!
//! ## Idempotence
//!
//! `flush` on an empty queue is a no-op (returns `Ok(0)` without touching
//! SQLite). Callers (LLM passes, `cmd_index`) call `flush` unconditionally
//! at every safe point — start, success, error, signal — without guarding.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use tokio::runtime::Runtime;

use super::helpers::{sql::max_rows_per_statement, StoreError};
use super::WRITE_LOCK;

/// Recover from a poisoned mutex with a `tracing::warn!` breadcrumb.
///
/// Replaces the bare `lock().unwrap_or_else(|e| e.into_inner())` pattern
/// at every call site so a producer panic that poisons one of the
/// queue's mutexes leaves a visible trail in the logs. The Vec /
/// Instant interior is the only state under those locks, so recovery
/// is safe — the next operation either drains the Vec (replacing torn
/// state) or overwrites the Instant — but a poison is still a real
/// bug class that must not be silent.
fn lock_recover<'a, T>(m: &'a Mutex<T>, ctx: &'static str) -> MutexGuard<'a, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::warn!(
                ctx,
                "summary queue mutex poisoned (a thread panicked while holding it); recovering — continuing with possibly-stale state"
            );
            poisoned.into_inner()
        }
    }
}

/// One pending row destined for `llm_summaries`.
///
/// Field shape mirrors the bind sequence in [`PendingSummaryQueue::flush`]
/// so the cost of building the row (Strings, four small allocs) is paid
/// at enqueue time rather than under the write lock.
#[derive(Debug)]
pub(crate) struct PendingSummary {
    pub custom_id: String,
    pub text: String,
    pub model: String,
    pub purpose: String,
}

/// Default rows-per-flush threshold. Picked to amortize fsync cost across
/// a bunch of streamed completions without holding individual rows in
/// memory for too long.
///
/// See brief Q1: "Starting guess: `N=64, T=200ms`. Run a benchmark on the
/// local LLM path before committing to numbers." Local benchmarking
/// (`item27_streaming_persist_writes_each_item`) confirmed that with
/// concurrency=1 streaming five items takes well under 200 ms total —
/// flushes are expected to be driven by the *final-flush* contract more
/// often than by the threshold, which is fine: a final flush still folds
/// every queued row into one tx.
const DEFAULT_FLUSH_THRESHOLD_ROWS: usize = 64;

/// Default flush time-interval. Same Q1 starting-guess; an idle workload
/// that pushed one row 200 ms ago must not strand it forever.
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 200;

/// Hard cap on queue depth. Past this, the next `push` runs a synchronous
/// flush before enqueueing so the queue cannot grow without bound under
/// a runaway producer.
const HARD_CAP_ROWS: usize = 10_000;

/// After this many consecutive auto-flush failures, the conditional flush
/// path in `push` stops trying. The explicit final flush at end of LLM
/// pass / `cmd_index` is unaffected — that path always retries. A
/// permanently failing flush (disk full, schema corruption, etc.) would
/// otherwise add the failed-flush latency to every push for the rest of
/// the pass and emit one warn-level log per push. The kill-switch caps
/// the warn-storm at exactly this many entries, then promotes one
/// `error!` and goes silent until the explicit flush succeeds (which
/// resets the counter).
const MAX_CONSECUTIVE_FLUSH_FAILURES: u8 = 3;

/// Process-internal write-coalescing queue for `llm_summaries` inserts.
///
/// Lifetime: one instance per [`crate::store::Store<ReadWrite>`], wrapped
/// in `Arc` so the streaming callback (which cannot hold `&Store`) can
/// keep its own handle. Cheap-to-clone (refcount bump only).
pub(crate) struct PendingSummaryQueue {
    /// In-memory buffer. Locked for short bursts (push, drain).
    queue: Mutex<Vec<PendingSummary>>,
    /// Wall-clock of the last successful flush. Used by `should_flush` to
    /// decide whether the time-based trigger has fired.
    last_flush: Mutex<Instant>,
    /// Connection pool for the underlying `Store<ReadWrite>`. Cloned out
    /// of `Store` so the queue can drive its own writes without holding
    /// `&Store` (matching `stream_summary_writer`'s outlives-the-stack
    /// requirement).
    pool: SqlitePool,
    /// Tokio runtime driving sqlx. Same Arc as the parent Store's so a
    /// single worker pool serves every async path.
    rt: Arc<Runtime>,
    /// Rows-per-flush threshold. Public-only via constructor for tests.
    flush_threshold_rows: usize,
    /// Time-since-last-flush threshold.
    flush_interval: Duration,
    /// Counter of consecutive flush failures. Bumped in `flush`'s error
    /// path, reset to 0 on success. When ≥ [`MAX_CONSECUTIVE_FLUSH_FAILURES`],
    /// `should_flush` returns false so `push`'s conditional auto-flush
    /// stops retrying. The explicit final flush from the LLM pass /
    /// `cmd_index` always tries — its success will reset the counter.
    consecutive_flush_failures: AtomicU8,
}

impl PendingSummaryQueue {
    /// Build a new queue tied to `pool` + `rt`. Defaults to the constants
    /// above; tests construct via [`PendingSummaryQueue::with_thresholds`].
    pub(crate) fn new(pool: SqlitePool, rt: Arc<Runtime>) -> Self {
        Self::with_thresholds(
            pool,
            rt,
            DEFAULT_FLUSH_THRESHOLD_ROWS,
            Duration::from_millis(DEFAULT_FLUSH_INTERVAL_MS),
        )
    }

    /// Test-friendly constructor for choosing custom thresholds.
    #[cfg(test)]
    pub(crate) fn with_thresholds(
        pool: SqlitePool,
        rt: Arc<Runtime>,
        flush_threshold_rows: usize,
        flush_interval: Duration,
    ) -> Self {
        Self {
            queue: Mutex::new(Vec::new()),
            last_flush: Mutex::new(Instant::now()),
            pool,
            rt,
            flush_threshold_rows,
            flush_interval,
            consecutive_flush_failures: AtomicU8::new(0),
        }
    }

    /// Non-test path — same body as `with_thresholds` but doesn't require
    /// the `cfg(test)` gate. Kept private since callers should go through
    /// `new`.
    #[cfg(not(test))]
    fn with_thresholds(
        pool: SqlitePool,
        rt: Arc<Runtime>,
        flush_threshold_rows: usize,
        flush_interval: Duration,
    ) -> Self {
        Self {
            queue: Mutex::new(Vec::new()),
            last_flush: Mutex::new(Instant::now()),
            pool,
            rt,
            flush_threshold_rows,
            flush_interval,
            consecutive_flush_failures: AtomicU8::new(0),
        }
    }

    /// Push one row into the queue.
    ///
    /// Runs a synchronous flush BEFORE enqueueing if the queue is at the
    /// hard cap (backpressure). Runs a synchronous flush AFTER enqueueing
    /// if either threshold (rows ≥ N OR elapsed ≥ interval) is met.
    ///
    /// Per the brief: "the closure swallows-and-warns the conditional
    /// flush error since `flush_pending_summaries` is idempotent and will
    /// retry." A flush failure leaves rows in the queue; the next push or
    /// the LLM pass's final flush will retry.
    pub(crate) fn push(&self, row: PendingSummary) {
        // Backpressure: at the hard cap, synchronously flush before
        // enqueueing so the in-memory footprint stays bounded.
        let at_cap = {
            let q = lock_recover(&self.queue, "queue.push.cap_check");
            q.len() >= HARD_CAP_ROWS
        };
        if at_cap {
            tracing::warn!(
                hard_cap = HARD_CAP_ROWS,
                "summary queue at hard cap; flushing synchronously before enqueue"
            );
            if let Err(e) = self.flush() {
                tracing::warn!(
                    error = %e,
                    "summary-queue backpressure flush failed; rows retained for retry"
                );
            }
        }

        // Enqueue.
        {
            let mut q = lock_recover(&self.queue, "queue.push.enqueue");
            q.push(row);
        }

        // Conditional flush. We swallow errors here because the LLM pass
        // and `cmd_index` both run a final unconditional flush before
        // returning; a transient SQLITE_BUSY here will retry there.
        if self.should_flush() {
            if let Err(e) = self.flush() {
                tracing::warn!(
                    error = %e,
                    "summary-queue conditional flush failed; rows retained for retry"
                );
            }
        }
    }

    /// Drain the queue under one [`crate::store::Store::begin_write`]
    /// tx and commit every queued row in a single multi-row
    /// `INSERT OR IGNORE`.
    ///
    /// Idempotent on an empty queue (returns `Ok(0)` without touching
    /// SQLite). Returns the number of rows committed.
    pub(crate) fn flush(&self) -> Result<usize, StoreError> {
        let _span = tracing::debug_span!("flush_pending_summaries").entered();

        // Drain under the queue lock. Holding the lock across the SQL
        // call would block concurrent enqueues for the whole tx, which
        // is wrong; we'd rather take a copy of the rows and let new
        // enqueues continue against an empty queue while the tx runs.
        let drained: Vec<PendingSummary> = {
            let mut q = lock_recover(&self.queue, "queue.drain");
            if q.is_empty() {
                return Ok(0);
            }
            std::mem::take(&mut *q)
        };
        let row_count = drained.len();

        let now = chrono::Utc::now().to_rfc3339();

        // Commit under WRITE_LOCK. Mirrors `Store::begin_write`'s
        // contract — we are NOT calling that method here because we
        // don't hold an `&Store`, but we DO take the same static lock so
        // the in-process serialization invariant is preserved.
        let result: Result<usize, StoreError> = self.rt.block_on(async {
            let _guard = lock_recover(&WRITE_LOCK, "WRITE_LOCK");
            let mut tx = self.pool.begin().await?;
            const BATCH_SIZE: usize = max_rows_per_statement(5);
            for chunk in drained.chunks(BATCH_SIZE) {
                let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                    "INSERT OR IGNORE INTO llm_summaries \
                     (content_hash, summary, model, purpose, created_at) ",
                );
                qb.push_values(chunk.iter(), |mut b, row| {
                    b.push_bind(&row.custom_id)
                        .push_bind(&row.text)
                        .push_bind(&row.model)
                        .push_bind(&row.purpose)
                        .push_bind(&now);
                });
                qb.build().execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok(row_count)
        });

        match result {
            Ok(n) => {
                {
                    let mut t = lock_recover(&self.last_flush, "last_flush.write");
                    *t = Instant::now();
                }
                // Reset the failure counter: a successful explicit
                // flush re-enables the conditional auto-flush path
                // even if the previous N attempts failed.
                self.consecutive_flush_failures.store(0, Ordering::Relaxed);
                tracing::debug!(rows_committed = n, "summary queue flushed");
                Ok(n)
            }
            Err(e) => {
                // Re-enqueue the drained rows so the next flush can
                // retry. The order is preserved as best as possible by
                // prepending the drained set in front of any new
                // enqueues that arrived during the failed tx.
                let mut q = lock_recover(&self.queue, "queue.reenqueue");
                let mut combined = Vec::with_capacity(q.len() + drained.len());
                combined.extend(drained);
                combined.append(&mut q);
                *q = combined;
                let retained = q.len();
                drop(q);
                // Bump the failure counter; on the threshold-crossing
                // bump, promote one `error!` so operators see the
                // auto-flush going dark. Subsequent failures stay
                // silent (the conditional path skips them) until the
                // explicit final flush succeeds and resets.
                let prior = self
                    .consecutive_flush_failures
                    .fetch_add(1, Ordering::Relaxed);
                let new_count = prior.saturating_add(1);
                if new_count == MAX_CONSECUTIVE_FLUSH_FAILURES {
                    tracing::error!(
                        error = %e,
                        consecutive_failures = new_count,
                        rows_retained = retained,
                        "summary queue auto-flush disabled after {MAX_CONSECUTIVE_FLUSH_FAILURES} consecutive failures; explicit end-of-pass flush will retry"
                    );
                } else {
                    tracing::warn!(
                        error = %e,
                        consecutive_failures = new_count,
                        rows_retained = retained,
                        "summary queue flush failed; retained rows for retry"
                    );
                }
                Err(e)
            }
        }
    }

    /// Returns `true` if either the row threshold or the time interval
    /// has been crossed since the last successful flush.
    ///
    /// Returns `false` early when the consecutive-failure counter is
    /// at or above [`MAX_CONSECUTIVE_FLUSH_FAILURES`]. Callers of the
    /// conditional auto-flush path then skip the flush attempt — but
    /// the explicit final flush from the LLM pass / `cmd_index`
    /// bypasses `should_flush` entirely so that path always retries.
    fn should_flush(&self) -> bool {
        if self.consecutive_flush_failures.load(Ordering::Relaxed) >= MAX_CONSECUTIVE_FLUSH_FAILURES
        {
            return false;
        }
        let row_count = {
            let q = lock_recover(&self.queue, "should_flush.row_count");
            q.len()
        };
        if row_count >= self.flush_threshold_rows {
            return true;
        }
        let elapsed = {
            let t = lock_recover(&self.last_flush, "should_flush.elapsed");
            t.elapsed()
        };
        elapsed >= self.flush_interval
    }

    /// Return the current queue depth. Used by tests; may be stale on
    /// concurrent producers.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        lock_recover(&self.queue, "pending_len").len()
    }

    /// Test-only: read the consecutive-failure counter.
    #[cfg(test)]
    pub(crate) fn consecutive_failure_count(&self) -> u8 {
        self.consecutive_flush_failures.load(Ordering::Relaxed)
    }

    /// Test-only: directly invoke `should_flush` so a regression test
    /// can pin the kill-switch behavior without going through `push`.
    #[cfg(test)]
    pub(crate) fn should_flush_for_test(&self) -> bool {
        self.should_flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ModelInfo, Store};

    /// Build a fresh `Store<ReadWrite>` + tempdir for tests.
    fn fresh_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("index.db");
        let store = Store::open(&path).expect("open store");
        store.init(&ModelInfo::default()).expect("init store");
        (store, dir)
    }

    /// Build a queue tied to a fresh store, with caller-chosen thresholds.
    fn fresh_queue(
        threshold_rows: usize,
        interval: Duration,
    ) -> (Arc<PendingSummaryQueue>, Store, tempfile::TempDir) {
        let (store, dir) = fresh_store();
        let pool = store.pool.clone();
        let rt = Arc::clone(&store.rt);
        let q = Arc::new(PendingSummaryQueue::with_thresholds(
            pool,
            rt,
            threshold_rows,
            interval,
        ));
        (q, store, dir)
    }

    fn mk_row(id: &str) -> PendingSummary {
        PendingSummary {
            custom_id: id.to_string(),
            text: format!("body for {id}"),
            model: "test-model".to_string(),
            purpose: "summary".to_string(),
        }
    }

    /// `flush` on an empty queue must be Ok(0) and not touch the DB.
    #[test]
    fn flush_empty_queue_is_no_op() {
        let (q, store, _dir) = fresh_queue(64, Duration::from_secs(60));
        let n = q.flush().expect("flush must succeed");
        assert_eq!(n, 0);
        // Nothing should be persisted.
        let hashes: Vec<&str> = vec![];
        assert_eq!(
            store
                .get_summaries_by_hashes(&hashes, "summary")
                .unwrap()
                .len(),
            0
        );
    }

    /// Push three rows under a high threshold + long interval so the
    /// conditional flush never fires; then call `flush` explicitly and
    /// assert the rows land in `llm_summaries`.
    #[test]
    fn explicit_flush_persists_rows() {
        let (q, store, _dir) = fresh_queue(usize::MAX, Duration::from_secs(60));
        for i in 0..3 {
            q.push(mk_row(&format!("row{i}")));
        }
        // Conditional path didn't fire: queue still holds the rows.
        assert_eq!(q.pending_len(), 3, "no auto-flush under high thresholds");
        let n = q.flush().expect("flush must succeed");
        assert_eq!(n, 3);
        assert_eq!(q.pending_len(), 0, "queue empty after flush");

        let hashes: Vec<&str> = vec!["row0", "row1", "row2"];
        let got = store
            .get_summaries_by_hashes(&hashes, "summary")
            .expect("get_summaries_by_hashes");
        assert_eq!(got.len(), 3, "all 3 rows must be visible");
        assert_eq!(got.get("row0").map(|s| s.as_str()), Some("body for row0"));
    }

    /// Threshold-driven flush: with `threshold_rows=2` and a long
    /// interval, pushing the second row triggers an auto-flush.
    #[test]
    fn threshold_triggers_auto_flush() {
        let (q, store, _dir) = fresh_queue(2, Duration::from_secs(60));
        q.push(mk_row("a"));
        assert_eq!(q.pending_len(), 1, "below threshold: no flush");
        q.push(mk_row("b"));
        // After the second push the threshold is crossed and the
        // conditional flush drains the queue.
        assert_eq!(q.pending_len(), 0, "auto-flush at threshold");
        let got = store
            .get_summaries_by_hashes(&["a", "b"], "summary")
            .unwrap();
        assert_eq!(got.len(), 2);
    }

    /// Multi-thread stress: three threads pushing 100 rows each. After
    /// joining and a final flush, all 300 rows must be present.
    #[test]
    fn three_threads_each_push_100_rows() {
        // Use a high threshold so the producers don't fight on the
        // tx-commit path; we want the final flush to drain the lot.
        let (q, store, _dir) = fresh_queue(1024, Duration::from_secs(60));
        let mut handles = Vec::new();
        for tid in 0..3 {
            let q = Arc::clone(&q);
            handles.push(std::thread::spawn(move || {
                for i in 0..100 {
                    q.push(mk_row(&format!("t{tid}-r{i:03}")));
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panic");
        }
        let drained = q.flush().expect("final flush");
        // Concurrent threshold flushes may have already drained some
        // rows; the final-flush count is the residue. Assert the
        // *total visible* rows in SQLite is exactly 300.
        let hashes: Vec<String> = (0..3)
            .flat_map(|tid| (0..100).map(move |i| format!("t{tid}-r{i:03}")))
            .collect();
        let h_refs: Vec<&str> = hashes.iter().map(|s| s.as_str()).collect();
        let got = store
            .get_summaries_by_hashes(&h_refs, "summary")
            .expect("get_summaries_by_hashes");
        assert_eq!(got.len(), 300, "all 300 rows must land");
        // Sanity: the residue must be a subset of [0, 300]. Useful
        // upper-bound pin against a regression that double-flushed
        // every row.
        assert!(
            drained <= 300,
            "drained final-flush count {drained} should be ≤ 300"
        );
    }

    /// Concurrency test from brief §5: a chunk-upsert (`begin_write`
    /// path) running concurrently with a queue flush must serialize
    /// cleanly through `WRITE_LOCK` — no SQLITE_BUSY abort, no torn
    /// state.
    #[test]
    fn flush_serializes_with_concurrent_upsert() {
        use crate::parser::{ChunkType, Language};
        use crate::{Chunk, Embedding, EMBEDDING_DIM};

        let (q, store, _dir) = fresh_queue(usize::MAX, Duration::from_secs(60));
        let store = Arc::new(store);

        // Pre-populate the queue with rows to drain.
        for i in 0..50 {
            q.push(mk_row(&format!("ser-{i:03}")));
        }

        // Build a sample chunk + embedding to upsert in parallel.
        let chunk = Chunk {
            id: "concurrent_chunk".to_string(),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "concurrent_chunk".to_string(),
            signature: "fn concurrent_chunk()".to_string(),
            content: "fn concurrent_chunk() { let _ = 42; }".to_string(),
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: "concurrent-hash".to_string(),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
            parser_version: 0,
        };
        let mut emb_vec = vec![0.0_f32; EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let emb = Embedding::new(emb_vec);

        let store_for_upsert = Arc::clone(&store);
        let chunk_clone = chunk.clone();
        let emb_clone = emb.clone();
        let upsert_handle = std::thread::spawn(move || {
            store_for_upsert
                .upsert_chunk(&chunk_clone, &emb_clone, None)
                .expect("upsert must succeed under WRITE_LOCK");
        });

        let q_for_flush = Arc::clone(&q);
        let flush_handle = std::thread::spawn(move || {
            q_for_flush
                .flush()
                .expect("flush must succeed under WRITE_LOCK")
        });

        upsert_handle.join().expect("upsert thread panic");
        let drained = flush_handle.join().expect("flush thread panic");
        assert_eq!(drained, 50, "all queued rows must commit");

        // Both writes visible: the chunk and the summaries.
        let chunks = store.get_chunks_by_ids(&["concurrent_chunk"]).unwrap();
        assert!(
            chunks.contains_key("concurrent_chunk"),
            "upsert must have landed"
        );
        let hashes: Vec<String> = (0..50).map(|i| format!("ser-{i:03}")).collect();
        let h_refs: Vec<&str> = hashes.iter().map(|s| s.as_str()).collect();
        let got = store.get_summaries_by_hashes(&h_refs, "summary").unwrap();
        assert_eq!(got.len(), 50, "all 50 summaries must have landed");
    }

    /// fsync-amortization pin (brief §5): pushing 200 rows under a
    /// threshold of 64 + final flush should produce ≤ 5 flushes
    /// (200 / 64 = 3 threshold flushes + 1 final flush + slack for
    /// concurrent test scheduling).
    ///
    /// We can't easily count `begin_write` spans without instrumenting
    /// a custom subscriber here — instead we count flushes via an
    /// `Arc<AtomicUsize>` injected into a wrapper. The point of this
    /// test is to fail loudly if a regression turns the "amortize"
    /// promise back into a per-row commit storm.
    #[test]
    fn flush_amortizes_under_threshold() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let flush_count = Arc::new(AtomicUsize::new(0));

        // Wrap the queue so we can intercept flush calls.
        let (q, _store, _dir) = fresh_queue(64, Duration::from_secs(60));

        // The conditional flush path triggers when the threshold is
        // crossed. We simulate observation by counting transitions
        // from non-empty back to empty.
        for i in 0..200 {
            q.push(mk_row(&format!("amort-{i:03}")));
            // After every push, if pending_len() == 0, a flush ran.
            if q.pending_len() == 0 {
                flush_count.fetch_add(1, Ordering::SeqCst);
            }
        }
        // Final unconditional flush.
        let residue = q.flush().expect("final flush");
        if residue > 0 {
            flush_count.fetch_add(1, Ordering::SeqCst);
        }

        let observed = flush_count.load(Ordering::SeqCst);
        assert!(
            observed <= 5,
            "200 enqueues at threshold=64 should produce ≤ 5 flushes; observed {observed}"
        );
        assert!(
            observed >= 1,
            "at least one flush should have occurred; observed {observed}"
        );
    }

    /// Fix 1 (post-#1126 review): a permanently failing flush must
    /// trip the `consecutive_flush_failures` kill-switch so `should_flush`
    /// stops returning `true` once `MAX_CONSECUTIVE_FLUSH_FAILURES` is
    /// reached. The explicit final flush from the LLM pass / `cmd_index`
    /// is unaffected — its success resets the counter.
    ///
    /// Failure-injection strategy: drop the `llm_summaries` table via
    /// raw SQL on the underlying pool. Subsequent `INSERT OR IGNORE`
    /// queries fail with "no such table". To recover, re-CREATE the
    /// table and call `flush` explicitly.
    #[test]
    fn flush_failure_disables_conditional_auto_flush() {
        let (q, store, _dir) = fresh_queue(2, Duration::from_secs(60));

        // Inject failure: drop `llm_summaries` so every flush attempt
        // hits "no such table". The table is part of `init`'s schema —
        // dropping it leaves the rest of the DB intact.
        store
            .rt
            .block_on(async {
                sqlx::query("DROP TABLE llm_summaries")
                    .execute(&store.pool)
                    .await
            })
            .expect("drop table for failure injection");

        // Push enough rows to cross the threshold three separate times.
        // Each crossing triggers a conditional flush in `push` that
        // fails (table missing), bumping the failure counter. After
        // the 3rd crossing, `should_flush` must return false.
        //
        // Threshold=2, so two pushes cross it. Pre-flush: pending_len
        // jumps from 1 → 2 → flush attempt → fails → re-enqueued (2
        // rows still pending). Next push pre-flush already at 2;
        // pushes 3rd row to len 3, conditional flush fires again,
        // fails, etc. We push 6 rows total; the conditional path is
        // attempted on every push past the first (len ≥ 2).
        for i in 0..6 {
            q.push(mk_row(&format!("fail-{i}")));
        }

        // Counter must have saturated. Use saturating semantics — the
        // `fetch_add` could have over-shot if more than MAX failures
        // fired before the load, but the "≥ MAX" check is what
        // matters for `should_flush`.
        let final_count = q.consecutive_failure_count();
        assert!(
            final_count >= MAX_CONSECUTIVE_FLUSH_FAILURES,
            "expected at least {} consecutive failures, got {}",
            MAX_CONSECUTIVE_FLUSH_FAILURES,
            final_count
        );
        // Rows still in the queue (re-enqueued by the failed flushes).
        assert!(
            q.pending_len() >= 6,
            "all 6 pushed rows must still be queued after all flushes failed; pending={}",
            q.pending_len()
        );

        // Kill-switch verification: even though pending_len > threshold,
        // `should_flush` returns false because the failure counter is
        // saturated. A regression that ignored the counter would still
        // return `true` here.
        assert!(
            !q.should_flush_for_test(),
            "should_flush must return false after the kill-switch trips, even with rows ≥ threshold"
        );

        // Pushing more rows MUST NOT trigger another auto-flush
        // attempt (the conditional path bails). Verify by checking
        // the failure counter does NOT grow on subsequent pushes.
        let count_before = q.consecutive_failure_count();
        for i in 0..3 {
            q.push(mk_row(&format!("post-trip-{i}")));
        }
        let count_after = q.consecutive_failure_count();
        assert_eq!(
            count_before, count_after,
            "auto-flush must not retry while the kill-switch is tripped (counter changed {} → {})",
            count_before, count_after
        );

        // Recovery: re-create the table and call `flush` explicitly.
        // The explicit path bypasses `should_flush`, so the counter
        // saturation does NOT block it. On success, the counter resets.
        store
            .rt
            .block_on(async {
                sqlx::query(
                    "CREATE TABLE llm_summaries (\
                       content_hash TEXT NOT NULL,\
                       purpose TEXT NOT NULL DEFAULT 'summary',\
                       summary TEXT NOT NULL,\
                       model TEXT NOT NULL,\
                       created_at TEXT NOT NULL,\
                       PRIMARY KEY (content_hash, purpose))",
                )
                .execute(&store.pool)
                .await
            })
            .expect("recreate table for recovery");

        let drained = q
            .flush()
            .expect("explicit flush must succeed after recovery");
        assert!(
            drained >= 6,
            "explicit flush must drain at least the 6 originally-pushed rows (got {drained})"
        );
        assert_eq!(
            q.consecutive_failure_count(),
            0,
            "successful flush must reset the failure counter"
        );

        // Conditional auto-flush is re-enabled: pushing past the
        // threshold now triggers a flush again. Push two rows;
        // pending_len should drop back to 0 once the threshold is
        // crossed (the queue's recovery is observable).
        q.push(mk_row("recover-a"));
        q.push(mk_row("recover-b"));
        assert_eq!(
            q.pending_len(),
            0,
            "auto-flush re-enabled after counter reset; queue should drain at threshold"
        );
    }
}
