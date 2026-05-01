//! Daemon GC sweeps and the in-memory `last_indexed_mtime` prune.
//!
//! Two flavors:
//! - **Startup GC** (`run_daemon_startup_gc`): one-shot at `cqs watch
//!   --serve` boot. Catches missing-file rows and retroactive gitignore
//!   pollution that the live watcher missed pre-v1.26.0.
//! - **Periodic GC** (`run_daemon_periodic_gc`): bounded idle-time sweep
//!   so a deeply-polluted index converges over many ticks rather than
//!   one stop-the-world prune.
//!
//! Plus `prune_last_indexed_mtime`, which trims the watch loop's dedup
//! map by recency to keep its memory bounded on long-running daemons.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cqs::parser::Parser as CqParser;
use cqs::store::Store;

/// #969: recency threshold for pruning `last_indexed_mtime`.
///
/// Entries older than this are dropped when the map grows past
/// `last_indexed_prune_size_threshold()`. 1 day is long enough to survive an
/// overnight idle (the map skips duplicate events on re-indexed files) but
/// short enough that stale entries from deleted/moved files age out without
/// a per-entry `stat()` syscall. Previously the prune called `Path::exists()`
/// on every entry, which stalls the watch thread on WSL 9P mounts (up to 5000
/// serial syscalls). The map's `SystemTime` values make the recency check a
/// pure in-memory comparison.
///
/// Tunable by editing this constant.
pub(super) const LAST_INDEXED_PRUNE_AGE_SECS: u64 = 86_400;

/// #969: default size threshold that triggers the `last_indexed_mtime` prune.
///
/// SHL-V1.30-9: env override `CQS_WATCH_PRUNE_SIZE_THRESHOLD`. The audit found
/// that `cqs ref` index sizes can exceed 5_000 entries, so the previous
/// "intentionally not an env var" stance was too restrictive. Documented in
/// README.md.
pub(super) const LAST_INDEXED_PRUNE_SIZE_THRESHOLD_DEFAULT: usize = 5_000;

/// SHL-V1.30-9: resolve `CQS_WATCH_PRUNE_SIZE_THRESHOLD` (default 5_000).
fn last_indexed_prune_size_threshold() -> usize {
    std::env::var("CQS_WATCH_PRUNE_SIZE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(LAST_INDEXED_PRUNE_SIZE_THRESHOLD_DEFAULT)
}

/// #969: O(n) in-memory prune of `last_indexed_mtime` by recency.
///
/// Replaces a per-entry `Path::exists()` loop that issued a `stat()` syscall
/// for every tracked file. On WSL 9P mounts, that stalled the watch thread for
/// seconds on bulk reindex cycles. The recency check is a `SystemTime`
/// comparison — no I/O.
///
/// Returns the number of entries removed (useful for tracing and tests).
pub(super) fn prune_last_indexed_mtime(map: &mut HashMap<PathBuf, SystemTime>) -> usize {
    if map.len() <= last_indexed_prune_size_threshold() {
        return 0;
    }
    let before = map.len();
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(LAST_INDEXED_PRUNE_AGE_SECS))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    map.retain(|_, mtime| *mtime >= cutoff);
    before - map.len()
}
/// #1024: Default cap on the number of distinct origins examined per
/// idle-time periodic GC tick. Keeps each tick short — at ~10k origins the
/// matcher walk is microseconds-scale, but capping keeps the write
/// transaction's lock window small even on much larger indexes. Override
/// with `CQS_DAEMON_PERIODIC_GC_CAP` (parsed at first read).
const DAEMON_PERIODIC_GC_CAP_DEFAULT: usize = 1000;

// #1024 / SHL-V1.29-9: Idle-time periodic GC interval and idle gap live
// in `super::limits` behind `daemon_periodic_gc_interval_secs()` and
// `daemon_periodic_gc_idle_secs()` so they honor
// `CQS_DAEMON_PERIODIC_GC_INTERVAL_SECS` / `CQS_DAEMON_PERIODIC_GC_IDLE_SECS`,
// matching the sibling `daemon_periodic_gc_cap()` resolver pattern below.

