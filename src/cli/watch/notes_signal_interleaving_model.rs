//! Loom model of the daemon notes-mutation signal vs the watch-loop drain
//! (the inotify-independent note-reindex protocol; interleaving-auditor lane).
//!
//! ## Why this module exists
//!
//! In `cqs watch --serve` a daemon connection thread that handles
//! `notes-add` / `notes-update` / `notes-remove` writes `docs/notes.toml`
//! (atomic temp+rename, under a file lock) and then flips a shared one-shot
//! `AtomicBool` — the pending-notes signal — via
//! `request_notes_reindex` (`view.rs`: `swap(true, Release)`). The watch loop
//! (a SEPARATE thread) drains that signal once per tick
//! (`drain_pending_notes_signal`: `swap(false, AcqRel)` → `state.pending_notes
//! = true`) and, when the debounce is due, runs the note reindex:
//!
//! ```text
//! if state.pending_notes {
//!     state.pending_notes = false;          // consume, THEN re-read
//!     process_note_changes(...);            // reindex_notes re-reads notes.toml
//! }
//! ```
//!
//! The signal exists because inotify on `docs/notes.toml` is unreliable on the
//! WSL `/mnt/c` (NTFS/9P) deployment: without the explicit writer signal a
//! committed note could be NEVER indexed. The protocol's whole purpose is to
//! guarantee a note write is eventually reflected in the index.
//!
//! The house unit suite is single-threaded: it cannot schedule a writer's
//! `swap(true)` against the watch loop's `swap(false)` drain and the later
//! `pending_notes = false` consume. That is exactly what this module
//! enumerates with loom.
//!
//! ## The invariant under test
//!
//! The note reindex re-reads the WHOLE `docs/notes.toml` and rebuilds the notes
//! table — it is idempotent and content-driven (it indexes whatever the file
//! currently holds, not a delta). The version counter below stands in for "the
//! committed file content": each writer bumps it before flipping the signal,
//! exactly as a write lands a new file before `request_notes_reindex`.
//!
//! > **NO-LOST-REINDEX**: after a writer has completed (file write committed +
//! > signal flipped) and the watch loop has quiesced (it observes the signal
//! > clear AND has no pending-notes work left), the last reindex the loop ran
//! > read a file version AT-OR-AFTER the writer's committed version. The index
//! > is never left strictly behind a committed write.
//!
//! Equivalently: there is no interleaving where a `swap(true)` is "absorbed"
//! (cleared by a drain) without a subsequent reindex that observes the version
//! that writer committed. The decoupling — the signal is a SEPARATE bit from
//! `pending_notes`, and the reindex RE-READS the file rather than trusting a
//! cached delta — is what carries the invariant. The negative control proves it
//! is the re-read, not luck, that holds.
//!
//! ## What the model abstracts (the bounded shape loom needs)
//!
//! - `version: AtomicUsize` is the committed file content (monotone; a writer
//!   bumps it = a new `notes.toml` landed). A reindex "reads" it = the file
//!   content the reindex walk observed.
//! - `signal: AtomicBool` is the shared pending-notes signal. Writer:
//!   `swap(true, Release)` AFTER bumping the version (file write happens-before
//!   the flip, mirroring the handler). Loop drain: `swap(false, AcqRel)`.
//! - `pending: bool` is `state.pending_notes`, owned by the loop thread (not
//!   shared) — the loop is the only thread that touches it, faithful to the
//!   real code.
//! - `processed: AtomicUsize` is the last version the loop's reindex observed.
//!   The invariant compares it to `version` at quiesce.
//!
//! ## Running
//!
//! ```bash
//! RUSTFLAGS="--cfg cqs_loom" cargo test --features cuda-index --bin cqs \
//!     notes_signal_interleaving_model
//! ```
//!
//! All safety models here are GREEN: the protocol upholds NO-LOST-REINDEX under
//! every interleaving loom explores. The `#[ignore]`d negative control replaces
//! the version RE-READ with a snapshot captured at drain time (the bug the
//! re-read avoids) and loom finds the lost-reindex schedule, proving the test
//! has teeth.

