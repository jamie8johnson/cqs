//! Watch-mode freshness snapshot.
//!
//! `cqs watch` (the daemon) keeps a small live picture of the index's
//! relationship to the working tree: how many files have been observed
//! changing, whether a rebuild is in flight, when the last reindex
//! finished. That state is owned by the watch loop thread.
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

/// `Display` so `tracing::info!(state = %snap.state)` works without callers
/// reaching for `.as_str()` everywhere. Delegates to `as_str()` so the
/// wire-shape lowercase strings stay the single source of truth — JSON
/// consumers, structured logs, and human-readable text all see the same
/// spelling.
impl std::fmt::Display for FreshnessState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Latency record for the most recent completed reindex pass.
///
/// Recorded by the watch loop where `reindex_files` returns (success
/// path) — previously this only existed as a tracing span duration, so
/// answering "how long did the last save-triggered reindex take?" meant
/// journalctl grepping. `cqs status --watch` surfaces it instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReindexLatency {
    /// Unix timestamp (UTC seconds) when the pass completed.
    pub at_unix_secs: i64,
    /// Wall-clock duration of the pass in milliseconds.
    pub duration_ms: u64,
    /// Number of files drained in the pass.
    pub files: u64,
}

/// Most recent watch-loop error (reindex or notes-reindex failure).
///
/// Sticky: survives subsequent successful cycles so an operator polling
/// `cqs status --watch` still sees an error that fired between polls.
/// The timestamp disambiguates "current" from "historical" — compare
/// against `last_reindex.at_unix_secs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchErrorInfo {
    /// Unix timestamp (UTC seconds) when the error was observed.
    pub at_unix_secs: i64,
    /// Rendered error message, verbatim from the failing operation.
    pub message: String,
}

/// Per-slot freshness entry inside [`WatchOpsStats::slots`].
///
/// The first entry is always the active slot; sibling slots tracked by
/// the slot-parallel reindex propagation follow. Sibling states:
/// `stale` while their delta queue is non-empty (or the slot errored),
/// `fresh` once a drain or reconcile pass has converged them, `unknown`
/// for slots the daemon knows about but does not propagate to (a
/// foreign-model slot without `CQS_WATCH_ALL_SLOTS`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotWatchStatus {
    /// Slot name (`default` unless `--slot`/`CQS_SLOT` selected another).
    pub name: String,
    /// Freshness state of this slot's index.
    pub state: FreshnessState,
    /// Unix timestamp (UTC seconds) of the last completed reindex for
    /// this slot (`index.db` mtime). `None` when missing/unreadable.
    pub last_synced_at: Option<i64>,
    /// Latency of the most recent reindex pass against this slot.
    pub last_reindex: Option<ReindexLatency>,
    /// Files queued for this slot but not yet drained. For the active
    /// slot this mirrors [`WatchSnapshot::modified_files`]; for sibling
    /// slots it is the depth of the propagation delta queue.
    #[serde(default)]
    pub queue_depth: u64,
    /// Most recent error observed against this slot (sticky, same
    /// semantics as [`WatchOpsStats::last_error`]). For sibling slots
    /// this is how a stale-schema / locked-DB / missing-index slot
    /// surfaces without poisoning the watch loop.
    #[serde(default)]
    pub last_error: Option<WatchErrorInfo>,
}