/// #1024 / SHL-V1.30-10: Resolve `CQS_DAEMON_PERIODIC_GC_CAP` on every call.
///
/// Previously cached in a `OnceLock`, which made `systemctl set-environment`
/// and `systemctl --user reload-or-restart cqs-watch` ineffective at retuning
/// the cap mid-process. One `getenv` per GC tick is microseconds; ticks are
/// minutes apart (see `daemon_periodic_gc_interval_secs()`). Matches
/// `reconcile_enabled()` semantics, where the env var is read each call.
fn daemon_periodic_gc_cap() -> usize {
    std::env::var("CQS_DAEMON_PERIODIC_GC_CAP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DAEMON_PERIODIC_GC_CAP_DEFAULT)
}

/// #1024: Run the daemon's startup GC sweep — Pass 1 (drop chunks for
/// files no longer on disk) and Pass 2 (drop chunks for paths now matched
/// by `.gitignore`). Runs once when `cqs watch --serve` starts, before
/// the first request is served. Both passes are best-effort: failures are
/// logged at warn and the daemon proceeds with whatever rows survived.
///
/// The eval-reliability motivating case: a `cqs index --force` on a
/// long-running index dropped chunk count by 30 % (15 517 → 10 748). The
/// extra 4 769 rows were a mix of deleted files and gitignored worktree
/// pollution that accumulated before v1.26.0 added gitignore-respect to
/// `cqs watch`. The startup pass closes that gap incrementally so the
/// daemon converges to the same state a `--force` reindex would produce,
/// without paying the embed cost.
///
/// Disable with `CQS_DAEMON_STARTUP_GC=0`.
pub(super) fn run_daemon_startup_gc(
    store: &Store,
    root: &Path,
    parser: &CqParser,
    matcher: Option<&ignore::gitignore::Gitignore>,
) {
    let _span = tracing::info_span!("daemon_startup_gc").entered();
    // OB-V1.30.1-7: capture elapsed time for the terminal log line so
    // operators can correlate startup-GC cost with daemon boot time in
    // journalctl.
    let start = std::time::Instant::now();

    if std::env::var("CQS_DAEMON_STARTUP_GC").as_deref() == Ok("0") {
        tracing::info!("CQS_DAEMON_STARTUP_GC=0 — daemon startup GC disabled");
        return;
    }

    // before/after counts are best-effort; if `stats()` fails we still run
    // the prunes (the alternative is silent skip on a transient SQLite
    // hiccup, which defeats the purpose of having a startup sweep).
    let before = store.stats().map(|s| s.total_chunks).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to read stats() before startup GC");
        0
    });

    // Pass 1: prune chunks for files no longer on disk. Re-uses the same
    // `prune_missing` path that `cqs gc` and `cqs index` call.
    let exts = parser.supported_extensions();
    let after_missing = match cqs::enumerate_files(root, &exts, false) {
        Ok(files) => {
            let file_set: std::collections::HashSet<_> = files.into_iter().collect();
            match store.prune_missing(&file_set, root) {
                Ok(n) => {
                    if n > 0 {
                        tracing::info!(pruned = n, "Daemon startup GC: pruned missing-file chunks");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Daemon startup GC: prune_missing failed — continuing");
                }
            }
            store.stats().map(|s| s.total_chunks).unwrap_or(before)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Daemon startup GC: enumerate_files failed — skipping prune_missing");
            before
        }
    };

    // Pass 2: retroactive gitignore prune. v1.26.0 only filters new events;
    // pre-v1.26.0 rows (or rows added by `cqs index` before the
    // gitignore-respect change) need this sweep to disappear.
    let after = if let Some(gi) = matcher {
        match store.prune_gitignored(gi, root, None) {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(pruned = n, "Daemon startup GC: pruned gitignored chunks");
                }
                store
                    .stats()
                    .map(|s| s.total_chunks)
                    .unwrap_or(after_missing)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Daemon startup GC: prune_gitignored failed — continuing");
                after_missing
            }
        }
    } else {
        tracing::debug!("No gitignore matcher available — skipping retroactive gitignore prune");
        after_missing
    };

    let pruned_missing = before.saturating_sub(after_missing);
    let pruned_ignored = after_missing.saturating_sub(after);
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    tracing::info!(
        before,
        after_missing,
        after,
        pruned_missing,
        pruned_ignored,
        elapsed_ms,
        "Daemon startup GC complete"
    );
}

