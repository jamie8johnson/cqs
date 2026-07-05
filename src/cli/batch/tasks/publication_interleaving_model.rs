//! Loom model of the daemon TaskStore's cross-thread publication + admission
//! protocol (MCP Phase 3b floor, the interleaving-auditor lane for #2237).
//!
//! This is a *bounded, faithful* extraction of the only cross-thread shared
//! state a `cqs_eval` task touches after admission — the per-[`super::TaskEntry`]
//! `AtomicU8` status, the write-once `Mutex<Option<TerminalResult>>` result slot,
//! and the map-lock-guarded admission caps. It exists so loom can exhaustively
//! enumerate the interleavings the single-threaded daemon suite cannot reach:
//! a worker publishing a terminal outcome while `tasks/get`/`tasks/result`
//! readers race it, two publishers racing the write-once slot, and two admissions
//! racing the count cap.
//!
//! ## The actors, and why they run the way they do
//!
//! Production's admission path is *serialized* by the single-flight
//! `EVAL_IN_FLIGHT` guard (`analysis.rs:197` — held from `try_acquire` through
//! `spawn_eval_worker`, so at most one worker and one admission are ever live).
//! That guard is the reason the count/bytes caps and the single-writer publish
//! are safe in the deployed daemon. These models deliberately run the primitives
//! *without* that guard so loom proves the primitives are safe **on their own** —
//! i.e. the caps and the publication ordering do not merely rely on single-flight
//! for their correctness. That is the stronger, durable guarantee: if a future
//! lane ever relaxes single-flight (a per-slot eval pool, say), these primitives
//! still hold.
//!
//! 1. **Publisher** — [`super::TaskEntry::publish`]: fills the result slot under
//!    the slot mutex, then `status.store(terminal, Release)`. Write-once: a second
//!    publish (a duplicate, or the publish-on-drop `failed` handle after a normal
//!    publish) finds the slot `Some` and returns without touching status.
//! 2. **Reader** — [`super::TaskStore::result`]: `status.load(Acquire)` FIRST;
//!    only if terminal does it lock the slot and read it. A terminal status with
//!    an empty slot is a retry (`Pending`), never an error.
//! 3. **Admission** — [`super::TaskStore::try_admit`]: under the map lock,
//!    re-check the caps then insert (one critical section).
//!
//! ## The invariants these models pin
//!
//! - **Publication ordering (seam-2c).** A reader that observes a terminal status
//!   via `load(Acquire)` ALWAYS finds the slot filled — the fill happens-before
//!   the `Release` status store, and the reader's `Acquire` synchronizes-with it.
//!   The negative control [`tests::reordered_publish_manufactures_empty_terminal`]
//!   reorders the two stores and loom finds the terminal-with-empty-slot a reader
//!   "trusting the atomic alone" would trip on — proving the ordering is
//!   load-bearing, not decorative.
//! - **Write-once terminal coherence (seam-1b / D6 frozen-terminal).** Under two
//!   racing publishers exactly ONE wins BOTH the slot and the status, so the
//!   published `(status, slot)` pair is always coherent (the status never comes
//!   from a publisher that lost the slot). The negative control
//!   [`tests::status_store_outside_writeonce_tears`] stores status unconditionally
//!   and loom finds the torn `status=Completed / slot=Failed` pair.
//! - **Admission cap atomicity (RT-DOS-3a).** Two concurrent `try_admit`s against
//!   a store one below the cap admit AT MOST one; the map never exceeds the cap.
//!   The negative control [`tests::split_admission_check_exceeds_cap`] splits the
//!   check and the insert into two critical sections and loom finds the cap
//!   overrun.
//!
//! ## Running
//!
//! Same private-cfg gate and run shape as the sibling models (see
//! `super::super::interleaving_model` for the rationale on `cqs_loom` vs the bare
//! `loom` cfg). `cli/` is part of the BINARY crate, so run under `--bin cqs`:
//!
//! ```text
//! RUSTFLAGS="--cfg cqs_loom" CARGO_TARGET_DIR=<private-loom-dir> \
//!     cargo test --features cuda-index --bin cqs \
//!     publication_interleaving_model -- --nocapture
//! ```

// The `mod publication_interleaving_model;` declaration in `tasks/mod.rs` carries
// the `#[cfg(all(cqs_loom, test))]` gate, so a normal build never sees `use loom`
// and `loom` stays a `cfg(cqs_loom)`-only dev-dependency.

use loom::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};

/// Status byte, mirroring [`super::TaskStatus`]'s `#[repr(u8)]`. `0` is the sole
/// non-terminal state; the reader treats any non-zero as terminal.
const WORKING: u8 = 0;
const COMPLETED: u8 = 1;
const FAILED: u8 = 2;