/// Operational stats block for `cqs status --watch`.
///
/// Composes with the freshness fields already on [`WatchSnapshot`]:
/// queue depth is `modified_files`, dropped events is
/// `dropped_this_cycle`. This block carries the daemon-operational rest
/// — what previously required journalctl grepping.
///
/// Embedding-cache hit rate is intentionally absent: the watch loop's
/// cache consultation happens inside `reindex_files` with no live
/// counters, and `cqs cache stats` already answers the offline
/// question. A live hit-rate counter is deferred until something needs
/// it badly enough to justify the plumbing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchOpsStats {
    /// Daemon socket clients currently being served (the accept loop's
    /// in-flight counter, sampled at snapshot-publish time). Always 0
    /// when the watch session runs without `--serve`.
    pub in_flight_clients: u64,
    /// Whether an out-of-band reconcile request is pending (the
    /// [`SharedReconcileSignal`] is set but the watch loop hasn't
    /// drained it yet). Together with [`WatchSnapshot::state`] this is
    /// the reconcile-visible state.
    pub reconcile_pending: bool,
    /// Latency of the most recent completed reindex pass. `None` until
    /// the first save-triggered reindex of the session.
    pub last_reindex: Option<ReindexLatency>,
    /// Most recent reindex/notes-reindex error. Sticky across
    /// subsequent successes — see [`WatchErrorInfo`].
    pub last_error: Option<WatchErrorInfo>,
    /// Per-slot freshness. Exactly one entry today (the active slot);
    /// The slot-parallel reindex work extends this vec.
    pub slots: Vec<SlotWatchStatus>,
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
    /// Unix timestamp (UTC seconds) when the watch loop last observed
    /// *any* filesystem event. Lets clients compute fresh idle on
    /// demand (`now - last_event_unix_secs`) without retransacting
    /// through the daemon. A frozen-at-publish-time idle value would go
    /// stale the moment the client reads it N seconds later.
    pub last_event_unix_secs: i64,
    /// Unix timestamp (UTC seconds) of the last completed reindex —
    /// the mtime of `index.db` after the most recent write. `None`
    /// when the file is missing or unreadable.
    pub last_synced_at: Option<i64>,
    /// Unix timestamp (UTC seconds) when this snapshot was published.
    /// Lets clients tell how stale the *snapshot itself* is (stale
    /// snapshot ⇒ daemon hasn't ticked recently). `None` only on a
    /// clock-before-epoch system error — a silent `0` would make every
    /// snapshot look "56 years stale" and trip downstream freshness gates.
    /// Operators see a once-per-process warn from `now_unix_secs` when this
    /// happens.
    pub snapshot_at: Option<i64>,
    /// Name of the slot the daemon is currently serving. Lets `cqs slot
    /// remove <name>` refuse to unlink a slot
    /// directory while a long-lived daemon holds open file descriptors
    /// against `slots/<name>/index.db` — on Linux the unlink succeeds
    /// against the held inode and the daemon's WAL checkpoints persist
    /// into a detached directory tree that gets reaped on daemon exit,
    /// silently losing hours of incremental rebuild work. `None` ⇒
    /// daemon hasn't published a snapshot yet (still ramping up).
    #[serde(default)]
    pub active_slot: Option<String>,
    /// Operational stats for `cqs status --watch`. `None` when
    /// the snapshot came from a daemon that predates the field (older
    /// binary) or from the initial `unknown()` placeholder — lets the
    /// CLI distinguish "stats unavailable" from real zeros.
    #[serde(default)]
    pub ops: Option<WatchOpsStats>,
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
            last_event_unix_secs: 0,
            last_synced_at: None,
            snapshot_at: now_unix_secs(),
            active_slot: None,
            ops: None,
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
/// out-of-band reconciliation pass on its next tick.
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

/// One-shot "a note write landed in `docs/notes.toml` — reindex notes" signal,
/// the notes-table sibling of [`SharedReconcileSignal`].
///
/// The daemon notes-mutation handlers (`dispatch_notes_add` / `update` /
/// `remove`) write the file under the lock, then flip this to `true`. The watch
/// loop swaps it back to `false` on its next tick and drains the note reindex —
/// the same flush path an inotify event on the notes file triggers.
///
/// Exists because inotify on `docs/notes.toml` is unreliable on the WSL
/// `/mnt/c` (Windows NTFS / 9P) deployment: a successfully-written note could
/// otherwise be NEVER indexed, leaving the index diverged from the file
/// indefinitely. Driving the reindex off this explicit writer signal — rather
/// than off a filesystem event that may never arrive — closes that gap.
///
/// Atomic, not RwLock-guarded, for the same reason as the reconcile signal: a
/// single one-shot bit, drained by exactly one consumer (the watch loop's
/// `swap(false, AcqRel)`), where coalescing two writes into one reindex walk is
/// correct because the notes reindex is idempotent.
pub type SharedNotesSignal = Arc<AtomicBool>;

/// Build the canonical pending-notes-signal handle, initialised to `false`
/// (no note write pending). The watch loop and the daemon thread each keep an
/// `Arc` clone; flipping it to `true` from any thread races safely with the
/// loop's `swap`.
pub fn shared_notes_signal() -> SharedNotesSignal {
    Arc::new(AtomicBool::new(false))
}

