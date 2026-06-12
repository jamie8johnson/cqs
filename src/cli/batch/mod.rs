//! Batch mode — persistent Store + Embedder, JSONL output
//!
//! Reads commands from stdin, executes against a shared Store and lazily-loaded
//! Embedder, outputs compact JSON per line. Amortizes ~100ms Store open and
//! ~500ms Embedder ONNX init across N commands.
//!
//! Supports pipeline syntax: `search "error" | callers | test-map` chains
//! commands where upstream names feed downstream commands via fan-out.

mod commands;
mod context;
mod handlers;
mod pipeline;
mod session;
mod view;

pub(crate) use commands::{dispatch, BatchInput};
pub(crate) use pipeline::{execute_pipeline, has_pipe_token};

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use anyhow::Result;
use clap::Parser;

use cqs::embedder::ModelConfig;
use cqs::index::VectorIndex;
use cqs::reference::ReferenceIndex;
use cqs::store::{ReadOnly, Store};
use cqs::Embedder;

use super::open_project_store_readonly;

pub(crate) use context::BatchContext;
pub(crate) use session::{cmd_batch, create_context, create_context_with_runtime};
pub(crate) use view::{checkout_view_from_arc, dispatch_via_view, BatchView};
// Shared LRU helpers live in `view` but are called from `context`'s
// `get_ref` / `get_all_refs`; surface them at the module root so the sibling
// reaches them via `use super::*`.
pub(crate) use view::{get_all_refs_via_refs_lru, get_ref_via_refs_lru};

#[cfg(test)]
pub(in crate::cli) use session::create_test_context;

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
/// WSL DrvFS / NTFS report mtime at 1-second resolution. A tight
/// `cqs index --force` followed by a daemon query burst could share the
/// same mtime bucket, causing `BatchContext` to keep serving results from
/// the orphaned inode. Mixing in inode and size closes that sub-second
/// race: the rename-over gives a new inode immediately, regardless of
/// whether the mtime ticked.
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

/// Longer idle window for the heavyweight data caches (`hnsw`,
/// `splade_index`, `call_graph`, `test_chunks`, `notes_cache`,
/// `file_set`). The ONNX-session timeout (`CQS_BATCH_IDLE_MINUTES`,
/// default 5 min) is tuned so the *next* user query stays responsive —
/// reloading an ONNX model is ~500 ms. The data caches cost much more to
/// rebuild (HNSW + SPLADE inverted index can take seconds), so we hold
/// them for a longer window before invalidating. 30 min mirrors the
/// audit-mode auto-expire window and is a safe default for an
/// interactive workstation.
///
/// Override via `CQS_BATCH_DATA_IDLE_MINUTES`. Set to `0` to disable
/// data-cache eviction entirely.
const DEFAULT_DATA_CACHE_IDLE_MINUTES: u64 = 30;