fn is_terminal(status: u8) -> bool {
    status != WORKING
}

/// The model's stand-in for a filled [`super::TerminalResult`]. The only property
/// the invariants turn on is "which publish committed this slot", so the payload
/// is just the status tag the filling publisher intended. Coherence == the entry
/// `status` atomic equals this tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Payload {
    /// The `status` the publisher that filled this slot intended to store. In a
    /// coherent state it equals the entry's `status` atomic.
    status_tag: u8,
}

/// A bounded [`super::TaskEntry`]: the status atomic, the write-once slot, and the
/// retained-bytes atomic — every cross-thread field, nothing else.
struct Entry {
    status: AtomicU8,
    result: Mutex<Option<Payload>>,
    retained_bytes: AtomicUsize,
}

impl Entry {
    fn new_working() -> Arc<Entry> {
        Arc::new(Entry {
            status: AtomicU8::new(WORKING),
            result: Mutex::new(None),
            retained_bytes: AtomicUsize::new(0),
        })
    }

    /// Faithful transcription of [`super::TaskEntry::publish`]: fill the slot
    /// FIRST (under the mutex, write-once), then `retained_bytes` (Relaxed), then
    /// `status` (Release) LAST.
    fn publish(&self, status: u8, bytes: usize) {
        {
            let mut slot = self.result.lock().unwrap();
            if slot.is_some() {
                return; // write-once: a second publish is ignored, status untouched.
            }
            *slot = Some(Payload { status_tag: status });
        }
        self.retained_bytes.store(bytes, Ordering::Relaxed);
        // Flip status LAST, with Release, so `status.load(Acquire)` implies the
        // slot is filled.
        self.status.store(status, Ordering::Release);
    }
}

/// The outcome of a [`super::TaskStore::result`]-shaped read.
#[derive(Debug, PartialEq, Eq)]
enum ReadOutcome {
    /// Terminal status observed AND the slot was filled: `(status, payload)`.
    Ready(u8, Payload),
    /// Non-terminal, OR a terminal-status-with-empty-slot torn read — a retry.
    Pending,
}

/// Faithful transcription of [`super::TaskStore::result`]'s per-entry read: load
/// status (Acquire) FIRST; only if terminal lock the slot and read it. An empty
/// slot under a terminal status is `Pending` (retry), never an error.
fn read_result(entry: &Entry) -> ReadOutcome {
    let status = entry.status.load(Ordering::Acquire);
    if !is_terminal(status) {
        return ReadOutcome::Pending;
    }
    let slot = entry.result.lock().unwrap();
    match *slot {
        Some(p) => ReadOutcome::Ready(status, p),
        None => ReadOutcome::Pending, // torn read window — retry.
    }
}

// ─── admission model ─────────────────────────────────────────────────────────

/// The model's cap (production's [`super::RETAINED_TASK_COUNT_CAP`] is 32; 2 is
/// enough to exercise the check+insert atomicity — the property is independent of
/// the numeric cap).
const MODEL_CAP: usize = 2;

/// A bounded [`super::TaskStore`] map: just the length behind the map lock. The
/// bytes ceiling is the same shape (a sum read under the same lock), so the count
/// cap models both.
struct Store {
    len: Mutex<usize>,
}

impl Store {
    fn with_len(n: usize) -> Arc<Store> {
        Arc::new(Store { len: Mutex::new(n) })
    }