/// Event-driven freshness notifier: a single server-side park-and-wake
/// rather than a client-side poll loop.
///
/// The watch loop calls [`FreshNotifier::set_fresh`] on every
/// `publish_watch_snapshot` cycle (cheap when the value is unchanged —
/// just acquires the mutex, compares, releases). On a `false → true`
/// transition the inner `Condvar` notifies all parked waiters in one
/// shot. The daemon's `wait_fresh` handler parks on
/// [`FreshNotifier::wait_until_fresh`] until the notifier flips or the
/// caller's deadline expires.
///
/// Predicate-under-the-mutex pattern (not a generation counter): the
/// `is_fresh` boolean is mutated under the same lock the waiters park
/// on, so a publish that flips `false → true` between a waiter's
/// initial predicate read and its `wait_timeout` cannot be missed.
/// `Condvar::wait_timeout` re-checks the predicate after spurious
/// wake-ups (per `wait_until_fresh`'s loop body).
#[derive(Debug, Default)]
pub struct FreshNotifier {
    /// `true` ⇔ the most recent watch publish was `FreshnessState::Fresh`.
    is_fresh: std::sync::Mutex<bool>,
    /// Notified on every `false → true` transition. `notify_all` because
    /// the daemon may have multiple parked `wait_fresh` clients (one per
    /// blocked CLI eval gate); we want the lot to wake at once.
    cv: std::sync::Condvar,
}

