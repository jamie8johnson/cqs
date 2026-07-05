//! Daemon-scoped Tasks store (MCP Phase 3b floor, lane 1).
//!
//! The daemon owns exactly one long-running task tenant: a `cqs_eval` run
//! augmented with an MCP `task`. Tasks live here — in the daemon, the single
//! execution authority — NOT in a transport session, so a `cqs mcp --http`
//! front-end bounce never strands a `taskId`. Ownership is single-principal:
//! possession of the crypto-secure `taskId` is the capability handle (there is
//! no per-task ACL under the shared-token model; isolation rests on `taskId`
//! entropy plus the no-log posture below).
//!
//! Invariants this module enforces:
//!
//! - **Terminal publication order**: a worker fills the result slot BEFORE it
//!   flips `status` to a terminal value with `Release`; readers `load(Acquire)`
//!   the status first. A reader that ever observes a terminal status with an
//!   empty slot (a torn read) retries — it is NEVER an error.
//! - **Write-once terminal**: the first publish wins; a terminal state never
//!   transitions again.
//! - **Admission-enforced caps**: the retained-count cap AND the total retained
//!   bytes ceiling are checked at admission, before any entry is inserted or
//!   worker spawned — GC is a memory-reclaim backstop, never the cap enforcer.
//!   Both an at-cap admission and a single-flight-busy admission surface ONE
//!   byte-identical opaque `Unavailable`, so the reject is not a store-state
//!   oracle.
//! - **No `taskId` in logs / thread names**: the id is a capability handle;
//!   nothing here renders it into tracing, and the worker thread name (see
//!   [`runner`]) uses a non-secret monotonic job counter instead.
//! - **Freshness is load-bearing at read**: the epoch a run's substrate belongs
//!   to is stamped on the entry and compared to the live invalidation epoch when
//!   the result is read; a mismatch injects a staleness marker so a consumer
//!   never treats a 20-min-old verdict as describing the current index.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;

use crate::cli::json_envelope::ErrorCode;

pub(crate) mod runner;

/// Loom model of the TaskStore cross-thread publication + admission protocol (the
/// interleaving-auditor lane for the MCP 3b floor). Compiled ONLY under
/// `--cfg cqs_loom` AND a test build — `loom` is a dev-dependency and the module
/// `use`s it at module scope, so it must never reach a plain `cargo build`. Same
/// gate + run shape as the sibling `interleaving_model`s. Absent from a normal
/// build.
#[cfg(all(cqs_loom, test))]
mod publication_interleaving_model;

/// Multi-threaded stress harness for the real TaskStore (the integration
/// schedules loom can't bound: `HashMap` map lock, `std::thread` workers,
/// get/result reads racing publishes + the GC sweep). Test-only; a fast default
/// iteration count keeps the normal suite quick, `CQS_TEST_STRESS_ITERS` cranks it.
#[cfg(test)]
mod stress;

/// Max number of tasks (working + retained terminal) held at once. Checked at
/// admission: the `(N+1)`th augmented call is refused with the opaque
/// `Unavailable` and allocates nothing, so the store stays bounded even across a
/// missed GC sweep. In the 16–64 band the design specifies.
pub(crate) const RETAINED_TASK_COUNT_CAP: usize = 32;

/// Ceiling on the SUM of retained terminal-result bytes. The count cap alone
/// does not bound memory (N large `EvalOutput`s could be GBs), so admission also
/// refuses when the already-retained bytes reach this ceiling. Tens-of-MB band.
pub(crate) const RETAINED_TASK_BYTES_CEILING: usize = 32 * 1024 * 1024;

/// Byte cap on a fixture the daemon reads for an eval (task or direct). Bounds
/// an attacker-controlled in-root fixture so it cannot force an arbitrarily
/// large read. MB-range.
pub(in crate::cli::batch) const MAX_FIXTURE_BYTES: usize = 8 * 1024 * 1024;

/// Default retention (seconds) applied when an augmentation omits `ttl`.
/// Measured from the terminal instant; the count/bytes caps bound memory
/// regardless of ttl, so this only governs how long a *completed* result stays
/// retrievable. Short by design.
pub(crate) const DEFAULT_TASK_TTL_SECS: u64 = 900;

