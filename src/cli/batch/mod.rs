//! Batch mode — persistent Store + Embedder, JSONL output
//!
//! Reads commands from stdin, executes against a shared Store and lazily-loaded
//! Embedder, outputs compact JSON per line. Amortizes ~100ms Store open and
//! ~500ms Embedder ONNX init across N commands.
//!
//! Supports pipeline syntax: `search "error" | callers | test-map` chains
//! commands where upstream names feed downstream commands via fan-out.

mod commands;
mod handlers;
mod pipeline;
mod types;

pub(crate) use commands::{dispatch, BatchInput};
pub(crate) use pipeline::{execute_pipeline, has_pipe_token};

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

use anyhow::Result;
use clap::Parser;

use cqs::embedder::ModelConfig;
use cqs::index::VectorIndex;
use cqs::reference::ReferenceIndex;
use cqs::store::{ReadOnly, Store};
use cqs::Embedder;

use super::open_project_store_readonly;

/// Opaque identity of `index.db` used to detect that it has been replaced
/// or rewritten between two observations.
///
/// Combines inode (unix), size, and mtime. This catches:
///
/// - **Replacement via rename** (e.g. `cqs index --force` writes a fresh
///   `index.db.tmp` then renames it over `index.db`): the new inode
///   differs, so the identity changes even if size/mtime happened to
///   match.
/// - **In-place size change**: size differs.
/// - **Overwrite that kept the size**: mtime differs (modulo the
///   filesystem's mtime resolution).
///
/// ## Why not mtime alone?
///
/// DS-V1.25-6: WSL DrvFS / NTFS report mtime at 1-second resolution.
/// A tight `cqs index --force` followed by a daemon query burst could
/// share the same mtime bucket, causing `BatchContext` to keep serving
/// results from the orphaned inode. Mixing in inode and size closes
/// that sub-second race: the rename-over gives a new inode immediately,
/// regardless of whether the mtime ticked.
///
/// On non-unix platforms the inode fields are omitted and the struct
/// falls back to `(size, mtime)`; replacement on Windows still changes
/// the mtime and/or the size, so this is weaker than unix but strictly
/// better than mtime alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DbFileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    inode: u64,
    size: u64,
    mtime: Option<SystemTime>,
}

impl DbFileIdentity {
    /// Read the identity fields for `path`, returning `None` if the
    /// metadata stat fails (path missing, permission denied, etc.).
    fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        // mtime is best-effort — some exotic filesystems don't record
        // it. Falling back to `None` here still leaves inode + size as
        // useful discriminators.
        let mtime = meta.modified().ok();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Self {
                dev: meta.dev(),
                inode: meta.ino(),
                size: meta.len(),
                mtime,
            })
        }
        #[cfg(not(unix))]
        {
            Some(Self {
                size: meta.len(),
                mtime,
            })
        }
    }
}

/// Maximum batch stdin line length (1MB). Lines exceeding this are rejected
/// to prevent unbounded memory allocation from malicious input.
const MAX_BATCH_LINE_LEN: usize = 1_048_576;

/// Default idle timeout for ONNX sessions (embedder, reranker) in minutes.
/// After this many minutes without a command, sessions are cleared to free
/// memory. Matches watch mode's ~5-minute idle clear pattern. Override via
/// `CQS_BATCH_IDLE_MINUTES` (workstation users with 48GB VRAM can push to
/// 60+; laptops with shared GPU may want 2).
const DEFAULT_IDLE_TIMEOUT_MINUTES: u64 = 5;