/// #1024: Run the periodic idle-time GC sweep. Called from the main loop
/// when `last_event` is older than `daemon_periodic_gc_idle_secs()` and
/// the previous GC ran more than `daemon_periodic_gc_interval_secs()` ago.
///
/// Bounded: examines at most `daemon_periodic_gc_cap()` distinct origins
/// per pass so a single tick never holds the write transaction longer
/// than necessary. The cap means a deeply-polluted index converges over
/// many ticks rather than one big stop-the-world prune.
///
/// PF-V1.30.1-3 (#1226): when the caller has already enumerated the
/// working tree (e.g. because reconcile is also firing on the same
/// idle tick), pass `disk_files: Some(&shared_set)` to skip the
/// internal walk. `disk_files: None` falls back to the original
/// `cqs::enumerate_files` call.
///
/// Disable with `CQS_DAEMON_PERIODIC_GC=0`.
pub(super) fn run_daemon_periodic_gc(
    store: &Store,
    root: &Path,
    parser: &CqParser,
    matcher: Option<&ignore::gitignore::Gitignore>,
    disk_files: Option<&std::collections::HashSet<PathBuf>>,
) {
    let _span = tracing::info_span!("daemon_periodic_gc").entered();
    // OB-V1.30.1-7: capture elapsed time so operators can spot a
    // periodic-GC tick that wedges on a slow filesystem (WSL 9P, network
    // mount). The terminal log line was previously bare.
    let start = std::time::Instant::now();

    let cap = daemon_periodic_gc_cap();

    // Pass 1: missing-file prune. `enumerate_files` is the heavier call
    // here (one full walk of the tree); running it on idle is fine —
    // by definition there is no contention.
    //
    // PF-V1.30.1-3 (#1226): reuse the caller-supplied walk when present,
    // so a tick that fires both gc and reconcile only walks the tree
    // once. The fallback path keeps the function self-contained for
    // callers that haven't pre-walked (today: no one in production —
    // mod.rs always pre-walks when either gate fires — but the option
    // keeps the function testable in isolation).
    let exts = parser.supported_extensions();
    let walked: Option<std::collections::HashSet<PathBuf>> = if disk_files.is_none() {
        match cqs::enumerate_files(root, &exts, false) {
            Ok(files) => Some(files.into_iter().collect()),
            Err(e) => {
                tracing::warn!(error = %e, "Periodic GC: enumerate_files failed");
                None
            }
        }
    } else {
        None
    };
    let file_set: Option<&std::collections::HashSet<PathBuf>> = disk_files.or(walked.as_ref());
    if let Some(file_set) = file_set {
        match store.prune_missing(file_set, root) {
            Ok(n) if n > 0 => {
                tracing::info!(pruned = n, "Periodic GC: pruned missing-file chunks");
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "Periodic GC: prune_missing failed");
            }
        }
    }

    // Pass 2: bounded gitignore prune. `cap` limits how many origins this
    // tick examines, so a deeply-polluted index converges over many ticks
    // rather than one giant batch.
    if let Some(gi) = matcher {
        match store.prune_gitignored(gi, root, Some(cap)) {
            Ok(n) if n > 0 => {
                tracing::info!(
                    pruned = n,
                    cap,
                    "Periodic GC: pruned gitignored chunks (capped batch)"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "Periodic GC: prune_gitignored failed");
            }
        }
    }

    // OB-V1.30.1-7: terminal info line so journalctl shows a single
    // "tick complete" entry per cadence. Per-pass success lines stay at
    // info; no-op passes log nothing — this line gives the operator a
    // heartbeat regardless.
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::info!(elapsed_ms, cap, "Daemon periodic GC tick complete");
}