impl FreshNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the cached freshness state. Notifies parked waiters when
    /// the value transitions `false → true`. Idempotent on
    /// `true → true` and `false → false` — the watch loop calls this
    /// every cycle (~100 ms) and the no-op path costs one mutex
    /// acquire + boolean compare.
    ///
    /// On a `RwLock` poison the function logs and returns without
    /// notifying — a poisoned mutex means a parked waiter panicked
    /// while holding the predicate, in which case the safest behaviour
    /// is silence rather than re-entering and risking a double panic.
    /// The waiter's own `wait_timeout` deadline still triggers a clean
    /// timeout.
    pub fn set_fresh(&self, fresh: bool) {
        let mut guard = match self.is_fresh.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::warn!("FreshNotifier mutex poisoned — recovering");
                poisoned.into_inner()
            }
        };
        if !*guard && fresh {
            *guard = fresh;
            // Drop guard before notify so a freshly-woken waiter doesn't
            // immediately block on us re-acquiring the lock.
            drop(guard);
            self.cv.notify_all();
        } else {
            *guard = fresh;
        }
    }

    /// Park until either the cached state is `fresh` or `deadline`
    /// expires. Returns `true` if the wake happened because the state
    /// flipped to fresh; `false` on deadline.
    ///
    /// Race-free against `set_fresh`: the predicate is read under the
    /// same mutex the notifier writes under, so a `false → true`
    /// transition between the initial read and the `wait_timeout` call
    /// would be observed by the predicate check inside the loop.
    /// Spurious wake-ups (allowed by `Condvar`) re-enter the loop and
    /// re-check.
    pub fn wait_until_fresh(&self, deadline: std::time::Instant) -> bool {
        let mut guard = match self.is_fresh.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        loop {
            if *guard {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            let timeout = deadline - now;
            let (g, result) = match self.cv.wait_timeout(guard, timeout) {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard = g;
            if result.timed_out() && !*guard {
                return false;
            }
        }
    }
}

/// Shared notifier handle for the watch loop (writer) and the daemon
/// thread (waiters). Cloned at startup, mirrors [`SharedWatchSnapshot`].
pub type SharedFreshNotifier = Arc<FreshNotifier>;

/// Build the canonical handle, initialised to `not fresh` (the watch
/// loop's first publish updates it).
pub fn shared_fresh_notifier() -> SharedFreshNotifier {
    Arc::new(FreshNotifier::new())
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
    /// Borrowed slot name set once at watch startup. Cloned into the
    /// snapshot's `active_slot: Option<String>`
    /// each tick so the daemon's status response can name what it's
    /// currently serving. The lifetime ties this borrow to the watch
    /// loop's owned `WatchState` field.
    pub active_slot: Option<&'a str>,
    /// Daemon socket clients currently in flight (sampled by the watch
    /// loop from the shared accept-loop counter). 0 without `--serve`.
    pub in_flight_clients: usize,
    /// Whether the shared reconcile signal is set but undrained.
    pub reconcile_pending: bool,
    /// Borrowed latency record of the last completed reindex pass.
    pub last_reindex: Option<&'a ReindexLatency>,
    /// Borrowed most-recent watch-loop error (sticky).
    pub last_error: Option<&'a WatchErrorInfo>,
    /// Pre-built status entries for sibling slots tracked by the
    /// slot-parallel propagation machinery. Appended after the active
    /// slot's entry in [`WatchOpsStats::slots`].
    pub sibling_slots: Vec<SlotWatchStatus>,
}

impl<'a> WatchSnapshotInput<'a> {
    /// Named-field constructor for the counter core; ops-block fields
    /// default to zero/`None` and are layered on via the builder
    /// methods below so existing call sites stay readable.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pending_files_count: usize,
        pending_notes: bool,
        rebuild_in_flight: bool,
        delta_saturated: bool,
        incremental_count: usize,
        dropped_this_cycle: usize,
        last_event: std::time::Instant,
        last_synced_at: Option<i64>,
    ) -> Self {
        Self {
            pending_files_count,
            pending_notes,
            rebuild_in_flight,
            delta_saturated,
            incremental_count,
            dropped_this_cycle,
            last_event,
            last_synced_at,
            active_slot: None,
            in_flight_clients: 0,
            reconcile_pending: false,
            last_reindex: None,
            last_error: None,
            sibling_slots: Vec::new(),
        }
    }

    /// Builder-style chain for the slot name. The watch loop calls
    /// `WatchSnapshotInput::new(...).with_active_slot(&s)`
    /// each tick so the snapshot publishes the slot the daemon is
    /// serving. Default is `None` to keep existing call sites that
    /// don't care about slot tracking compiling unchanged.
    pub fn with_active_slot(mut self, slot: &'a str) -> Self {
        self.active_slot = Some(slot);
        self
    }

    /// Builder-style chain for the `cqs status --watch` ops block
    ///. The watch loop samples the daemon's in-flight counter
    /// and the reconcile signal each tick, and borrows the
    /// last-reindex/last-error records off its owned `WatchState`.
    pub fn with_ops(
        mut self,
        in_flight_clients: usize,
        reconcile_pending: bool,
        last_reindex: Option<&'a ReindexLatency>,
        last_error: Option<&'a WatchErrorInfo>,
    ) -> Self {
        self.in_flight_clients = in_flight_clients;
        self.reconcile_pending = reconcile_pending;
        self.last_reindex = last_reindex;
        self.last_error = last_error;
        self
    }

    /// Builder-style chain for sibling-slot status entries. The watch
    /// loop builds these from its `SiblingSet` once per publish tick;
    /// they are appended after the active slot in the ops block's
    /// `slots` vec.
    pub fn with_sibling_slots(mut self, sibling_slots: Vec<SlotWatchStatus>) -> Self {
        self.sibling_slots = sibling_slots;
        self
    }
}

