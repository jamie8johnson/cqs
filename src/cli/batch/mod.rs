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

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;

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
/// VRAM cost: ~3 KB per vector (769-dim × 4 bytes), so 100k chunks ≈ 300 MB.
pub(crate) struct BatchContext {
    pub store: Store,
    embedder: OnceLock<Embedder>,
    hnsw: OnceLock<Option<Box<dyn VectorIndex>>>,
    // Single-threaded by design — RefCell is correct, no Mutex needed
    refs: RefCell<HashMap<String, ReferenceIndex>>,
    pub root: PathBuf,
    pub cqs_dir: PathBuf,
    file_set: OnceLock<HashSet<PathBuf>>,
    // Intentionally never invalidated — notes/audit state fixed for session duration
    audit_state: OnceLock<cqs::audit::AuditMode>,
    notes_cache: OnceLock<Vec<cqs::note::Note>>,
    call_graph: OnceLock<cqs::store::CallGraph>,
    test_chunks: OnceLock<Vec<cqs::store::ChunkSummary>>,
    config: OnceLock<cqs::config::Config>,
    reranker: OnceLock<cqs::Reranker>,
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
    fn check_idle_timeout(&self) {
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

    /// Get or create the embedder (~500ms first call).
    pub fn embedder(&self) -> Result<&Embedder> {
        if let Some(e) = self.embedder.get() {
            return Ok(e);
        }
        let _span = tracing::info_span!("batch_embedder_init").entered();
        let e = Embedder::new()?;
        // Race is fine — OnceLock ensures only one value is stored
        let _ = self.embedder.set(e);
        Ok(self
            .embedder
            .get()
            .expect("embedder OnceLock populated by set() above"))
    }

    /// Get or build the vector index (CAGRA/HNSW/brute-force, cached).
    pub fn vector_index(&self) -> Result<Option<&dyn VectorIndex>> {
        if let Some(idx) = self.hnsw.get() {
            return Ok(idx.as_deref());
        }
        let _span = tracing::info_span!("batch_vector_index_init").entered();
        let idx = build_vector_index(&self.store, &self.cqs_dir)?;
        let _ = self.hnsw.set(idx);
        Ok(self
            .hnsw
            .get()
            .expect("hnsw OnceLock populated by set() above")
            .as_deref())
    }

    /// Get a cached reference index by name, loading on first access.
    ///
    /// Uses cached config (RM-21) and loads only the target reference (RM-16),
    /// not all references.
    pub fn get_ref(&self, name: &str) -> Result<()> {
        let refs = self.refs.borrow();
        if refs.contains_key(name) {
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
        self.refs.borrow_mut().insert(name.to_string(), found);
        Ok(())
    }

    /// Get or build the file set for staleness checks (cached).
    pub(super) fn file_set(&self) -> Result<&HashSet<PathBuf>> {
        if let Some(fs) = self.file_set.get() {
            return Ok(fs);
        }
        let _span = tracing::info_span!("batch_file_set").entered();
        let exts: Vec<&str> = cqs::language::REGISTRY.supported_extensions().collect();
        let files = cqs::enumerate_files(&self.root, &exts, false)?;
        let set: HashSet<PathBuf> = files.into_iter().collect();
        let _ = self.file_set.set(set);
        Ok(self
            .file_set
            .get()
            .expect("file_set OnceLock populated by set() above"))
    }

    /// Get cached audit state (loaded once per session).
    pub(super) fn audit_state(&self) -> &cqs::audit::AuditMode {
        self.audit_state
            .get_or_init(|| cqs::audit::load_audit_state(&self.cqs_dir))
    }

    /// Get cached notes (parsed once per session).
    pub(super) fn notes(&self) -> &[cqs::note::Note] {
        self.notes_cache.get_or_init(|| {
            let notes_path = self.root.join("docs/notes.toml");
            if notes_path.exists() {
                match cqs::note::parse_notes(&notes_path) {
                    Ok(notes) => notes,
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to parse notes.toml for batch");
                        vec![]
                    }
                }
            } else {
                vec![]
            }
        })
    }

    /// Borrow a reference index by name (must be loaded via `get_ref` first).
    ///
    /// Returns `None` if the reference hasn't been loaded yet.
    pub fn borrow_ref(&self, name: &str) -> Option<std::cell::Ref<'_, ReferenceIndex>> {
        let map = self.refs.borrow();
        if map.contains_key(name) {
            Some(std::cell::Ref::map(map, |m| {
                m.get(name).expect("checked contains_key above")
            }))
        } else {
            None
        }
    }

    /// Get or load the call graph (cached for session). (PERF-22)
    pub(super) fn call_graph(&self) -> Result<&cqs::store::CallGraph> {
        if let Some(g) = self.call_graph.get() {
            return Ok(g);
        }
        let _span = tracing::info_span!("batch_call_graph_init").entered();
        let g = self.store.get_call_graph()?;
        let _ = self.call_graph.set(g);
        Ok(self
            .call_graph
            .get()
            .expect("call_graph OnceLock populated by set() above"))
    }

    /// Get or load test chunks (cached for session).
    pub(super) fn test_chunks(&self) -> Result<&[cqs::store::ChunkSummary]> {
        if let Some(tc) = self.test_chunks.get() {
            return Ok(tc);
        }
        let _span = tracing::info_span!("batch_test_chunks_init").entered();
        let tc = self.store.find_test_chunks()?;
        let _ = self.test_chunks.set(tc);
        Ok(self
            .test_chunks
            .get()
            .expect("test_chunks OnceLock populated by set() above"))
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
) -> Result<Option<Box<dyn VectorIndex>>> {
    let _ = store; // Used only with gpu-index feature
    #[cfg(feature = "gpu-index")]
    {
        const CAGRA_THRESHOLD: u64 = 5000;
        let chunk_count = store.chunk_count().unwrap_or(0);
        if chunk_count >= CAGRA_THRESHOLD && cqs::CagraIndex::gpu_available() {
            match cqs::CagraIndex::build_from_store(store) {
                Ok(idx) => {
                    tracing::info!("Using CAGRA GPU index ({} vectors)", idx.len());
                    return Ok(Some(Box::new(idx) as Box<dyn VectorIndex>));
                }
                Err(e) => {
                    tracing::warn!("Failed to build CAGRA index, falling back to HNSW: {}", e);
                }
            }
        } else if chunk_count < CAGRA_THRESHOLD {
            tracing::debug!(
                "Index too small for CAGRA ({} < {}), using HNSW",
                chunk_count,
                CAGRA_THRESHOLD
            );
        } else {
            tracing::debug!("GPU not available, using HNSW");
        }
    }
    Ok(cqs::HnswIndex::try_load(cqs_dir))
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

/// Entry point for `cqs batch`.
pub(crate) fn cmd_batch(_cli: &super::Cli) -> Result<()> {
    let _span = tracing::info_span!("cmd_batch").entered();

    let (store, root, cqs_dir) = open_project_store()?;
    let ctx = BatchContext {
        store,
        embedder: OnceLock::new(),
        hnsw: OnceLock::new(),
        refs: RefCell::new(HashMap::new()),
        root,
        cqs_dir,
        file_set: OnceLock::new(),
        audit_state: OnceLock::new(),
        notes_cache: OnceLock::new(),
        call_graph: OnceLock::new(),
        test_chunks: OnceLock::new(),
        config: OnceLock::new(),
        reranker: OnceLock::new(),
        error_count: AtomicU64::new(0),
        last_command_time: Cell::new(Instant::now()),
    };

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read stdin line");
                break;
            }
        };

        // SEC-12: Reject lines exceeding 1MB to prevent unbounded memory allocation
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