/// Crypto-secure task identifier — the capability handle.
///
/// 32 random bytes URL-safe-base64-no-pad (43 ASCII chars, 256 bits), the same
/// RNG grade `serve::auth::AuthToken::random` uses (`rand::random::<[u8; 32]>`,
/// `getrandom`-backed on Linux). Under the shared-token model the id must resist
/// guessing, so entropy is the isolation boundary. `Debug` is redacted so a
/// stray `?task_id` in a tracing call can never leak it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct TaskId(Arc<str>);

impl TaskId {
    /// Mint a fresh, unguessable id.
    fn generate() -> TaskId {
        let buf: [u8; 32] = rand::random();
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
        TaskId(Arc::from(encoded.as_str()))
    }

    /// Wrap a wire string for a lookup. No validation: an unknown id resolves to
    /// [`TaskLookup::Gone`] / [`TaskResultLookup::Gone`], never an oracle.
    pub(crate) fn from_wire(s: &str) -> TaskId {
        TaskId(Arc::from(s))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the raw capability handle — a `?task_id` in a tracing
        // call must not leak it into logs or crash traces.
        f.write_str("TaskId(redacted)")
    }
}

/// Task lifecycle status. `Working` is the sole non-terminal state; the three
/// terminal states are frozen once set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum TaskStatus {
    Working = 0,
    Completed = 1,
    Failed = 2,
    Cancelled = 3,
}

impl TaskStatus {
    fn from_u8(v: u8) -> TaskStatus {
        match v {
            1 => TaskStatus::Completed,
            2 => TaskStatus::Failed,
            3 => TaskStatus::Cancelled,
            // 0 and any unexpected byte read as the non-terminal state, which is
            // the safe default (a reader waits rather than reading an empty slot).
            _ => TaskStatus::Working,
        }
    }

    fn is_terminal(self) -> bool {
        !matches!(self, TaskStatus::Working)
    }

    /// The MCP wire string (`working`, not `in_progress`).
    pub(crate) fn as_wire(self) -> &'static str {
        match self {
            TaskStatus::Working => "working",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }
}

/// The terminal outcome a `tasks/result` read returns verbatim.
#[derive(Clone)]
pub(crate) enum TerminalKind {
    /// A successful JSON-RPC response carrying a CallToolResult body (for eval,
    /// the `EvalOutput` JSON). `is_error` is the tool-level error flag: it maps
    /// the STATUS to `failed`, but the body is still returned verbatim as a
    /// successful response.
    ToolResult {
        value: serde_json::Value,
        is_error: bool,
    },
    /// The call produced a JSON-RPC error; `tasks/result` returns that same
    /// error, not a synthetic CallToolResult.
    RpcError { code: ErrorCode, message: String },
}

impl TerminalKind {
    /// Approximate retained heap cost, summed at admission for the bytes ceiling.
    fn retained_bytes(&self) -> usize {
        match self {
            TerminalKind::ToolResult { value, .. } => {
                serde_json::to_string(value).map(|s| s.len()).unwrap_or(0)
            }
            TerminalKind::RpcError { message, .. } => message.len(),
        }
    }
}

/// Filled-once terminal payload plus the bookkeeping GC and the wire need.
struct TerminalResult {
    kind: TerminalKind,
    /// Monotonic instant the task terminalized — GC compares `finished_at + ttl`.
    finished_at: Instant,
    /// RFC3339 wall-clock stamp for the wire `lastUpdatedAt`.
    finished_at_iso: String,
}

/// Freshness marker injected into `_meta` when the index moved under a run.
/// A `tasks/result` consumer keys off `index_moved_since_run` to
/// refuse a verdict describing a substrate that no longer exists.
#[derive(Clone, serde::Serialize)]
pub(crate) struct StaleMeta {
    pub computed_at_epoch: u64,
    pub index_moved_since_run: bool,
}

/// Wire-facing snapshot of a task's identity + lifecycle fields. Returned by
/// admission and `tasks/get`; the transport shapes the two wire forms
/// (nested `CreateTaskResult`, flat `GetTaskResult`) from it.
pub(crate) struct TaskDescriptor {
    pub task_id: String,
    pub status: TaskStatus,
    pub created_at: String,
    pub last_updated_at: String,
    pub ttl_secs: Option<u64>,
}