impl WatchSnapshot {
    /// Compute a fresh snapshot from the watch loop's owned state.
    /// Pure function — pulls only the fields it needs and resolves the
    /// `state` machine deterministically.
    pub fn compute(input: WatchSnapshotInput<'_>) -> Self {
        // Trace the freshness state machine so operators investigating "why
        // did the gate report Stale at 14:32:01?" can see what `compute()`
        // saw at that timestamp. `debug_span` keeps the per-tick noise behind
        // RUST_LOG=debug.
        let _span = tracing::debug_span!(
            "watch_snapshot_compute",
            pending_files = input.pending_files_count,
            pending_notes = input.pending_notes,
            rebuilding = input.rebuild_in_flight,
            dropped = input.dropped_this_cycle,
            delta_saturated = input.delta_saturated,
        )
        .entered();
        let state = if input.rebuild_in_flight {
            FreshnessState::Rebuilding
        } else if input.pending_files_count > 0
            || input.pending_notes
            || input.dropped_this_cycle > 0
            || input.delta_saturated
        {
            // A saturated delta means the rebuilt HNSW is discarded on swap;
            // the on-disk index is whatever was there before the rebuild
            // started. Treat as Stale until the next threshold rebuild
            // lands cleanly, so `cqs eval --require-fresh` waits.
            FreshnessState::Stale
        } else {
            FreshnessState::Fresh
        };

        // Anchor the last-event timestamp to wall-clock so consumers can
        // compute fresh idle (`now - last_event_unix_secs`) on read.
        // `WatchState.last_event` is an `Instant` (monotonic, no wall-clock
        // conversion), so reconstruct the wall-clock value as "now minus
        // elapsed". Saturating arithmetic handles the (in practice
        // unreachable) clock-before-epoch case symmetrically with
        // `now_unix_secs`. `unix_secs_i64()` falls back to 0 on epoch errors
        // with a once-per-process warn upstream.
        let last_event_unix_secs = crate::unix_secs_i64()
            .map(|now| {
                let elapsed_i64 =
                    i64::try_from(input.last_event.elapsed().as_secs()).unwrap_or(i64::MAX);
                now.saturating_sub(elapsed_i64)
            })
            .unwrap_or(0);

        // Emit the resolved state so the per-call decision is queryable
        // without rebuilding the state-machine inputs.
        tracing::trace!(state = %state, "compute decision");

        // Ops block. The per-slot vec leads with the active slot and
        // appends sibling-slot entries from the slot-parallel
        // propagation machinery. Built whenever the slot name is known —
        // `active_slot == None` only happens for synthetic inputs that
        // never reach the daemon wire.
        let mut slots = input
            .active_slot
            .map(|name| {
                vec![SlotWatchStatus {
                    name: name.to_string(),
                    state,
                    last_synced_at: input.last_synced_at,
                    last_reindex: input.last_reindex.cloned(),
                    queue_depth: u64::try_from(input.pending_files_count).unwrap_or(u64::MAX),
                    last_error: input.last_error.cloned(),
                }]
            })
            .unwrap_or_default();
        slots.extend(input.sibling_slots.iter().cloned());
        let ops = Some(WatchOpsStats {
            in_flight_clients: u64::try_from(input.in_flight_clients).unwrap_or(u64::MAX),
            reconcile_pending: input.reconcile_pending,
            last_reindex: input.last_reindex.cloned(),
            last_error: input.last_error.cloned(),
            slots,
        });

        Self {
            state,
            // Saturating `usize → u64`. The cast is total on every supported
            // platform, but the saturating shape costs nothing and keeps the
            // wire surface uniform — defense-in-depth against a usize that
            // happens to come from a wrapping counter elsewhere.
            modified_files: u64::try_from(input.pending_files_count).unwrap_or(u64::MAX),
            pending_notes: input.pending_notes,
            rebuild_in_flight: input.rebuild_in_flight,
            delta_saturated: input.delta_saturated,
            incremental_count: u64::try_from(input.incremental_count).unwrap_or(u64::MAX),
            dropped_this_cycle: u64::try_from(input.dropped_this_cycle).unwrap_or(u64::MAX),
            last_event_unix_secs,
            last_synced_at: input.last_synced_at,
            snapshot_at: now_unix_secs(),
            active_slot: input.active_slot.map(|s| s.to_string()),
            ops,
        }
    }
}