    /// Faithful [`super::TaskStore::try_admit`]: check the cap and insert in ONE
    /// critical section under the map lock.
    fn try_admit(&self) -> bool {
        let mut len = self.len.lock().unwrap();
        if *len >= MODEL_CAP {
            return false;
        }
        *len += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Publication ordering (seam-2c) ───────────────────────────────────────

    /// Model 1 — a publisher fills-then-flips while a reader races it. THE
    /// load-bearing ordering guarantee: a reader that ever observes a terminal
    /// status ALWAYS finds the slot filled, and the filled slot's tag equals the
    /// observed status. Loom enumerates every interleaving of the two stores
    /// against the reader's load+lock.
    ///
    /// This is the schedule the D5 poll loop rides in production: `tasks/get`
    /// observes `completed`, then `tasks/result` dials and reads the slot. It must
    /// never manufacture a terminal-with-no-result. PASSES because the fill
    /// happens-before the `Release` status store the reader's `Acquire` reads.
    #[test]
    fn reader_observing_terminal_always_sees_a_coherent_slot() {
        loom::model(|| {
            let entry = Entry::new_working();

            let e_pub = Arc::clone(&entry);
            let publisher = loom::thread::spawn(move || {
                e_pub.publish(COMPLETED, 128);
            });

            match read_result(&entry) {
                ReadOutcome::Ready(status, payload) => {
                    assert!(
                        is_terminal(status),
                        "a Ready read must carry a terminal status"
                    );
                    assert_eq!(
                        status, payload.status_tag,
                        "TORN PUBLICATION: reader observed status {status} but the \
                         slot was filled by a publisher intending {} — status and \
                         slot disagree",
                        payload.status_tag
                    );
                }
                // Pending is always legal (observed Working, or the torn-read
                // retry window). The invariant bites only on Ready.
                ReadOutcome::Pending => {}
            }

            publisher.join().unwrap();
        });
    }

    /// Model 2 — after the publisher has fully finished, a reader ALWAYS gets a
    /// coherent Ready (never a stuck Pending). This is the liveness half: fill +
    /// flip means a settled terminal is always readable.
    #[test]
    fn settled_terminal_is_always_readable() {
        loom::model(|| {
            let entry = Entry::new_working();

            let e_pub = Arc::clone(&entry);
            let publisher = loom::thread::spawn(move || {
                e_pub.publish(FAILED, 64);
            });
            publisher.join().unwrap();

            match read_result(&entry) {
                ReadOutcome::Ready(status, payload) => {
                    assert_eq!(status, FAILED);
                    assert_eq!(payload.status_tag, FAILED);
                }
                ReadOutcome::Pending => {
                    panic!("a fully-settled terminal must be readable, got Pending")
                }
            }
        });
    }

    /// Negative control — reorder the two stores (status FIRST, then fill the
    /// slot) and a reader that TRUSTS THE ATOMIC ALONE (asserts a terminal status
    /// implies a filled slot) trips. Loom finds the schedule where it loads
    /// terminal then locks a still-empty slot. `#[should_panic]`: GREEN exactly
    /// while the reorder manufactures the empty-terminal — proving the
    /// fill-before-flip ordering in `publish` is what closes it.
    #[test]
    #[should_panic(expected = "EMPTY TERMINAL")]
    fn reordered_publish_manufactures_empty_terminal() {
        loom::model(|| {
            let entry = Entry::new_working();

            let e_pub = Arc::clone(&entry);
            let publisher = loom::thread::spawn(move || {
                // WRONG ORDER: flip status terminal BEFORE filling the slot.
                e_pub.status.store(COMPLETED, Ordering::Release);
                let mut slot = e_pub.result.lock().unwrap();
                *slot = Some(Payload {
                    status_tag: COMPLETED,
                });
            });

            // A reader that trusts the atomic alone: sees terminal ⇒ demands Some.
            let status = entry.status.load(Ordering::Acquire);
            if is_terminal(status) {
                let slot = entry.result.lock().unwrap();
                assert!(
                    slot.is_some(),
                    "EMPTY TERMINAL: status is terminal but the slot is still None"
                );
            }

            publisher.join().unwrap();
        });
    }

    // ── Write-once terminal coherence (seam-1b / D6 frozen-terminal) ─────────

    /// Model 3 — two publishers (a `completed` and a `failed`, standing in for a
    /// normal publish racing the publish-on-drop `failed` handle) race the
    /// write-once slot. Invariant: exactly one wins BOTH slot and status, so the
    /// final `(status, slot)` is coherent — the status never comes from the
    /// publisher that lost the slot. Loom enumerates every interleaving.
    ///
    /// PASSES because the loser hits `slot.is_some()` and returns BEFORE its
    /// status store, so only the slot-winner ever writes status.
    #[test]
    fn two_publishers_leave_a_coherent_frozen_terminal() {
        loom::model(|| {
            let entry = Entry::new_working();

            let e_a = Arc::clone(&entry);
            let a = loom::thread::spawn(move || e_a.publish(COMPLETED, 100));
            let e_b = Arc::clone(&entry);
            let b = loom::thread::spawn(move || e_b.publish(FAILED, 200));

            a.join().unwrap();
            b.join().unwrap();

            let status = entry.status.load(Ordering::Acquire);
            assert!(
                is_terminal(status),
                "both publishers ran; status is terminal"
            );
            let slot = entry.result.lock().unwrap();
            let payload = slot.expect("a terminal entry has a filled slot");
            assert_eq!(
                status, payload.status_tag,
                "TORN TERMINAL: status {status} disagrees with the slot's committing \
                 publisher {} — the write-once guard let a loser store status",
                payload.status_tag
            );
        });
    }

    /// Model 4 — an explicit publish followed (same thread) by a duplicate
    /// publish (the publish-on-drop `failed` handle after a normal `completed`
    /// publish) racing a reader. Invariant: the reader NEVER observes the
    /// duplicate's status — a terminal is frozen; the drop's write-once `failed`
    /// can't overwrite a `completed`.
    #[test]
    fn drop_duplicate_never_overwrites_a_frozen_terminal() {
        loom::model(|| {
            let entry = Entry::new_working();

            let e_worker = Arc::clone(&entry);
            let worker = loom::thread::spawn(move || {
                // Explicit publish, THEN the publish-on-drop handle fires (a
                // no-op because the slot is already Some) — modelled as two
                // sequential same-thread calls.
                e_worker.publish(COMPLETED, 100);
                e_worker.publish(FAILED, 100); // the drop handle; write-once ⇒ ignored.
            });

            match read_result(&entry) {
                ReadOutcome::Ready(status, payload) => {
                    assert_eq!(
                        status, COMPLETED,
                        "a frozen `completed` must never flip to the drop's `failed`"
                    );
                    assert_eq!(payload.status_tag, COMPLETED);
                }
                ReadOutcome::Pending => {}
            }

            worker.join().unwrap();
        });
    }

    /// Negative control — store status UNCONDITIONALLY (outside the write-once
    /// guard), so a publisher that LOST the slot still overwrites status. Loom
    /// finds the torn `status=failed / slot=completed`. `#[should_panic]`: GREEN
    /// while the write-once status guard is load-bearing.
    #[test]
    #[should_panic(expected = "TORN TERMINAL")]
    fn status_store_outside_writeonce_tears() {
        fn publish_broken(entry: &Entry, status: u8) {
            {
                let mut slot = entry.result.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(Payload { status_tag: status });
                }
            }
            // BUG: store status even when this publisher did NOT fill the slot.
            entry.status.store(status, Ordering::Release);
        }

        loom::model(|| {
            let entry = Entry::new_working();

            let e_a = Arc::clone(&entry);
            let a = loom::thread::spawn(move || publish_broken(&e_a, COMPLETED));
            let e_b = Arc::clone(&entry);
            let b = loom::thread::spawn(move || publish_broken(&e_b, FAILED));

            a.join().unwrap();
            b.join().unwrap();

            let status = entry.status.load(Ordering::Acquire);
            let slot = entry.result.lock().unwrap();
            let payload = slot.expect("slot filled");
            assert_eq!(
                status, payload.status_tag,
                "TORN TERMINAL: status {status} disagrees with slot {}",
                payload.status_tag
            );
        });
    }