impl std::fmt::Debug for TaskDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redact `task_id`: a `?descriptor` in a tracing call must not leak the
        // capability handle, same posture as `TaskId`'s Debug. The
        // wire response carries the id via explicit field access, not Debug.
        f.debug_struct("TaskDescriptor")
            .field("task_id", &"redacted")
            .field("status", &self.status)
            .field("created_at", &self.created_at)
            .field("last_updated_at", &self.last_updated_at)
            .field("ttl_secs", &self.ttl_secs)
            .finish()
    }
}

/// One task. Map ops touch only the `Arc<TaskEntry>`; per-entry state lives
/// behind the entry's own sync so `tasks/get` never contends the map for longer
/// than a clone.
pub(crate) struct TaskEntry {
    id: TaskId,
    /// Lock-free status for cheap reads. Written LAST on publish (`Release`),
    /// after the result slot is filled; readers `load(Acquire)` first.
    status: AtomicU8,
    /// Write-once terminal payload. `None` until the worker (or its
    /// publish-on-drop handle) fills it — filled BEFORE `status` flips terminal.
    result: Mutex<Option<TerminalResult>>,
    /// RFC3339 creation stamp (wire `createdAt`).
    created_at_iso: String,
    /// TTL seconds, or `None` for unlimited (spec: `number | null`). Governs GC
    /// retention only; the caps bound memory regardless.
    ttl_secs: Option<u64>,
    /// The `invalidation_epoch` the run's substrate snapshot belonged to. Read
    /// back and compared to the live epoch for the freshness-lie guard.
    spawn_epoch: u64,
    /// Retained bytes of the terminal result, lifted onto the entry so the
    /// admission byte-sum reads it without locking every result slot. `0` until
    /// terminal.
    retained_bytes: AtomicUsize,
}

impl TaskEntry {
    /// A fresh `working` entry, before insert.
    fn new_working(id: TaskId, spawn_epoch: u64, ttl_secs: Option<u64>) -> TaskEntry {
        TaskEntry {
            id,
            status: AtomicU8::new(TaskStatus::Working as u8),
            result: Mutex::new(None),
            created_at_iso: now_rfc3339(),
            ttl_secs,
            spawn_epoch,
            retained_bytes: AtomicUsize::new(0),
        }
    }

    fn status(&self) -> TaskStatus {
        TaskStatus::from_u8(self.status.load(Ordering::Acquire))
    }

    /// Publish the terminal outcome. Fills the result slot FIRST, then flips
    /// `status` with `Release` so a reader that observes a terminal
    /// status via `load(Acquire)` always sees the filled slot. Write-once: a
    /// second publish (e.g. the drop handle after a normal publish, or a late
    /// duplicate) is ignored.
    pub(in crate::cli::batch) fn publish(&self, kind: TerminalKind, status: TaskStatus) {
        let bytes = kind.retained_bytes();
        {
            let mut slot = self.result.lock().unwrap_or_else(|p| p.into_inner());
            if slot.is_some() {
                return;
            }
            *slot = Some(TerminalResult {
                kind,
                finished_at: Instant::now(),
                finished_at_iso: now_rfc3339(),
            });
        }
        self.retained_bytes.store(bytes, Ordering::Relaxed);
        // Flip status LAST, with Release, so `status.load(Acquire)` implies the
        // slot is filled.
        self.status.store(status as u8, Ordering::Release);
    }

    fn descriptor(&self) -> TaskDescriptor {
        let status = self.status();
        // `lastUpdatedAt` moves to the terminal instant once terminal; before
        // that it equals `createdAt`.
        let last_updated_at = if status.is_terminal() {
            self.result
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .map(|t| t.finished_at_iso.clone())
                .unwrap_or_else(|| self.created_at_iso.clone())
        } else {
            self.created_at_iso.clone()
        };
        TaskDescriptor {
            task_id: self.id.as_str().to_string(),
            status,
            created_at: self.created_at_iso.clone(),
            last_updated_at,
            ttl_secs: self.ttl_secs,
        }
    }

    /// GC predicate: a terminal task whose `terminal_at + ttl` has passed.
    /// Non-terminal and unlimited-ttl (`None`) entries are never reclaimed here.
    fn is_expired(&self, now: Instant) -> bool {
        if !self.status().is_terminal() {
            return false;
        }
        let Some(ttl) = self.ttl_secs else {
            return false;
        };
        let slot = self.result.lock().unwrap_or_else(|p| p.into_inner());
        match slot.as_ref() {
            Some(tr) => now.saturating_duration_since(tr.finished_at) > Duration::from_secs(ttl),
            // Terminal status but no slot yet (a torn read window) — keep it.
            None => false,
        }
    }
}