/// Resolve the idle-timeout minutes from env; 0 disables eviction entirely.
fn idle_timeout_minutes() -> u64 {
    std::env::var("CQS_BATCH_IDLE_MINUTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_IDLE_TIMEOUT_MINUTES)
}

/// P2 #67 / P2 #68: longer idle window for the heavyweight data caches
/// (`hnsw`, `splade_index`, `call_graph`, `test_chunks`, `notes_cache`,
/// `file_set`). The ONNX-session timeout (`CQS_BATCH_IDLE_MINUTES`,
/// default 5 min) is tuned so the *next* user query stays responsive —
/// reloading an ONNX model is ~500 ms. The data caches cost much more to
/// rebuild (HNSW + SPLADE inverted index can take seconds), so we hold
/// them for a longer window before invalidating. 30 min mirrors the
/// audit-mode auto-expire window and is a safe default for an
/// interactive workstation.
///
/// Override via `CQS_BATCH_DATA_IDLE_MINUTES`. Set to `0` to disable
/// data-cache eviction entirely (the previous behavior).
const DEFAULT_DATA_CACHE_IDLE_MINUTES: u64 = 30;

/// Resolve the data-cache idle-timeout minutes from env; 0 disables data-
/// cache eviction entirely. P2 #67 / #68.
fn data_cache_idle_timeout_minutes() -> u64 {
    std::env::var("CQS_BATCH_DATA_IDLE_MINUTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DATA_CACHE_IDLE_MINUTES)
}

/// P2 #69: TTL for the `audit_state` reload cache. The audit-mode file
/// (`.cqs/audit-mode.json`) carries its own embedded `expires_at`, but the
/// daemon's `OnceLock` cached the loaded value forever — a user who flipped
/// `cqs audit-mode on` after the daemon booted, or whose audit window
/// auto-expired mid-session, kept seeing the stale state. Re-reading every
/// 30 s on each query is cheap (sub-ms file read) and bounds the staleness.
const AUDIT_STATE_RELOAD_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// P2 #69: TTL for the `config` reload cache. `.cqs/config.toml` edits
/// (e.g. tuning `splade_alpha` or `ef_search`) previously took effect only
/// after a daemon restart because the config was held in `OnceLock`. 5 min
/// is long enough to avoid hot-loop file reads while keeping ad-hoc config
/// tweaks usable without `systemctl restart cqs-watch`.
const CONFIG_RELOAD_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Default number of reference indexes kept in the LRU cache. A "reference"
/// is a sibling cqs project loaded via `@name` syntax. Memory-constrained
/// environments can keep 2; workstation users can bump via `CQS_REFS_LRU_SIZE`.
const DEFAULT_REFS_LRU_SIZE: usize = 2;

/// Resolve the refs-cache LRU size from env, clamping to at least 1 slot.
fn refs_lru_size() -> std::num::NonZeroUsize {
    let size = std::env::var("CQS_REFS_LRU_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_REFS_LRU_SIZE);
    // SAFETY: filter above guarantees size > 0; const fallback is 2.
    std::num::NonZeroUsize::new(size).unwrap_or(std::num::NonZeroUsize::new(1).unwrap())
}

/// Minimum interval between `fs::metadata` calls on `index.db` during a
/// batch session. PF-V1.25-10: `store()` is called on virtually every
/// handler hop, and `ctx.store()` calls `check_index_staleness` which in
/// turn calls `fs::metadata`. Most filesystem mtime resolutions are 1 ms
/// on Linux ext4 / WSL, so polling more often than ~100 ms cannot detect
/// anything mtime-based — we just pay a syscall per poll. 100 ms caps the
/// syscall rate at ~10 Hz per batch session while keeping reindex
/// detection latency well under a second.
const STALENESS_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// P2 #69: a cached value paired with the instant it was loaded. The
/// accessor consults `loaded_at.elapsed()` against a per-field reload
/// interval; once the cache is older than the interval the value is
/// re-loaded from the underlying source.
///
/// Replaces the prior `OnceLock<T>` pattern for `config` and `audit_state`
/// where the OnceLock cached the boot-time value forever — a documented
/// 30-min audit-mode auto-expire would never fire on a long-lived daemon,
/// and `.cqs/config.toml` edits required `systemctl restart cqs-watch`.
struct CachedReload<T> {
    value: T,
    loaded_at: Instant,
}

// ─── BatchContext ────────────────────────────────────────────────────────────

/// Shared resources for a batch session.
///
/// Store is opened once. Embedder and vector index are lazily initialized on
/// first use and cached for the session. References are cached per-name.
///
/// The CAGRA/HNSW index is held for the full session lifetime; this is
/// intentional. Rebuilding between commands would add seconds of latency.
/// VRAM cost: ~3-4 KB per vector (768-1024 dim × 4 bytes, depending on model), so 100k chunks ≈ 300 MB.
///
/// # Cache invalidation
///
/// **Stable caches** (embedder, reranker, config, audit_state) use `OnceLock`
/// and live for the session. ONNX sessions are cleared after idle timeout.
///
/// **Mutable caches** (hnsw, call_graph, test_chunks, file_set, notes_cache)
/// use `RefCell<Option<T>>` and are auto-invalidated when the index.db mtime
/// changes. This detects concurrent `cqs index` runs during long `cqs chat`
/// sessions. On invalidation, the Store is also re-opened since it has its own
/// internal `OnceLock` caches (call_graph_cache, test_chunks_cache).
///
/// Manual invalidation is available via the `refresh` batch command.
pub(crate) struct BatchContext {
    // Wrapped in RefCell so we can re-open it when the index changes.
    // Access via store() method which checks staleness first.
    //
    // #946 typestate: BatchContext is the daemon's shared store, which only
    // ever dispatches read-only queries (daemon handlers never mutate). The
    // compiler refuses to call a write method on a `Store<ReadOnly>`, so
    // the class of runtime errors from PR #945 / #944 / dispatch_gc is
    // structurally impossible on this path.
    store: RefCell<Store<ReadOnly>>,
    /// #968: the tokio runtime driving `store`. Kept here as well so
    /// `invalidate()` and `check_index_staleness()` can re-open the
    /// store on the same runtime — without this they would rebuild a
    /// fresh current-thread runtime on every index swap and drift
    /// apart from the daemon's shared pool.
    runtime: std::sync::Arc<tokio::runtime::Runtime>,
    // Stable caches — keep OnceLock (not index-derived)
    //
    // RM-V1.25-28: `OnceLock<Arc<Embedder>>` so the watch outer scope
    // can hand the same Embedder instance down into the daemon thread.
    // Previously BatchContext owned its own Embedder and the watch
    // loop owned a second one — two ~500 MB ONNX sessions could be
    // resident at the same time. `BatchContext::new_with_embedder`
    // accepts a pre-built Arc; `create_context` (CLI path) still
    // creates a fresh one lazily via `warm`.
    embedder: OnceLock<std::sync::Arc<Embedder>>,
    /// P2 #69: was `OnceLock`. Now `RefCell<Option<CachedReload<Config>>>`
    /// so a `.cqs/config.toml` edit shows up after `CONFIG_RELOAD_INTERVAL`
    /// (default 5 min) instead of requiring a daemon restart. The reload is
    /// a sub-ms file read; cost is negligible per query.
    config: RefCell<Option<CachedReload<cqs::config::Config>>>,
    reranker: OnceLock<cqs::Reranker>,
    /// P2 #69: was `OnceLock`. Now `RefCell<Option<CachedReload<AuditMode>>>`
    /// so the documented 30-min audit auto-expire actually fires while the
    /// daemon is up — previously the OnceLock cached the boot-time state
    /// forever. Reloads from `.cqs/audit-mode.json` every
    /// `AUDIT_STATE_RELOAD_INTERVAL` (default 30 s); the file already
    /// carries its own embedded `expires_at` so the load itself respects
    /// expiration.
    audit_state: RefCell<Option<CachedReload<cqs::audit::AuditMode>>>,
    // Mutable caches — RefCell<Option<T>> for invalidation on index change
    hnsw: RefCell<Option<std::sync::Arc<dyn VectorIndex>>>,
    base_hnsw: RefCell<Option<std::sync::Arc<dyn VectorIndex>>>,
    call_graph: RefCell<Option<std::sync::Arc<cqs::store::CallGraph>>>,
    test_chunks: RefCell<Option<std::sync::Arc<Vec<cqs::store::ChunkSummary>>>>,
    /// P3 #123: cache returns `Arc<HashSet<PathBuf>>` so callers don't clone
    /// the full set on every invocation. Mirrors `call_graph` / `test_chunks`.
    file_set: RefCell<Option<std::sync::Arc<HashSet<PathBuf>>>>,
    notes_cache: RefCell<Option<Vec<cqs::note::Note>>>,
    // Single-threaded by design — RefCell is correct, no Mutex needed
    // RM-27: Reduced from 4 to 2 — each ReferenceIndex holds Store + HNSW (50-200MB)
    refs: RefCell<lru::LruCache<String, ReferenceIndex>>,
    splade_encoder: OnceLock<Option<cqs::splade::SpladeEncoder>>,
    splade_index: RefCell<Option<cqs::splade::index::SpladeIndex>>,
    pub root: PathBuf,
    pub cqs_dir: PathBuf,
    pub model_config: cqs::embedder::ModelConfig,
    /// Last-seen identity (inode + size + mtime on unix; size + mtime
    /// elsewhere) of index.db, used to detect concurrent index updates.
    ///
    /// DS-V1.25-6: previously this tracked `SystemTime` alone. WSL NTFS
    /// has 1-s mtime resolution, so a fast `cqs index --force` plus a
    /// daemon query burst could share the same mtime bucket and keep
    /// serving results from the orphaned inode. `DbFileIdentity` mixes
    /// in inode + size so sub-second replacements still register.
    index_id: Cell<Option<DbFileIdentity>>,
    /// When the staleness check last ran. Used to rate-limit `fs::metadata`
    /// on `index.db` — see [`STALENESS_CHECK_INTERVAL`]. PF-V1.25-10.
    last_staleness_check: Cell<Option<Instant>>,
    pub(crate) error_count: AtomicU64,
    /// Tracks when the last command was processed.
    /// Used to clear ONNX sessions (embedder, reranker) after idle timeout.
    last_command_time: Cell<Instant>,
    /// Wall-clock instant when this `BatchContext` was constructed.
    ///
    /// Task B2: surfaces `uptime_secs` for `cqs ping`. Held as `Instant`
    /// rather than `SystemTime` so it's monotonic — daylight-savings or
    /// `ntpd` slewing won't cause a sudden negative uptime.
    started_at: Instant,
    /// Cumulative number of socket / stdin queries this `BatchContext` has
    /// dispatched. Bumped inside `dispatch_line` so both the daemon socket
    /// path and the `cqs batch` stdin path increment the same counter.
    /// Read by the `ping` handler.
    query_count: AtomicU64,
}

impl BatchContext {
    /// Check idle timeout and clear ONNX sessions if enough time has passed.
    ///
    /// Call this at the start of each command. Clears embedder and reranker
    /// sessions after `CQS_BATCH_IDLE_MINUTES` (default 5) of no commands,
    /// freeing ~500MB+. Set to 0 to disable eviction entirely. Sessions
    /// re-initialize lazily on next use.
    pub(crate) fn check_idle_timeout(&self) {
        self.sweep_idle_sessions();
        self.last_command_time.set(Instant::now());
    }

    /// Clear ONNX sessions if idle too long without resetting the clock.
    ///
    /// RM-V1.25-3: called both from `check_idle_timeout` (on command arrival)
    /// and from a periodic accept-loop tick (watch.rs), so a truly idle daemon
    /// still releases ~500MB+ after the idle timeout. Unlike
    /// `check_idle_timeout` it does NOT update `last_command_time`; the tick
    /// is a passive observer.
    ///
    /// SHL-V1.25-16: ONNX timeout is configurable via `CQS_BATCH_IDLE_MINUTES`
    /// (default 5). Set to 0 to disable ONNX eviction entirely.
    ///
    /// P2 #67 / P2 #68: also clears the data caches (`hnsw`, `splade_index`,
    /// `call_graph`, `test_chunks`, `notes_cache`, `file_set`) after a
    /// longer idle window, configurable via `CQS_BATCH_DATA_IDLE_MINUTES`
    /// (default 30 min). Without this, a daemon idle for hours holds 600 MB+
    /// of HNSW + SPLADE-index + call-graph caches that no agent is using.
    /// The split timeout (5 min ONNX, 30 min data) preserves first-query
    /// responsiveness — the next user query pays a sub-second ONNX init
    /// rather than a multi-second HNSW rebuild.
    pub(crate) fn sweep_idle_sessions(&self) {
        let timeout_minutes = idle_timeout_minutes();
        if timeout_minutes > 0 {
            let elapsed = self.last_command_time.get().elapsed();
            let timeout = std::time::Duration::from_secs(timeout_minutes * 60);
            if elapsed >= timeout {
                if let Some(emb) = self.embedder.get() {
                    emb.clear_session();
                    tracing::info!(
                        idle_minutes = elapsed.as_secs() / 60,
                        "Cleared embedder session after idle timeout"
                    );
                }
                if let Some(rr) = self.reranker.get() {
                    rr.clear_session();
                    tracing::info!(
                        idle_minutes = elapsed.as_secs() / 60,
                        "Cleared reranker session after idle timeout"
                    );
                }
                // RM-3: Also clear SPLADE encoder session
                if let Some(splade) = self.splade_encoder.get().and_then(|opt| opt.as_ref()) {
                    splade.clear_session();
                    tracing::info!(
                        idle_minutes = elapsed.as_secs() / 60,
                        "Cleared SPLADE session after idle timeout"
                    );
                }
            }
        }

        // P2 #67 / P2 #68: separate (longer) idle window for the heavyweight
        // data caches. Independent of the ONNX-session check above so an
        // operator can disable one without disabling the other.
        let data_timeout_minutes = data_cache_idle_timeout_minutes();
        if data_timeout_minutes == 0 {
            return;
        }
        let elapsed = self.last_command_time.get().elapsed();
        let data_timeout = std::time::Duration::from_secs(data_timeout_minutes * 60);
        if elapsed >= data_timeout {
            tracing::info!(
                idle_minutes = elapsed.as_secs() / 60,
                "Clearing data caches (hnsw / splade_index / call_graph / test_chunks / notes / file_set) after idle timeout"
            );
            // Reuses the same try_borrow_mut-tolerant code path as the
            // index-change invalidation, so a handler holding a Ref<...>
            // mid-query simply defers the eviction to the next sweep tick.
            self.invalidate_mutable_caches();
        }
    }

    /// Check if index.db identity changed since last access. If so, clear
    /// all mutable caches and re-open the Store (which resets its internal
    /// OnceLock caches like call_graph_cache, test_chunks_cache).
    ///
    /// DS-V1.25-6: identity is `(inode, size, mtime)` on unix and
    /// `(size, mtime)` elsewhere. The extra discriminators catch
    /// sub-second replacements on filesystems with 1-s mtime resolution
    /// (WSL NTFS/DrvFS): a `cqs index --force` rename-over yields a new
    /// inode immediately, so the batch session invalidates even when two
    /// events share the same mtime bucket.
    ///
    /// PF-V1.25-10: rate-limited to at most once per [`STALENESS_CHECK_INTERVAL`].
    /// Every `ctx.store()` and every `vector_index` / `file_set` / etc. accessor
    /// calls this; before the rate-limit it ran `fs::metadata` on every call,
    /// producing dozens of syscalls per pipelined batch command for no benefit.
    pub(crate) fn check_index_staleness(&self) {
        let now = Instant::now();
        if let Some(prev) = self.last_staleness_check.get() {
            if now.duration_since(prev) < STALENESS_CHECK_INTERVAL {
                return;
            }
        }
        self.last_staleness_check.set(Some(now));

        let index_path = self.cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let current_id = match DbFileIdentity::from_path(&index_path) {
            Some(id) => id,
            None => {
                // v1.22.0 audit EH-8: previously silent return. If the DB
                // becomes temporarily unstattable (permissions, concurrent
                // rebuild, NFS glitch), every subsequent command in the batch
                // session keeps using stale caches forever.
                tracing::warn!(
                    path = %index_path.display(),
                    "Cannot stat index.db for staleness check — caches may remain stale"
                );
                return;
            }
        };

        let last = self.index_id.get();
        if last.is_some() && last != Some(current_id) {
            let _span = tracing::info_span!("batch_index_invalidation").entered();
            tracing::info!("index.db identity changed, invalidating mutable caches");
            self.invalidate_mutable_caches();

            // Re-open the Store to reset its internal OnceLock caches.
            // #968: reuse the shared runtime so this re-open doesn't spin
            // up a transient current_thread runtime on every index swap.
            match Store::open_readonly_pooled_with_runtime(
                &index_path,
                std::sync::Arc::clone(&self.runtime),
            ) {
                Ok(new_store) => {
                    // DS-43: Check if index dimension changed — OnceLock model_config
                    // can't be cleared, so warn the user to restart the batch session.
                    let new_dim = new_store.dim();
                    if new_dim != self.model_config.dim {
                        tracing::warn!(
                            old_dim = self.model_config.dim,
                            new_dim = new_dim,
                            "Index dimension changed — queries may return wrong results until batch restart"
                        );
                    }
                    *self.store.borrow_mut() = new_store;
                    tracing::info!("Store re-opened after index change");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to re-open Store after index change");
                }
            }
        }
        self.index_id.set(Some(current_id));
    }

    /// Clear all mutable caches. Called on index identity change or manual refresh.
    ///
    /// v1.22.0 audit (CQ-2 / RM-3 / EH-8 / TC-2, quintuple-confirmed across
    /// five independent auditors): previously this omitted `splade_index`,
    /// so a long-lived batch session that had loaded the SPLADE posting
    /// map once would serve results from the pre-reindex generation forever
    /// after a concurrent `cqs index`. Clearing the RefCell here lets
    /// `ensure_splade_index` see `None` on the next call and rebuild from
    /// the freshly persisted `splade.index.bin` (or SQLite fallback).
    fn invalidate_mutable_caches(&self) {
        // Use try_borrow_mut: a search handler may still hold a Ref<...>
        // to splade_index or hnsw across an accessor call that triggers
        // staleness re-check (for example handlers/search.rs does
        // `let splade_index_ref = ctx.borrow_splade_index()` then later
        // calls `ctx.store().search_hybrid(...)`). Panicking on
        // borrow_mut() would crash the whole batch session for what is
        // just a deferral case. Slots that are busy stay populated; we
        // reset the rate-limit so the next accessor retries the
        // invalidation as soon as the in-flight handler releases its Ref.
        let mut all_clear = true;
        macro_rules! try_clear_to_none {
            ($field:expr, $name:literal) => {
                match $field.try_borrow_mut() {
                    Ok(mut g) => *g = None,
                    Err(_) => {
                        all_clear = false;
                        tracing::debug!(slot = $name, "borrow held; deferring invalidation");
                    }
                }
            };
        }
        try_clear_to_none!(self.hnsw, "hnsw");
        try_clear_to_none!(self.base_hnsw, "base_hnsw");
        try_clear_to_none!(self.call_graph, "call_graph");
        try_clear_to_none!(self.test_chunks, "test_chunks");
        try_clear_to_none!(self.file_set, "file_set");
        try_clear_to_none!(self.notes_cache, "notes_cache");
        try_clear_to_none!(self.splade_index, "splade_index");
        match self.refs.try_borrow_mut() {
            Ok(mut g) => g.clear(),
            Err(_) => {
                all_clear = false;
                tracing::debug!(slot = "refs", "borrow held; deferring invalidation");
            }
        }

        if !all_clear {
            // Reset rate-limit so the next accessor reattempts immediately
            // (rather than waiting STALENESS_CHECK_INTERVAL with stale caches).
            self.last_staleness_check.set(None);
            tracing::debug!("partial cache invalidation; will retry on next accessor");
        }
    }

    /// Manually invalidate all mutable caches and re-open the Store.
    /// Used by the `refresh` batch command.
    pub(crate) fn invalidate(&self) -> Result<()> {
        let _span = tracing::info_span!("batch_manual_invalidation").entered();
        self.invalidate_mutable_caches();

        let index_path = self.cqs_dir.join(cqs::INDEX_DB_FILENAME);
        // #968: pass the shared runtime so manual refreshes keep using
        // the same worker pool as the session they're refreshing.
        let new_store = Store::open_readonly_pooled_with_runtime(
            &index_path,
            std::sync::Arc::clone(&self.runtime),
        )
        .map_err(|e| anyhow::anyhow!("Failed to re-open Store: {e}"))?;
        *self.store.borrow_mut() = new_store;

        // Update identity to current so we don't immediately re-invalidate.
        if let Some(id) = DbFileIdentity::from_path(&index_path) {
            self.index_id.set(Some(id));
        }
        // PF-V1.25-10: treat the manual refresh as a fresh staleness check
        // so the next batch command hits the rate-limit fast path.
        self.last_staleness_check.set(Some(Instant::now()));

        tracing::info!("Manual cache invalidation complete");
        Ok(())
    }

    /// Dispatch a single command line (e.g. "search foo -n 5 --json") and
    /// write the JSON result to `out`. Used by the daemon socket handler.
    ///
    /// Task B2: every line that reaches the dispatcher bumps `query_count`
    /// (so the ping handler can report total queries served), and any
    /// parse / dispatch failure bumps `error_count` (so the daemon's
    /// `cmd_batch` stdin loop and the daemon socket handler converge on
    /// the same counter — previously only `cmd_batch` bumped `error_count`,
    /// leaving socket queries invisible).
    pub(crate) fn dispatch_line(&self, line: &str, out: &mut impl std::io::Write) {
        use crate::cli::json_envelope::error_codes;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        let tokens = match shell_words::split(trimmed) {
            Ok(t) => t,
            Err(e) => {
                self.error_count.fetch_add(1, Ordering::Relaxed);
                let msg = format!("Parse error: {e}");
                tracing::warn!(code = error_codes::PARSE_ERROR, error = %msg, "Batch dispatch_line: tokenization failed");
                let _ = write_envelope_error(out, error_codes::PARSE_ERROR, &msg);
                return;
            }
        };
        if tokens.is_empty() {
            return;
        }
        // D.2: NUL byte check parity with the daemon socket loop in cmd_batch.
        // Both surfaces share downstream handlers; they must share input
        // validation too. RT-INJ-2.
        if let Err(msg) = reject_null_tokens(&tokens) {
            self.error_count.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                code = error_codes::INVALID_INPUT,
                error = msg,
                "Batch dispatch_line: NUL byte in tokens"
            );
            let _ = write_envelope_error(out, error_codes::INVALID_INPUT, msg);
            return;
        }
        self.check_idle_timeout();
        // Bump query_count *after* the early returns above (empty input is
        // not a query). Counts both successes and errors — symmetric with
        // how `total_queries` is described in PingResponse.
        self.query_count.fetch_add(1, Ordering::Relaxed);
        match commands::BatchInput::try_parse_from(&tokens) {
            Ok(input) => match commands::dispatch(self, input.cmd) {
                Ok(value) => {
                    let _ = write_json_line(out, &value);
                }
                Err(e) => {
                    self.error_count.fetch_add(1, Ordering::Relaxed);
                    // P2 #33: redact_error walks the source chain and emits
                    // a stable (code, message) pair instead of echoing the
                    // raw anyhow chain (which can carry HTTP bodies, sqlite
                    // query text, filesystem paths). The full unredacted
                    // chain is logged via tracing::warn! inside redact_error
                    // so an operator can correlate by chain-id.
                    let (code, msg) = crate::cli::json_envelope::redact_error(&e);
                    let _ = write_envelope_error(out, code.as_str(), &msg);
                }
            },
            Err(e) => {
                self.error_count.fetch_add(1, Ordering::Relaxed);
                // Parse errors come from user-supplied tokens — they're
                // safe to surface verbatim and useful for the agent to
                // correct its query. No redaction needed.
                let msg = format!("{e:#}");
                tracing::warn!(code = error_codes::PARSE_ERROR, error = %msg, "Batch dispatch_line: clap parse failed");
                let _ = write_envelope_error(out, error_codes::PARSE_ERROR, &msg);
            }
        }
    }

    /// Build a [`cqs::daemon_translate::PingResponse`] snapshot of the
    /// daemon's current state.
    ///
    /// Task B2: pure read-side helper — bumps no counters, blocks no
    /// I/O, takes no locks. The `splade_loaded` / `reranker_loaded`
    /// flags peek the `OnceLock`s without forcing a load, so calling
    /// `ping` does not warm any ONNX session that wasn't already
    /// resident. `last_indexed_at` reads `index.db`'s mtime as the
    /// best available signal for "when did the index last change"; a
    /// missing file or unreadable metadata yields `None` rather than
    /// failing the whole ping.
    pub(crate) fn ping_snapshot(&self) -> cqs::daemon_translate::PingResponse {
        let last_indexed_at = std::fs::metadata(self.cqs_dir.join(cqs::INDEX_DB_FILENAME))
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        // SPLADE encoder slot is `OnceLock<Option<...>>`: only "loaded" if
        // the inner Option is Some. A user with no SPLADE model configured
        // populates the OnceLock with None on first sparse query; that's
        // not a "loaded" model from the operator's POV.
        let splade_loaded = self
            .splade_encoder
            .get()
            .map(|opt| opt.is_some())
            .unwrap_or(false);
        cqs::daemon_translate::PingResponse {
            model: self.model_config.name.clone(),
            // Cast usize→u32. Real models are 384 / 768 / 1024 dim; clamp
            // to u32::MAX rather than wrap if a future custom config goes
            // pathological. The cast is information-preserving in practice.
            dim: u32::try_from(self.model_config.dim).unwrap_or(u32::MAX),
            uptime_secs: self.started_at.elapsed().as_secs(),
            last_indexed_at,
            error_count: self.error_count.load(Ordering::Relaxed),
            total_queries: self.query_count.load(Ordering::Relaxed),
            splade_loaded,
            reranker_loaded: self.reranker.get().is_some(),
        }
    }

    /// Borrow the Store, checking for index staleness first.
    pub fn store(&self) -> std::cell::Ref<'_, Store<ReadOnly>> {
        self.check_index_staleness();
        self.store.borrow()
    }

    /// Pre-warm the embedder so the first query doesn't pay the ~500ms ONNX init.
    /// Called once at session start. Errors are logged but non-fatal.
    ///
    /// RM-V1.25-28: if the watch outer scope installed a shared Embedder
    /// via `adopt_embedder`, the OnceLock is already populated and this
    /// is a no-op for model loading (cache eviction still runs).
    pub fn warm(&self) {
        if self.embedder.get().is_none() {
            let _span = tracing::info_span!("batch_warm").entered();
            match Embedder::new(self.model_config.clone()) {
                Ok(e) => {
                    let _ = self.embedder.set(std::sync::Arc::new(e));
                    tracing::info!("Embedder pre-warmed");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Embedder warm failed — will retry on first query");
                }
            }
        }
        // RM-V1.25-5: Evict global EmbeddingCache at daemon startup.
        // `evict()` was previously only called at the tail of the full
        // `cqs index` pipeline (src/cli/pipeline/mod.rs), so long-lived
        // daemons / watch sessions on machines that never run a manual
        // index can grow the shared ~/.cache/cqs/embeddings.db past the
        // 10GB cap (CQS_CACHE_MAX_SIZE) without ever trimming. Kick off
        // a single post-warm eviction so the daemon self-heals on boot.
        //
        // #968: reuse the batch context's runtime so this one-shot open
        // doesn't spawn a fresh current_thread runtime.
        evict_global_embedding_cache_with_runtime(
            "daemon startup",
            Some(std::sync::Arc::clone(&self.runtime)),
        );
    }

    /// RM-V1.25-28: Install a shared Embedder from the outer watch scope.
    ///
    /// Returns `true` if the Arc was installed, `false` if the OnceLock was
    /// already populated (lazy init already happened, or another caller won
    /// the race). The caller can use this result to decide whether to fall
    /// back to its own lazily-initialized embedder.
    pub fn adopt_embedder(&self, shared: std::sync::Arc<Embedder>) -> bool {
        self.embedder.set(shared).is_ok()
    }

    /// Get or create the embedder (~500ms first call).
    pub fn embedder(&self) -> Result<&Embedder> {
        if let Some(e) = self.embedder.get() {
            return Ok(e.as_ref());
        }
        let _span = tracing::info_span!("batch_embedder_init").entered();
        let e = Embedder::new(self.model_config.clone())?;
        // Race is fine — OnceLock ensures only one value is stored
        let _ = self.embedder.set(std::sync::Arc::new(e));
        Ok(self
            .embedder
            .get()
            .map(|arc| arc.as_ref())
            .expect("embedder OnceLock populated by set() above"))
    }

    /// Get or lazily load the SPLADE encoder. Returns None if model unavailable.
    ///
    /// Path resolution is delegated to [`cqs::splade::resolve_splade_model_dir`]
    /// so the env-var override (`CQS_SPLADE_MODEL`) and vocab-mismatch probe
    /// stay consistent across the interactive (`cqs query`) and batch
    /// (`cqs search`) paths.
    pub fn splade_encoder(&self) -> Option<&cqs::splade::SpladeEncoder> {
        let opt = self.splade_encoder.get_or_init(|| {
            let model_dir = cqs::splade::resolve_splade_model_dir()?;
            match cqs::splade::SpladeEncoder::new(
                &model_dir,
                cqs::splade::SpladeEncoder::default_threshold(),
            ) {
                Ok(enc) => Some(enc),
                Err(e) => {
                    tracing::warn!(
                        path = %model_dir.display(),
                        error = %e,
                        "SPLADE encoder unavailable in batch mode"
                    );
                    None
                }
            }
        });
        opt.as_ref()
    }

    /// Ensure SPLADE index is loaded, then borrow it.
    /// Call `ensure_splade_index()` first, then `borrow_splade_index()`.
    ///
    /// Uses the same persist-and-load path as the single-shot CLI: tries
    /// `splade.index.bin` first, falls back to SQLite rebuild + persist if
    /// the file is absent, stale, or corrupt. Staleness is detected via
    /// the `splade_generation` metadata counter. If the generation cannot
    /// be read at all (audit EH-3), this returns without populating the
    /// RefCell — falling through with `0` would let a later persist write
    /// a gen-0 file whose header lies about the DB state, creating a
    /// self-perpetuating cache-poison loop.
    pub fn ensure_splade_index(&self) {
        self.check_index_staleness();
        if self.splade_index.borrow().is_some() {
            return;
        }
        let generation = match self.store().splade_generation() {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to read splade_generation — skipping SPLADE entirely for this \
                     batch session; search will fall back to dense-only"
                );
                return;
            }
        };
        let splade_path = self.cqs_dir.join(cqs::splade::index::SPLADE_INDEX_FILENAME);
        let store = self.store();
        // RM-V1.25-11: time the build so operators can diagnose first-query
        // latency spikes after a reindex. Full rebuild on a 200k-chunk repo
        // with SPLADE-Code 0.6B takes ~45 s — scoped-down fix in lieu of
        // an incremental update path; actual fix is tracked as P2 follow-up.
        // The `rebuilt` flag comes back from `load_or_build` so we can split
        // the log into a cheap cache hit vs a visible rebuild.
        let build_start = Instant::now();
        let (idx, rebuilt) = cqs::splade::index::SpladeIndex::load_or_build(
            &splade_path,
            generation,
            || match store.load_all_sparse_vectors() {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to load sparse vectors, falling back to cosine-only"
                    );
                    Vec::new()
                }
            },
        );
        let build_ms = build_start.elapsed().as_millis() as u64;
        if idx.is_empty() {
            // no vectors — cosine-only; leave the RefCell as None
            return;
        }
        if rebuilt {
            tracing::info!(
                chunks = idx.len(),
                tokens = idx.unique_tokens(),
                rebuild_ms = build_ms,
                "SPLADE index rebuilt from SQLite (batch)"
            );
            // Surface very-long rebuilds at warn so operators notice. Empirical
            // threshold: 10 s on a 200k-chunk SPLADE-Code index is already
            // "user waited visibly"; 30 s is "probably a problem."
            if build_ms > 30_000 {
                tracing::warn!(
                    rebuild_ms = build_ms,
                    chunks = idx.len(),
                    "SPLADE index rebuild exceeded 30s — first daemon query after \
                     reindex will have been blocked this long"
                );
            }
        } else {
            tracing::info!(
                chunks = idx.len(),
                tokens = idx.unique_tokens(),
                load_ms = build_ms,
                "SPLADE index loaded from disk (batch)"
            );
        }
        *self.splade_index.borrow_mut() = Some(idx);
    }

    /// Borrow the SPLADE index (call ensure_splade_index first).
    pub fn borrow_splade_index(
        &self,
    ) -> std::cell::Ref<'_, Option<cqs::splade::index::SpladeIndex>> {
        self.splade_index.borrow()
    }

    /// Get or build the vector index (CAGRA/HNSW/brute-force, cached).
    ///
    /// Checks index staleness before returning cached value. If the index.db
    /// changed, rebuilds the vector index from the fresh Store.
    /// Returns a cloneable Arc so callers can hold a reference past RefCell borrow scope.
    ///
    /// RM-V1.25-19: if the cached index reports `is_poisoned()` (only the
    /// CAGRA GPU backend currently does), the cache slot is cleared and a
    /// fresh index is built. Reusing a poisoned CUDA context risks
    /// double-free and CUDA faults.
    pub fn vector_index(&self) -> Result<Option<std::sync::Arc<dyn VectorIndex>>> {
        self.check_index_staleness();
        {
            let cached = self.hnsw.borrow();
            if let Some(arc) = cached.as_ref() {
                if arc.is_poisoned() {
                    tracing::warn!(
                        name = arc.name(),
                        "Cached vector index is poisoned — discarding and rebuilding"
                    );
                } else {
                    return Ok(Some(std::sync::Arc::clone(arc)));
                }
            }
        }
        // Clear any poisoned cache before rebuilding.
        *self.hnsw.borrow_mut() = None;
        let _span = tracing::info_span!("batch_vector_index_init").entered();
        let store = self.store.borrow();
        let idx = build_vector_index(&store, &self.cqs_dir, self.config().ef_search)?;
        let result = idx.map(|boxed| -> std::sync::Arc<dyn VectorIndex> { boxed.into() });
        let ret = result.clone();
        *self.hnsw.borrow_mut() = result;
        Ok(ret)
    }

    /// Get or build the base (non-enriched) vector index, cached.
    /// Returns `None` if the base index files don't exist or `CQS_DISABLE_BASE_INDEX=1`.
    pub fn base_vector_index(&self) -> Result<Option<std::sync::Arc<dyn VectorIndex>>> {
        self.check_index_staleness();
        {
            let cached = self.base_hnsw.borrow();
            if let Some(arc) = cached.as_ref() {
                return Ok(Some(std::sync::Arc::clone(arc)));
            }
        }
        let _span = tracing::info_span!("batch_base_vector_index_init").entered();
        let store = self.store.borrow();
        let idx = crate::cli::build_base_vector_index(&store, &self.cqs_dir)?;
        let result = idx.map(|boxed| -> std::sync::Arc<dyn VectorIndex> { boxed.into() });
        let ret = result.clone();
        *self.base_hnsw.borrow_mut() = result;
        Ok(ret)
    }

    /// Get a cached reference index by name, loading on first access.
    ///
    /// Uses cached config (RM-21) and loads only the target reference (RM-16),
    /// not all references.
    ///
    /// RM-V1.25-7: before serving a cached entry, peek its `is_stale()` so
    /// a concurrent `cqs ref update <name>` (which rewrites the reference's
    /// `index.db` without touching the primary project's `.cqs/index.db`)
    /// forces a fresh load. Without this, a long-lived daemon would keep
    /// serving results from a closed WAL snapshot / stale HNSW bytes for
    /// days.
    pub fn get_ref(&self, name: &str) -> Result<()> {
        let _span = tracing::info_span!("batch_get_ref", %name).entered();
        {
            let mut refs = self.refs.borrow_mut();
            if let Some(existing) = refs.peek(name) {
                if existing.is_stale() {
                    tracing::info!(
                        reference = %name,
                        "Cached reference stale (index.db mtime/size changed) — evicting for reload"
                    );
                    refs.pop(name);
                } else {
                    return Ok(());
                }
            }
        }

        let config = self.config();
        // Filter to just the target reference instead of loading all (RM-16)
        let single: Vec<_> = config
            .references
            .iter()
            .filter(|r| r.name == name)
            .cloned()
            .collect();
        if single.is_empty() {
            anyhow::bail!(
                "Reference '{}' not found. Run 'cqs ref list' to see available references.",
                name
            );
        }
        let loaded = cqs::reference::load_references(&single);
        let found = loaded.into_iter().next().ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to load reference '{}'. Run 'cqs ref update {}' first.",
                name,
                name
            )
        })?;
        self.refs.borrow_mut().put(name.to_string(), found);
        Ok(())
    }

    /// Get or build the file set for staleness checks (cached).
    ///
    /// P3 #123: returns `Arc<HashSet<PathBuf>>` so callers don't clone the
    /// full set per invocation. Mirrors `call_graph` / `test_chunks`.
    pub(super) fn file_set(&self) -> Result<std::sync::Arc<HashSet<PathBuf>>> {
        self.check_index_staleness();
        {
            let cached = self.file_set.borrow();
            if let Some(fs) = cached.as_ref() {
                return Ok(std::sync::Arc::clone(fs));
            }
        }
        let _span = tracing::info_span!("batch_file_set").entered();
        let exts: Vec<&str> = cqs::language::REGISTRY.supported_extensions().collect();
        let files = cqs::enumerate_files(&self.root, &exts, false)?;
        let set: HashSet<PathBuf> = files.into_iter().collect();
        let arc = std::sync::Arc::new(set);
        *self.file_set.borrow_mut() = Some(std::sync::Arc::clone(&arc));
        Ok(arc)
    }

    /// Get cached audit state. Reloads from `.cqs/audit-mode.json` when the
    /// cached value is older than [`AUDIT_STATE_RELOAD_INTERVAL`] (default
    /// 30 s), then returns an owned snapshot.
    ///
    /// P2 #69: previously `OnceLock<AuditMode>`, which cached the boot-time
    /// state forever. CLAUDE.md documents a 30-min auto-expire, but the
    /// daemon never re-read the file — so `cqs audit-mode on` after daemon
    /// boot, or audit-mode auto-expiring mid-session, both went unnoticed.
    /// The file is sub-ms to read; the 30 s interval bounds staleness while
    /// keeping accessor cost negligible. Returning owned `AuditMode`
    /// (rather than `&AuditMode` from a borrow) keeps the existing
    /// `let audit = ctx.audit_state(); &audit` call-site pattern working
    /// without juggling `Ref<'_, ...>` lifetimes.
    pub(super) fn audit_state(&self) -> cqs::audit::AuditMode {
        let needs_reload = match self.audit_state.borrow().as_ref() {
            Some(c) => c.loaded_at.elapsed() >= AUDIT_STATE_RELOAD_INTERVAL,
            None => true,
        };
        if needs_reload {
            let fresh = cqs::audit::load_audit_state(&self.cqs_dir);
            *self.audit_state.borrow_mut() = Some(CachedReload {
                value: fresh,
                loaded_at: Instant::now(),
            });
        }
        // Clone the cached value for the caller. AuditMode is small (bool +
        // Option<DateTime>), so the clone is cheap and frees the RefCell
        // borrow before any downstream code runs.
        self.audit_state
            .borrow()
            .as_ref()
            .expect("audit_state populated above")
            .value
            .clone()
    }

    /// Get cached notes (parsed once per session, invalidated on index change).
    pub(super) fn notes(&self) -> Vec<cqs::note::Note> {
        self.check_index_staleness();
        {
            let cached = self.notes_cache.borrow();
            if let Some(notes) = cached.as_ref() {
                return notes.clone();
            }
        }
        let notes_path = self.root.join("docs/notes.toml");
        let notes = if notes_path.exists() {
            match cqs::note::parse_notes(&notes_path) {
                Ok(notes) => notes,
                // P3 #97: split absent-file (TOCTOU after the .exists()
                // check above) from genuine parse failures, and include
                // the path in the warn so the journal isn't ambiguous
                // about which notes file failed.
                Err(e) => {
                    if let cqs::NoteError::Io(ref io_err) = e {
                        if io_err.kind() == std::io::ErrorKind::NotFound {
                            tracing::debug!(
                                path = %notes_path.display(),
                                "notes.toml disappeared between exists() and parse — treating as empty"
                            );
                            vec![]
                        } else {
                            tracing::warn!(
                                path = %notes_path.display(),
                                error = %e,
                                "Failed to parse notes.toml for batch"
                            );
                            vec![]
                        }
                    } else {
                        tracing::warn!(
                            path = %notes_path.display(),
                            error = %e,
                            "Failed to parse notes.toml for batch"
                        );
                        vec![]
                    }
                }
            }
        } else {
            vec![]
        };
        let result = notes.clone();
        *self.notes_cache.borrow_mut() = Some(notes);
        result
    }

    /// Borrow a reference index by name (must be loaded via `get_ref` first).
    ///
    /// Returns `None` if the reference hasn't been loaded yet.
    /// Uses `borrow_mut` because `LruCache::get()` promotes the entry (marks
    /// as recently used), which requires `&mut self`.
    pub fn borrow_ref(&self, name: &str) -> Option<std::cell::RefMut<'_, ReferenceIndex>> {
        let cache = self.refs.borrow_mut();
        if cache.contains(name) {
            Some(std::cell::RefMut::map(cache, |m| {
                m.get_mut(name).expect("checked contains above")
            }))
        } else {
            None
        }
    }

    /// Get or load the call graph (cached, invalidated on index change). (PERF-22)
    pub(super) fn call_graph(&self) -> Result<std::sync::Arc<cqs::store::CallGraph>> {
        self.check_index_staleness();
        {
            let cached = self.call_graph.borrow();
            if let Some(g) = cached.as_ref() {
                return Ok(std::sync::Arc::clone(g));
            }
        }
        let _span = tracing::info_span!("batch_call_graph_init").entered();
        let store = self.store.borrow();
        let g = store.get_call_graph()?;
        let result = std::sync::Arc::clone(&g);
        *self.call_graph.borrow_mut() = Some(g);
        Ok(result)
    }

    /// Get or load test chunks (cached, invalidated on index change).
    /// PERF-1: Returns Arc<Vec<ChunkSummary>> — O(1) clone.
    pub(super) fn test_chunks(&self) -> Result<std::sync::Arc<Vec<cqs::store::ChunkSummary>>> {
        self.check_index_staleness();
        {
            let cached = self.test_chunks.borrow();
            if let Some(tc) = cached.as_ref() {
                return Ok(std::sync::Arc::clone(tc));
            }
        }
        let _span = tracing::info_span!("batch_test_chunks_init").entered();
        let store = self.store.borrow();
        let tc = store.find_test_chunks()?;
        let result = std::sync::Arc::clone(&tc);
        *self.test_chunks.borrow_mut() = Some(tc);
        Ok(result)
    }

    /// Get cached project config. Reloads from `.cqs/config.toml` when the
    /// cached value is older than [`CONFIG_RELOAD_INTERVAL`] (default 5 min),
    /// then returns an owned snapshot.
    ///
    /// P2 #69 (originally RM-21): previously `OnceLock<Config>` which
    /// cached the boot-time config forever — `.cqs/config.toml` edits
    /// (e.g. `splade_alpha`, `ef_search`) required `systemctl restart
    /// cqs-watch`. The 5-minute interval is conservative enough to avoid
    /// hot-loop file reads while keeping ad-hoc tweaks usable. Returning
    /// owned `Config` keeps existing call sites unchanged
    /// (`self.config().ef_search` and `self.config().references` both
    /// work via auto-deref).
    pub(super) fn config(&self) -> cqs::config::Config {
        let needs_reload = match self.config.borrow().as_ref() {
            Some(c) => c.loaded_at.elapsed() >= CONFIG_RELOAD_INTERVAL,
            None => true,
        };
        if needs_reload {
            let fresh = cqs::config::Config::load(&self.root);
            *self.config.borrow_mut() = Some(CachedReload {
                value: fresh,
                loaded_at: Instant::now(),
            });
        }
        self.config
            .borrow()
            .as_ref()
            .expect("config populated above")
            .value
            .clone()
    }

    /// Get or create the reranker (cached for session). (RM-18)
    pub(super) fn reranker(&self) -> Result<&cqs::Reranker> {
        if let Some(r) = self.reranker.get() {
            return Ok(r);
        }
        let _span = tracing::info_span!("batch_reranker_init").entered();
        let r = cqs::Reranker::new().map_err(|e| anyhow::anyhow!("Reranker init failed: {e}"))?;
        let _ = self.reranker.set(r);
        Ok(self
            .reranker
            .get()
            .expect("reranker OnceLock populated by set() above"))
    }
}