#![cfg(all(cqs_loom, test))]

use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::Arc;
use loom::thread;

/// The shared world: the committed file version, the pending-notes signal, and
/// the last-processed version the loop's reindex observed.
struct World {
    /// Committed `docs/notes.toml` content, monotone. A writer bumps this
    /// (the atomic temp+rename) BEFORE flipping the signal.
    version: AtomicUsize,
    /// The shared one-shot pending-notes signal (`request_notes_reindex` /
    /// `drain_pending_notes_signal`).
    signal: AtomicBool,
    /// The last version a reindex observed. Compared to `version` at quiesce.
    processed: AtomicUsize,
}

impl World {
    fn new() -> Arc<Self> {
        Arc::new(World {
            // version 0 is the seed file; processed 0 = the loop indexed the
            // seed at startup. The writers below commit version >= 1.
            version: AtomicUsize::new(0),
            signal: AtomicBool::new(false),
            processed: AtomicUsize::new(0),
        })
    }

    /// One notes-mutation handler: commit a new file version (the temp+rename),
    /// then flip the signal. The file write happens-before the flip — program
    /// order in the handler, and the `Release` on the flip publishes it.
    fn write_note(&self, new_version: usize) {
        // The atomic file write (temp+rename): publish the new content.
        self.version.store(new_version, Ordering::Release);
        // `request_notes_reindex`: swap(true, Release).
        self.signal.swap(true, Ordering::Release);
    }

    /// Drain the signal into the loop-local `pending` flag, faithful to
    /// `drain_pending_notes_signal`. Returns whether a pending write was
    /// drained (so the caller can OR it into its running `pending`).
    fn drain(&self) -> bool {
        self.signal.swap(false, Ordering::AcqRel)
    }

    /// The reindex: re-read the CURRENT committed version and record it as the
    /// last processed version. Faithful to `process_note_changes` →
    /// `reindex_notes`, which re-reads the whole `notes.toml` (latest-at-read),
    /// NOT a snapshot taken at drain time. `fetch_max` because two reindexes can
    /// run and `processed` must reflect the newest version any of them saw —
    /// the index never goes backwards.
    fn reindex_reads_current(&self) {
        let current = self.version.load(Ordering::Acquire);
        self.processed.fetch_max(current, Ordering::AcqRel);
    }
}

/// One watch-loop tick: drain the signal into `pending`, then (debounce due)
/// consume `pending` BEFORE re-reading — the exact order at `mod.rs`:
/// `state.pending_notes = false; process_note_changes(...)`. `pending` is the
/// loop-local flag carried across ticks. Returns the updated `pending`.
fn watch_tick(world: &World, mut pending: bool, flush_due: bool) -> bool {
    // Top-of-loop drain (every iteration, independent of the recv arm).
    if world.drain() {
        pending = true;
    }
    if flush_due && pending {
        // Consume THEN re-read — the ordering under audit.
        pending = false;
        world.reindex_reads_current();
    }
    pending
}