/// A non-blocking `tasks/get` outcome.
pub(crate) enum TaskLookup {
    Found(TaskDescriptor),
    /// Unknown / expired / GC'd id — the transport maps this to `-32602`, never
    /// an HTTP 404 (404 is session-expiry only).
    Gone,
}

/// A `tasks/result` read outcome.
pub(crate) enum TaskResultLookup {
    /// Terminal result available. `staleness` is `Some` only when the index
    /// moved since the run; the transport injects it into `_meta`.
    Ready {
        kind: TerminalKind,
        staleness: Option<StaleMeta>,
    },
    /// Known task, not yet terminal — OR a terminal-status-with-empty-slot torn
    /// read. The caller retries; this is NOT an error.
    Pending,
    Gone,
}

/// The daemon Tasks store. One instance is owned by `cmd_watch`, `Arc`-cloned
/// into the daemon thread (adopted into the `BatchContext`) and snapshotted into
/// every `BatchView` at checkout.
pub(crate) struct TaskStore {
    tasks: Mutex<HashMap<TaskId, Arc<TaskEntry>>>,
    /// Daemon-shutdown cancel token handed to every worker. Flipped by
    /// [`Self::signal_shutdown`] on teardown; `eval_core` checks it once per
    /// query so a long run bails within one query instead of pinning the process
    /// against systemd's stop timeout. Shutdown-only until a user-facing
    /// `tasks/cancel` lane reuses it.
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    /// Monotonic NON-SECRET job counter for the worker thread name
    /// — never the `taskId`.
    job_seq: AtomicU64,
}

impl TaskStore {
    pub(crate) fn new() -> Arc<TaskStore> {
        Arc::new(TaskStore {
            tasks: Mutex::new(HashMap::new()),
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            job_seq: AtomicU64::new(0),
        })
    }