/// Build the best available vector index for the store.
fn build_vector_index<Mode: crate::cli::store::ClearHnswDirty>(
    store: &Store<Mode>,
    cqs_dir: &std::path::Path,
    ef_search: Option<usize>,
) -> Result<Option<Box<dyn VectorIndex>>> {
    crate::cli::build_vector_index_with_config(store, cqs_dir, ef_search)
}

/// RM-V1.25-5: Evict the global embedding cache if it exceeds its size cap.
///
/// `EmbeddingCache::evict` is a no-op below `CQS_CACHE_MAX_SIZE` (default
/// 10GB), so it's cheap to call. Opens the cache (WAL-mode SQLite, one
/// connection), runs the eviction, then drops. Used by the daemon
/// startup and the watch reindex path to keep the shared cache bounded
/// even when the user never runs a full `cqs index`.
///
/// #968: takes an optional shared runtime so the daemon's one
/// multi-thread pool drives this open instead of spinning up a fresh
/// `current_thread` runtime. Pass `None` to fall back to the per-open
/// runtime constructor (used by non-daemon callers like `cqs index`).
pub(crate) fn evict_global_embedding_cache_with_runtime(
    trigger: &str,
    runtime: Option<std::sync::Arc<tokio::runtime::Runtime>>,
) {
    let _span = tracing::debug_span!("daemon_cache_evict", trigger).entered();
    let cache_path = cqs::cache::EmbeddingCache::default_path();
    let cache = match cqs::cache::EmbeddingCache::open_with_runtime(&cache_path, runtime) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %cache_path.display(),
                "Cache evict skipped — open failed"
            );
            return;
        }
    };
    match cache.evict() {
        Ok(n) if n > 0 => {
            tracing::info!(
                evicted = n,
                trigger,
                path = %cache_path.display(),
                "Global embedding cache evicted"
            );
        }
        Ok(_) => {
            tracing::debug!(trigger, "Global embedding cache under cap, no eviction");
        }
        Err(e) => {
            tracing::warn!(error = %e, trigger, "Global cache eviction failed");
        }
    }

    // P3 #124: same daemon tick also evicts the persistent QueryCache. The
    // QueryCache is per-user disk-resident and grew unbounded before the
    // 100 MB default cap landed; one shared tick keeps both caches honest
    // without a second timer.
    let q_path = cqs::cache::QueryCache::default_path();
    if q_path.exists() {
        match cqs::cache::QueryCache::open(&q_path) {
            Ok(qc) => match qc.evict() {
                Ok(n) if n > 0 => {
                    tracing::info!(
                        evicted = n,
                        trigger,
                        path = %q_path.display(),
                        "Query cache evicted"
                    );
                }
                Ok(_) => {
                    tracing::debug!(trigger, "Query cache under cap, no eviction");
                }
                Err(e) => {
                    tracing::warn!(error = %e, trigger, "Query cache eviction failed");
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, path = %q_path.display(), "Query cache evict skipped — open failed");
            }
        }
    }
}

