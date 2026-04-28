//! Watch-mode freshness snapshot. (#1182 — Layer 3)
//!
//! `cqs watch` (the daemon) keeps a small live picture of the index's
//! relationship to the working tree: how many files have been observed
//! changing, whether a rebuild is in flight, when the last reindex
//! finished. That state is owned by the watch loop thread and not
//! visible to socket clients today.
//!
//! This module exposes the snapshot via an `Arc<RwLock<WatchSnapshot>>`
//! that the watch loop updates once per cycle and the daemon's
//! `dispatch_status` handler reads. The wire shape is JSON-serializable
//! so `cqs status --watch-fresh --json` can hand it back to agents that
//! want to gate their work on freshness (eval runners, ceremony
//! commands, in-IDE pre-query checks).
//!
//! The snapshot itself doesn't *make* the index fresh — Layers 1 and 2
//! (git hooks + periodic reconciliation) close the missed-event classes.
//! Layer 3 (this module) gives those layers an observable surface, and
//! makes the existing inotify path's state machine queryable.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Freshness state of the watched index. Computed from the in-flight
/// counters at snapshot-update time.
///
/// State machine:
/// - `Rebuilding`: a background HNSW rebuild is in flight. The index is
///   technically "behind" until the rebuild swap completes.
/// - `Stale`: at least one file event has been observed but not yet
///   reindexed (debounce window, queued, or dropped at the cap), or the
///   notes file is dirty. Includes the case where Layer 2 reconciliation
///   has detected divergent files but not yet drained them.
/// - `Fresh`: no pending work. Every observed change has been absorbed.
/// - `Unknown`: no snapshot has ever been published. Daemon is up but
///   the watch loop hasn't completed its first cycle, or `cqs status`
///   was queried against a non-`watch --serve` daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FreshnessState {
    Fresh,
    Stale,
    Rebuilding,
    Unknown,
}

impl FreshnessState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Rebuilding => "rebuilding",
            Self::Unknown => "unknown",
        }
    }
}

/// Snapshot of the watch loop's view of "how fresh is the index?". The
/// wire shape returned by `cqs status --watch-fresh --json`.
///
/// Keep this struct cheap to construct and clone — the watch loop calls
/// `update()` once per 100 ms tick, and the daemon dispatch path reads
/// it on every status query. No heap walks; just atomic counters and
/// `Instant` arithmetic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchSnapshot {
    /// Computed state — what the consuming agent should branch on.
    pub state: FreshnessState,
    /// Files queued for reindex but not yet drained. Includes events
    /// observed in the current debounce window.
    pub modified_files: u64,
    /// Whether the dirty `docs/notes.toml` flag is currently set. Notes
    /// affect search rankings, so this counts toward "stale" even
    /// without per-file events.
    pub pending_notes: bool,
    /// Whether a background HNSW rebuild is currently running. While
    /// true, `state == Rebuilding`. Drains on swap completion.
    pub rebuild_in_flight: bool,
    /// Whether the in-flight rebuild's incremental delta saturated
    /// (`MAX_PENDING_REBUILD_DELTA`). When true, the rebuilt index is
    /// discarded on swap and the next threshold rebuild reads SQLite
    /// fresh — operators should know if this is happening repeatedly.
    pub delta_saturated: bool,
    /// Accumulated incremental insert count since the last full HNSW
    /// rebuild. Useful diagnostic for "how close am I to the rebuild
    /// threshold". Approximates the `state.incremental_count` field.
    pub incremental_count: u64,
    /// File events the daemon dropped this cycle because
    /// `pending_files` was at `CQS_WATCH_MAX_PENDING`. Surfaces a real
    /// correctness issue (some changes will be silently lost until the
    /// next reconciliation pass) — non-zero is always cause for
    /// attention.
    pub dropped_this_cycle: u64,
    /// Seconds since the watch loop last observed *any* filesystem
    /// event. Useful for telling whether the daemon is genuinely idle
    /// vs. mid-burst.
    pub idle_secs: u64,
    /// Unix timestamp (UTC seconds) of the last completed reindex —
    /// the mtime of `index.db` after the most recent write. `None`
    /// when the file is missing or unreadable.
    pub last_synced_at: Option<i64>,
    /// Unix timestamp (UTC seconds) when this snapshot was published.
    /// Lets clients tell how stale the *snapshot itself* is (stale
    /// snapshot ⇒ daemon hasn't ticked recently). Always populated.
    pub snapshot_at: i64,
}