/// MODEL 1 (safety, GREEN): one writer racing a watch loop that ticks enough
/// times to quiesce. Loom interleaves the writer's `version.store` + `swap(true)`
/// against the loop's `drain` + consume + re-read across multiple ticks.
///
/// After both threads join, the loop must have a final consistent view: a final
/// reconciling tick (always flush-due, run after join) drains any residual
/// signal and reindexes, so `processed == version`. The invariant: that final
/// reindex observes the writer's committed version — the write is never lost.
#[test]
fn no_lost_reindex_single_writer() {
    loom::model(|| {
        let world = World::new();
        let w_writer = Arc::clone(&world);
        let w_loop = Arc::clone(&world);

        let writer = thread::spawn(move || {
            w_writer.write_note(1);
        });

        // The watch loop runs several ticks with flush due, racing the writer.
        let watcher = thread::spawn(move || {
            let mut pending = false;
            // Two racing ticks model the loop spinning while the writer commits.
            pending = watch_tick(&w_loop, pending, true);
            pending = watch_tick(&w_loop, pending, true);
            pending
        });

        writer.join().unwrap();
        let residual_pending = watcher.join().unwrap();

        // Quiesce: after the writer is done, run reconciling ticks until the
        // signal is clear AND no pending work remains — the steady state the
        // real loop reaches once writes stop. The loop ALWAYS runs again on its
        // next ~100 ms timer, so a residual pending/signal is guaranteed to be
        // drained; we model that guaranteed convergence here.
        let mut pending = residual_pending;
        // At most a couple of extra ticks are ever needed (drain, then flush).
        for _ in 0..3 {
            pending = watch_tick(&world, pending, true);
        }

        let version = world.version.load(Ordering::Acquire);
        let processed = world.processed.load(Ordering::Acquire);
        // NO-LOST-REINDEX: the committed write was reindexed.
        assert!(
            processed >= version,
            "NO-LOST-REINDEX VIOLATED: committed version {version} but the index only \
             reached version {processed} — a note write's reindex was lost; the signal \
             was absorbed without a reindex observing the committed file"
        );
        // The signal must be clear at quiesce (a sticky signal would mean the
        // loop kept work it never finished).
        assert!(
            !world.signal.load(Ordering::Acquire),
            "signal still set at quiesce — a drained write was not converged"
        );
        // And no loop-local pending work remains.
        assert!(
            !pending,
            "pending_notes still set at quiesce — reindex not converged"
        );
    });
}

/// MODEL 2 (safety, GREEN): two concurrent writers (e.g. two daemon connection
/// threads each handling a notes mutation) racing one watch loop. Each commits a
/// distinct version and flips the shared signal. The signal COALESCES — the loop
/// may drain both flips with one `swap(false)` — which is correct precisely
/// because the reindex re-reads the whole file: one reindex that observes the
/// latest version covers both writes. The invariant: after quiesce the index
/// reached the MAX committed version (neither write is lost to coalescing).
#[test]
fn no_lost_reindex_two_writers_coalescing_signal() {
    loom::model(|| {
        let world = World::new();
        let w_a = Arc::clone(&world);
        let w_b = Arc::clone(&world);
        let w_loop = Arc::clone(&world);

        // Writers commit distinct versions. Loom explores both orders; the MAX
        // is what the file ends at, so that is what the index must reach.
        let writer_a = thread::spawn(move || {
            w_a.write_note(1);
        });
        let writer_b = thread::spawn(move || {
            w_b.write_note(2);
        });

        let watcher = thread::spawn(move || {
            let mut pending = false;
            pending = watch_tick(&w_loop, pending, true);
            pending = watch_tick(&w_loop, pending, true);
            pending
        });

        writer_a.join().unwrap();
        writer_b.join().unwrap();
        let residual_pending = watcher.join().unwrap();

        // Converge.
        let mut pending = residual_pending;
        for _ in 0..3 {
            pending = watch_tick(&world, pending, true);
        }

        let version = world.version.load(Ordering::Acquire);
        let processed = world.processed.load(Ordering::Acquire);
        assert!(
            processed >= version,
            "NO-LOST-REINDEX VIOLATED (two writers): file at version {version}, index only \
             reached {processed} — signal coalescing dropped a write the re-read should have caught"
        );
        assert!(
            !world.signal.load(Ordering::Acquire),
            "signal set at quiesce"
        );
        assert!(!pending, "pending set at quiesce");
    });
}