    /// The shutdown cancel token a worker threads into `EvalCoreParams::cancel`.
    pub(in crate::cli::batch) fn shutdown_token(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    /// Signal daemon shutdown so in-flight workers bail at their next per-query
    /// check. Idempotent.
    pub(crate) fn signal_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Next non-secret worker-thread job id.
    pub(in crate::cli::batch) fn next_job_seq(&self) -> u64 {
        self.job_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Shared cap predicate — the single source of truth for both the
    /// fast-reject probe and the authoritative admit.
    fn is_full(map: &HashMap<TaskId, Arc<TaskEntry>>) -> bool {
        if map.len() >= RETAINED_TASK_COUNT_CAP {
            return true;
        }
        let retained: usize = map
            .values()
            .map(|e| e.retained_bytes.load(Ordering::Relaxed))
            .sum();
        retained >= RETAINED_TASK_BYTES_CEILING
    }

    /// Non-mutating cap probe. Lets admission fast-reject BEFORE it reads a
    /// fixture, so a saturated store does no I/O and allocates nothing.
    pub(in crate::cli::batch) fn at_capacity(&self) -> bool {
        let map = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        Self::is_full(&map)
    }

    /// Mint a fresh `taskId` and build a `working` entry for it. Kept separate
    /// from insertion so the caller can stamp the epoch/ttl before committing.
    pub(in crate::cli::batch) fn new_entry(
        &self,
        spawn_epoch: u64,
        ttl_secs: Option<u64>,
    ) -> Arc<TaskEntry> {
        Arc::new(TaskEntry::new_working(
            TaskId::generate(),
            spawn_epoch,
            ttl_secs,
        ))
    }

    /// Authoritative admission: under the map lock, re-check the caps and insert
    /// the entry. Returns `false` (allocating nothing) at cap — the caller maps
    /// `false` to the opaque `Unavailable`, byte-identical to single-flight-busy
    /// . Insert-then-spawn ordering: the caller spawns the worker
    /// only after this returns `true`, and the worker's publish-on-drop handle
    /// self-heals a spawn-fail orphan to `failed`.
    pub(in crate::cli::batch) fn try_admit(&self, entry: Arc<TaskEntry>) -> bool {
        let mut map = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        if Self::is_full(&map) {
            return false;
        }
        map.insert(entry.id.clone(), entry);
        true
    }

    /// Snapshot an entry's wire-facing descriptor — used by admission to return
    /// the freshly created task without a map round-trip.
    pub(in crate::cli::batch) fn describe(&self, entry: &Arc<TaskEntry>) -> TaskDescriptor {
        entry.descriptor()
    }

    /// Non-blocking status/timestamps read (`tasks/get`).
    pub(crate) fn get(&self, id: &TaskId) -> TaskLookup {
        let entry = {
            let map = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
            map.get(id).map(Arc::clone)
        };
        match entry {
            Some(e) => TaskLookup::Found(e.descriptor()),
            None => TaskLookup::Gone,
        }
    }

    /// Terminal-result read (`tasks/result`). `current_epoch` is the live
    /// invalidation epoch the caller reads from its own view; a mismatch against
    /// the stamped `spawn_epoch` injects the staleness marker.
    pub(crate) fn result(&self, id: &TaskId, current_epoch: u64) -> TaskResultLookup {
        let entry = {
            let map = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
            map.get(id).map(Arc::clone)
        };
        let Some(entry) = entry else {
            return TaskResultLookup::Gone;
        };
        // Read status (Acquire) before the slot, per the publish ordering.
        if !entry.status().is_terminal() {
            return TaskResultLookup::Pending;
        }
        let kind = {
            let slot = entry.result.lock().unwrap_or_else(|p| p.into_inner());
            match slot.as_ref() {
                Some(tr) => tr.kind.clone(),
                // Terminal status, empty slot: a torn read — retry, not error.
                None => return TaskResultLookup::Pending,
            }
        };
        let staleness = if entry.spawn_epoch != current_epoch {
            Some(StaleMeta {
                computed_at_epoch: entry.spawn_epoch,
                index_moved_since_run: true,
            })
        } else {
            None
        };
        TaskResultLookup::Ready { kind, staleness }
    }

    /// GC backstop (NOT the cap enforcer). Drops terminal entries past their
    /// `terminal_at + ttl`. `try_lock` so a busy map never blocks the accept
    /// loop; a skipped sweep is harmless because admission — not GC — bounds the
    /// store.
    pub(crate) fn sweep_expired(&self) {
        let now = Instant::now();
        let Ok(mut map) = self.tasks.try_lock() else {
            return;
        };
        map.retain(|_, entry| !entry.is_expired(now));
    }
}

/// Current time as an RFC3339 / ISO-8601 string (NOT epoch ms — the spec wants
/// the string form on every task response).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_entry(store: &Arc<TaskStore>, entry: Arc<TaskEntry>) -> TaskId {
        let id = entry.id.clone();
        assert!(store.try_admit(entry), "admit under cap must succeed");
        id
    }

