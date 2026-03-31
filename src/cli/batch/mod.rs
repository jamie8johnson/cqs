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
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

use anyhow::Result;
use clap::Parser;

use cqs::embedder::ModelConfig;
use cqs::index::VectorIndex;
use cqs::reference::ReferenceIndex;
use cqs::store::Store;
use cqs::Embedder;

use super::open_project_store;

/// Maximum batch stdin line length (1MB). Lines exceeding this are rejected
/// to prevent unbounded memory allocation from malicious input.
const MAX_BATCH_LINE_LEN: usize = 1_048_576;

/// Idle timeout for ONNX sessions (embedder, reranker) in minutes.
/// After this many minutes without a command, sessions are cleared to free memory.
/// Matches watch mode's ~5-minute idle clear pattern.
const IDLE_TIMEOUT_MINUTES: u64 = 5;

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
    store: RefCell<Store>,
    // Stable caches — keep OnceLock (not index-derived)
    embedder: OnceLock<Embedder>,
    config: OnceLock<cqs::config::Config>,
    reranker: OnceLock<cqs::Reranker>,
    // Time-bounded (30min expiry), not index-derived — keep OnceLock
    audit_state: OnceLock<cqs::audit::AuditMode>,
    // Mutable caches — RefCell<Option<T>> for invalidation on index change
    hnsw: RefCell<Option<std::sync::Arc<dyn VectorIndex>>>,
    call_graph: RefCell<Option<std::sync::Arc<cqs::store::CallGraph>>>,
    test_chunks: RefCell<Option<Vec<cqs::store::ChunkSummary>>>,
    file_set: RefCell<Option<HashSet<PathBuf>>>,
    notes_cache: RefCell<Option<Vec<cqs::note::Note>>>,
    // Single-threaded by design — RefCell is correct, no Mutex needed
    // RM-27: Reduced from 4 to 2 — each ReferenceIndex holds Store + HNSW (50-200MB)
    refs: RefCell<lru::LruCache<String, ReferenceIndex>>,
    pub root: PathBuf,
    pub cqs_dir: PathBuf,
    pub model_config: cqs::embedder::ModelConfig,
    /// Last-seen mtime of index.db, used to detect concurrent index updates.
    index_mtime: Cell<Option<SystemTime>>,
    error_count: AtomicU64,
    /// Tracks when the last command was processed.
    /// Used to clear ONNX sessions (embedder, reranker) after idle timeout.
    last_command_time: Cell<Instant>,
}