    // ── Admission cap atomicity (RT-DOS-3a) ──────────────────────────────────

    /// Model 5 — two concurrent admissions against a store ONE below the cap.
    /// Invariant: at most one succeeds and the map NEVER exceeds the cap. Loom
    /// enumerates both lock orders. PASSES because the check and the insert share
    /// one critical section under the map lock.
    #[test]
    fn concurrent_admissions_never_exceed_cap() {
        loom::model(|| {
            let store = Store::with_len(MODEL_CAP - 1);

            let s_a = Arc::clone(&store);
            let a = loom::thread::spawn(move || s_a.try_admit());
            let s_b = Arc::clone(&store);
            let b = loom::thread::spawn(move || s_b.try_admit());

            let ok_a = a.join().unwrap();
            let ok_b = b.join().unwrap();

            let final_len = *store.len.lock().unwrap();
            assert!(
                final_len <= MODEL_CAP,
                "CAP OVERRUN: map grew to {final_len} past cap {MODEL_CAP}"
            );
            // Exactly one free slot existed, so exactly one admission wins.
            assert_eq!(
                (ok_a as usize) + (ok_b as usize),
                1,
                "exactly the one free slot admits"
            );
        });
    }

    /// Negative control — split the cap CHECK and the INSERT into two separate
    /// critical sections (the TOCTOU shape). Loom finds both admissions reading
    /// `len < cap` before either inserts, then both inserting. `#[should_panic]`:
    /// GREEN while the single-critical-section `try_admit` is load-bearing.
    #[test]
    #[should_panic(expected = "CAP OVERRUN")]
    fn split_admission_check_exceeds_cap() {
        fn try_admit_racy(store: &Store) -> bool {
            // Check in one critical section...
            let full = {
                let len = store.len.lock().unwrap();
                *len >= MODEL_CAP
            };
            if full {
                return false;
            }
            // ...insert in a SEPARATE one (the TOCTOU window).
            let mut len = store.len.lock().unwrap();
            *len += 1;
            true
        }

        loom::model(|| {
            let store = Store::with_len(MODEL_CAP - 1);

            let s_a = Arc::clone(&store);
            let a = loom::thread::spawn(move || try_admit_racy(&s_a));
            let s_b = Arc::clone(&store);
            let b = loom::thread::spawn(move || try_admit_racy(&s_b));

            a.join().unwrap();
            b.join().unwrap();

            let final_len = *store.len.lock().unwrap();
            assert!(
                final_len <= MODEL_CAP,
                "CAP OVERRUN: map grew to {final_len} past cap {MODEL_CAP}"
            );
        });
    }
}