    fn count(store: &Arc<TaskStore>) -> usize {
        store.tasks.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// The taskId is a capability handle; its `Debug` must never render the raw
    /// value, so a `?task_id` in a tracing call or a crash trace can't leak it.
    /// A future `#[derive(Debug)]` would silently un-redact it — this guard
    /// fails if the redaction is ever removed.
    #[test]
    fn task_id_debug_is_redacted() {
        let secret = "aGVsbG8td29ybGQtc2VjcmV0LXRhc2staWQtZXhhbXBsZS0x";
        let rendered = format!("{:?}", TaskId::from_wire(secret));
        assert!(
            !rendered.contains(secret),
            "TaskId Debug must not leak the raw capability handle, got {rendered:?}"
        );
        // Pin the redaction MARKER, not just the absence of the secret: an empty
        // (or otherwise gutted) `Debug` also happens not to contain the secret,
        // so the leak-absence check alone stays green if `fmt` is neutered to
        // write nothing. Requiring the "redacted" marker keeps the guard biting
        // when the render is silently emptied rather than un-redacted.
        assert!(
            rendered.contains("redacted"),
            "TaskId Debug must render the explicit redaction marker, got {rendered:?}"
        );
    }

    /// `TaskDescriptor`'s `Debug` redacts `task_id` with the same posture as
    /// `TaskId` — a `?descriptor` in a tracing call must never leak the handle.
    /// No existing test formats a descriptor with `{:?}`, so its redaction had
    /// zero coverage; this pins it (both leak-absence and the marker).
    #[test]
    fn descriptor_debug_redacts_task_id() {
        let secret = "aGVsbG8td29ybGQtc2VjcmV0LXRhc2staWQtZXhhbXBsZS0x";
        let entry = Arc::new(TaskEntry::new_working(
            TaskId::from_wire(secret),
            0,
            Some(60),
        ));
        let rendered = format!("{:?}", entry.descriptor());
        assert!(
            !rendered.contains(secret),
            "TaskDescriptor Debug must not leak the taskId, got {rendered:?}"
        );
        assert!(
            rendered.contains("redacted"),
            "TaskDescriptor Debug must render the redaction marker, got {rendered:?}"
        );
    }

    /// Every task response carries `createdAt` / `lastUpdatedAt` as RFC3339 /
    /// ISO-8601 strings (a D6 MUST — NOT epoch ms). Several tests build a
    /// descriptor and read `.status`, but NONE asserted the timestamp shape, so
    /// `now_rfc3339` returning `""` or garbage left the suite green. This pins
    /// the format at both the pre-terminal and terminal reads, and that
    /// `lastUpdatedAt` advances to the terminal instant.
    #[test]
    fn task_timestamps_are_rfc3339() {
        let store = TaskStore::new();
        let entry = store.new_entry(0, Some(60));

        // Pre-terminal: `lastUpdatedAt` equals `createdAt`, both valid RFC3339.
        let d0 = entry.descriptor();
        chrono::DateTime::parse_from_rfc3339(&d0.created_at)
            .expect("createdAt must be a valid RFC3339 string");
        assert_eq!(
            d0.last_updated_at, d0.created_at,
            "pre-terminal lastUpdatedAt equals createdAt"
        );

        // A tick so the terminal stamp is observably distinct from creation.
        std::thread::sleep(Duration::from_millis(2));
        entry.publish(
            TerminalKind::ToolResult {
                value: serde_json::json!({"ok": true}),
                is_error: false,
            },
            TaskStatus::Completed,
        );

        // Terminal: both stamps valid RFC3339; lastUpdatedAt moved to the finish
        // instant (never before creation).
        let d1 = entry.descriptor();
        let created = chrono::DateTime::parse_from_rfc3339(&d1.created_at)
            .expect("terminal createdAt must be a valid RFC3339 string");
        let updated = chrono::DateTime::parse_from_rfc3339(&d1.last_updated_at)
            .expect("terminal lastUpdatedAt must be a valid RFC3339 string");
        assert!(
            updated >= created,
            "terminal lastUpdatedAt ({}) advances to the finish instant, not before createdAt ({})",
            d1.last_updated_at,
            d1.created_at
        );
        // The terminal stamp MOVED — it is the finish instant, not a frozen copy
        // of createdAt. Pins the `descriptor()` terminal branch (which cargo-
        // mutants could only replace wholesale, an unviable Default); the 2ms
        // tick above guarantees a distinct RFC3339 rendering.
        assert_ne!(
            d1.last_updated_at, d1.created_at,
            "terminal lastUpdatedAt must move to the finish instant, not stay == createdAt"
        );
    }

    /// The GC horizon is STRICT: at exactly `finished_at + ttl` the task is NOT
    /// yet expired; it expires only strictly past it. The sweep test steps OVER
    /// the horizon (a 5ms sleep against a 0s ttl), so a `>`→`>=` off-by-one at
    /// the boundary survived it. This hits the horizon exactly by reading the
    /// terminal instant and feeding `is_expired` a controlled `now`.
    #[test]
    fn is_expired_horizon_is_strict() {
        let store = TaskStore::new();
        let ttl_secs = 60u64;
        let entry = store.new_entry(0, Some(ttl_secs));
        entry.publish(
            TerminalKind::ToolResult {
                value: serde_json::json!({}),
                is_error: false,
            },
            TaskStatus::Completed,
        );
        // The exact instant the predicate compares against.
        let finished_at = {
            let slot = entry.result.lock().unwrap_or_else(|p| p.into_inner());
            slot.as_ref().expect("published ⇒ slot filled").finished_at
        };
        let horizon = finished_at + Duration::from_secs(ttl_secs);
        assert!(
            !entry.is_expired(horizon),
            "at exactly finished_at + ttl the task is NOT expired (strict >, not >=)"
        );
        assert!(
            !entry.is_expired(horizon - Duration::from_nanos(1)),
            "just before the horizon: not expired"
        );
        assert!(
            entry.is_expired(horizon + Duration::from_nanos(1)),
            "one tick past the horizon: expired"
        );
    }

    /// `signal_shutdown` must flip the SAME `AtomicBool` that `shutdown_token`
    /// hands each worker (the seam-1d cancel side-channel). No test exercised the
    /// wiring, so a `shutdown_token` returning a fresh unshared token — or a
    /// `signal_shutdown` that did nothing — went unnoticed. This pins the share.
    #[test]
    fn signal_shutdown_flips_the_worker_token() {
        let store = TaskStore::new();
        let token = store.shutdown_token();
        assert!(
            !token.load(Ordering::Acquire),
            "the worker token starts un-signalled"
        );
        store.signal_shutdown();
        assert!(
            token.load(Ordering::Acquire),
            "signal_shutdown must flip the SAME token handed to workers"
        );
    }

    /// Admission refuses the `(N+1)`th task with the opaque `Unavailable` path
    /// (here: `try_admit` returns false), and the store stays bounded at the cap
    /// even without any GC sweep.
    #[test]
    fn admission_cap_bounds_store_without_gc() {
        let store = TaskStore::new();
        for _ in 0..RETAINED_TASK_COUNT_CAP {
            let e = store.new_entry(0, Some(60));
            assert!(store.try_admit(e), "each entry up to the cap is admitted");
        }
        assert_eq!(count(&store), RETAINED_TASK_COUNT_CAP);
        assert!(
            store.at_capacity(),
            "at the count cap the probe reports full"
        );
        let overflow = store.new_entry(0, Some(60));
        assert!(
            !store.try_admit(overflow),
            "the (N+1)th admission is refused"
        );
        // No GC ran; the store never exceeded the cap.
        assert_eq!(
            count(&store),
            RETAINED_TASK_COUNT_CAP,
            "store stays bounded across a missed sweep"
        );
    }

    /// The total-retained-bytes ceiling refuses admission even below the count
    /// cap (the count alone does not bound memory).
    #[test]
    fn admission_bytes_ceiling_refuses_below_count_cap() {
        let store = TaskStore::new();
        // One terminal entry whose retained bytes exceed the ceiling.
        let big = store.new_entry(0, Some(60));
        let big_value = serde_json::json!({
            "blob": "x".repeat(RETAINED_TASK_BYTES_CEILING + 1)
        });
        big.publish(
            TerminalKind::ToolResult {
                value: big_value,
                is_error: false,
            },
            TaskStatus::Completed,
        );
        assert!(store.try_admit(big), "first (huge) entry admits");
        assert!(
            count(&store) < RETAINED_TASK_COUNT_CAP,
            "well under count cap"
        );
        assert!(
            store.at_capacity(),
            "the bytes ceiling alone reports full below the count cap"
        );
        let next = store.new_entry(0, Some(60));
        assert!(
            !store.try_admit(next),
            "admission refused by the bytes ceiling, not the count cap"
        );
    }

    /// A terminal status published with an empty result slot (a torn-read
    /// window) reads back as `Pending` — retry, not error.
    #[test]
    fn terminal_status_empty_slot_is_retry_not_error() {
        let store = TaskStore::new();
        let entry = store.new_entry(7, Some(60));
        // Force the status terminal WITHOUT filling the slot (simulate the
        // instant between the two stores if they were reordered).
        entry
            .status
            .store(TaskStatus::Completed as u8, Ordering::Release);
        let id = insert_entry(&store, entry);
        assert!(
            matches!(store.result(&id, 7), TaskResultLookup::Pending),
            "terminal-status + empty-slot must be Pending, never an error"
        );
    }

    /// A normal publish fills the slot before flipping status, so a reader that
    /// sees terminal always sees the payload.
    #[test]
    fn publish_fills_slot_before_terminal_status() {
        let store = TaskStore::new();
        let entry = store.new_entry(3, Some(60));
        entry.publish(
            TerminalKind::ToolResult {
                value: serde_json::json!({"r": 1}),
                is_error: false,
            },
            TaskStatus::Completed,
        );
        let id = insert_entry(&store, entry);
        match store.result(&id, 3) {
            TaskResultLookup::Ready { kind, staleness } => {
                assert!(staleness.is_none(), "same epoch ⇒ no staleness marker");
                match kind {
                    TerminalKind::ToolResult { value, is_error } => {
                        assert_eq!(value, serde_json::json!({"r": 1}));
                        assert!(!is_error);
                    }
                    _ => panic!("expected ToolResult"),
                }
            }
            _ => panic!("expected Ready"),
        }
    }

    /// A read whose live epoch differs from the stamped spawn epoch injects the
    /// `_meta` staleness marker (the freshness-lie guard).
    #[test]
    fn epoch_mismatch_injects_staleness_marker() {
        let store = TaskStore::new();
        let entry = store.new_entry(5, Some(60));
        entry.publish(
            TerminalKind::ToolResult {
                value: serde_json::json!({"ok": true}),
                is_error: false,
            },
            TaskStatus::Completed,
        );
        let id = insert_entry(&store, entry);
        match store.result(&id, 9) {
            TaskResultLookup::Ready { staleness, .. } => {
                let marker = staleness.expect("epoch moved ⇒ staleness marker present");
                assert_eq!(marker.computed_at_epoch, 5);
                assert!(marker.index_moved_since_run);
            }
            _ => panic!("expected Ready with staleness"),
        }
    }

    /// An unknown / GC'd id resolves to `Gone` on both read paths (the transport
    /// maps `Gone` to `-32602`, never 404).
    #[test]
    fn unknown_id_is_gone() {
        let store = TaskStore::new();
        let ghost = TaskId::from_wire("does-not-exist");
        assert!(matches!(store.get(&ghost), TaskLookup::Gone));
        assert!(matches!(store.result(&ghost, 0), TaskResultLookup::Gone));
    }

    /// GC sweeps terminal + ttl-expired entries and leaves fresh + unlimited-ttl
    /// + still-working ones.
    #[test]
    fn gc_sweeps_only_terminal_ttl_expired() {
        let store = TaskStore::new();

        // Expired: terminal with a zero ttl already elapsed.
        let expired = store.new_entry(0, Some(0));
        expired.publish(
            TerminalKind::ToolResult {
                value: serde_json::json!({}),
                is_error: false,
            },
            TaskStatus::Completed,
        );
        let expired_id = insert_entry(&store, expired);

        // Fresh: terminal but a long ttl not yet elapsed.
        let fresh = store.new_entry(0, Some(3600));
        fresh.publish(
            TerminalKind::ToolResult {
                value: serde_json::json!({}),
                is_error: false,
            },
            TaskStatus::Completed,
        );
        let fresh_id = insert_entry(&store, fresh);

        // Unlimited: terminal with null ttl — never GC'd by ttl.
        let unlimited = store.new_entry(0, None);
        unlimited.publish(
            TerminalKind::ToolResult {
                value: serde_json::json!({}),
                is_error: false,
            },
            TaskStatus::Completed,
        );
        let unlimited_id = insert_entry(&store, unlimited);

        // Working: not terminal — never GC'd.
        let working = store.new_entry(0, Some(0));
        let working_id = insert_entry(&store, working);

        // Let the zero-ttl entry's horizon pass.
        std::thread::sleep(Duration::from_millis(5));
        store.sweep_expired();

        assert!(
            matches!(store.get(&expired_id), TaskLookup::Gone),
            "terminal + ttl-expired swept"
        );
        assert!(
            matches!(store.get(&fresh_id), TaskLookup::Found(_)),
            "terminal + ttl-fresh retained"
        );
        assert!(
            matches!(store.get(&unlimited_id), TaskLookup::Found(_)),
            "terminal + unlimited-ttl retained"
        );
        assert!(
            matches!(store.get(&working_id), TaskLookup::Found(_)),
            "still-working entry never swept"
        );
    }

    /// A failed eval surfaces the JSON-RPC error verbatim through the terminal
    /// read (the failed-task passthrough shape), with `failed` status.
    #[test]
    fn failed_task_returns_rpc_error_verbatim() {
        let store = TaskStore::new();
        let entry = store.new_entry(0, Some(60));
        entry.publish(
            TerminalKind::RpcError {
                code: ErrorCode::Unavailable,
                message: "boom".to_string(),
            },
            TaskStatus::Failed,
        );
        let id = insert_entry(&store, entry);
        match store.get(&id) {
            TaskLookup::Found(d) => assert_eq!(d.status, TaskStatus::Failed),
            _ => panic!("expected Found"),
        }
        match store.result(&id, 0) {
            TaskResultLookup::Ready { kind, .. } => match kind {
                TerminalKind::RpcError { code, message } => {
                    assert_eq!(code, ErrorCode::Unavailable);
                    assert_eq!(message, "boom");
                }
                _ => panic!("expected RpcError passthrough"),
            },
            _ => panic!("expected Ready"),
        }
    }
}