/// Thin delegator to the central [`crate::unix_secs_i64`] helper.
/// Watch-status snapshots prefer `Option<i64>` so the bad-clock case can
/// surface as JSON `null` rather than a silent `0`.
fn now_unix_secs() -> Option<i64> {
    let result = crate::unix_secs_i64();
    if result.is_none() {
        // Leave a per-call trace breadcrumb when the clock is bad. The
        // central helper already emits a once-per-process warn, but a
        // per-call trace lets operators correlate individual snapshot
        // publications with the bad-clock condition under RUST_LOG=trace
        // without re-firing the noisier warn.
        tracing::trace!("now_unix_secs: clock before epoch — returning None");
    }
    result
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
        WatchSnapshotInput::new(
            pending_files_count,
            pending_notes,
            rebuild_in_flight,
            false,
            0,
            dropped_this_cycle,
            std::time::Instant::now(),
            None,
        )
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

    /// Zero pending + zero saturation + rebuild in flight is the canonical
    /// "Rebuilding" path — operator sees a clean rebuild without any backlog.
    /// `is_fresh()` must be false because a rebuild is in flight (the daemon
    /// hasn't published the new chunks yet).
    #[test]
    fn compute_with_rebuild_in_flight_zero_pending_returns_rebuilding() {
        let snap = WatchSnapshot::compute(input(0, true, false, 0));
        assert_eq!(snap.state, FreshnessState::Rebuilding);
        assert!(!snap.is_fresh(), "Rebuilding must not be considered fresh");
        assert_eq!(snap.modified_files, 0);
        assert!(!snap.pending_notes);
        assert!(snap.rebuild_in_flight);
        assert_eq!(snap.dropped_this_cycle, 0);
        assert!(!snap.delta_saturated);
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

    /// A saturated delta means the in-flight rebuild's pending delta exceeded
    /// `MAX_PENDING_REBUILD_DELTA` and the rebuilt HNSW will be discarded on
    /// swap. Until the next threshold rebuild reads SQLite fresh, the on-disk
    /// index is stale. The flag is published; `compute()` must treat it as a
    /// Stale signal so `cqs eval --require-fresh` doesn't accept a doomed
    /// rebuild.
    #[test]
    fn delta_saturated_marks_stale_when_no_other_work() {
        let snap = WatchSnapshot::compute(WatchSnapshotInput::new(
            0,
            false,
            false,
            true,
            0,
            0,
            std::time::Instant::now(),
            None,
        ));
        assert_eq!(snap.state, FreshnessState::Stale);
        assert!(snap.delta_saturated);
    }

    /// `Rebuilding` still wins when the rebuild is in flight even with a
    /// saturated delta — the saturation will be observed when the rebuild
    /// drains and `rebuild_in_flight` flips to false.
    #[test]
    fn rebuild_in_flight_dominates_over_delta_saturated() {
        let snap = WatchSnapshot::compute(WatchSnapshotInput::new(
            0,
            false,
            true,
            true,
            0,
            0,
            std::time::Instant::now(),
            None,
        ));
        assert_eq!(snap.state, FreshnessState::Rebuilding);
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

    /// A reconcile signal starts cleared and round-trips through `swap`
    /// cleanly. Pin both halves so a refactor of `shared_reconcile_signal()`
    /// (e.g. switching to a notifier crate) can't silently regress to
    /// "always pending".
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

    // ===== cqs status --watch ops block =====

    fn full_ops_input<'a>(
        last_reindex: &'a ReindexLatency,
        last_error: &'a WatchErrorInfo,
    ) -> WatchSnapshotInput<'a> {
        WatchSnapshotInput::new(
            2,
            false,
            false,
            false,
            7,
            0,
            std::time::Instant::now(),
            Some(1_750_000_000),
        )
        .with_active_slot("default")
        .with_ops(3, true, Some(last_reindex), Some(last_error))
    }

    #[test]
    fn compute_populates_ops_block() {
        let lr = ReindexLatency {
            at_unix_secs: 1_750_000_100,
            duration_ms: 842,
            files: 4,
        };
        let le = WatchErrorInfo {
            at_unix_secs: 1_749_999_000,
            message: "Reindex error: disk full".to_string(),
        };
        let snap = WatchSnapshot::compute(full_ops_input(&lr, &le));

        let ops = snap.ops.as_ref().expect("compute must populate ops");
        assert_eq!(ops.in_flight_clients, 3);
        assert!(ops.reconcile_pending);
        assert_eq!(ops.last_reindex.as_ref(), Some(&lr));
        assert_eq!(ops.last_error.as_ref(), Some(&le));
        // Per-slot vec carries exactly the active slot, populated now —
        // not a dead placeholder.
        assert_eq!(ops.slots.len(), 1);
        let slot = &ops.slots[0];
        assert_eq!(slot.name, "default");
        assert_eq!(slot.state, snap.state);
        assert_eq!(slot.last_synced_at, Some(1_750_000_000));
        assert_eq!(slot.last_reindex.as_ref(), Some(&lr));
        // Active-slot queue depth mirrors modified_files; last_error
        // mirrors the ops-level sticky error.
        assert_eq!(slot.queue_depth, snap.modified_files);
        assert_eq!(slot.last_error.as_ref(), Some(&le));
    }

    /// Sibling-slot entries handed to `with_sibling_slots` are appended
    /// after the active slot in the ops block, verbatim. This is the
    /// wire shape the slot-parallel propagation publishes through.
    #[test]
    fn compute_appends_sibling_slot_entries() {
        let sibling = SlotWatchStatus {
            name: "exp-a".to_string(),
            state: FreshnessState::Stale,
            last_synced_at: Some(1_749_000_000),
            last_reindex: None,
            queue_depth: 4,
            last_error: Some(WatchErrorInfo {
                at_unix_secs: 1_749_000_100,
                message: "drain failed: locked".to_string(),
            }),
        };
        let snap = WatchSnapshot::compute(
            input(0, false, false, 0)
                .with_active_slot("default")
                .with_sibling_slots(vec![sibling.clone()]),
        );
        let ops = snap.ops.expect("ops present");
        assert_eq!(ops.slots.len(), 2, "active + one sibling");
        assert_eq!(ops.slots[0].name, "default");
        assert_eq!(ops.slots[1], sibling);
    }

    /// Old wire shape (per-slot entries without queue_depth/last_error)
    /// must still deserialize — the fields are serde-defaulted.
    #[test]
    fn slot_status_without_new_fields_deserializes() {
        let old = serde_json::json!({
            "name": "default",
            "state": "fresh",
            "last_synced_at": null,
            "last_reindex": null,
        });
        let entry: SlotWatchStatus = serde_json::from_value(old).expect("old slot shape");
        assert_eq!(entry.queue_depth, 0);
        assert!(entry.last_error.is_none());
    }

    #[test]
    fn compute_default_ops_is_zeroed_not_missing() {
        // Without `.with_ops(...)` the block still serializes (real
        // zeros), distinguishing "daemon publishes, nothing happened
        // yet" from "daemon predates the field" (`ops: None`).
        let snap = WatchSnapshot::compute(input(0, false, false, 0).with_active_slot("default"));
        let ops = snap.ops.expect("ops present on every computed snapshot");
        assert_eq!(ops.in_flight_clients, 0);
        assert!(!ops.reconcile_pending);
        assert!(ops.last_reindex.is_none());
        assert!(ops.last_error.is_none());
        assert_eq!(ops.slots.len(), 1);
    }

    /// CLI==daemon parity pin: both surfaces serialize/deserialize the
    /// same `WatchSnapshot` type (daemon's `dispatch_status` writes it,
    /// CLI's `daemon_status` reads it). A lossless serde round-trip of
    /// a fully-populated snapshot guarantees the two adapters can't
    /// disagree on the wire shape.
    #[test]
    fn ops_snapshot_serde_round_trip_is_lossless() {
        let lr = ReindexLatency {
            at_unix_secs: 1_750_000_100,
            duration_ms: 842,
            files: 4,
        };
        let le = WatchErrorInfo {
            at_unix_secs: 1_749_999_000,
            message: "Reindex error: disk full".to_string(),
        };
        let snap = WatchSnapshot::compute(full_ops_input(&lr, &le));
        let wire = serde_json::to_value(&snap).expect("serialize");
        let back: WatchSnapshot = serde_json::from_value(wire.clone()).expect("deserialize");
        let wire_again = serde_json::to_value(&back).expect("re-serialize");
        assert_eq!(wire, wire_again, "round trip must be lossless");
        assert_eq!(back.ops, snap.ops);
    }

    /// Back-compat: a snapshot from a daemon that predates the ops block (no
    /// `ops` key on the wire) must deserialize with `ops == None`, not
    /// error — `cqs status --watch` against an old daemon degrades to
    /// "stats unavailable" instead of a BadResponse.
    #[test]
    fn snapshot_without_ops_key_deserializes_to_none() {
        let old_wire = serde_json::json!({
            "state": "fresh",
            "modified_files": 0,
            "pending_notes": false,
            "rebuild_in_flight": false,
            "delta_saturated": false,
            "incremental_count": 0,
            "dropped_this_cycle": 0,
            "last_event_unix_secs": 1_750_000_000_i64,
            "last_synced_at": null,
            "snapshot_at": 1_750_000_001_i64,
        });
        let snap: WatchSnapshot = serde_json::from_value(old_wire).expect("old wire shape");
        assert!(snap.ops.is_none());
        assert!(snap.active_slot.is_none());
    }
}