// ─── JSON serialization helpers ──────────────────────────────────────────────

// `sanitize_json_floats` lives in `crate::cli::json_envelope` so all
// JSON-emitting surfaces (CLI `emit_json`, batch `write_json_line`, chat REPL)
// share one definition and one retry pattern. D.1 audit fix.
use crate::cli::json_envelope::sanitize_json_floats;

/// Wrap a payload in the standard envelope and serialize to a JSONL record on
/// stdout. Sanitizes NaN/Infinity before serialization to prevent serde_json
/// panics. Returns Err on write failure (broken pipe).
///
/// Callers pass the raw per-handler payload (a `serde_json::Value` from
/// `commands::dispatch`); this function wraps it with `{data, error: null,
/// version}` so every batch / daemon-socket line shares one shape. See
/// [`crate::cli::json_envelope`].
///
/// P2 #28: streams the envelope directly to `out` via a `Vec<u8>` buffer
/// + `serde_json::to_writer` instead of allocating a full intermediate
/// `serde_json::Value` for the wrap. Steady-state hot path is now
/// `to_writer(payload)` (no payload clone) plus three small literal writes
/// for the `{"data":..."error":null,"version":N}` shell. The retry-on-NaN
/// path falls back to the legacy `wrap_value` + sanitize pattern with one
/// clone — that's a rare failure mode (typed serde struct emitting NaN),
/// so the clone stays bounded to the recovery path. Saves multi-MB of
/// allocator churn per dispatched daemon query at scale.
fn write_json_line(
    out: &mut impl std::io::Write,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    // Steady-state: build the line in a `Vec<u8>` so the entire envelope
    // is one `writeln!` (avoids interleaved partial writes if `out` is a
    // shared TcpStream / UnixStream). Buffering also amortizes allocator
    // hits across many small literal writes.
    //
    // The envelope is opened by hand and the payload is streamed via
    // `to_writer` — no intermediate `Value` allocation. The version
    // literal is emitted as a constant so a future `JSON_OUTPUT_VERSION`
    // bump still flows through.
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    buf.extend_from_slice(b"{\"data\":");
    match serde_json::to_writer(&mut buf, value) {
        Ok(()) => {
            buf.extend_from_slice(b",\"error\":null,\"version\":");
            // Version is a small u32; emit as decimal text directly so we
            // don't pay another to_writer call for an integer.
            let version = crate::cli::json_envelope::JSON_OUTPUT_VERSION;
            buf.extend_from_slice(version.to_string().as_bytes());
            buf.push(b'}');
            buf.push(b'\n');
            out.write_all(&buf)
        }
        Err(_) => {
            // NaN / Infinity in the payload caused `to_writer` to fail
            // partway through. The buffer holds a half-written prefix
            // (`{"data":...`) — discard it and retry via the sanitize-
            // and-retry path that the CLI / chat surfaces share.
            // Mirrors `format_envelope_to_string`'s recovery semantics.
            let wrapped = crate::cli::json_envelope::wrap_value(value);
            let mut sanitized = wrapped;
            sanitize_json_floats(&mut sanitized);
            match serde_json::to_string(&sanitized) {
                Ok(s) => writeln!(out, "{}", s),
                Err(e) => {
                    tracing::warn!(error = %e, "JSON serialization failed after sanitization");
                    let fallback = crate::cli::json_envelope::wrap_error(
                        crate::cli::json_envelope::error_codes::INTERNAL,
                        "JSON serialization failed",
                    );
                    let s = serde_json::to_string(&fallback)
                        .unwrap_or_else(|_| String::from(r#"{"data":null,"error":{"code":"internal","message":"JSON serialization failed"},"version":1}"#));
                    writeln!(out, "{}", s)
                }
            }
        }
    }
}

/// Serialize a pre-built envelope error directly. Used by error-emission
/// sites that already need an envelope error (rather than wrapping a raw
/// payload). Skips the success-path wrap performed by [`write_json_line`].
fn write_envelope_error(
    out: &mut impl std::io::Write,
    code: &str,
    message: &str,
) -> std::io::Result<()> {
    let env = crate::cli::json_envelope::wrap_error(code, message);
    match serde_json::to_string(&env) {
        Ok(s) => writeln!(out, "{}", s),
        Err(_) => writeln!(
            out,
            r#"{{"data":null,"error":{{"code":"internal","message":"JSON serialization failed"}},"version":1}}"#
        ),
    }
}

/// RT-INJ-2: Reject token sequences containing NUL bytes. Returns the
/// canonical error string (caller passes to [`write_envelope_error`] with
/// `error_codes::INVALID_INPUT`) on rejection, `Ok(())` otherwise.
///
/// D.2 audit fix: the daemon socket loop (`cmd_batch` stdin path at
/// `cmd_batch`) and the daemon socket handler (`BatchContext::dispatch_line`)
/// share the same downstream handlers but had divergent input validation —
/// the CLI dispatch_line path missed the NUL check. Centralizing here
/// keeps both call sites in lock-step on the rejection contract.
fn reject_null_tokens(tokens: &[String]) -> Result<(), &'static str> {
    if tokens.iter().any(|t| t.contains('\0')) {
        Err("Input contains null bytes")
    } else {
        Ok(())
    }
}

// ─── Main loop ───────────────────────────────────────────────────────────────

/// Create a shared batch context: open store, prepare lazy caches.
///
/// Used by both `cmd_batch` and `cmd_chat`.
pub(crate) fn create_context() -> Result<BatchContext> {
    create_context_with_runtime(None)
}

/// #968: Variant that reuses a caller-supplied tokio runtime so the daemon
/// (`watch_and_serve`) can build one `Arc<Runtime>` at process start and
/// hand the same handle to both its outer read-write Store and the batch
/// context's read-only Store. Subsequent `EmbeddingCache` / `QueryCache`
/// opens through [`BatchContext::warm`] pick up the same runtime via
/// [`cqs::Store::runtime`]. When `runtime` is `None`, behaves exactly as
/// the pre-968 `create_context` and constructs its own current-thread
/// runtime for the read-only Store.
pub(crate) fn create_context_with_runtime(
    runtime: Option<std::sync::Arc<tokio::runtime::Runtime>>,
) -> Result<BatchContext> {
    let root = super::config::find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");
    if !index_path.exists() {
        anyhow::bail!("Index not found. Run 'cqs init && cqs index' first.");
    }
    let store = if let Some(rt) = runtime {
        Store::open_readonly_pooled_with_runtime(&index_path, rt).map_err(|e| {
            anyhow::anyhow!("Failed to open index at {}: {}", index_path.display(), e)
        })?
    } else {
        let (s, _root, _cqs_dir) = open_project_store_readonly()?;
        s
    };
    // #968: cache the store's runtime Arc so subsequent re-opens and
    // lazily-opened caches stay on the same pool.
    let runtime = std::sync::Arc::clone(store.runtime());

    // Capture initial index.db identity (inode/size/mtime on unix).
    // DS-V1.25-6: previously this was mtime alone, which sub-second
    // replacements on WSL NTFS could miss.
    let index_id = DbFileIdentity::from_path(&cqs_dir.join(cqs::INDEX_DB_FILENAME));
    if index_id.is_none() {
        tracing::debug!("Could not stat index.db — staleness detection will be skipped until first successful stat");
    }

    // Index-aware model resolution: prefer the model recorded in the store
    // metadata over CQS_EMBEDDING_MODEL / config / default. Without this,
    // running `CQS_EMBEDDING_MODEL=foo` against a `bar`-model index gives
    // silent zero-result queries (the dim mismatch only surfaces as a
    // tracing::warn! deep in the index backend). See ROADMAP.md "Embedder
    // swap workflow".
    let stored_model = store.stored_model_name();
    let project_config = cqs::config::Config::load(&root);
    let model_config = ModelConfig::resolve_for_query(
        stored_model.as_deref(),
        None,
        project_config.embedding.as_ref(),
    )
    .apply_env_overrides();

    Ok(BatchContext {
        store: RefCell::new(store),
        runtime,
        embedder: OnceLock::new(),
        // P2 #69: was OnceLock — see field doc.
        config: RefCell::new(None),
        reranker: OnceLock::new(),
        // P2 #69: was OnceLock — see field doc.
        audit_state: RefCell::new(None),
        hnsw: RefCell::new(None),
        base_hnsw: RefCell::new(None),
        call_graph: RefCell::new(None),
        test_chunks: RefCell::new(None),
        file_set: RefCell::new(None),
        notes_cache: RefCell::new(None),
        splade_encoder: OnceLock::new(),
        splade_index: RefCell::new(None),
        refs: RefCell::new(lru::LruCache::new(refs_lru_size())),
        root,
        cqs_dir,
        model_config,
        index_id: Cell::new(index_id),
        // PF-V1.25-10: None means the first check runs unconditionally; the
        // 100ms rate-limit kicks in only after the first successful stat.
        last_staleness_check: Cell::new(None),
        error_count: AtomicU64::new(0),
        last_command_time: Cell::new(Instant::now()),
        // Task B2: `started_at` is captured here so `uptime_secs` in the
        // ping response measures from BatchContext creation — which is the
        // meaningful event for the daemon (the embedder load may be later).
        started_at: Instant::now(),
        query_count: AtomicU64::new(0),
    })
}

/// Create a BatchContext for testing with a temporary store.
///
/// Visibility: `pub(in crate::cli::batch)` under `#[cfg(test)]` so submodule
/// tests (handlers/search.rs tests for issue #973) can reuse the same fixture
/// wiring as the in-file `mod tests`.
///
/// The store is opened RO at the SQLite connection level via
/// [`Store::open_readonly_after_init`] (#986) — the DB is expected to be
/// pre-initialized by `setup_test_store` so the closure is a no-op, but
/// the constructor path matches production code that may need fixture setup.
#[cfg(test)]
pub(in crate::cli::batch) fn create_test_context(
    cqs_dir: &std::path::Path,
) -> Result<BatchContext> {
    let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    // #986: open_readonly_after_init returns Store<ReadOnly> directly —
    // the unsafe into_readonly() type-erasure is gone.
    let store = Store::<ReadOnly>::open_readonly_after_init(&index_path, |_| Ok(()))
        .map_err(|e| anyhow::anyhow!("Failed to open test store: {e}"))?;
    let root = cqs_dir.parent().unwrap_or(cqs_dir).to_path_buf();
    let index_id = DbFileIdentity::from_path(&index_path);
    // #968: cache the runtime Arc so test contexts re-open on the same pool.
    let runtime = std::sync::Arc::clone(store.runtime());

    Ok(BatchContext {
        store: RefCell::new(store),
        runtime,
        embedder: OnceLock::new(),
        // P2 #69: was OnceLock — see field doc.
        config: RefCell::new(None),
        reranker: OnceLock::new(),
        // P2 #69: was OnceLock — see field doc.
        audit_state: RefCell::new(None),
        hnsw: RefCell::new(None),
        base_hnsw: RefCell::new(None),
        call_graph: RefCell::new(None),
        test_chunks: RefCell::new(None),
        file_set: RefCell::new(None),
        notes_cache: RefCell::new(None),
        splade_encoder: OnceLock::new(),
        splade_index: RefCell::new(None),
        refs: RefCell::new(lru::LruCache::new(refs_lru_size())),
        root,
        cqs_dir: cqs_dir.to_path_buf(),
        model_config: ModelConfig::resolve(None, None).apply_env_overrides(),
        index_id: Cell::new(index_id),
        last_staleness_check: Cell::new(None),
        error_count: AtomicU64::new(0),
        last_command_time: Cell::new(Instant::now()),
        // Task B2: same fields as production constructor — keep parity so
        // ping-handler tests against `create_test_context` see realistic
        // counter / uptime values.
        started_at: Instant::now(),
        query_count: AtomicU64::new(0),
    })
}

/// Entry point for `cqs batch`.
pub(crate) fn cmd_batch() -> Result<()> {
    let _span = tracing::info_span!("cmd_batch").entered();

    let ctx = create_context()?;
    ctx.warm(); // Pre-warm embedder so first query doesn't pay ~500ms ONNX init

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut reader = std::io::BufReader::new(stdin.lock());

    // SEC-1: read_line allocates incrementally (8KB chunks) until newline or EOF.
    // A multi-GB line without newlines could OOM before the post-hoc check below.
    // Accepted risk: batch input is from a controlling process (AI agent or pipe),
    // not from untrusted network input. The 1MB check prevents processing, not allocation.
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read stdin line");
                break;
            }
        };

        // Reject lines exceeding 1MB to prevent further processing.
        if line.len() > MAX_BATCH_LINE_LEN {
            ctx.error_count.fetch_add(1, Ordering::Relaxed);
            // Hardcoded JSON — no serialization needed, no NaN risk
            if writeln!(stdout, r#"{{"error":"Line too long (max 1MB)"}}"#).is_err() {
                break;
            }
            let _ = stdout.flush();
            continue;
        }

        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Quit/exit
        if trimmed.eq_ignore_ascii_case("quit") || trimmed.eq_ignore_ascii_case("exit") {
            break;
        }

        // Tokenize the line
        let tokens = match shell_words::split(trimmed) {
            Ok(t) => t,
            Err(e) => {
                ctx.error_count.fetch_add(1, Ordering::Relaxed);
                let msg = format!("Parse error: {}", e);
                tracing::warn!(
                    code = crate::cli::json_envelope::error_codes::PARSE_ERROR,
                    error = %msg,
                    "Batch cmd_batch: tokenization failed"
                );
                if write_envelope_error(
                    &mut stdout,
                    crate::cli::json_envelope::error_codes::PARSE_ERROR,
                    &msg,
                )
                .is_err()
                {
                    break;
                }
                let _ = stdout.flush();
                continue;
            }
        };

        if tokens.is_empty() {
            continue;
        }

        // D.2: NUL byte rejection via shared helper. Both this stdin loop
        // and `BatchContext::dispatch_line` (daemon socket handler) share
        // the same downstream commands and must share the same input
        // validation. RT-INJ-2.
        if let Err(msg) = reject_null_tokens(&tokens) {
            ctx.error_count.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                code = crate::cli::json_envelope::error_codes::INVALID_INPUT,
                error = msg,
                "Batch cmd_batch: NUL byte in tokens"
            );
            if write_envelope_error(
                &mut stdout,
                crate::cli::json_envelope::error_codes::INVALID_INPUT,
                msg,
            )
            .is_err()
            {
                break;
            }
            continue;
        }

        // Check idle timeout — clear ONNX sessions if idle too long
        ctx.check_idle_timeout();

        // Pipeline detection: if tokens contain a standalone `|`, route to pipeline
        if pipeline::has_pipe_token(&tokens) {
            match pipeline::execute_pipeline(&ctx, &tokens, trimmed) {
                Ok(value) => {
                    if write_json_line(&mut stdout, &value).is_err() {
                        break;
                    }
                }
                Err(pe) => {
                    ctx.error_count.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        code = pe.code,
                        error = %pe.message,
                        "Batch cmd_batch: pipeline failed"
                    );
                    if write_envelope_error(&mut stdout, pe.code, &pe.message).is_err() {
                        break;
                    }
                }
            }
        } else {
            // Single command — existing path
            match commands::BatchInput::try_parse_from(&tokens) {
                Ok(input) => match commands::dispatch(&ctx, input.cmd) {
                    Ok(value) => {
                        if write_json_line(&mut stdout, &value).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        ctx.error_count.fetch_add(1, Ordering::Relaxed);
                        // P2 #33: redact_error walks the source chain and
                        // emits a stable (code, message) pair instead of
                        // echoing the raw anyhow chain. Full unredacted
                        // chain is logged via tracing::warn! inside
                        // redact_error for operator correlation.
                        let (code, msg) = crate::cli::json_envelope::redact_error(&e);
                        if write_envelope_error(&mut stdout, code.as_str(), &msg).is_err() {
                            break;
                        }
                    }
                },
                Err(e) => {
                    ctx.error_count.fetch_add(1, Ordering::Relaxed);
                    let msg = format!("{e:#}");
                    tracing::warn!(
                        code = crate::cli::json_envelope::error_codes::PARSE_ERROR,
                        error = %msg,
                        "Batch cmd_batch: clap parse failed"
                    );
                    if write_envelope_error(
                        &mut stdout,
                        crate::cli::json_envelope::error_codes::PARSE_ERROR,
                        &msg,
                    )
                    .is_err()
                    {
                        break;
                    }
                }
            }
        }

        let _ = stdout.flush();
    }

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::store::ModelInfo;
    use std::thread;
    use std::time::Duration;

    /// Create a temp dir with an initialized index.db for testing.
    fn setup_test_store() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let store = Store::open(&index_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();
        drop(store);
        (dir, cqs_dir)
    }

    #[test]
    fn test_invalidate_clears_mutable_caches() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // Populate mutable caches
        *ctx.file_set.borrow_mut() = Some(std::sync::Arc::new(HashSet::new()));
        *ctx.notes_cache.borrow_mut() = Some(vec![]);
        *ctx.call_graph.borrow_mut() = Some(std::sync::Arc::new(
            cqs::store::CallGraph::from_string_maps(Default::default(), Default::default()),
        ));
        *ctx.test_chunks.borrow_mut() = Some(std::sync::Arc::new(vec![]));

        // Verify caches are populated
        assert!(ctx.file_set.borrow().is_some());
        assert!(ctx.notes_cache.borrow().is_some());
        assert!(ctx.call_graph.borrow().is_some());
        assert!(ctx.test_chunks.borrow().is_some());

        // Invalidate
        ctx.invalidate().unwrap();

        // Verify all mutable caches are cleared
        assert!(ctx.file_set.borrow().is_none());
        assert!(ctx.notes_cache.borrow().is_none());
        assert!(ctx.call_graph.borrow().is_none());
        assert!(ctx.test_chunks.borrow().is_none());
        assert!(ctx.hnsw.borrow().is_none());
    }

    #[test]
    fn test_mtime_staleness_detection() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // Populate a cache
        *ctx.notes_cache.borrow_mut() = Some(vec![]);
        assert!(ctx.notes_cache.borrow().is_some());

        // First staleness check — sets baseline mtime, no invalidation
        ctx.check_index_staleness();
        assert!(
            ctx.notes_cache.borrow().is_some(),
            "First check should not invalidate"
        );

        // Touch index.db to simulate concurrent `cqs index`
        // Sleep to ensure mtime changes (filesystem granularity is ~1s on some FS)
        thread::sleep(Duration::from_secs(2));
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        // Append a byte to force mtime change
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&index_path)
                .unwrap();
            file.write_all(b" ").unwrap();
            file.sync_all().unwrap();
        }

        // Second staleness check — mtime changed, should invalidate
        ctx.check_index_staleness();
        assert!(
            ctx.notes_cache.borrow().is_none(),
            "Mtime change should invalidate cache"
        );
    }

    /// DS-V1.25-6: BatchContext freshness detection must catch a rename-over
    /// replacement even if the new file's mtime happens to match the old one.
    /// Previously the check used `SystemTime` alone, so on WSL NTFS (1-s mtime
    /// resolution) a tight `cqs index --force` + query burst could re-use a
    /// stale pool against the orphaned inode. The fix mixes inode + size
    /// into the identity so the rename-over is detected immediately.
    #[cfg(unix)]
    #[test]
    fn test_sub_second_rename_replacement_invalidates_cache() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // Populate a cache and run the first check to capture baseline identity.
        *ctx.notes_cache.borrow_mut() = Some(vec![]);
        ctx.check_index_staleness();
        assert!(
            ctx.notes_cache.borrow().is_some(),
            "First check should not invalidate"
        );

        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let original_mtime = std::fs::metadata(&index_path).unwrap().modified().unwrap();
        let original_ino = std::fs::metadata(&index_path).unwrap().ino();

        // Build a fresh SQLite DB in a sibling path, then rename it over the
        // original. The new file has a distinct inode — this is exactly the
        // `cqs index --force` rename-over pattern.
        let replacement = cqs_dir.join("index.db.replacement");
        let store = Store::open(&replacement).unwrap();
        store.init(&ModelInfo::default()).unwrap();
        drop(store);

        // Force-set mtime on the replacement to match the original so we are
        // explicitly testing the inode-based discriminator rather than an
        // incidental mtime bump.
        {
            use std::fs::File;
            let f = File::open(&replacement).unwrap();
            f.set_modified(original_mtime).unwrap();
        }
        std::fs::rename(&replacement, &index_path).unwrap();

        // Sanity: the replacement changed the inode even though mtime matches.
        let new_meta = std::fs::metadata(&index_path).unwrap();
        assert_ne!(
            new_meta.ino(),
            original_ino,
            "Test precondition: rename-over must change inode"
        );
        assert_eq!(
            new_meta.modified().unwrap(),
            original_mtime,
            "Test precondition: mtime matches — this is the sub-second race",
        );

        // PF-V1.25-10 added a 100ms rate-limit on staleness checks. The setup
        // above (create replacement Store + init + drop + rename) is faster
        // than that on modern disks, so clear the throttle so the check runs.
        ctx.last_staleness_check.set(None);

        // The staleness check should now invalidate even though mtime is
        // identical. Without the DS-V1.25-6 fix this would silently pass
        // through and keep the stale cache.
        ctx.check_index_staleness();
        assert!(
            ctx.notes_cache.borrow().is_none(),
            "DS-V1.25-6: rename-over replacement (same mtime, new inode) should invalidate cache"
        );
    }

    #[test]
    fn test_stable_caches_survive_invalidation() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // P2 #69: audit_state moved from OnceLock to RefCell<Option<CachedReload>>
        // for time-bounded reload. Populate the slot directly so the test does
        // not depend on a real .cqs/audit-mode.json being present.
        *ctx.audit_state.borrow_mut() = Some(CachedReload {
            value: cqs::audit::AuditMode {
                enabled: false,
                expires_at: None,
            },
            loaded_at: Instant::now(),
        });

        // Invalidate mutable caches (does NOT touch time-bounded caches like
        // audit_state — it survives index-change invalidation).
        ctx.invalidate().unwrap();

        // Verify the slot survives index-change invalidation. (It may still
        // be reloaded later by the accessor's TTL-driven refresh; the
        // invariant tested here is "invalidate() does not clear it".)
        assert!(
            ctx.audit_state.borrow().is_some(),
            "audit_state should survive invalidate (only TTL reload clears it)"
        );
    }

    #[test]
    fn test_refresh_command_parses() {
        let input = commands::BatchInput::try_parse_from(["refresh"]).unwrap();
        assert!(matches!(input.cmd, commands::BatchCmd::Refresh));
    }

    #[test]
    fn test_invalidate_alias_parses() {
        let input = commands::BatchInput::try_parse_from(["invalidate"]).unwrap();
        assert!(matches!(input.cmd, commands::BatchCmd::Refresh));
    }

    #[test]
    fn test_store_accessor_returns_valid_ref() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // store() should return a usable Ref
        let store_ref = ctx.store();
        // Verify we can call a method on it (stats() queries the DB)
        let stats = store_ref.stats();
        assert!(stats.is_ok(), "Store should be usable via store() accessor");
    }

    // Task B2: dispatch_line bumps query_count once per non-empty line and
    // bumps error_count when the parser rejects the input. The two are
    // independent so a `cqs ping` reading both at once gets a consistent
    // pair (parse-error queries are still queries).
    #[test]
    fn test_dispatch_line_bumps_query_counter() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();
        assert_eq!(ctx.query_count.load(Ordering::Relaxed), 0);
        assert_eq!(ctx.error_count.load(Ordering::Relaxed), 0);

        // `bogus` is not a valid BatchCmd — dispatch_line bumps both
        // counters. write to /dev/null equivalent (a Vec).
        let mut sink = Vec::new();
        ctx.dispatch_line("bogus", &mut sink);
        assert_eq!(
            ctx.query_count.load(Ordering::Relaxed),
            1,
            "every non-empty line is a query, even parse failures"
        );
        assert_eq!(
            ctx.error_count.load(Ordering::Relaxed),
            1,
            "clap rejection bumps error_count"
        );

        // `stats` parses fine but the underlying handler may or may not
        // succeed against the empty test store. The key invariant is that
        // query_count goes up regardless. Error count only goes up if the
        // handler errors — we don't pin that here because Stats may
        // legitimately succeed against an init-only store.
        sink.clear();
        ctx.dispatch_line("stats", &mut sink);
        assert_eq!(
            ctx.query_count.load(Ordering::Relaxed),
            2,
            "second call bumps to 2 regardless of dispatch outcome"
        );

        // Empty / whitespace lines must NOT bump either counter — they
        // never reached the dispatcher in pre-B2 behaviour either.
        sink.clear();
        ctx.dispatch_line("", &mut sink);
        ctx.dispatch_line("   ", &mut sink);
        assert_eq!(ctx.query_count.load(Ordering::Relaxed), 2);
    }

    // Task B2: ping_snapshot returns a coherent picture even on an empty
    // BatchContext (no commands run yet, no embedder warmed). Pins the
    // initial values so the CLI can rely on the field shape.
    #[test]
    fn test_ping_snapshot_initial_state() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        let resp = ctx.ping_snapshot();
        assert_eq!(resp.error_count, 0);
        assert_eq!(resp.total_queries, 0);
        // Reranker isn't lazy-loaded by anything in the test fixture.
        assert!(!resp.reranker_loaded);
        // SPLADE encoder slot stays unpopulated until first query that
        // needs it; ping must not trigger init.
        assert!(!resp.splade_loaded);
        // Model name comes from the test context's resolved ModelConfig
        // — non-empty regardless of which model the env points at.
        assert!(!resp.model.is_empty(), "model name should be populated");
        assert!(resp.dim > 0, "dim should be populated, got {}", resp.dim);
        // index.db exists in the test store, so last_indexed_at is Some.
        assert!(
            resp.last_indexed_at.is_some(),
            "test store has index.db, so mtime should be readable"
        );
    }

    // Task B2: ping_snapshot reflects counter bumps from dispatch_line
    // — the integration that gives `cqs ping` its value.
    #[test]
    fn test_ping_snapshot_reflects_counters() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        let mut sink = Vec::new();
        // Three dispatches: one parse error, two parse-ok stats calls.
        ctx.dispatch_line("bogus_cmd", &mut sink);
        sink.clear();
        ctx.dispatch_line("stats", &mut sink);
        sink.clear();
        ctx.dispatch_line("stats", &mut sink);

        let resp = ctx.ping_snapshot();
        assert_eq!(
            resp.total_queries, 3,
            "ping must surface the same query_count atomic dispatch_line bumps"
        );
        assert!(
            resp.error_count >= 1,
            "at least the parse error should be counted; got {}",
            resp.error_count
        );
    }

    // TC-7: sanitize_json_floats replaces NaN in nested objects
    #[test]
    fn test_sanitize_json_floats_nan_in_object() {
        let mut val = serde_json::json!({
            "score": f64::NAN,
            "name": "foo",
            "nested": {"inner_score": f64::NAN, "ok": 1.5}
        });
        sanitize_json_floats(&mut val);
        assert!(val["score"].is_null(), "NaN should become null");
        assert!(val["nested"]["inner_score"].is_null());
        assert_eq!(val["nested"]["ok"], 1.5);
        assert_eq!(val["name"], "foo");
    }

    // TC-7: sanitize_json_floats replaces NaN in nested arrays
    #[test]
    fn test_sanitize_json_floats_nan_in_array() {
        let mut val = serde_json::json!([1.0, f64::NAN, [f64::INFINITY, 2.0]]);
        sanitize_json_floats(&mut val);
        assert_eq!(val[0], 1.0);
        assert!(val[1].is_null(), "NaN should become null");
        assert!(val[2][0].is_null(), "Infinity should become null");
        assert_eq!(val[2][1], 2.0);
    }

    // TC-7: sanitize_json_floats is no-op on clean values
    #[test]
    fn test_sanitize_json_floats_clean_passthrough() {
        let mut val = serde_json::json!({"a": 1, "b": "text", "c": [true, null, 2.5]});
        let expected = val.clone();
        sanitize_json_floats(&mut val);
        assert_eq!(val, expected);
    }

    // TC-7: write_json_line outputs valid JSON for clean values
    // Wraps payload in the standard `{data, error, version}` envelope.
    #[test]
    fn test_write_json_line_clean() {
        let val = serde_json::json!({"name": "foo", "score": 0.95});
        let mut buf = Vec::new();
        write_json_line(&mut buf, &val).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(parsed["data"]["name"], "foo");
        assert_eq!(parsed["data"]["score"], 0.95);
        assert!(parsed["error"].is_null());
        assert_eq!(
            parsed["version"],
            crate::cli::json_envelope::JSON_OUTPUT_VERSION
        );
    }

    // TC-7: write_json_line sanitizes NaN via retry path and produces valid JSON.
    // The wrapped payload still wraps in the envelope; sanitization runs on the wrap.
    #[test]
    fn test_write_json_line_nan_retry() {
        let val = serde_json::json!({"score": f64::NAN, "name": "bar"});
        let mut buf = Vec::new();
        write_json_line(&mut buf, &val).unwrap();
        let output = String::from_utf8(buf).unwrap();
        // Must be valid JSON (no panic, no NaN literal)
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert!(
            parsed["data"]["score"].is_null(),
            "NaN should be sanitized to null"
        );
        assert_eq!(parsed["data"]["name"], "bar");
    }

    // P2 #28: write_json_line streams via to_writer instead of allocating
    // an intermediate Value. The shape (data/error/version) and the version
    // literal must still match the typed Envelope::ok path so consumers
    // see one envelope across all surfaces.
    #[test]
    fn test_write_json_line_matches_envelope_ok_shape() {
        let val = serde_json::json!({"big": (0..50).collect::<Vec<_>>(), "name": "stream-test"});
        let mut buf = Vec::new();
        write_json_line(&mut buf, &val).unwrap();
        let streamed = String::from_utf8(buf).unwrap();
        let parsed_streamed: serde_json::Value = serde_json::from_str(streamed.trim()).unwrap();

        let typed = serde_json::to_value(crate::cli::json_envelope::Envelope::ok(&val)).unwrap();
        assert_eq!(
            parsed_streamed, typed,
            "streamed envelope must match typed Envelope::ok shape"
        );
    }

    // D.2: reject_null_tokens helper unit test. Pure function, no fixture
    // needed. Pins the contract both call sites depend on.
    #[test]
    fn test_reject_null_tokens_accepts_clean_input() {
        let tokens = vec!["search".to_string(), "foo".to_string(), "bar".to_string()];
        assert!(reject_null_tokens(&tokens).is_ok());
    }

    #[test]
    fn test_reject_null_tokens_rejects_nul_in_any_token() {
        // NUL embedded mid-token (the RT-INJ-2 attack shape — splits a string
        // arg downstream consumers might C-truncate).
        let tokens = vec!["search".to_string(), "foo\0bar".to_string()];
        assert_eq!(
            reject_null_tokens(&tokens),
            Err("Input contains null bytes")
        );
    }

    #[test]
    fn test_reject_null_tokens_rejects_nul_at_start() {
        let tokens = vec!["\0".to_string()];
        assert!(reject_null_tokens(&tokens).is_err());
    }

    // D.2: dispatch_line (daemon socket path) must reject NUL-byte tokens
    // with the same envelope error code (`invalid_input`) as the cmd_batch
    // stdin loop. Previously dispatch_line skipped this check entirely —
    // the daemon socket handler would forward NUL-tainted tokens to
    // commands::dispatch downstream.
    #[test]
    fn test_dispatch_line_rejects_null_byte_tokens() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        let mut sink = Vec::new();
        // shell_words::split keeps NUL bytes inside double-quoted args, so
        // this exercises the post-tokenization validation path.
        ctx.dispatch_line("search \"foo\0bar\"", &mut sink);

        let output = String::from_utf8(sink).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).expect("envelope JSON");
        assert!(
            parsed["data"].is_null(),
            "expected error envelope, got {output}"
        );
        assert_eq!(
            parsed["error"]["code"],
            crate::cli::json_envelope::error_codes::INVALID_INPUT
        );
        assert_eq!(parsed["error"]["message"], "Input contains null bytes");
        // error_count must bump so `cqs ping` reflects the rejection.
        assert!(
            ctx.error_count.load(Ordering::Relaxed) >= 1,
            "NUL rejection must bump error_count"
        );
        // query_count must NOT bump — early-return before the increment, so
        // ping's total_queries stays accurate. Mirrors the empty-tokens path.
        assert_eq!(
            ctx.query_count.load(Ordering::Relaxed),
            0,
            "NUL rejection happens before query_count bump"
        );
    }

    // P2 #51: alias for the rename suggested in the audit findings — keeps
    // the contract grep-discoverable under the new name as well.
    #[test]
    fn test_dispatch_line_handles_embedded_null_byte() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();
        let mut sink = Vec::new();
        // Embedded NUL within a double-quoted token. shell_words preserves
        // NUL bytes inside quoted strings; the validator must reject them.
        ctx.dispatch_line("search \"foo\0bar\"", &mut sink);
        let output = String::from_utf8(sink).unwrap();
        // (a) no panic — implicit by reaching this line.
        // (b) envelope error with code `invalid_input`.
        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("must produce a parseable envelope");
        assert!(
            parsed["data"].is_null(),
            "expected error envelope, got {output}"
        );
        assert_eq!(
            parsed["error"]["code"],
            crate::cli::json_envelope::error_codes::INVALID_INPUT
        );
        // (c) message identifies the rejection class without echoing the
        // raw NUL-tainted token.
        let msg = parsed["error"]["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("null byte"),
            "expected NUL-byte rejection message, got {msg:?}"
        );
        assert!(
            !msg.contains('\0'),
            "raw NUL byte must not echo into envelope message"
        );
    }

    // P2 #51: shell_words::split fails on unbalanced quotes; the dispatcher
    // must surface a parse_error envelope (no panic, no half-tokenized
    // command leaking downstream).
    #[test]
    fn test_dispatch_line_handles_unbalanced_quote() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();
        let mut sink = Vec::new();
        // Trailing unmatched double quote.
        ctx.dispatch_line("search \"unclosed", &mut sink);
        let output = String::from_utf8(sink).unwrap();
        // (a) no panic.
        // (b) envelope error with code `parse_error`.
        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("must produce a parseable envelope");
        assert!(
            parsed["data"].is_null(),
            "expected error envelope, got {output}"
        );
        assert_eq!(
            parsed["error"]["code"],
            crate::cli::json_envelope::error_codes::PARSE_ERROR,
            "unbalanced quote must emit parse_error envelope"
        );
        // error_count bumps; query_count stays at 0 because we never
        // reached the post-tokenization increment.
        assert!(
            ctx.error_count.load(Ordering::Relaxed) >= 1,
            "tokenization failure must bump error_count"
        );
        assert_eq!(
            ctx.query_count.load(Ordering::Relaxed),
            0,
            "tokenization failure happens before query_count bump"
        );
    }
}
