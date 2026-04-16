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
    config: OnceLock<cqs::config::Config>,
    reranker: OnceLock<cqs::Reranker>,
    // Time-bounded (30min expiry), not index-derived — keep OnceLock
    audit_state: OnceLock<cqs::audit::AuditMode>,
    // Mutable caches — RefCell<Option<T>> for invalidation on index change
    hnsw: RefCell<Option<std::sync::Arc<dyn VectorIndex>>>,
    base_hnsw: RefCell<Option<std::sync::Arc<dyn VectorIndex>>>,
    call_graph: RefCell<Option<std::sync::Arc<cqs::store::CallGraph>>>,
    test_chunks: RefCell<Option<std::sync::Arc<Vec<cqs::store::ChunkSummary>>>>,
    file_set: RefCell<Option<HashSet<PathBuf>>>,
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
    error_count: AtomicU64,
    /// Tracks when the last command was processed.
    /// Used to clear ONNX sessions (embedder, reranker) after idle timeout.
    last_command_time: Cell<Instant>,
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
    /// SHL-V1.25-16: timeout is configurable via `CQS_BATCH_IDLE_MINUTES`
    /// (default 5). Set to 0 to disable eviction entirely.
    pub(crate) fn sweep_idle_sessions(&self) {
        let timeout_minutes = idle_timeout_minutes();
        if timeout_minutes == 0 {
            return;
        }
        let elapsed = self.last_command_time.get().elapsed();
        let timeout = std::time::Duration::from_secs(timeout_minutes * 60);
        if elapsed < timeout {
            return;
        }
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
    pub(crate) fn dispatch_line(&self, line: &str, out: &mut impl std::io::Write) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        let tokens = match shell_words::split(trimmed) {
            Ok(t) => t,
            Err(e) => {
                let err = serde_json::json!({"error": format!("Parse error: {e}")});
                let _ = write_json_line(out, &err);
                return;
            }
        };
        if tokens.is_empty() {
            return;
        }
        self.check_idle_timeout();
        match commands::BatchInput::try_parse_from(&tokens) {
            Ok(input) => match commands::dispatch(self, input.cmd) {
                Ok(value) => {
                    let _ = write_json_line(out, &value);
                }
                Err(e) => {
                    // EH-12: use anyhow chain formatter (`:#`) so the real
                    // root cause (e.g. CUDA OOM) surfaces to daemon clients
                    // instead of the flattened top-level "embedding failed".
                    let err = serde_json::json!({"error": format!("{e:#}")});
                    let _ = write_json_line(out, &err);
                }
            },
            Err(e) => {
                let err = serde_json::json!({"error": format!("{e:#}")});
                let _ = write_json_line(out, &err);
            }
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
    pub(super) fn file_set(&self) -> Result<HashSet<PathBuf>> {
        self.check_index_staleness();
        {
            let cached = self.file_set.borrow();
            if let Some(fs) = cached.as_ref() {
                return Ok(fs.clone());
            }
        }
        let _span = tracing::info_span!("batch_file_set").entered();
        let exts: Vec<&str> = cqs::language::REGISTRY.supported_extensions().collect();
        let files = cqs::enumerate_files(&self.root, &exts, false)?;
        let set: HashSet<PathBuf> = files.into_iter().collect();
        let result = set.clone();
        *self.file_set.borrow_mut() = Some(set);
        Ok(result)
    }

    /// Get cached audit state (loaded once per session).
    /// NOT index-derived — time-bounded (30min expiry). Stays OnceLock.
    pub(super) fn audit_state(&self) -> &cqs::audit::AuditMode {
        self.audit_state
            .get_or_init(|| cqs::audit::load_audit_state(&self.cqs_dir))
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
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to parse notes.toml for batch");
                    vec![]
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

    /// Get cached project config (loaded once per session). (RM-21)
    pub(super) fn config(&self) -> &cqs::config::Config {
        self.config
            .get_or_init(|| cqs::config::Config::load(&self.root))
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
}

// ─── JSON serialization helpers ──────────────────────────────────────────────

/// Recursively replace NaN/Infinity f64 values with null in a serde_json::Value.
/// serde_json::to_string panics on NaN — this prevents that.
fn sanitize_json_floats(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f.is_nan() || f.is_infinite() {
                    *value = serde_json::Value::Null;
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                sanitize_json_floats(item);
            }
        }
        serde_json::Value::Object(map) => {
            for (_k, v) in map.iter_mut() {
                sanitize_json_floats(v);
            }
        }
        _ => {}
    }
}

/// Serialize a JSON value to a line on stdout. Sanitizes NaN/Infinity before
/// serialization to prevent serde_json panics. Returns Err on write failure
/// (broken pipe).
fn write_json_line(
    out: &mut impl std::io::Write,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    match serde_json::to_string(value) {
        Ok(s) => writeln!(out, "{}", s),
        Err(_) => {
            // NaN/Infinity in the value — sanitize and retry
            let mut sanitized = value.clone();
            sanitize_json_floats(&mut sanitized);
            match serde_json::to_string(&sanitized) {
                Ok(s) => writeln!(out, "{}", s),
                Err(e) => {
                    tracing::warn!(error = %e, "JSON serialization failed after sanitization");
                    writeln!(out, r#"{{"error":"JSON serialization failed"}}"#)
                }
            }
        }
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

    Ok(BatchContext {
        store: RefCell::new(store),
        runtime,
        embedder: OnceLock::new(),
        config: OnceLock::new(),
        reranker: OnceLock::new(),
        audit_state: OnceLock::new(),
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
        model_config: ModelConfig::resolve(None, None).apply_env_overrides(),
        index_id: Cell::new(index_id),
        // PF-V1.25-10: None means the first check runs unconditionally; the
        // 100ms rate-limit kicks in only after the first successful stat.
        last_staleness_check: Cell::new(None),
        error_count: AtomicU64::new(0),
        last_command_time: Cell::new(Instant::now()),
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
        config: OnceLock::new(),
        reranker: OnceLock::new(),
        audit_state: OnceLock::new(),
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
                let error_json = serde_json::json!({"error": format!("Parse error: {}", e)});
                match serde_json::to_string(&error_json) {
                    Ok(s) => {
                        if writeln!(stdout, "{}", s).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        if writeln!(
                            stdout,
                            r#"{{"error":"Parse error (serialization failed)"}}"#
                        )
                        .is_err()
                        {
                            break;
                        }
                    }
                }
                let _ = stdout.flush();
                continue;
            }
        };

        if tokens.is_empty() {
            continue;
        }

        // RT-INJ-2: Reject tokens containing null bytes — they can bypass
        // string processing in downstream consumers.
        if tokens.iter().any(|t| t.contains('\0')) {
            ctx.error_count.fetch_add(1, Ordering::Relaxed);
            let error_json = serde_json::json!({"error": "Input contains null bytes"});
            if write_json_line(&mut stdout, &error_json).is_err() {
                break;
            }
            continue;
        }

        // Check idle timeout — clear ONNX sessions if idle too long
        ctx.check_idle_timeout();

        // Pipeline detection: if tokens contain a standalone `|`, route to pipeline
        if pipeline::has_pipe_token(&tokens) {
            let result = pipeline::execute_pipeline(&ctx, &tokens, trimmed);
            if write_json_line(&mut stdout, &result).is_err() {
                break;
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
                        // EH-12: `:#` preserves anyhow's context chain so the
                        // root cause (e.g. CUDA OOM) reaches the caller
                        // instead of being flattened to a single message.
                        let error_json = serde_json::json!({"error": format!("{e:#}")});
                        if write_json_line(&mut stdout, &error_json).is_err() {
                            break;
                        }
                    }
                },
                Err(e) => {
                    ctx.error_count.fetch_add(1, Ordering::Relaxed);
                    let error_json = serde_json::json!({"error": format!("{e:#}")});
                    if write_json_line(&mut stdout, &error_json).is_err() {
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
        *ctx.file_set.borrow_mut() = Some(HashSet::new());
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

        // Set audit_state (stable — OnceLock, not index-derived)
        let _ = ctx.audit_state.set(cqs::audit::AuditMode {
            enabled: false,
            expires_at: None,
        });

        // Invalidate mutable caches
        ctx.invalidate().unwrap();

        // Verify stable cache survives
        assert!(
            ctx.audit_state.get().is_some(),
            "audit_state should survive invalidation"
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
    #[test]
    fn test_write_json_line_clean() {
        let val = serde_json::json!({"name": "foo", "score": 0.95});
        let mut buf = Vec::new();
        write_json_line(&mut buf, &val).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(parsed["name"], "foo");
        assert_eq!(parsed["score"], 0.95);
    }

    // TC-7: write_json_line sanitizes NaN via retry path and produces valid JSON
    #[test]
    fn test_write_json_line_nan_retry() {
        let val = serde_json::json!({"score": f64::NAN, "name": "bar"});
        let mut buf = Vec::new();
        write_json_line(&mut buf, &val).unwrap();
        let output = String::from_utf8(buf).unwrap();
        // Must be valid JSON (no panic, no NaN literal)
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert!(parsed["score"].is_null(), "NaN should be sanitized to null");
        assert_eq!(parsed["name"], "bar");
    }
}