/// Resolve the data-cache idle-timeout minutes from env; 0 disables data-
/// cache eviction entirely.
fn data_cache_idle_timeout_minutes() -> u64 {
    std::env::var("CQS_BATCH_DATA_IDLE_MINUTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DATA_CACHE_IDLE_MINUTES)
}

/// Default TTL (seconds) for the `audit_state` reload cache. The audit-mode
/// file (`.cqs/audit-mode.json`) carries its own embedded `expires_at`.
/// Re-reading every 30 s on each query is cheap (sub-ms file read) and bounds
/// the staleness so the 30-min audit-mode auto-expire fires while the daemon
/// is up, and `cqs audit-mode on` after daemon boot takes effect.
const AUDIT_STATE_RELOAD_SECS_DEFAULT: u64 = 30;

/// Resolve the `audit_state` reload TTL honoring `CQS_BATCH_AUDIT_RELOAD_SECS`.
/// Falls back to [`AUDIT_STATE_RELOAD_SECS_DEFAULT`] when unset, empty,
/// unparseable, or zero.
fn audit_state_reload_interval() -> std::time::Duration {
    std::time::Duration::from_secs(cqs::limits::parse_env_u64(
        "CQS_BATCH_AUDIT_RELOAD_SECS",
        AUDIT_STATE_RELOAD_SECS_DEFAULT,
    ))
}

/// Default TTL (seconds) for the `config` reload cache. `.cqs/config.toml`
/// edits (e.g. tuning `splade_alpha` or `ef_search`) take effect after this
/// interval without a daemon restart. 5 min is long enough to avoid hot-loop
/// file reads while keeping ad-hoc config tweaks usable without
/// `systemctl restart cqs-watch`.
const CONFIG_RELOAD_SECS_DEFAULT: u64 = 5 * 60;

/// Resolve the `config` reload TTL honoring `CQS_BATCH_CONFIG_RELOAD_SECS`.
/// Falls back to [`CONFIG_RELOAD_SECS_DEFAULT`] when unset, empty,
/// unparseable, or zero.
fn config_reload_interval() -> std::time::Duration {
    std::time::Duration::from_secs(cqs::limits::parse_env_u64(
        "CQS_BATCH_CONFIG_RELOAD_SECS",
        CONFIG_RELOAD_SECS_DEFAULT,
    ))
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

/// Minimum interval between staleness probes (`fs::metadata` on `index.db`
/// plus one `PRAGMA data_version` on the long-lived probe connection) during
/// a batch session. `store()` is called on virtually every handler hop, and
/// `ctx.store()` calls `check_index_staleness` which runs both probes. Most
/// filesystem mtime resolutions are 1 ms on Linux ext4 / WSL, so polling
/// more often than ~100 ms cannot detect anything mtime-based — we just pay
/// a syscall per poll (the pragma is a connection-local counter read,
/// cheaper still). 100 ms caps the probe rate at ~10 Hz per batch session
/// while keeping reindex detection latency well under a second.
const STALENESS_CHECK_MS_DEFAULT: u64 = 100;

/// Resolve the minimum staleness-probe interval honoring
/// `CQS_BATCH_STALENESS_CHECK_MS`. Falls back to
/// [`STALENESS_CHECK_MS_DEFAULT`] when unset, empty, unparseable, or zero.
fn staleness_check_interval() -> std::time::Duration {
    std::time::Duration::from_millis(cqs::limits::parse_env_u64(
        "CQS_BATCH_STALENESS_CHECK_MS",
        STALENESS_CHECK_MS_DEFAULT,
    ))
}

/// A cached value paired with the instant it was loaded. The accessor
/// consults `loaded_at.elapsed()` against a per-field reload interval; once
/// the cache is older than the interval the value is re-loaded from the
/// underlying source. Used for `config` and `audit_state` so file edits and
/// auto-expiry take effect without a daemon restart.
struct CachedReload<T> {
    value: T,
    loaded_at: Instant,
}

/// Build the best available vector index for the store.
fn build_vector_index<Mode: cqs::store::ClearHnswDirty>(
    store: &Store<Mode>,
    cqs_dir: &std::path::Path,
    ef_search: Option<usize>,
) -> Result<Option<Box<dyn VectorIndex>>> {
    crate::cli::build_vector_index_with_config(store, cqs_dir, ef_search)
}

/// Evict the embeddings cache at `cache_path` if it exceeds its size cap.
///
/// `EmbeddingCache::evict` is a no-op below `CQS_CACHE_MAX_SIZE` (default
/// 10GB), so it's cheap to call. Opens the cache (WAL-mode SQLite, one
/// connection), runs the eviction, then drops. Used by the daemon startup and
/// the watch reindex path to keep the shared cache bounded even when the user
/// never runs a full `cqs index`.
///
/// Callers resolve `cache_path` to `<project>/.cqs/embeddings_cache.db`.
///
/// Takes an optional shared runtime so the daemon's one multi-thread pool
/// drives this open instead of spinning up a fresh `current_thread` runtime.
/// Pass `None` to fall back to the per-open runtime constructor (used by
/// non-daemon callers like `cqs index`).
pub(crate) fn evict_embeddings_cache_with_runtime(
    cache_path: &std::path::Path,
    trigger: &str,
    runtime: Option<std::sync::Arc<tokio::runtime::Runtime>>,
) {
    let _span = tracing::debug_span!(
        "daemon_cache_evict",
        trigger,
        path = %cache_path.display()
    )
    .entered();
    let cache = match cqs::cache::EmbeddingCache::open_with_runtime(cache_path, runtime.clone()) {
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

    // Same daemon tick also evicts the persistent QueryCache. The QueryCache
    // is per-user disk-resident, capped at 100 MB; one shared tick keeps both
    // caches honest without a second timer.
    let q_path = cqs::cache::QueryCache::default_path();
    if q_path.exists() {
        // Reuse the shared daemon runtime instead of spinning up a fresh
        // `current_thread` runtime every eviction tick.
        match cqs::cache::QueryCache::open_with_runtime(&q_path, runtime) {
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
// share one definition and one retry pattern.
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
/// Streams the envelope directly to `out` via a `Vec<u8>` buffer +
/// `serde_json::to_writer` instead of allocating a full intermediate
/// `serde_json::Value` for the wrap. Steady-state hot path is
/// `to_writer(payload)` (no payload clone) plus three small literal writes for
/// the `{"data":..."error":null,"version":N}` shell. The retry-on-NaN path
/// falls back to a `wrap_value` + sanitize pattern with one clone — a rare
/// failure mode (typed serde struct emitting NaN), so the clone stays bounded
/// to the recovery path. Saves multi-MB of allocator churn per dispatched
/// daemon query at scale.
fn write_json_line(
    out: &mut impl std::io::Write,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    // Per-response envelope meta: handlers may attach a reserved top-level
    // `_meta` key to their payload (e.g. `stale_origins` from the search
    // handler). Lift it out of the payload and onto the envelope — sibling
    // of `data`, the same wire position `worktree_stale` occupies — so the
    // CLI client's slim-envelope translation sees one meta channel. Rare
    // path (handlers attach `_meta` skip-when-empty), so the payload clone
    // stays off the steady-state line.
    if let Some(obj) = value.as_object() {
        if obj.contains_key("_meta") {
            return write_json_line_lifting_meta(out, value);
        }
    }

    // Steady-state: build the line in a `Vec<u8>` so the entire envelope
    // is one `writeln!` (avoids interleaved partial writes if `out` is a
    // shared TcpStream / UnixStream). Buffering also amortizes allocator
    // hits across many small literal writes.
    //
    // The envelope is opened by hand and the payload is streamed via
    // `to_writer` — no intermediate `Value` allocation.
    //
    // Slim shape: drop `error: null` and `version` (always-redundant on the
    // success path) and skip `_meta` when empty — the hot-path
    // `meta_json_fragment` returns "" in that case so the splice is a no-op.
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    buf.extend_from_slice(b"{\"data\":");
    match serde_json::to_writer(&mut buf, value) {
        Ok(()) => {
            // The fragment is "" when meta is empty; ",\"_meta\":{...}"
            // otherwise (e.g. stale worktree). Splice verbatim.
            buf.extend_from_slice(crate::cli::json_envelope::meta_json_fragment().as_bytes());
            buf.push(b'}');
            buf.push(b'\n');
            out.write_all(&buf)
        }
        Err(first) => {
            // NaN / Infinity in the payload caused `to_writer` to fail
            // partway through. The buffer holds a half-written prefix
            // (`{"data":...`) — discard it and retry via the sanitize-
            // and-retry path that the CLI / chat surfaces share.
            // Mirrors `format_envelope_to_string`'s recovery semantics.
            //
            // Preserve the first error. NaN is the typical cause but
            // `to_writer` can also fail on a downstream `io::Write` error
            // (broken socket, full disk) or on serde custom Serialize errors —
            // a sanitize-retry doesn't fix those, and the operator needs the
            // first error to diagnose.
            tracing::debug!(
                error = %first,
                "to_writer failed; retrying after float-sanitize"
            );
            let wrapped = crate::cli::json_envelope::wrap_value(value);
            let mut sanitized = wrapped;
            sanitize_json_floats(&mut sanitized);
            match serde_json::to_string(&sanitized) {
                Ok(s) => writeln!(out, "{}", s),
                Err(e) => {
                    tracing::warn!(
                        first_error = %first,
                        retry_error = %e,
                        "JSON serialization failed before AND after sanitization"
                    );
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

/// Cold path of [`write_json_line`]: the payload carries a reserved
/// top-level `_meta` key. Remove it from the payload, merge its entries
/// with the process-level meta (worktree state), and emit
/// `{"data": <payload-without-_meta>, "_meta": {...merged...}}`.
///
/// Sanitizes NaN / Infinity up front — this path already owns a payload
/// clone, so the streamed `to_writer` + retry dance of the hot path buys
/// nothing here.
fn write_json_line_lifting_meta(
    out: &mut impl std::io::Write,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let mut payload = value.clone();
    let per_meta = match payload.as_object_mut().and_then(|o| o.remove("_meta")) {
        Some(serde_json::Value::Object(m)) => m,
        // Non-object `_meta` (handler bug) — drop it rather than emit a
        // malformed meta block.
        Some(other) => {
            tracing::warn!(
                meta = %other,
                "Payload _meta is not a JSON object; dropping per-response meta"
            );
            serde_json::Map::new()
        }
        None => serde_json::Map::new(),
    };

    let mut env = serde_json::Map::with_capacity(2);
    env.insert("data".to_string(), payload);
    if let Some(meta) = crate::cli::json_envelope::merged_meta_value(per_meta) {
        env.insert("_meta".to_string(), meta);
    }
    let mut env = serde_json::Value::Object(env);
    sanitize_json_floats(&mut env);
    match serde_json::to_string(&env) {
        Ok(s) => writeln!(out, "{}", s),
        Err(e) => {
            tracing::warn!(error = %e, "JSON serialization failed after sanitization");
            write_envelope_error(
                out,
                crate::cli::json_envelope::error_codes::INTERNAL,
                "JSON serialization failed",
            )
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

/// Reject token sequences containing NUL bytes. Returns the canonical error
/// string (caller passes to [`write_envelope_error`] with
/// `error_codes::INVALID_INPUT`) on rejection, `Ok(())` otherwise.
///
/// The daemon socket loop (`cmd_batch` stdin path) and the daemon socket
/// handler (`BatchContext::dispatch_line`) share the same downstream handlers,
/// so they share this NUL check to stay in lock-step on the rejection contract.
fn reject_null_tokens(tokens: &[String]) -> Result<(), &'static str> {
    if tokens.iter().any(|t| t.contains('\0')) {
        Err("Input contains null bytes")
    } else {
        Ok(())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::store::ModelInfo;
    use std::assert_matches;
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
        *ctx.file_set.lock().unwrap() = Some(std::sync::Arc::new(HashSet::new()));
        *ctx.notes_cache.lock().unwrap() = Some(std::sync::Arc::new(vec![]));
        *ctx.call_graph.borrow_mut() = Some(std::sync::Arc::new(
            cqs::store::CallGraph::from_string_maps(Default::default(), Default::default()),
        ));
        *ctx.test_chunks.borrow_mut() = Some(std::sync::Arc::new(vec![]));
        // Build a real cross-project context (no references → one local store)
        // and seat it in the cell so the post-invalidate clear is meaningful.
        {
            let cross = cqs::cross_project::CrossProjectContext::from_config(&ctx.root).unwrap();
            let fingerprint = cross.fingerprint().unwrap_or(0);
            *ctx.cross_project.lock().unwrap() = Some(context::CachedCrossProject {
                ctx: std::sync::Arc::new(Mutex::new(cross)),
                fingerprint,
            });
        }

        // Verify caches are populated
        assert!(ctx.file_set.lock().unwrap().is_some());
        assert!(ctx.notes_cache.lock().unwrap().is_some());
        assert!(ctx.call_graph.borrow().is_some());
        assert!(ctx.test_chunks.borrow().is_some());
        assert!(ctx.cross_project.lock().unwrap().is_some());

        // Invalidate
        let epoch_before = ctx.invalidation_epoch.load(Ordering::SeqCst);
        ctx.invalidate().unwrap();

        // Verify all mutable caches are cleared
        assert!(ctx.file_set.lock().unwrap().is_none());
        assert!(ctx.notes_cache.lock().unwrap().is_none());
        assert!(ctx.call_graph.borrow().is_none());
        assert!(ctx.test_chunks.borrow().is_none());
        assert!(ctx.hnsw.lock().unwrap().is_none());
        assert!(ctx.base_hnsw.lock().unwrap().is_none());
        assert!(
            ctx.cross_project.lock().unwrap().is_none(),
            "cross-project cell must clear on invalidation — the merged graph \
             includes the local project's edges, so a local reindex must not \
             serve a stale graph"
        );
        // Epoch bumps so in-flight view builds can't republish stale values.
        assert!(
            ctx.invalidation_epoch.load(Ordering::SeqCst) > epoch_before,
            "invalidation must bump the epoch"
        );
        assert_eq!(
            ctx.pending_invalidation.get(),
            0,
            "full clear must not leave the pending mask set"
        );
    }

    /// First `cross_project()` call on a view builds the context and writes it
    /// back into the shared cell; a second checkout serves the SAME `Arc`
    /// instead of rebuilding.
    #[test]
    fn test_cross_project_cell_write_back_and_served_from_cell() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // Cell starts empty.
        assert!(
            ctx.cross_project.lock().unwrap().is_none(),
            "cross-project cell starts empty"
        );

        // First view builds and publishes.
        let view1 = ctx.build_view(None);
        let cross1 = view1.cross_project().expect("first cross_project build");
        assert!(
            ctx.cross_project.lock().unwrap().is_some(),
            "first cross_project() must write the context back into the cell"
        );

        // Second checkout (fresh view) must serve the same cached Arc — no
        // rebuild. Arc pointer identity proves the cell was reused.
        let view2 = ctx.build_view(None);
        let cross2 = view2.cross_project().expect("second cross_project served");
        assert!(
            Arc::ptr_eq(&cross1, &cross2),
            "second checkout must serve the cached context, not rebuild a fresh one"
        );
    }

    /// A reference-config fingerprint mismatch forces a rebuild even though
    /// the cell is populated and the epoch is unchanged. Simulated by seating
    /// a context with a deliberately wrong fingerprint and confirming the next
    /// `cross_project()` replaces it.
    #[test]
    fn test_cross_project_cell_rebuilds_on_fingerprint_mismatch() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        let stale = cqs::cross_project::CrossProjectContext::from_config(&ctx.root).unwrap();
        let stale_arc = std::sync::Arc::new(Mutex::new(stale));
        *ctx.cross_project.lock().unwrap() = Some(context::CachedCrossProject {
            ctx: Arc::clone(&stale_arc),
            // Fingerprint that cannot match the real (empty-references) config.
            fingerprint: u64::MAX,
        });

        let view = ctx.build_view(None);
        let fresh = view
            .cross_project()
            .expect("rebuild on fingerprint mismatch");
        assert!(
            !Arc::ptr_eq(&stale_arc, &fresh),
            "fingerprint mismatch must rebuild, not serve the stale cached context"
        );
    }

    /// The 5 cross-project dispatch sites hit the cache: a cross-project
    /// `callers` dispatch through a view leaves the cell populated, and a
    /// second dispatch serves the same context (no rebuild). This is the
    /// representative dispatch-path assertion for the cluster — the other four
    /// sites route through the identical `ctx.cross_project()` accessor.
    #[test]
    fn test_cross_project_dispatch_populates_and_reuses_cell() {
        use super::handlers::dispatch_callers;
        use crate::cli::args::{CallersArgs, LimitArg};

        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        let args = CallersArgs {
            name: "anything".into(),
            cross_project: true,
            edge_kind: None,
            limit_arg: LimitArg { limit: 5 },
        };

        let view1 = ctx.build_view(None);
        let _ = dispatch_callers(&view1, &args).expect("first cross-project dispatch");
        let cached_after_first = {
            let guard = ctx.cross_project.lock().unwrap();
            guard
                .as_ref()
                .map(|c| Arc::clone(&c.ctx))
                .expect("dispatch must populate the cross-project cell")
        };

        let view2 = ctx.build_view(None);
        let _ = dispatch_callers(&view2, &args).expect("second cross-project dispatch");
        let cached_after_second = {
            let guard = ctx.cross_project.lock().unwrap();
            guard
                .as_ref()
                .map(|c| Arc::clone(&c.ctx))
                .expect("cell still populated")
        };

        assert!(
            Arc::ptr_eq(&cached_after_first, &cached_after_second),
            "second cross-project dispatch must reuse the cached context"
        );
    }

    #[test]
    fn test_mtime_staleness_detection() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // Populate a cache
        *ctx.notes_cache.lock().unwrap() = Some(std::sync::Arc::new(vec![]));
        assert!(ctx.notes_cache.lock().unwrap().is_some());

        // First staleness check — sets baseline mtime, no invalidation
        ctx.check_index_staleness();
        assert!(
            ctx.notes_cache.lock().unwrap().is_some(),
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
            ctx.notes_cache.lock().unwrap().is_none(),
            "Mtime change should invalidate cache"
        );
    }

    /// BatchContext freshness detection must catch a rename-over replacement
    /// even if the new file's mtime happens to match the old one. On WSL NTFS
    /// (1-s mtime resolution) a tight `cqs index --force` + query burst can
    /// share an mtime bucket; mixing inode + size into the identity detects the
    /// rename-over immediately.
    #[cfg(unix)]
    #[test]
    fn test_sub_second_rename_replacement_invalidates_cache() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // Populate a cache and run the first check to capture baseline identity.
        *ctx.notes_cache.lock().unwrap() = Some(std::sync::Arc::new(vec![]));
        ctx.check_index_staleness();
        assert!(
            ctx.notes_cache.lock().unwrap().is_some(),
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

        // Staleness checks are rate-limited to 100ms. The setup above (create
        // replacement Store + init + drop + rename) is faster than that on
        // modern disks, so clear the throttle so the check runs.
        ctx.last_staleness_check.set(None);

        // The staleness check should now invalidate even though mtime is
        // identical (rename-over with same mtime, new inode).
        ctx.check_index_staleness();
        assert!(
            ctx.notes_cache.lock().unwrap().is_none(),
            "DS-V1.25-6: rename-over replacement (same mtime, new inode) should invalidate cache"
        );
    }

    /// The daemon runs WAL mode — the watch loop's
    /// incremental writes go to `index.db-wal`, and the main file's identity
    /// (inode/size/mtime) doesn't change until checkpoint. Identity alone
    /// would serve stale caches through any number of incremental reindexes.
    /// The `PRAGMA data_version` probe must catch the commit anyway.
    #[test]
    fn test_wal_write_without_checkpoint_invalidates_cache() {
        use sqlx::{ConnectOptions, Connection};

        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // Populate a cache and run the first check to baseline both
        // discriminators (identity + data_version).
        *ctx.notes_cache.lock().unwrap() = Some(std::sync::Arc::new(vec![]));
        ctx.check_index_staleness();
        assert!(
            ctx.notes_cache.lock().unwrap().is_some(),
            "baseline check must not invalidate"
        );

        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let id_before = DbFileIdentity::from_path(&index_path).unwrap();

        // Second connection, same process (the watch-loop-vs-batch-context
        // shape): commit a write through WAL with NO checkpoint. The
        // connection stays open across the assertions — closing the last
        // writer would auto-checkpoint into the main file and let the
        // identity discriminator mask the one under test.
        let mut writer = ctx
            .runtime
            .block_on(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&index_path)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                    .connect(),
            )
            .unwrap();
        ctx.runtime
            .block_on(async {
                sqlx::query("CREATE TABLE IF NOT EXISTS wal_poke (x INTEGER)")
                    .execute(&mut writer)
                    .await?;
                sqlx::query("INSERT INTO wal_poke (x) VALUES (1)")
                    .execute(&mut writer)
                    .await?;
                Ok::<_, sqlx::Error>(())
            })
            .unwrap();

        // Precondition: the commit landed in the WAL, not the main file —
        // if identity moved, this test would prove nothing about the
        // data_version discriminator.
        assert_eq!(
            DbFileIdentity::from_path(&index_path).unwrap(),
            id_before,
            "test precondition: WAL commit must leave main-file identity unchanged"
        );

        // Clear the 100ms rate limit and re-check: data_version must fire.
        ctx.last_staleness_check.set(None);
        ctx.check_index_staleness();
        assert!(
            ctx.notes_cache.lock().unwrap().is_none(),
            "DS-V1.40-1: WAL commit with no checkpoint must invalidate caches via data_version"
        );

        let _ = ctx.runtime.block_on(writer.close());
    }

    /// After a rename-over replacement (identity invalidation), the probe
    /// connection must be re-opened against the new file — the old fd points
    /// at the deleted inode and its data_version would never move again.
    /// Pin the rebaseline by WAL-committing against the NEW file (no
    /// checkpoint) and requiring a second invalidation.
    #[cfg(unix)]
    #[test]
    fn test_probe_rebaselines_after_rename_over() {
        use sqlx::{ConnectOptions, Connection};

        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

        *ctx.notes_cache.lock().unwrap() = Some(std::sync::Arc::new(vec![]));
        ctx.check_index_staleness();
        assert!(ctx.notes_cache.lock().unwrap().is_some());

        // Rename-over (the `cqs index --force` shape).
        let replacement = cqs_dir.join("index.db.replacement");
        let store = Store::open(&replacement).unwrap();
        store.init(&ModelInfo::default()).unwrap();
        drop(store);
        std::fs::rename(&replacement, &index_path).unwrap();

        ctx.last_staleness_check.set(None);
        ctx.check_index_staleness(); // identity fires; probe rebaselines here
        assert!(
            ctx.notes_cache.lock().unwrap().is_none(),
            "identity change must invalidate"
        );

        // Repopulate, then WAL-commit against the NEW file with no checkpoint.
        *ctx.notes_cache.lock().unwrap() = Some(std::sync::Arc::new(vec![]));
        let id_before = DbFileIdentity::from_path(&index_path).unwrap();
        let mut writer = ctx
            .runtime
            .block_on(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&index_path)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                    .connect(),
            )
            .unwrap();
        ctx.runtime
            .block_on(async {
                sqlx::query("CREATE TABLE IF NOT EXISTS wal_poke (x INTEGER)")
                    .execute(&mut writer)
                    .await?;
                sqlx::query("INSERT INTO wal_poke (x) VALUES (1)")
                    .execute(&mut writer)
                    .await?;
                Ok::<_, sqlx::Error>(())
            })
            .unwrap();
        assert_eq!(
            DbFileIdentity::from_path(&index_path).unwrap(),
            id_before,
            "test precondition: WAL commit must leave main-file identity unchanged"
        );

        ctx.last_staleness_check.set(None);
        ctx.check_index_staleness();
        assert!(
            ctx.notes_cache.lock().unwrap().is_none(),
            "probe must be re-opened against the new inode after rename-over — \
             a stale probe fd would never observe this commit"
        );

        let _ = ctx.runtime.block_on(writer.close());
    }

    #[test]
    fn test_stable_caches_survive_invalidation() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // audit_state is `RefCell<Option<CachedReload>>` for time-bounded
        // reload. Populate the slot directly so the test does not depend on a
        // real .cqs/audit-mode.json being present.
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
        assert_matches!(input.cmd, commands::BatchCmd::Refresh);
    }

    #[test]
    fn test_invalidate_alias_parses() {
        let input = commands::BatchInput::try_parse_from(["invalidate"]).unwrap();
        assert_matches!(input.cmd, commands::BatchCmd::Refresh);
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

    // dispatch_line bumps query_count once per non-empty line and bumps
    // error_count when the parser rejects the input. The two are independent
    // so a `cqs ping` reading both at once gets a consistent pair (parse-error
    // queries are still queries).
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

        // Empty / whitespace lines must NOT bump either counter — they never
        // reach the dispatcher.
        sink.clear();
        ctx.dispatch_line("", &mut sink);
        ctx.dispatch_line("   ", &mut sink);
        assert_eq!(ctx.query_count.load(Ordering::Relaxed), 2);
    }

    // A response-write failure on the stdin batch surface (EPIPE, full-disk
    // redirect) must not vanish: `write_ok_tracked` warns and bumps
    // `error_count` so the agent's lost response line is observable. Drives the
    // success path (`refresh` invalidates cleanly, then writes the ok payload)
    // through a writer that errors on every write.
    #[test]
    fn test_dispatch_response_write_failure_bumps_error_count() {
        struct FailingWriter;
        impl std::io::Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "epipe"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "epipe"))
            }
        }

        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();
        assert_eq!(ctx.error_count.load(Ordering::Relaxed), 0);

        // `refresh` invalidates the caches and then writes a success payload.
        // The write fails, so write_ok_tracked must bump error_count exactly
        // once even though the dispatch itself succeeded.
        let mut out = FailingWriter;
        ctx.dispatch_line("refresh", &mut out);
        assert_eq!(
            ctx.error_count.load(Ordering::Relaxed),
            1,
            "a failed response write on a successful dispatch must bump error_count"
        );

        // Sanity: the same successful dispatch against a working writer does
        // NOT bump error_count.
        let mut ok_out = Vec::new();
        ctx.dispatch_line("refresh", &mut ok_out);
        assert_eq!(
            ctx.error_count.load(Ordering::Relaxed),
            1,
            "a successful dispatch+write leaves error_count unchanged"
        );
    }

    // ping_snapshot returns a coherent picture even on an empty BatchContext
    // (no commands run yet, no embedder warmed). Pins the initial values so the
    // CLI can rely on the field shape.
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

    // ping_snapshot reflects counter bumps from dispatch_line — the
    // integration that gives `cqs ping` its value.
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

    // sanitize_json_floats replaces NaN in nested objects
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

    // sanitize_json_floats replaces NaN in nested arrays
    #[test]
    fn test_sanitize_json_floats_nan_in_array() {
        let mut val = serde_json::json!([1.0, f64::NAN, [f64::INFINITY, 2.0]]);
        sanitize_json_floats(&mut val);
        assert_eq!(val[0], 1.0);
        assert!(val[1].is_null(), "NaN should become null");
        assert!(val[2][0].is_null(), "Infinity should become null");
        assert_eq!(val[2][1], 2.0);
    }

    // sanitize_json_floats is no-op on clean values
    #[test]
    fn test_sanitize_json_floats_clean_passthrough() {
        let mut val = serde_json::json!({"a": 1, "b": "text", "c": [true, null, 2.5]});
        let expected = val.clone();
        sanitize_json_floats(&mut val);
        assert_eq!(val, expected);
    }

    // write_json_line emits the slim envelope. The `data` payload is present;
    // `error` and `version` keys are absent (always-redundant on the success
    // path).
    #[test]
    fn test_write_json_line_clean() {
        let val = serde_json::json!({"name": "foo", "score": 0.95});
        let mut buf = Vec::new();
        write_json_line(&mut buf, &val).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(parsed["data"]["name"], "foo");
        assert_eq!(parsed["data"]["score"], 0.95);
        assert!(
            parsed.get("error").is_none(),
            "slim shape drops error key on success; got: {parsed}"
        );
        assert!(
            parsed.get("version").is_none(),
            "slim shape drops version key; got: {parsed}"
        );
    }

    // write_json_line sanitizes NaN via retry path and produces valid JSON.
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

    // write_json_line (slim) and the typed Envelope::ok path (always full)
    // intentionally diverge. Pin the contract: streamed output is
    // `{"data": ...}` (no error, version, or empty _meta), and the typed
    // full envelope adds the verbose keys. Tests that need to cross-check a
    // full envelope shape use `Envelope::ok` directly.
    #[test]
    fn test_write_json_line_slim_shape() {
        let val = serde_json::json!({"big": (0..50).collect::<Vec<_>>(), "name": "stream-test"});
        let mut buf = Vec::new();
        write_json_line(&mut buf, &val).unwrap();
        let streamed = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(streamed.trim()).unwrap();

        // Slim path: `data` always; no error / version / empty _meta.
        assert_eq!(parsed["data"], val);
        assert!(parsed.get("error").is_none(), "got: {parsed}");
        assert!(parsed.get("version").is_none(), "got: {parsed}");
    }

    // Payload-level `_meta` is lifted onto the envelope, sibling of `data` —
    // the same wire position `worktree_stale` occupies. The payload itself
    // arrives meta-free so consumers never see `data._meta`.
    #[test]
    fn test_write_json_line_lifts_payload_meta_to_envelope() {
        let val = serde_json::json!({
            "query": "q",
            "results": [],
            "_meta": {"stale_origins": ["src/lib.rs"]},
        });
        let mut buf = Vec::new();
        write_json_line(&mut buf, &val).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(String::from_utf8(buf).unwrap().trim()).unwrap();
        assert_eq!(
            parsed["_meta"]["stale_origins"],
            serde_json::json!(["src/lib.rs"]),
            "per-response meta must land on the envelope; got: {parsed}"
        );
        assert!(
            parsed["data"].get("_meta").is_none(),
            "lifted meta must be removed from the data payload; got: {parsed}"
        );
        assert_eq!(parsed["data"]["query"], "q");
    }

    // Lifted-meta path sanitizes non-finite floats like the hot path does.
    #[test]
    fn test_write_json_line_lifts_meta_and_sanitizes_nan() {
        let val = serde_json::json!({
            "score": f64::NAN,
            "_meta": {"stale_origins": ["a.rs"]},
        });
        let mut buf = Vec::new();
        write_json_line(&mut buf, &val).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(String::from_utf8(buf).unwrap().trim()).unwrap();
        assert!(parsed["data"]["score"].is_null());
        assert_eq!(
            parsed["_meta"]["stale_origins"],
            serde_json::json!(["a.rs"])
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

    // dispatch_line (daemon socket path) must reject NUL-byte tokens with the
    // same envelope error code (`invalid_input`) as the cmd_batch stdin loop,
    // so the daemon socket handler doesn't forward NUL-tainted tokens to
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

    // Alias kept so the contract stays grep-discoverable under the other
    // name as well.
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

    // shell_words::split fails on unbalanced quotes; the dispatcher must
    // surface a parse_error envelope (no panic, no half-tokenized
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

    // ===== shell_words with control sequences =====
    //
    // `dispatch_line` runs the caller's raw line through `shell_words::split`
    // which is a POSIX-sh tokenizer, NOT a sanitizer. ANSI escape sequences,
    // BEL (0x07), and CR (0x0D) all survive tokenization and reach
    // `dispatch_parsed_tokens`. The NUL path is covered upstream; these pin
    // the other control-byte classes.

    /// An ANSI colour-escape sequence embedded in an argument survives
    /// tokenization and reaches the parser. What shell_words does with it
    /// depends on quoting — bare ESC passes through as a token character,
    /// producing a single-token "search" followed by an argument containing
    /// the escape bytes verbatim.
    #[test]
    fn test_dispatch_line_handles_ansi_escape_in_arg() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();
        let mut sink = Vec::new();

        // CSI red: ESC[31m ... ESC[0m. Quote the whole arg so the ESC bytes
        // stay inside one token.
        ctx.dispatch_line("search \"\x1b[31mred-query\x1b[0m\"", &mut sink);
        // (a) no panic — implicit by reaching here.
        // (b) envelope JSON produced (some result — either a successful
        //     empty search or an error envelope — not a panic-crashed pipe).
        let output = String::from_utf8(sink).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("must produce parseable envelope");
        // slim shape skips `version` on success; pin "envelope
        // produced and parseable" via the `data` key instead.
        assert!(
            parsed.get("data").is_some() || parsed.get("error").is_some(),
            "envelope must carry data or error, got {output}"
        );
        // (c) query_count bumped — ANSI-tainted input is a valid query
        //     from dispatch_line's perspective; the handler runs.
        assert_eq!(
            ctx.query_count.load(Ordering::Relaxed),
            1,
            "ANSI-tainted arg should still count as a dispatch"
        );
    }

    /// A BEL byte (0x07) in an arg is a non-control printable from the
    /// shell's point of view. shell_words preserves it; dispatch reaches
    /// the handler (which may or may not succeed depending on how the
    /// handler handles the byte).
    #[test]
    fn test_dispatch_line_handles_bel_byte_in_arg() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();
        let mut sink = Vec::new();

        ctx.dispatch_line("search \"ring\x07bell\"", &mut sink);
        // No panic + parseable envelope.
        let output = String::from_utf8(sink).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("must produce parseable envelope");
        assert!(
            parsed.get("data").is_some() || parsed.get("error").is_some(),
            "envelope must carry data or error, got {output}"
        );
    }

    /// A bare CR inside a double-quoted arg is preserved as a literal byte.
    /// shell_words does NOT treat CR as whitespace or a line terminator
    /// inside quotes. The daemon must survive the byte without crashing
    /// and without splitting the command into two lines.
    #[test]
    fn test_dispatch_line_handles_cr_in_quoted_arg() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();
        let mut sink = Vec::new();

        // CR embedded inside a quoted arg. If the split happened at CR
        // we'd get a partial command; instead we should get a single
        // "search" dispatch with the CR-containing query.
        ctx.dispatch_line("search \"foo\rbar\"", &mut sink);
        let output = String::from_utf8(sink).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("must produce parseable envelope");
        assert!(
            parsed.get("data").is_some() || parsed.get("error").is_some(),
            "envelope must be present even with CR in arg, got {output}"
        );
        // Exactly one dispatch (not two from a CR-split).
        assert_eq!(
            ctx.query_count.load(Ordering::Relaxed),
            1,
            "CR inside a quoted arg must not split the dispatch"
        );
    }

    // ===== dispatch_line happy-path envelope =====
    //
    // The existing dispatch_line tests pin error shapes (NUL, unbalanced
    // quote, bogus command, empty input). There was no positive test that
    // a known-good command produces a parseable success envelope and
    // bumps counters correctly.

    /// `ping` is the cheapest handler that exercises the full dispatch
    /// body — it needs no embedder, no index contents, no HNSW load. The
    /// response must be a valid envelope with `data` populated.
    #[test]
    fn test_dispatch_line_ping_happy_path_envelope() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();
        let mut sink = Vec::new();

        ctx.dispatch_line("ping", &mut sink);

        let output = String::from_utf8(sink).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(output.trim())
            .unwrap_or_else(|e| panic!("ping envelope must parse as JSON ({e}): {output}"));

        // slim shape: `data` populated, no `error` / `version` keys
        // (those only appear in the error envelope or under
        // CQS_OUTPUT_FORMAT=v1).
        assert!(
            parsed.get("error").is_none(),
            "ping success slim envelope drops error key, got {output}"
        );
        assert!(
            parsed["data"].is_object(),
            "ping data must be an object (PingResponse), got {output}"
        );

        // PingResponse has `total_queries` and `error_count` fields; both
        // should be numeric (0 at this point).
        assert!(
            parsed["data"]["total_queries"].is_number(),
            "ping response must have total_queries, got {output}"
        );
        assert!(
            parsed["data"]["error_count"].is_number(),
            "ping response must have error_count, got {output}"
        );

        // Counters — success bumps query_count only, not error_count.
        assert_eq!(
            ctx.query_count.load(Ordering::Relaxed),
            1,
            "a successful dispatch_line call must bump query_count"
        );
        assert_eq!(
            ctx.error_count.load(Ordering::Relaxed),
            0,
            "a successful dispatch_line call must NOT bump error_count"
        );
    }

    /// `stats` against an init-only store — another handler with no model
    /// dependency. Pins that the envelope `data` field is populated and
    /// each dispatch bumps query_count exactly once even across multiple
    /// calls.
    #[test]
    fn test_dispatch_line_stats_multiple_dispatches_bump_counter_monotonically() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        for expected in 1..=3u64 {
            let mut sink = Vec::new();
            ctx.dispatch_line("stats", &mut sink);
            let output = String::from_utf8(sink).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(output.trim())
                .unwrap_or_else(|e| panic!("stats envelope must parse ({e}): {output}"));
            assert!(
                parsed["data"].is_object() || parsed["error"].is_object(),
                "each dispatch_line call must emit a valid envelope, got {output}"
            );
            assert_eq!(
                ctx.query_count.load(Ordering::Relaxed),
                expected,
                "query_count must bump once per dispatch (expected {expected})"
            );
        }
    }

    // ===== P3.52 — dispatch_line success-envelope shape pinning =====
    //
    // The existing tests cover error/adversarial paths (NUL bytes, ANSI
    // escapes, unbalanced quotes, unknown commands) and counter bumps,
    // but no test asserts the *shape* of a successful response — that
    // `error` is `null`, `data` carries the documented fields, and the
    // envelope `version` is set. A regression that swapped `data` and
    // `error` placements (or dropped the `version` key) would slip past
    // every existing assertion.
    #[test]
    fn test_dispatch_line_stats_emits_success_envelope_shape() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();
        let mut sink = Vec::new();

        ctx.dispatch_line("stats", &mut sink);

        let output = String::from_utf8(sink).unwrap();
        let line = output.lines().next().unwrap_or("");
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stats envelope must parse as JSON ({e}): {output}"));

        // slim shape: `data` populated, no `error` / `version` keys
        // on success.
        assert!(
            parsed.get("error").is_none(),
            "stats success slim envelope drops error key, got {output}"
        );
        assert!(
            parsed["data"].is_object(),
            "stats data must be an object, got {output}"
        );

        // Stats-specific shape: `total_chunks` is the load-bearing field.
        // An init-only store reports 0; the type just has to be numeric.
        assert!(
            parsed["data"]["total_chunks"].is_number(),
            "stats response must include total_chunks (numeric), got {output}"
        );

        // Counter invariant — success bumps query, leaves errors alone.
        assert_eq!(ctx.query_count.load(Ordering::Relaxed), 1);
        assert_eq!(ctx.error_count.load(Ordering::Relaxed), 0);
    }

    // ===== Shared write-back cells + invalidation epoch =====
    //
    // All production dispatch goes through BatchView. The view snapshots the
    // BatchContext caches at checkout; on a miss it builds the value and
    // publishes it back into the shared cell (epoch-guarded) so the daemon
    // doesn't rebuild the vector index / file set / notes on every query.

    /// Minimal `VectorIndex` for cell-plumbing tests — no GPU, no disk.
    struct MockIdx;
    impl VectorIndex for MockIdx {
        fn search(&self, _query: &cqs::embedder::Embedding, _k: usize) -> Vec<cqs::IndexResult> {
            vec![]
        }
        fn len(&self) -> usize {
            1
        }
        fn name(&self) -> &'static str {
            "mock"
        }
        fn dim(&self) -> usize {
            cqs::EMBEDDING_DIM
        }
    }

    /// A view whose checkout snapshotted empty caches must (1) serve a
    /// populated shared cell without rebuilding, and (2) publish its own
    /// fallback builds back into the cells so the next checkout snapshots
    /// them instead of rebuilding.
    #[test]
    fn test_view_fallback_writes_back_to_shared_cells() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        let view = ctx.build_view(None);
        assert!(view.cached_file_set.is_none(), "first checkout is a miss");
        assert!(view.cached_notes.is_none(), "first checkout is a miss");

        let _ = view.file_set().unwrap();
        let _ = view.notes();

        assert!(
            ctx.file_set.lock().unwrap().is_some(),
            "view fallback must write the file set back into the shared cell"
        );
        assert!(
            ctx.notes_cache.lock().unwrap().is_some(),
            "view fallback must write notes back into the shared cell"
        );

        let second = ctx.build_view(None);
        assert!(
            second.cached_file_set.is_some(),
            "second checkout must snapshot the published file set"
        );
        assert!(
            second.cached_notes.is_some(),
            "second checkout must snapshot the published notes"
        );
    }

    /// The vector-index cell is shared: a view checked out before the cell
    /// was populated still serves the cell value (no rebuild), and a later
    /// checkout snapshots it.
    #[test]
    fn test_view_vector_index_served_from_shared_cell() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // Checkout while the cell is empty — snapshot is a miss.
        let view = ctx.build_view(None);
        assert!(view.cached_vector_index.is_none());

        // A sibling view publishes (simulated by writing the cell directly).
        *ctx.hnsw.lock().unwrap() = Some(std::sync::Arc::new(MockIdx));

        // The earlier view falls back to the live cell — no rebuild.
        let idx = view.vector_index().unwrap().expect("cell value served");
        assert_eq!(idx.name(), "mock");

        // The next checkout snapshots the published value.
        let second = ctx.build_view(None);
        assert!(
            second.cached_vector_index.is_some(),
            "checkout must snapshot the populated vector-index cell"
        );
    }

    /// A deferred invalidation (handler held a borrow on a cache slot while
    /// invalidation fired) must be retried by the next staleness check even
    /// though the identity / data_version discriminators were already
    /// consumed — the sticky pending flag is the only remaining trigger.
    #[test]
    fn test_deferred_invalidation_retries_via_sticky_flag() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        *ctx.notes_cache.lock().unwrap() = Some(std::sync::Arc::new(vec![]));
        *ctx.call_graph.borrow_mut() = Some(std::sync::Arc::new(
            cqs::store::CallGraph::from_string_maps(Default::default(), Default::default()),
        ));
        ctx.check_index_staleness(); // baseline both discriminators

        // Hold a borrow across the invalidation — the live shape is a search
        // handler holding a cache value while a concurrent refresh fires.
        let held = ctx.call_graph.borrow();
        ctx.invalidate().unwrap();
        drop(held);

        assert!(
            ctx.call_graph.borrow().is_some(),
            "borrowed slot must be deferred, not cleared (and not panic)"
        );
        assert!(
            ctx.notes_cache.lock().unwrap().is_none(),
            "unborrowed slots clear immediately"
        );
        assert_ne!(
            ctx.pending_invalidation.get(),
            0,
            "deferral must set the sticky pending mask"
        );

        // Nothing changed on disk since invalidate() (identity refreshed,
        // probe rebaselined, rate-limit freshly armed) — only the sticky
        // flag can drive this retry.
        ctx.check_index_staleness();
        assert!(
            ctx.call_graph.borrow().is_none(),
            "pending retry must clear the deferred slot"
        );
        assert_eq!(
            ctx.pending_invalidation.get(),
            0,
            "mask clears once every deferred slot actually cleared"
        );
    }

    /// A view-side cache build that races an invalidation must not publish:
    /// the value was built from the pre-invalidation store snapshot, and
    /// re-publishing it would serve stale data indefinitely on a quiet repo.
    #[test]
    fn test_stale_epoch_publish_is_discarded() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        let view = ctx.build_view(None);
        // Invalidation runs after checkout — mid-dispatch from the view's
        // perspective. Bumps the epoch and clears the cells.
        ctx.invalidate().unwrap();

        // The in-flight dispatch still gets its (snapshot-consistent) values…
        let _ = view.file_set().unwrap();
        let _ = view.notes();

        // …but the stale-snapshot builds must not land in the shared cells.
        assert!(
            ctx.file_set.lock().unwrap().is_none(),
            "stale-epoch file_set publish must be discarded"
        );
        assert!(
            ctx.notes_cache.lock().unwrap().is_none(),
            "stale-epoch notes publish must be discarded"
        );
    }

    /// The read direction of the epoch guard: a view checked out BEFORE an
    /// invalidation must not serve cell values published AFTER it — those
    /// belong to the next index generation, and mixing them with the view's
    /// older store snapshot returns silently wrong rowids. The stale view
    /// must fall back to building from its own snapshot (whose publish is
    /// then discarded), leaving the sibling's published values untouched.
    #[test]
    fn test_stale_view_does_not_read_post_invalidation_cells() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        // Checkout with empty snapshots at the pre-invalidation epoch.
        let view = ctx.build_view(None);

        // Invalidation runs, then a post-invalidation sibling publishes
        // next-generation values into the shared cells (simulated by
        // writing the cells directly).
        ctx.invalidate().unwrap();
        *ctx.hnsw.lock().unwrap() = Some(std::sync::Arc::new(MockIdx));
        let sentinel = PathBuf::from("/sentinel/from/next/generation");
        *ctx.file_set.lock().unwrap() = Some(std::sync::Arc::new(
            [sentinel.clone()].into_iter().collect::<HashSet<_>>(),
        ));

        // The stale view must not serve either next-generation value.
        if let Some(idx) = view.vector_index().unwrap() {
            assert_ne!(
                idx.name(),
                "mock",
                "stale view must not serve a post-invalidation cell index"
            );
        }
        let fs = view.file_set().unwrap();
        assert!(
            !fs.contains(&sentinel),
            "stale view must enumerate from its own snapshot, not read the newer cell"
        );

        // And its own (discarded) rebuilds must not overwrite the sibling's
        // published next-generation values.
        assert_eq!(
            ctx.hnsw
                .lock()
                .unwrap()
                .as_ref()
                .map(|i| i.name())
                .unwrap_or("<cleared>"),
            "mock",
            "stale view's rebuild must not displace the published index"
        );
        assert!(
            ctx.file_set
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|fs| fs.contains(&sentinel)),
            "stale view's rebuild must not displace the published file set"
        );
    }

    /// The sticky retry is clear-only and masked: it must not re-bump the
    /// epoch (which would discard every in-flight fresh publish) and must
    /// not wipe slots that already cleared and were freshly rebuilt —
    /// otherwise one contended slot reintroduces the rebuild-per-query cost
    /// for all the others.
    #[test]
    fn test_sticky_retry_is_clear_only_and_masked() {
        let (_dir, cqs_dir) = setup_test_store();
        let ctx = create_test_context(&cqs_dir).unwrap();

        *ctx.notes_cache.lock().unwrap() = Some(std::sync::Arc::new(vec![]));
        *ctx.call_graph.borrow_mut() = Some(std::sync::Arc::new(
            cqs::store::CallGraph::from_string_maps(Default::default(), Default::default()),
        ));
        ctx.check_index_staleness(); // baseline discriminators

        // Invalidation fires while a handler holds the call_graph borrow —
        // that slot defers, everything else clears.
        let held = ctx.call_graph.borrow();
        ctx.invalidate().unwrap();
        drop(held);
        assert_ne!(ctx.pending_invalidation.get(), 0);

        // A fresh post-invalidation rebuild lands in a non-deferred slot.
        *ctx.notes_cache.lock().unwrap() = Some(std::sync::Arc::new(vec![]));
        let epoch_after_invalidate = ctx.invalidation_epoch.load(Ordering::SeqCst);

        // The retry clears ONLY the deferred slot.
        ctx.check_index_staleness();
        assert_eq!(
            ctx.invalidation_epoch.load(Ordering::SeqCst),
            epoch_after_invalidate,
            "retry must not re-bump the epoch"
        );
        assert!(
            ctx.notes_cache.lock().unwrap().is_some(),
            "retry must not wipe freshly rebuilt non-deferred slots"
        );
        assert!(
            ctx.call_graph.borrow().is_none(),
            "retry must clear the deferred slot"
        );
        assert_eq!(ctx.pending_invalidation.get(), 0);
    }

    /// Each daemon-interval resolver returns its compiled default when the
    /// env var is unset and the env value when set. Garbage/zero falls back
    /// to the default (per `parse_env_u64`).
    #[test]
    fn daemon_interval_resolvers_honor_env() {
        std::env::remove_var("CQS_BATCH_STALENESS_CHECK_MS");
        assert_eq!(
            staleness_check_interval(),
            Duration::from_millis(STALENESS_CHECK_MS_DEFAULT)
        );
        std::env::set_var("CQS_BATCH_STALENESS_CHECK_MS", "250");
        assert_eq!(staleness_check_interval(), Duration::from_millis(250));
        std::env::set_var("CQS_BATCH_STALENESS_CHECK_MS", "garbage");
        assert_eq!(
            staleness_check_interval(),
            Duration::from_millis(STALENESS_CHECK_MS_DEFAULT)
        );
        std::env::remove_var("CQS_BATCH_STALENESS_CHECK_MS");

        std::env::remove_var("CQS_BATCH_AUDIT_RELOAD_SECS");
        assert_eq!(
            audit_state_reload_interval(),
            Duration::from_secs(AUDIT_STATE_RELOAD_SECS_DEFAULT)
        );
        std::env::set_var("CQS_BATCH_AUDIT_RELOAD_SECS", "5");
        assert_eq!(audit_state_reload_interval(), Duration::from_secs(5));
        std::env::remove_var("CQS_BATCH_AUDIT_RELOAD_SECS");

        std::env::remove_var("CQS_BATCH_CONFIG_RELOAD_SECS");
        assert_eq!(
            config_reload_interval(),
            Duration::from_secs(CONFIG_RELOAD_SECS_DEFAULT)
        );
        std::env::set_var("CQS_BATCH_CONFIG_RELOAD_SECS", "120");
        assert_eq!(config_reload_interval(), Duration::from_secs(120));
        std::env::remove_var("CQS_BATCH_CONFIG_RELOAD_SECS");
    }
}