impl WatchSnapshot {
    /// Initial snapshot before the watch loop has ticked once. The
    /// daemon publishes this so a `cqs status` against a freshly
    /// started watch session gets a meaningful "unknown" rather than
    /// an empty / serialization error.
    pub fn unknown() -> Self {
        Self {
            state: FreshnessState::Unknown,
            modified_files: 0,
            pending_notes: false,
            rebuild_in_flight: false,
            delta_saturated: false,
            incremental_count: 0,
            dropped_this_cycle: 0,
            idle_secs: 0,
            last_synced_at: None,
            snapshot_at: now_unix_secs(),
        }
    }

    /// True when the index is fully caught up to every observed event.
    /// Convenience for `--watch-fresh --wait` polling loops.
    pub fn is_fresh(&self) -> bool {
        matches!(self.state, FreshnessState::Fresh)
    }
}

/// Shared snapshot handle. The watch loop holds a write-side; every
/// daemon socket client thread reads through this. Construct once at
/// `cmd_watch` startup, clone the `Arc` into both surfaces.
pub type SharedWatchSnapshot = Arc<RwLock<WatchSnapshot>>;

/// Build the canonical handle, pre-populated with the "unknown" snapshot.
pub fn shared_unknown() -> SharedWatchSnapshot {
    Arc::new(RwLock::new(WatchSnapshot::unknown()))
}

/// Cross-thread one-shot signal that asks the watch loop to run an
/// out-of-band reconciliation pass on its next tick. (#1182 — Layer 1.)
///
/// Set to `true` by:
///   - The daemon's `dispatch_reconcile` handler when a `cqs hook fire`
///     client posts a `reconcile` socket message after a git operation.
///   - The watch loop itself at startup if `.cqs/.dirty` exists (the
///     fallback that hook scripts touch when the daemon is offline).
///
/// Cleared (swap-to-false) by the watch loop once it actually runs the
/// reconcile pass. The bool is one-shot — coalescing two requests into
/// one walk is fine because the walk is idempotent.
///
/// Atomic, not RwLock-guarded: a single bit doesn't justify the lock,
/// and the watch loop's `swap(false, AcqRel)` gives the same
/// "exactly-one consumer" semantics.
pub type SharedReconcileSignal = Arc<AtomicBool>;

/// Build the canonical reconcile-signal handle, initialised to `false`
/// (no reconcile pending). The watch loop and the daemon thread each
/// keep an `Arc` clone; flipping it to `true` from any thread races
/// safely with the loop's `swap`.
pub fn shared_reconcile_signal() -> SharedReconcileSignal {
    Arc::new(AtomicBool::new(false))
}

/// Inputs the watch loop hands to [`WatchSnapshot::compute`] every cycle.
///
/// All fields are cheap reads off the loop's owned `WatchState`. Keep
/// the struct flat so the watch loop can populate it without borrowing
/// `WatchState` longer than it already holds.
#[derive(Debug)]
pub struct WatchSnapshotInput<'a> {
    pub pending_files_count: usize,
    pub pending_notes: bool,
    pub rebuild_in_flight: bool,
    pub delta_saturated: bool,
    pub incremental_count: usize,
    pub dropped_this_cycle: usize,
    pub last_event: std::time::Instant,
    pub last_synced_at: Option<i64>,
    /// Phantom keeps the API future-proof if we add borrow-only fields
    /// (e.g. last-error string). No-op today.
    pub _marker: std::marker::PhantomData<&'a ()>,
}