impl BatchContext {
    /// Check idle timeout and clear ONNX sessions if enough time has passed.
    ///
    /// Call this at the start of each command. Clears embedder and reranker
    /// sessions after IDLE_TIMEOUT_MINUTES of no commands, freeing ~500MB+.
    /// Sessions re-initialize lazily on next use.
    pub(crate) fn check_idle_timeout(&self) {
        let elapsed = self.last_command_time.get().elapsed();
        let timeout = std::time::Duration::from_secs(IDLE_TIMEOUT_MINUTES * 60);
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
        }
        self.last_command_time.set(Instant::now());
    }

    /// Check if index.db mtime changed since last access. If so, clear all
    /// mutable caches and re-open the Store (which resets its internal
    /// OnceLock caches like call_graph_cache, test_chunks_cache).
    pub(crate) fn check_index_staleness(&self) {
        let index_path = self.cqs_dir.join("index.db");
        let current_mtime = match std::fs::metadata(&index_path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return, // Can't stat — skip check
        };

        let last = self.index_mtime.get();
        if last.is_some() && last != Some(current_mtime) {
            let _span = tracing::info_span!("batch_index_invalidation").entered();
            tracing::info!("index.db mtime changed, invalidating mutable caches");
            self.invalidate_mutable_caches();

            // Re-open the Store to reset its internal OnceLock caches
            match Store::open(&index_path) {
                Ok(new_store) => {
                    *self.store.borrow_mut() = new_store;
                    tracing::info!("Store re-opened after index change");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to re-open Store after index change");
                }
            }
        }
        self.index_mtime.set(Some(current_mtime));
    }

    /// Clear all mutable caches. Called on index mtime change or manual refresh.
    fn invalidate_mutable_caches(&self) {
        *self.hnsw.borrow_mut() = None;
        *self.call_graph.borrow_mut() = None;
        *self.test_chunks.borrow_mut() = None;
        *self.file_set.borrow_mut() = None;
        *self.notes_cache.borrow_mut() = None;
        // Also clear LRU refs — reference indexes may also be stale
        self.refs.borrow_mut().clear();
    }

    /// Manually invalidate all mutable caches and re-open the Store.
    /// Used by the `refresh` batch command.
    pub(crate) fn invalidate(&self) -> Result<()> {
        let _span = tracing::info_span!("batch_manual_invalidation").entered();
        self.invalidate_mutable_caches();

        let index_path = self.cqs_dir.join("index.db");
        let new_store = Store::open(&index_path)
            .map_err(|e| anyhow::anyhow!("Failed to re-open Store: {e}"))?;
        *self.store.borrow_mut() = new_store;

        // Update mtime to current so we don't immediately re-invalidate
        if let Ok(mtime) = std::fs::metadata(&index_path).and_then(|m| m.modified()) {
            self.index_mtime.set(Some(mtime));
        }

        tracing::info!("Manual cache invalidation complete");
        Ok(())
    }

    /// Borrow the Store, checking for index staleness first.
    pub fn store(&self) -> std::cell::Ref<'_, Store> {
        self.check_index_staleness();
        self.store.borrow()
    }

    /// Get or create the embedder (~500ms first call).
    pub fn embedder(&self) -> Result<&Embedder> {
        if let Some(e) = self.embedder.get() {
            return Ok(e);
        }
        let _span = tracing::info_span!("batch_embedder_init").entered();
        let e = Embedder::new(self.model_config.clone())?;
        // Race is fine — OnceLock ensures only one value is stored
        let _ = self.embedder.set(e);
        Ok(self
            .embedder
            .get()
            .expect("embedder OnceLock populated by set() above"))
    }

    /// Get or build the vector index (CAGRA/HNSW/brute-force, cached).
    ///
    /// Checks index staleness before returning cached value. If the index.db
    /// changed, rebuilds the vector index from the fresh Store.
    /// Returns a cloneable Arc so callers can hold a reference past RefCell borrow scope.
    pub fn vector_index(&self) -> Result<Option<std::sync::Arc<dyn VectorIndex>>> {
        self.check_index_staleness();
        {
            let cached = self.hnsw.borrow();
            if let Some(arc) = cached.as_ref() {
                return Ok(Some(std::sync::Arc::clone(arc)));
            }
        }
        let _span = tracing::info_span!("batch_vector_index_init").entered();
        let store = self.store.borrow();
        let idx = build_vector_index(&store, &self.cqs_dir, self.config().ef_search)?;
        let result = idx.map(|boxed| -> std::sync::Arc<dyn VectorIndex> { boxed.into() });
        let ret = result.clone();
        *self.hnsw.borrow_mut() = result;
        Ok(ret)
    }

    /// Get a cached reference index by name, loading on first access.
    ///
    /// Uses cached config (RM-21) and loads only the target reference (RM-16),
    /// not all references.
    pub fn get_ref(&self, name: &str) -> Result<()> {
        let _span = tracing::info_span!("batch_get_ref", %name).entered();
        let refs = self.refs.borrow();
        if refs.contains(name) {
            return Ok(());
        }
        drop(refs);

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
    pub(super) fn test_chunks(&self) -> Result<Vec<cqs::store::ChunkSummary>> {
        self.check_index_staleness();
        {
            let cached = self.test_chunks.borrow();
            if let Some(tc) = cached.as_ref() {
                return Ok(tc.clone());
            }
        }
        let _span = tracing::info_span!("batch_test_chunks_init").entered();
        let store = self.store.borrow();
        let tc = store.find_test_chunks()?;
        let result = tc.clone();
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
fn build_vector_index(
    store: &Store,
    cqs_dir: &std::path::Path,
    ef_search: Option<usize>,
) -> Result<Option<Box<dyn VectorIndex>>> {
    crate::cli::build_vector_index_with_config(store, cqs_dir, ef_search)
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
    let (store, root, cqs_dir) = open_project_store()?;

    // Capture initial index.db mtime
    let index_mtime = std::fs::metadata(cqs_dir.join("index.db"))
        .and_then(|m| m.modified())
        .ok();
    if index_mtime.is_none() {
        tracing::debug!("Could not read index.db mtime — staleness detection will be skipped until first successful stat");
    }

    Ok(BatchContext {
        store: RefCell::new(store),
        embedder: OnceLock::new(),
        config: OnceLock::new(),
        reranker: OnceLock::new(),
        audit_state: OnceLock::new(),
        hnsw: RefCell::new(None),
        call_graph: RefCell::new(None),
        test_chunks: RefCell::new(None),
        file_set: RefCell::new(None),
        notes_cache: RefCell::new(None),
        refs: RefCell::new(lru::LruCache::new(std::num::NonZeroUsize::new(2).unwrap())),
        root,
        cqs_dir,
        model_config: ModelConfig::resolve(None, None),
        index_mtime: Cell::new(index_mtime),
        error_count: AtomicU64::new(0),
        last_command_time: Cell::new(Instant::now()),
    })
}

/// Create a BatchContext for testing with a temporary store.
#[cfg(test)]
fn create_test_context(cqs_dir: &std::path::Path) -> Result<BatchContext> {
    let index_path = cqs_dir.join("index.db");
    let store =
        Store::open(&index_path).map_err(|e| anyhow::anyhow!("Failed to open test store: {e}"))?;
    let root = cqs_dir.parent().unwrap_or(cqs_dir).to_path_buf();
    let index_mtime = std::fs::metadata(&index_path)
        .and_then(|m| m.modified())
        .ok();

    Ok(BatchContext {
        store: RefCell::new(store),
        embedder: OnceLock::new(),
        config: OnceLock::new(),
        reranker: OnceLock::new(),
        audit_state: OnceLock::new(),
        hnsw: RefCell::new(None),
        call_graph: RefCell::new(None),
        test_chunks: RefCell::new(None),
        file_set: RefCell::new(None),
        notes_cache: RefCell::new(None),
        refs: RefCell::new(lru::LruCache::new(std::num::NonZeroUsize::new(2).unwrap())),
        root,
        cqs_dir: cqs_dir.to_path_buf(),
        model_config: ModelConfig::resolve(None, None),
        index_mtime: Cell::new(index_mtime),
        error_count: AtomicU64::new(0),
        last_command_time: Cell::new(Instant::now()),
    })
}

/// Entry point for `cqs batch`.
pub(crate) fn cmd_batch() -> Result<()> {
    let _span = tracing::info_span!("cmd_batch").entered();

    let ctx = create_context()?;

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
                        let error_json = serde_json::json!({"error": format!("{}", e)});
                        if write_json_line(&mut stdout, &error_json).is_err() {
                            break;
                        }
                    }
                },
                Err(e) => {
                    ctx.error_count.fetch_add(1, Ordering::Relaxed);
                    let error_json = serde_json::json!({"error": format!("{}", e)});
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
        let index_path = cqs_dir.join("index.db");
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
        *ctx.test_chunks.borrow_mut() = Some(vec![]);

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
        let index_path = cqs_dir.join("index.db");
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
}