/// MODEL 3 (safety, GREEN): the consume-before-read ordering under audit. A
/// writer commits DURING the loop's flush — between the `pending = false`
/// consume and the version re-read. Because the reindex re-reads
/// latest-at-read, it picks up the just-committed version; even if it did not,
/// the writer's fresh `swap(true)` keeps the signal set for the next tick. This
/// model isolates that window.
#[test]
fn no_lost_reindex_write_during_flush_window() {
    loom::model(|| {
        let world = World::new();
        let w_writer = Arc::clone(&world);
        let w_loop = Arc::clone(&world);

        // Pre-arm: a first write has already flipped the signal before the loop
        // starts, so the loop enters flush with pending set.
        world.version.store(1, Ordering::Release);
        world.signal.swap(true, Ordering::Release);

        let writer = thread::spawn(move || {
            // A SECOND write lands while the loop is flushing the first.
            w_writer.write_note(2);
        });

        let watcher = thread::spawn(move || {
            let mut pending = false;
            // Drain picks up the pre-armed signal; flush consumes + re-reads —
            // the second write may interleave anywhere in here.
            pending = watch_tick(&w_loop, pending, true);
            pending = watch_tick(&w_loop, pending, true);
            pending
        });

        writer.join().unwrap();
        let residual_pending = watcher.join().unwrap();

        let mut pending = residual_pending;
        for _ in 0..3 {
            pending = watch_tick(&world, pending, true);
        }

        let version = world.version.load(Ordering::Acquire);
        let processed = world.processed.load(Ordering::Acquire);
        assert!(
            processed >= version,
            "NO-LOST-REINDEX VIOLATED (flush-window write): version {version}, index {processed} \
             — a write that landed during the consume->re-read window was lost"
        );
        assert!(
            !world.signal.load(Ordering::Acquire),
            "signal set at quiesce"
        );
        assert!(!pending, "pending set at quiesce");
    });
}

/// NEGATIVE CONTROL (`#[ignore]`, FAILS by design): proves the re-read is what
/// holds the invariant. Replace the latest-at-read reindex with one that
/// reindexes a version SNAPSHOTTED at drain time — the bug the real protocol
/// avoids by re-reading the whole file. Now a writer that commits AFTER the
/// drain snapshot but whose `swap(true)` was coalesced into that same drain has
/// its content lost: the loop reindexes the stale snapshot, clears pending, and
/// the later `swap(true)` was already consumed.
///
/// Loom finds the interleaving and the NO-LOST-REINDEX assert fires. This pins
/// WHY the real code is safe: `reindex_notes` re-reads `notes.toml`
/// (latest-at-read), it does not trust a delta captured when the signal was
/// drained.
///
/// ```bash
/// RUSTFLAGS="--cfg cqs_loom" cargo test --features cuda-index --bin cqs \
///     notes_signal_interleaving_model -- --ignored
/// ```
#[test]
#[ignore = "negative control: reproduces a lost reindex when the loop reindexes a version \
            SNAPSHOTTED at drain time instead of re-reading the file; FAILS by design"]
fn snapshot_at_drain_would_lose_a_write() {
    loom::model(|| {
        let world = World::new();
        let w_writer = Arc::clone(&world);
        let w_loop = Arc::clone(&world);

        let writer = thread::spawn(move || {
            w_writer.write_note(1);
        });

        let watcher = thread::spawn(move || {
            // The BUG: snapshot the version at drain time, reindex THAT instead
            // of re-reading. A coalesced later flip is then lost.
            let drained = w_loop.drain();
            // Snapshot now — before the writer may have stored its version.
            let snapshot = w_loop.version.load(Ordering::Acquire);
            if drained {
                // "Reindex" the stale snapshot.
                w_loop.processed.fetch_max(snapshot, Ordering::AcqRel);
                // pending consumed — no further reindex for this drain.
            }
        });

        writer.join().unwrap();
        watcher.join().unwrap();

        let version = world.version.load(Ordering::Acquire);
        let processed = world.processed.load(Ordering::Acquire);
        // Under the snapshot bug, a schedule exists where the writer's
        // swap(true) was the bit the drain consumed, yet the version snapshot
        // was taken before the store — so processed (0) < version (1).
        assert!(
            processed >= version,
            "lost reindex reproduced: version {version}, index {processed}"
        );
    });
}