impl WatchSnapshot {
    /// Compute a fresh snapshot from the watch loop's owned state.
    /// Pure function — pulls only the fields it needs and resolves the
    /// `state` machine deterministically.
    pub fn compute(input: WatchSnapshotInput<'_>) -> Self {
        let state = if input.rebuild_in_flight {
            FreshnessState::Rebuilding
        } else if input.pending_files_count > 0
            || input.pending_notes
            || input.dropped_this_cycle > 0
        {
            FreshnessState::Stale
        } else {
            FreshnessState::Fresh
        };

        Self {
            state,
            modified_files: input.pending_files_count as u64,
            pending_notes: input.pending_notes,
            rebuild_in_flight: input.rebuild_in_flight,
            delta_saturated: input.delta_saturated,
            incremental_count: input.incremental_count as u64,
            dropped_this_cycle: input.dropped_this_cycle as u64,
            idle_secs: input.last_event.elapsed().as_secs(),
            last_synced_at: input.last_synced_at,
            snapshot_at: now_unix_secs(),
        }
    }
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        pending_files_count: usize,
        rebuild_in_flight: bool,
        pending_notes: bool,
        dropped_this_cycle: usize,
    ) -> WatchSnapshotInput<'static> {
        WatchSnapshotInput {
            pending_files_count,
            pending_notes,
            rebuild_in_flight,
            delta_saturated: false,
            incremental_count: 0,
            dropped_this_cycle,
            last_event: std::time::Instant::now(),
            last_synced_at: None,
            _marker: std::marker::PhantomData,
        }
    }

    #[test]
    fn empty_state_is_fresh() {
        let snap = WatchSnapshot::compute(input(0, false, false, 0));
        assert_eq!(snap.state, FreshnessState::Fresh);
        assert!(snap.is_fresh());
    }

    #[test]
    fn pending_files_marks_stale() {
        let snap = WatchSnapshot::compute(input(3, false, false, 0));
        assert_eq!(snap.state, FreshnessState::Stale);
        assert_eq!(snap.modified_files, 3);
    }

    #[test]
    fn pending_notes_alone_marks_stale() {
        let snap = WatchSnapshot::compute(input(0, false, true, 0));
        assert_eq!(snap.state, FreshnessState::Stale);
        assert!(snap.pending_notes);
    }

    #[test]
    fn rebuild_dominates_over_stale_files() {
        // Even if files are queued, a running rebuild is the loud signal —
        // the rebuild will absorb the queue when it swaps.
        let snap = WatchSnapshot::compute(input(5, true, false, 0));
        assert_eq!(snap.state, FreshnessState::Rebuilding);
        assert!(snap.rebuild_in_flight);
    }

    #[test]
    fn dropped_events_mark_stale() {
        // dropped_this_cycle > 0 means the daemon lost events to the
        // pending_files cap. The index is silently behind reality —
        // surface as stale.
        let snap = WatchSnapshot::compute(input(0, false, false, 7));
        assert_eq!(snap.state, FreshnessState::Stale);
        assert_eq!(snap.dropped_this_cycle, 7);
    }

    #[test]
    fn unknown_is_initial_state() {
        let snap = WatchSnapshot::unknown();
        assert_eq!(snap.state, FreshnessState::Unknown);
        assert!(!snap.is_fresh());
    }

    #[test]
    fn freshness_state_serializes_lowercase() {
        // Wire format must be lowercase strings so JSON consumers don't
        // have to know about Rust's PascalCase enum variants.
        for (state, expected) in [
            (FreshnessState::Fresh, "fresh"),
            (FreshnessState::Stale, "stale"),
            (FreshnessState::Rebuilding, "rebuilding"),
            (FreshnessState::Unknown, "unknown"),
        ] {
            let v = serde_json::to_value(state).unwrap();
            assert_eq!(v, serde_json::Value::String(expected.to_string()));
            assert_eq!(state.as_str(), expected);
        }
    }

    #[test]
    fn shared_unknown_is_unknown() {
        let s = shared_unknown();
        assert_eq!(s.read().unwrap().state, FreshnessState::Unknown);
    }

    /// #1182 — Layer 1: a fresh signal starts cleared and round-trips
    /// through `swap` cleanly. Pin both halves so a future refactor of
    /// `shared_reconcile_signal()` (e.g. switching to a notifier crate)
    /// can't silently regress to "always pending".
    #[test]
    fn shared_reconcile_signal_starts_cleared_and_round_trips() {
        use std::sync::atomic::Ordering;
        let s = shared_reconcile_signal();
        assert!(!s.load(Ordering::Acquire));

        // store=true → swap returns the previous value (false), then
        // sets the bit.
        let prev = s.swap(true, Ordering::AcqRel);
        assert!(!prev);
        assert!(s.load(Ordering::Acquire));

        // Watch-loop drain pattern: swap to false, get the previous
        // (was-pending) state.
        let was_pending = s.swap(false, Ordering::AcqRel);
        assert!(was_pending);
        assert!(!s.load(Ordering::Acquire));
    }
}
